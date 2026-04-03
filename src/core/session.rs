use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::frontend::{Channel, SilentChannel};
use crate::team::TeammateStatus;
use crate::team::todo::TodoStatus;
use crate::intelligence::memory::MemoryWrite;
use crate::resilience::profile::{AuthProfile, ProfileManager};
use crate::resilience::runner::ResilienceRunner;
use crate::config::tuning;
use crate::routing::protocol::{ControlEvent, SessionControl};
use crate::scheduler::{LANE_SESSION, LaneLock};
use crate::scheduler::heartbeat::HeartbeatHandle;

const SESSIONS_DIR: &str = "sessions";
const CLEARED_TOOL_MARKER: &str = "[Old tool result content cleared]";

/// Index entry for a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub label: String,
    pub created_at: String,
    pub last_active: String,
    pub message_count: usize,
}

/// JSONL-based session persistence
///
/// Handles create, save, load, list for session files.
struct SessionStore {
    base_dir: PathBuf,
    index_path: PathBuf,
    index: HashMap<String, SessionMeta>,
    current_id: Option<String>,
}

impl SessionStore {
    fn new(base_dir: &Path) -> Result<Self> {
        let base_dir = base_dir.to_path_buf();
        std::fs::create_dir_all(&base_dir)?;
        let index_path = base_dir.join("sessions.json");

        let index = if index_path.exists() {
            let content = std::fs::read_to_string(&index_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            HashMap::new()
        };

        Ok(Self {
            base_dir,
            index_path,
            index,
            current_id: None,
        })
    }

    /// Create a new session and set it as current
    ///
    /// # Returns
    ///
    /// The generated session ID.
    fn create(&mut self, label: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        let now = Utc::now().to_rfc3339();

        self.index.insert(id.clone(), SessionMeta {
            label: label.to_string(),
            created_at: now.clone(),
            last_active: now,
            message_count: 0,
        });
        self.save_index()?;
        self.current_id = Some(id.clone());
        Ok(id)
    }

    /// Save full history to the current session's JSONL file
    ///
    /// # Errors
    ///
    /// Returns error if no active session is set.
    fn save(&mut self, history: &[Message]) -> Result<()> {
        let id = self.current_id.as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active session. Use /new first."))?;

        let path = self.session_path(id);
        let mut content = String::new();
        for msg in history {
            let line = serde_json::to_string(msg)?;
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(&path, &content)?;

        if let Some(meta) = self.index.get_mut(id) {
            meta.last_active = Utc::now().to_rfc3339();
            meta.message_count = history.len();
        }
        self.save_index()?;
        Ok(())
    }

    /// Load session history by replaying the JSONL file
    ///
    /// # Arguments
    ///
    /// * `label` - Session label or unique prefix
    ///
    /// # Returns
    ///
    /// The reconstructed message history. Returns empty `Vec` if file does not exist.
    ///
    /// # Errors
    ///
    /// Returns error if `label` matches zero or multiple sessions, or if JSONL is malformed.
    fn load(&mut self, label: &str) -> Result<Vec<Message>> {
        let matched = self.match_label(label)?;
        let path = self.session_path(&matched);
        if !path.exists() {
            self.current_id = Some(matched);
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&path)?;
        let mut history = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let msg: Message = serde_json::from_str(line)?;
            history.push(msg);
        }
        self.current_id = Some(matched);
        Ok(history)
    }

    /// List all sessions sorted by last active time (most recent first)
    fn list(&self) -> Vec<(String, SessionMeta)> {
        let mut items: Vec<_> = self.index
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        items.sort_by(|a, b| b.1.last_active.cmp(&a.1.last_active));
        items
    }

    fn session_path(&self, id: &str) -> PathBuf {
        self.base_dir.join(format!("{id}.jsonl"))
    }

    fn save_index(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.index)?;
        std::fs::write(&self.index_path, content)?;
        Ok(())
    }

    fn match_label(&self, prefix: &str) -> Result<String> {
        let matches: Vec<_> = self.index.iter()
            .filter(|(_, meta)| meta.label.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect();

        match matches.len() {
            0 => Err(anyhow::anyhow!("Session not found: {prefix}")),
            1 => Ok(matches[0].clone()),
            _ => {
                let labels: Vec<_> = matches.iter()
                    .filter_map(|k| self.index.get(k.as_str()))
                    .map(|m| m.label.as_str())
                    .collect();
                Err(anyhow::anyhow!(
                    "Ambiguous prefix, matches: {}",
                    labels.join(", ")
                ))
            }
        }
    }
}

/// Session orchestrator between Frontend and Agent
///
/// Manages user turns, persistence, and `/` commands.
/// Delegates CoT reasoning to Agent.
/// Does not own a Channel — receives one per turn.
pub struct Session {
    store: SharedStore,
    session_store: SessionStore,
    resilience: ResilienceRunner,
    http: reqwest::Client,
    lane_lock: LaneLock,
    heartbeat: Option<HeartbeatHandle>,
    /// Shared flag for external interrupt (set by Gateway, checked in cot_loop)
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
    pub fn new(
        store: SharedStore,
        lane_lock: LaneLock,
        heartbeat: Option<HeartbeatHandle>,
        extra_profiles: Vec<AuthProfile>,
        fallback_models: Vec<String>,
        interrupted: Arc<AtomicBool>,
    ) -> Result<Self> {
        let session_store = SessionStore::new(Path::new(SESSIONS_DIR))?;
        let http = reqwest::Client::new();

        let mut all_profiles = vec![store.state.config.llm.to_auth_profile()];
        all_profiles.extend(extra_profiles);
        let profiles = ProfileManager::new(all_profiles);
        let resilience = ResilienceRunner::new(profiles, fallback_models);

        Ok(Self {
            store,
            session_store,
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
    pub async fn turn(
        &mut self,
        input: &str,
        channel: &Arc<dyn Channel>,
    ) -> Result<()> {
        self.lane_lock.mark_active(LANE_SESSION).await;
        let result = self.turn_inner(input, channel).await;
        self.lane_lock.mark_done(LANE_SESSION).await;
        result
    }

    async fn turn_inner(
        &mut self,
        input: &str,
        channel: &Arc<dyn Channel>,
    ) -> Result<()> {
        if input.starts_with('/') {
            return self.handle_command(input, channel).await;
        }

        // Rebuild system prompt if intelligence is configured
        if let Some(prompt) = self
            .store
            .state
            .intelligence
            .as_ref()
            .map(|intel| intel.build_prompt())
        {
            self.store.context.system_prompt = prompt;
        }

        self.store.context.history.push(Message {
            role: Role::User,
            content: Some(input.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        let interrupted = Some(self.interrupted.clone());
        let total_tokens = self
            .resilience
            .run(
                &mut self.store,
                channel,
                &self.http,
                &interrupted,
            )
            .await?;

        // 3-layer compaction cascade
        let context_window = self.store.state.config.llm.context_window;
        if let Some(tokens) = total_tokens {
            let threshold = (context_window as f64
                * tuning().compact_threshold) as usize;
            if tokens > threshold {
                // L1: MicroCompact — free, no API call
                let cleared = Self::micro_compact(&mut self.store);
                if cleared > 0 {
                    channel
                        .send(&format!(
                            "[compact L1] Cleared {cleared} old tool results"
                        ))
                        .await;
                }

                // Re-estimate after micro
                let est = estimate_tokens(&self.store);
                if est > threshold {
                    // L2: AutoCompact — LLM summarization
                    if self.compact_failures
                        < tuning().compact_max_failures
                    {
                        channel
                            .send("[compact L2] Summarizing history...")
                            .await;
                        match self.auto_compact().await {
                            Ok(()) => {
                                self.compact_failures = 0;
                            }
                            Err(e) => {
                                self.compact_failures += 1;
                                log::warn!(
                                    "auto-compact failed ({}/{}): {e}",
                                    self.compact_failures,
                                    tuning().compact_max_failures,
                                );
                            }
                        }
                    }

                    // Re-estimate after auto
                    let est = estimate_tokens(&self.store);
                    if est > threshold {
                        // L3: Full Compact
                        channel
                            .send(
                                "[compact L3] Full compaction...",
                            )
                            .await;
                        self.full_compact().await?;
                    }
                }
            }
        }

        Ok(())
    }

    // ── L1: MicroCompact ───────��─────────────────────────────

    /// Clear old tool-result content in-place (no API call)
    ///
    /// Replaces tool message content with a placeholder for all tool
    /// messages except those in the most recent 20% of history.
    /// Returns the number of messages cleared.
    fn micro_compact(store: &mut SharedStore) -> usize {
        let total = store.context.history.len();
        if total <= 4 {
            return 0;
        }

        let keep_recent = std::cmp::max(4, total / 5);
        let boundary = total - keep_recent;
        let mut cleared = 0;

        for msg in &mut store.context.history[..boundary] {
            if msg.role == Role::Tool {
                if let Some(ref content) = msg.content {
                    if !content.starts_with(CLEARED_TOOL_MARKER) {
                        msg.content =
                            Some(CLEARED_TOOL_MARKER.into());
                        cleared += 1;
                    }
                }
            }
        }

        if cleared > 0 {
            log::info!("micro-compact: cleared {cleared} tool results");
        }
        cleared
    }

    // ── L2: AutoCompact ──────────────────────────────────────

    /// LLM-based summarization of older history
    ///
    /// Keeps the most recent 20% (min 4) messages intact.
    /// Summarizes the first 50% into a single user/assistant pair.
    /// Budget: `compact_summary_ratio` of context window.
    async fn auto_compact(&mut self) -> Result<()> {
        let total = self.store.context.history.len();
        if total <= 4 {
            return Ok(());
        }

        let keep_count = std::cmp::max(4, total / 5);
        let compress_count = std::cmp::min(
            std::cmp::max(2, total / 2),
            total - keep_count,
        );
        if compress_count < 2 {
            return Ok(());
        }

        let ctx_window = self.store.state.config.llm.context_window;
        let summary_budget = (ctx_window as f64
            * tuning().compact_summary_ratio) as usize;
        let summary_chars = summary_budget * 4; // 1 token ≈ 4 chars

        let old_text = format_messages_for_summary(
            &self.store.context.history[..compress_count],
        );

        let summary_prompt = format!(
            "CRITICAL: Respond with plain text ONLY. \
             Do NOT call any tools.\n\n\
             Summarize the following conversation concisely, \
             preserving key facts, decisions, and file paths. \
             Keep your summary under {summary_chars} characters. \
             Output only the summary, no preamble.\n\n{old_text}"
        );

        let summary_text =
            self.run_summarizer(&summary_prompt).await?;

        let mut compacted = vec![
            Message {
                role: Role::User,
                content: Some(format!(
                    "[Previous conversation summary]\n{summary_text}"
                )),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: Some(
                    "Understood, I have the context from our \
                     previous conversation."
                        .into(),
                ),
                tool_calls: None,
                tool_call_id: None,
            },
        ];
        compacted.extend_from_slice(
            &self.store.context.history[compress_count..],
        );
        self.store.context.history = compacted;

        log::info!(
            "auto-compact: {total} -> {} messages",
            self.store.context.history.len(),
        );
        Ok(())
    }

    // ── L3: Full Compact ───────────────────��─────────────────

    /// Full conversation summary with context re-injection
    ///
    /// Summarizes the entire history, then re-injects:
    /// - Recently read file paths
    /// - Active todo items
    /// Budget: `full_compact_summary_ratio` of context window.
    async fn full_compact(&mut self) -> Result<()> {
        let total = self.store.context.history.len();
        if total <= 2 {
            return Ok(());
        }

        let ctx_window = self.store.state.config.llm.context_window;
        let summary_budget = (ctx_window as f64
            * tuning().full_compact_summary_ratio) as usize;
        let summary_chars = summary_budget * 4;

        let old_text =
            format_messages_for_summary(&self.store.context.history);

        let summary_prompt = format!(
            "CRITICAL: Respond with plain text ONLY. \
             Do NOT call any tools.\n\n\
             Summarize the following entire conversation, \
             preserving ALL key facts, decisions, file paths, \
             code changes, and current task state. \
             Keep your summary under {summary_chars} characters. \
             Output only the summary, no preamble.\n\n{old_text}"
        );

        let summary_text =
            self.run_summarizer(&summary_prompt).await?;

        // Build re-injection context
        let mut reinject = String::new();

        // Re-inject recently read file paths
        if !self.store.state.read_file_state.is_empty() {
            reinject.push_str("\n\n[Recently read files]\n");
            for path in self.store.state.read_file_state.keys() {
                reinject.push_str(&format!("- {path}\n"));
            }
        }

        // Re-inject active todo items
        let active: Vec<_> = self
            .store
            .state
            .todo
            .items
            .iter()
            .filter(|t| !matches!(t.status, TodoStatus::Completed))
            .collect();
        if !active.is_empty() {
            reinject.push_str("\n[Active tasks]\n");
            for item in &active {
                reinject.push_str(&format!(
                    "- [{:?}] {}\n",
                    item.status, item.text,
                ));
            }
        }

        self.store.context.history = vec![
            Message {
                role: Role::User,
                content: Some(format!(
                    "[Full conversation summary]\n\
                     {summary_text}{reinject}"
                )),
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: Role::Assistant,
                content: Some(
                    "Understood. I have the full context from our \
                     conversation and will continue from here."
                        .into(),
                ),
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        log::info!(
            "full-compact: {total} -> {} messages",
            self.store.context.history.len(),
        );
        Ok(())
    }

    // ── Compact helpers ────────���─────────────────────────────

    /// Run a one-shot LLM summarization call (no tools)
    ///
    /// Temporarily swaps the store's context to run the summarizer,
    /// then restores the original system prompt.
    async fn run_summarizer(
        &mut self,
        prompt: &str,
    ) -> Result<String> {
        let original_history =
            std::mem::take(&mut self.store.context.history);
        let original_prompt = self.store.context.system_prompt.clone();

        self.store.context.system_prompt =
            "You are a conversation summarizer. Be concise and factual. \
             NEVER call tools — respond with plain text only."
                .into();
        self.store.context.history = vec![Message {
            role: Role::User,
            content: Some(prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }];

        let llm = LLMCall {
            channel: Arc::new(SilentChannel),
            http: self.http.clone(),
        };
        let response = llm.run(&mut self.store).await;

        self.store.context.system_prompt = original_prompt;
        self.store.context.history = original_history;

        response.map(|resp| resp.content.unwrap_or_default())
    }

    async fn handle_command(
        &mut self,
        input: &str,
        channel: &Arc<dyn Channel>,
    ) -> Result<()> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).unwrap_or(&"").trim();

        match cmd {
            "/help" => {
                channel
                    .send(
                        "Sessions\n\
                         \x20 /new <label>            New session\n\
                         \x20 /save                   Save session\n\
                         \x20 /load <label>           Load session\n\
                         \x20 /list                   List sessions\n\
                         \x20 /compact                Compact history\n\
                         \n\
                         Intelligence\n\
                         \x20 /prompt                 Show system prompt\n\
                         \x20 /remember <name> <txt>  Save memory\n\
                         \x20 /<skill> [args]         Invoke skill\n\
                         \n\
                         Team\n\
                         \x20 /team                   Show team roster\n\
                         \x20 /inbox                  Check lead inbox\n\
                         \x20 /tasks                  Show task board\n\
                         \x20 /worktrees              List worktrees\n\
                         \x20 /events                 Worktree event log\n\
                         \n\
                         Resilience\n\
                         \x20 /profiles               Show API key profiles\n\
                         \x20 /lanes                  Show lane stats\n\
                         \n\
                         Scheduler\n\
                         \x20 /heartbeat              Heartbeat status\n\
                         \x20 /trigger                Manual heartbeat\n\
                         \n\
                         /help",
                    )
                    .await;
            }
            "/new" => {
                let id = self.session_store.create(arg)?;
                self.store.context.history.clear();
                channel.send(&format!("Created session: {id}")).await;
            }
            "/save" => {
                self.session_store.save(&self.store.context.history)?;
                let id = self.session_store.current_id.as_deref()
                    .unwrap_or("none");
                channel.send(&format!("Saved session: {id}")).await;
            }
            "/load" => {
                if arg.is_empty() {
                    channel.send("Usage: /load <session_id>").await;
                    return Ok(());
                }
                let history = self.session_store.load(arg)?;
                let id = self.session_store.current_id.as_deref()
                    .unwrap_or("none");
                channel
                    .send(&format!(
                        "Loaded session: {id} ({} messages)",
                        history.len()
                    ))
                    .await;
                self.store.context.history = history;
            }
            "/list" => {
                let sessions = self.session_store.list();
                if sessions.is_empty() {
                    channel.send("No sessions found.").await;
                } else {
                    let mut output = String::from("Sessions:\n");
                    for (id, meta) in &sessions {
                        let current =
                            if self.session_store.current_id.as_deref()
                                == Some(id.as_str())
                            {
                                " <-- current"
                            } else {
                                ""
                            };
                        let label = if meta.label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", meta.label)
                        };
                        output.push_str(&format!(
                            "  {id}{label}  msgs={}  last={}{current}\n",
                            meta.message_count,
                            meta.last_active.get(..19).unwrap_or(&meta.last_active),
                        ));
                    }
                    channel.send(&output).await;
                }
            }
            "/compact" => {
                if self.store.context.history.len() <= 2 {
                    channel
                        .send("Too few messages to compact.")
                        .await;
                } else {
                    let before = self.store.context.history.len();
                    // L1 first (free), then L3 full compact
                    Self::micro_compact(&mut self.store);
                    self.full_compact().await?;
                    let after = self.store.context.history.len();
                    channel
                        .send(&format!(
                            "Compacted: {before} -> {after} messages"
                        ))
                        .await;
                }
            }
            "/prompt" => {
                let prompt = self
                    .store
                    .state
                    .intelligence
                    .as_ref()
                    .map(|i| i.build_prompt())
                    .unwrap_or_else(|| {
                        self.store.context.system_prompt.clone()
                    });
                channel.send(&prompt).await;
            }
            "/heartbeat" => match &self.heartbeat {
                Some(handle) => {
                    if arg == "stop" {
                        handle.stop();
                        channel.send("Heartbeat stopped.").await;
                    } else if let Some(st) = handle.status().await {
                        channel
                            .send(&format!(
                                "Heartbeat:\n\
                                 \x20 enabled={}  running={}  \
                                 should_run={}\n\
                                 \x20 reason: {}\n\
                                 \x20 last_run={}  next_in={}  \
                                 interval={}s\n\
                                 \x20 active_hours={}:00-{}:00  \
                                 outputs={}",
                                st.enabled,
                                st.running,
                                st.should_run,
                                st.reason,
                                st.last_run,
                                st.next_in,
                                st.interval_secs,
                                st.active_hours.0,
                                st.active_hours.1,
                                st.queue_size,
                            ))
                            .await;
                    }
                }
                None => {
                    channel
                        .send("No heartbeat (HEARTBEAT.md not found)")
                        .await;
                }
            },
            "/trigger" => match &self.heartbeat {
                Some(handle) => {
                    let result = handle.trigger().await;
                    channel.send(&result).await;
                }
                None => {
                    channel
                        .send("No heartbeat (HEARTBEAT.md not found)")
                        .await;
                }
            },
            "/profiles" => {
                let lines = self.resilience.profile_status();
                let output = format!(
                    "Profiles ({}):\n{}",
                    lines.len(),
                    lines.join("\n")
                );
                channel.send(&output).await;
            }
            "/lanes" => {
                let stats = self.lane_lock.all_stats().await;
                if stats.is_empty() {
                    channel.send("No lanes.").await;
                } else {
                    let mut output = String::from("Lanes:\n");
                    for s in &stats {
                        output.push_str(&format!(
                            "  {:<14} active={}\n",
                            s.name,
                            s.active,
                        ));
                    }
                    channel.send(&output).await;
                }
            }
            "/team" => match &self.store.state.team {
                Some(team) => {
                    let members = team.list();
                    if members.is_empty() {
                        channel.send("No teammates.").await;
                    } else {
                        let mut output = String::from("Team:\n");
                        for m in &members {
                            let status = match m.status {
                                TeammateStatus::Working => {
                                    "working"
                                }
                                TeammateStatus::Idle => "idle",
                                TeammateStatus::Shutdown => {
                                    "shutdown"
                                }
                            };
                            output.push_str(&format!(
                                "  {} [{}] role={}\n",
                                m.name, status, m.role,
                            ));
                        }
                        let requests = team.list_requests();
                        if !requests.is_empty() {
                            output.push_str(&requests);
                        }
                        channel.send(&output).await;
                    }
                }
                None => {
                    channel
                        .send(
                            "Team not available (set WORKSPACE_DIR)",
                        )
                        .await;
                }
            },
            "/tasks" => match &self.store.state.tasks {
                Some(mgr) => match mgr.list_formatted() {
                    Ok(output) => {
                        channel.send(&output).await;
                    }
                    Err(e) => {
                        channel
                            .send(&format!("Error: {e}"))
                            .await;
                    }
                },
                None => {
                    channel
                        .send("Task system not available")
                        .await;
                }
            },
            "/worktrees" => match &self.store.state.worktrees {
                Some(wt) => {
                    channel.send(&wt.list_formatted()).await;
                }
                None => {
                    channel
                        .send("Worktree system not available")
                        .await;
                }
            },
            "/events" => match &self.store.state.worktrees {
                Some(wt) => {
                    channel.send(&wt.events()).await;
                }
                None => {
                    channel
                        .send("Worktree system not available")
                        .await;
                }
            },
            "/inbox" => match &self.store.state.team {
                Some(team) => {
                    let result = team.read_inbox("lead");
                    channel.send(&result).await;
                }
                None => {
                    channel
                        .send(
                            "Team not available (set WORKSPACE_DIR)",
                        )
                        .await;
                }
            },
            "/remember" => {
                let rem_parts: Vec<&str> =
                    arg.splitn(2, ' ').collect();
                if rem_parts.len() < 2 || rem_parts[0].is_empty() {
                    channel
                        .send("Usage: /remember <name> <content>")
                        .await;
                    return Ok(());
                }
                let name = rem_parts[0];
                let content = rem_parts[1];

                let memory_dir = self
                    .store
                    .state
                    .intelligence
                    .as_ref()
                    .map(|i| i.memory.dir().to_path_buf());

                match memory_dir {
                    Some(dir) => {
                        let node = MemoryWrite {
                            content: content.to_string(),
                            name: name.to_string(),
                            memory_dir: dir,
                            http: self.http.clone(),
                        };
                        node.run(&mut self.store).await?;
                        channel
                            .send(&format!("Memory saved: {name}"))
                            .await;
                    }
                    None => {
                        channel
                            .send(
                                "Intelligence not configured \
                                 (set WORKSPACE_DIR)",
                            )
                            .await;
                    }
                }
            }
            _ => {
                // Check if cmd matches a skill name (strip leading '/')
                let skill_name = cmd.strip_prefix('/').unwrap_or(cmd);
                let skill_body = self
                    .store
                    .state
                    .intelligence
                    .as_ref()
                    .and_then(|i| i.find_skill(skill_name))
                    .and_then(|s| s.load_body());

                if let Some(body) = skill_body {
                    let skill_input = if arg.is_empty() {
                        format!("[Skill: {cmd}]\n\n{body}")
                    } else {
                        format!(
                            "[Skill: {cmd}]\n\n{body}\n\nUser input: {arg}"
                        )
                    };
                    self.store.context.history.push(Message {
                        role: Role::User,
                        content: Some(skill_input),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                    let interrupted = Some(self.interrupted.clone());
                    self.resilience
                        .run(
                            &mut self.store,
                            channel,
                            &self.http,
                            &interrupted,
                        )
                        .await?;
                    return Ok(());
                }

                channel
                    .send(&format!("Unknown command: {cmd}"))
                    .await;
            }
        }

        Ok(())
    }

    /// Handle a control message that requires session state
    pub fn handle_control(&mut self, ctrl: SessionControl) -> ControlEvent {
        match ctrl {
            SessionControl::ContextUsage => {
                let history_len = self.store.context.history.len();
                // Rough token estimate: 4 chars ≈ 1 token
                let estimated_tokens: usize = self
                    .store
                    .context
                    .history
                    .iter()
                    .map(|m| m.content.as_deref().unwrap_or("").len() / 4)
                    .sum::<usize>()
                    + self.store.context.system_prompt.len() / 4;

                ControlEvent::ContextInfo {
                    used_tokens: estimated_tokens,
                    total_tokens: self.store.state.config.llm.context_window,
                    history_messages: history_len,
                }
            }
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
                self.store.state.config.llm.model = model.clone();
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

/// Estimate token count from store context (4 chars ≈ 1 token)
fn estimate_tokens(store: &SharedStore) -> usize {
    store
        .context
        .history
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len() / 4)
        .sum::<usize>()
        + store.context.system_prompt.len() / 4
}

/// Format messages into a text block for summarization
fn format_messages_for_summary(messages: &[Message]) -> String {
    let mut text = String::with_capacity(messages.len() * 200);
    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        if let Some(content) = &msg.content {
            text.push_str(&format!("[{role}]: {content}\n"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_message(role: Role, content: &str) -> Message {
        Message {
            role,
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn test_session_store_create_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        let id = store.create("test-label").unwrap();

        assert_eq!(store.current_id.as_deref(), Some(id.as_str()));

        let sessions = store.list();
        assert!(sessions.iter().any(|(sid, meta)| {
            sid == &id && meta.label == "test-label"
        }));
    }

    #[test]
    fn test_session_store_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        store.create("save-load-test").unwrap();

        let history = vec![
            test_message(Role::User, "hello"),
            test_message(Role::Assistant, "hi there"),
        ];

        store.save(&history).unwrap();

        let loaded = store.load("save-load-test").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].content.as_deref(), Some("hello"));
        assert_eq!(loaded[1].content.as_deref(), Some("hi there"));
    }

    #[test]
    fn test_session_store_save_no_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        let result = store.save(&[]);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("No active session")
        );
    }

    #[test]
    fn test_session_store_load_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        let result = store.load("nonexistent_id_12345");
        assert!(result.is_err());
    }

    #[test]
    fn test_session_store_prefix_match() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        store.create("prefix-test").unwrap();

        let loaded = store.load("prefix").unwrap();
        assert!(loaded.is_empty());
    }
}
