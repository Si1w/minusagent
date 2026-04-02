use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;

use crate::core::agent::Agent;
use crate::core::store::SharedStore;
use crate::frontend::Channel;
use crate::resilience::classify::{FailoverReason, classify_failure};
use crate::config::tuning;
use crate::resilience::profile::ProfileManager;

/// Three-layer resilient runner wrapping `Agent::run()`
///
/// - Layer 1: Auth rotation across profiles
/// - Layer 2: Overflow recovery via history compaction
/// - Layer 3: `Agent::run()` (the tool-use loop)
pub struct ResilienceRunner {
    profiles: ProfileManager,
    fallback_models: Vec<String>,
}

impl ResilienceRunner {
    pub fn new(profiles: ProfileManager, fallback_models: Vec<String>) -> Self {
        Self {
            profiles,
            fallback_models,
        }
    }

    /// Run the agent with three-layer resilience
    ///
    /// # Returns
    ///
    /// The `total_tokens` from the last LLM call, if available.
    ///
    /// # Errors
    ///
    /// Returns error if all profiles and fallback models are exhausted.
    pub async fn run(
        &mut self,
        store: &mut SharedStore,
        channel: &Arc<dyn Channel>,
        http: &reqwest::Client,
        interrupted: &Option<Arc<AtomicBool>>,
    ) -> Result<Option<usize>> {
        // Save original LLM config for restoration
        let original_api_key = store.state.config.llm.api_key.clone();
        let original_base_url = store.state.config.llm.base_url.clone();
        let original_model = store.state.config.llm.model.clone();

        // LAYER 1: Auth Rotation
        let result = self
            .try_with_profiles(store, channel, http, &original_model, interrupted)
            .await;

        if let Ok(tokens) = result {
            return Ok(tokens);
        }
        let last_err = result.unwrap_err();

        // All profiles exhausted — try fallback models
        for fallback_model in &self.fallback_models.clone() {
            log::warn!("resilience: trying fallback model {fallback_model}");
            store.state.config.llm.model = fallback_model.clone();

            // Reset profile cooldowns for fallback attempt
            let result = self
                .try_with_profiles(store, channel, http, fallback_model, interrupted)
                .await;

            if let Ok(tokens) = result {
                return Ok(tokens);
            }
        }

        // Restore original config before returning error
        store.state.config.llm.api_key = original_api_key;
        store.state.config.llm.base_url = original_base_url;
        store.state.config.llm.model = original_model;

        Err(anyhow::anyhow!(
            "all profiles and fallbacks exhausted. last error: {last_err}"
        ))
    }

    /// Try all available profiles with the given model
    async fn try_with_profiles(
        &mut self,
        store: &mut SharedStore,
        channel: &Arc<dyn Channel>,
        http: &reqwest::Client,
        model: &str,
        interrupted: &Option<Arc<AtomicBool>>,
    ) -> Result<Option<usize>> {
        let mut last_err = anyhow::anyhow!("no profiles available");

        for _ in 0..self.profiles.len() {
            let idx = match self.profiles.select() {
                Some(idx) => idx,
                None => break,
            };

            let profile = self.profiles.get(idx).expect("valid index");
            store.state.config.llm.api_key = profile.api_key.clone();
            if let Some(base_url) = &profile.base_url {
                store.state.config.llm.base_url = base_url.clone();
            }
            store.state.config.llm.model = model.to_string();

            // LAYER 2: Overflow Recovery
            match self.try_with_compaction(store, channel, http, interrupted).await {
                Ok(tokens) => {
                    self.profiles.mark_success(idx);
                    return Ok(tokens);
                }
                Err(e) => {
                    let reason = classify_failure(&e);
                    let cooldown = reason.default_cooldown_secs();

                    match reason {
                        FailoverReason::Overflow => {
                            // Overflow already handled in Layer 2, profile is fine
                            last_err = e;
                        }
                        _ => {
                            log::warn!(
                                "resilience: profile {idx} failed ({reason}), \
                                 cooldown {cooldown}s"
                            );
                            self.profiles.mark_failure(idx, reason, cooldown);
                            last_err = e;
                        }
                    }
                }
            }
        }

        Err(last_err)
    }

    /// Layer 2: try agent run, compact on overflow and retry
    async fn try_with_compaction(
        &self,
        store: &mut SharedStore,
        channel: &Arc<dyn Channel>,
        http: &reqwest::Client,
        interrupted: &Option<Arc<AtomicBool>>,
    ) -> Result<Option<usize>> {
        let agent = Agent;

        for attempt in 0..tuning().max_overflow_compaction {
            // LAYER 3: Agent::run() — the tool-use loop
            match agent.run(store, channel, http, interrupted.clone()).await {
                Ok(tokens) => return Ok(tokens),
                Err(e) => {
                    let reason = classify_failure(&e);

                    if reason == FailoverReason::Overflow
                        && attempt + 1 < tuning().max_overflow_compaction
                    {
                        log::warn!(
                            "resilience: overflow on attempt {}, compacting history",
                            attempt + 1
                        );
                        channel
                            .send("[resilience] Context overflow, compacting...")
                            .await;
                        Self::emergency_compact(store);
                        continue;
                    }

                    // Non-overflow or final attempt — propagate to Layer 1
                    return Err(e);
                }
            }
        }

        Err(anyhow::anyhow!("overflow compaction attempts exhausted"))
    }

    /// Emergency compaction: keep only the most recent 25% of history
    ///
    /// This is a fast, LLM-free truncation. The session-level `compact()`
    /// (which uses LLM summarization) is the preferred path; this only
    /// fires when we've already hit a context overflow error.
    fn emergency_compact(store: &mut SharedStore) {
        let total = store.context.history.len();
        if total <= 4 {
            return;
        }

        let keep = std::cmp::max(4, total / 4);
        let drain_end = total - keep;

        // Truncate oversized tool results in kept messages
        for msg in &mut store.context.history[drain_end..] {
            if let Some(ref mut content) = msg.content {
                if content.len() > 2000 {
                    content.truncate(2000);
                    content.push_str("\n[truncated]");
                }
            }
        }

        let kept = store.context.history.split_off(drain_end);
        store.context.history = kept;

        log::info!(
            "resilience: emergency compact {total} -> {} messages",
            store.context.history.len()
        );
    }

    /// Profile status for display
    pub fn profile_status(&self) -> Vec<String> {
        self.profiles.status_lines()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::{Config, Context, LLMConfig, Message, Role, SystemState};
    use std::collections::HashMap;

    use crate::team::{BackgroundManager, TodoManager};
    use crate::intelligence::manager::SharedAgents;
    use crate::resilience::profile::AuthProfile;
    use crate::routing::protocol::ToolPolicy;

    fn test_store() -> SharedStore {
        SharedStore {
            context: Context {
                system_prompt: "test".into(),
                history: (0..20)
                    .map(|i| Message {
                        role: if i % 2 == 0 { Role::User } else { Role::Assistant },
                        content: Some(format!("message {i}")),
                        tool_calls: None,
                        tool_call_id: None,
                    })
                    .collect(),
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model: "test".into(),
                        base_url: "http://localhost".into(),
                        api_key: "key".into(),
                        context_window: 128_000,
                    },
                },
                intelligence: None,
                todo: TodoManager::new(),
                is_subagent: false,
                agents: SharedAgents::empty(),
                tasks: None,
                background: BackgroundManager::new(),
                team: None,
                team_name: None,
                worktrees: None,
                tool_policy: ToolPolicy::default(),
                idle_requested: false,
                plan_mode: false,
                cron: None,
                read_file_state: HashMap::new(),
            },
        }
    }

    #[test]
    fn test_emergency_compact() {
        let mut store = test_store();
        assert_eq!(store.context.history.len(), 20);

        ResilienceRunner::emergency_compact(&mut store);
        // Keep 25% = 5 messages
        assert_eq!(store.context.history.len(), 5);
        // Kept messages are the last 5
        assert_eq!(
            store.context.history[0].content.as_deref(),
            Some("message 15")
        );
    }

    #[test]
    fn test_emergency_compact_truncates_large_content() {
        let mut store = SharedStore {
            context: Context {
                system_prompt: "test".into(),
                history: vec![
                    Message {
                        role: Role::User,
                        content: Some("short".into()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Tool,
                        content: Some("x".repeat(5000)),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::User,
                        content: Some("q1".into()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Assistant,
                        content: Some("a1".into()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::User,
                        content: Some("q2".into()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    Message {
                        role: Role::Assistant,
                        content: Some("x".repeat(3000)),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                ],
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model: "test".into(),
                        base_url: "http://localhost".into(),
                        api_key: "key".into(),
                        context_window: 128_000,
                    },
                },
                intelligence: None,
                todo: TodoManager::new(),
                is_subagent: false,
                agents: SharedAgents::empty(),
                tasks: None,
                background: BackgroundManager::new(),
                team: None,
                team_name: None,
                worktrees: None,
                tool_policy: ToolPolicy::default(),
                idle_requested: false,
                plan_mode: false,
                cron: None,
                read_file_state: HashMap::new(),
            },
        };

        ResilienceRunner::emergency_compact(&mut store);
        // 6 messages, keep 25% = max(4, 1) = 4
        assert_eq!(store.context.history.len(), 4);
        // The last message had 3000 chars, should be truncated
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().len() < 3000);
        assert!(last.content.as_ref().unwrap().ends_with("[truncated]"));
    }

    #[test]
    fn test_new_with_fallbacks() {
        let profiles = ProfileManager::new(vec![
            AuthProfile::new("key-a".into(), None),
        ]);
        let runner = ResilienceRunner::new(
            profiles,
            vec!["gpt-4o-mini".into(), "gpt-3.5-turbo".into()],
        );
        assert_eq!(runner.fallback_models.len(), 2);
    }
}
