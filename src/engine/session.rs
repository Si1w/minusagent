mod command;
mod compact;
mod store;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

use self::command::handle_command;
use self::store::SessionStore;

use crate::engine::store::SharedStore;
use crate::frontend::Channel;
use crate::resilience::profile::{AuthProfile, ProfileManager};
use crate::resilience::runner::ResilienceRunner;
use crate::routing::protocol::{ControlEvent, SessionControl};
use crate::scheduler::heartbeat::HeartbeatHandle;
use crate::scheduler::{LANE_SESSION, LaneLock};

/// Session orchestrator between Frontend and Agent
///
/// Manages user turns, persistence, and `/` commands.
/// Delegates `CoT` reasoning to Agent.
/// Does not own a Channel — receives one per turn.
pub struct Session {
    store: SharedStore,
    sessions: SessionStore,
    resilience: ResilienceRunner,
    http: reqwest::Client,
    lane_lock: LaneLock,
    heartbeat: Option<HeartbeatHandle>,
    /// Shared flag for external interrupt (set by Gateway, checked in `cot_loop`)
    interrupted: Arc<AtomicBool>,
    /// Consecutive auto-compact failures (circuit breaker for L2)
    compact_failures: usize,
}

impl Session {
    /// Create a new session orchestrator
    ///
    /// # Arguments
    ///
    /// * `store` - Shared state container
    /// * `lane_lock` - Per-session lane lock for user/background priority
    /// * `heartbeat` - Heartbeat handle if HEARTBEAT.md exists
    ///
    /// # Errors
    ///
    /// Returns an error if the backing session store cannot be created.
    pub fn new(
        store: SharedStore,
        lane_lock: LaneLock,
        heartbeat: Option<HeartbeatHandle>,
        extra_profiles: Vec<AuthProfile>,
        fallback_models: Vec<String>,
        interrupted: Arc<AtomicBool>,
    ) -> Result<Self> {
        let sessions = SessionStore::new_default()?;
        let http = reqwest::Client::new();

        let mut all_profiles = vec![store.state.config.llm.to_auth_profile()];
        all_profiles.extend(extra_profiles);
        let profiles = ProfileManager::new(all_profiles);
        let resilience = ResilienceRunner::new(profiles, fallback_models);

        Ok(Self {
            store,
            sessions,
            resilience,
            http,
            lane_lock,
            heartbeat,
            interrupted,
            compact_failures: 0,
        })
    }

    /// Handle one user input: dispatch `/` commands or run agent turn
    ///
    /// Marks the session lane as active for the duration, so background
    /// tasks (heartbeat) yield when a user is active.
    ///
    /// # Errors
    ///
    /// Returns an error if command handling, model execution, or post-turn
    /// compaction fails.
    pub async fn turn(&mut self, input: &str, channel: &Arc<dyn Channel>) -> Result<()> {
        self.lane_lock.mark_active(LANE_SESSION).await;
        let result = self.turn_inner(input, channel).await;
        self.lane_lock.mark_done(LANE_SESSION).await;
        result
    }

    async fn turn_inner(&mut self, input: &str, channel: &Arc<dyn Channel>) -> Result<()> {
        if input.starts_with('/') {
            return handle_command(self, input, channel).await;
        }

        self.run_user_turn(input, channel).await
    }

    async fn run_user_turn(&mut self, input: &str, channel: &Arc<dyn Channel>) -> Result<()> {
        self.refresh_system_prompt();
        self.push_user_message(input);
        let total_tokens = self.run_resilience_turn(channel).await?;
        compact::compact_after_turn(self, total_tokens, channel).await
    }

    fn refresh_system_prompt(&mut self) {
        if let Some(prompt) = self
            .store
            .state
            .intelligence
            .as_ref()
            .map(crate::intelligence::Intelligence::build_prompt)
        {
            self.store.context.system_prompt = prompt;
        }
    }

    fn push_user_message(&mut self, input: &str) {
        self.store
            .context
            .history
            .push(crate::engine::store::Message {
                role: crate::engine::store::Role::User,
                content: Some(input.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
    }

    async fn run_resilience_turn(&mut self, channel: &Arc<dyn Channel>) -> Result<Option<usize>> {
        let interrupted = Some(self.interrupted.clone());
        self.resilience
            .run(&mut self.store, channel, &self.http, interrupted.as_ref())
            .await
    }

    /// Handle a control message that requires session state
    pub fn handle_control(&mut self, ctrl: SessionControl) -> ControlEvent {
        match ctrl {
            SessionControl::ContextUsage => ControlEvent::ContextInfo {
                used_tokens: compact::estimate_tokens(&self.store),
                total_tokens: self.store.state.config.llm.context_window,
                history_messages: self.store.context.history.len(),
            },
            SessionControl::Rewind { count } => {
                let len = self.store.context.history.len();
                let remove = count.min(len);
                self.store.context.history.truncate(len - remove);
                ControlEvent::Rewound {
                    removed: remove,
                    remaining: self.store.context.history.len(),
                }
            }
            SessionControl::ModelSwitch { model } => {
                self.store.state.config.llm.model.clone_from(&model);
                ControlEvent::TurnComplete {
                    text: Some(format!("model → {model}")),
                }
            }
            SessionControl::SetPermissionMode { mode } => {
                self.store.state.tool_policy.mode = mode;
                ControlEvent::TurnComplete {
                    text: Some("permission mode updated".into()),
                }
            }
        }
    }
}
