use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::frontend::{Channel, SilentChannel};
use crate::intelligence::memory::MemoryWrite;
use crate::resilience::profile::ProfileManager;
use crate::resilience::runner::ResilienceRunner;
use crate::scheduler::{LANE_SESSION, LaneLock};
use crate::scheduler::heartbeat::HeartbeatHandle;

const SESSIONS_DIR: &str = "sessions";
const COMPACT_THRESHOLD: f64 = 0.8;

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
    ) -> Result<Self> {
        let session_store = SessionStore::new(Path::new(SESSIONS_DIR))?;
        let http = reqwest::Client::new();

        let profiles = ProfileManager::from_env(
            &store.state.config.llm.api_key,
            &store.state.config.llm.base_url,
        );
        let fallbacks = ResilienceRunner::fallback_models_from_env();
        let resilience = ResilienceRunner::new(profiles, fallbacks);

        Ok(Self {
            store,
            session_store,
            resilience,
            http,
            lane_lock,
            heartbeat,
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

        let total_tokens = self
            .resilience
            .run(&mut self.store, channel, &self.http)
            .await?;

        // Check if compaction is needed
        let context_window = self.store.state.config.llm.context_window;
        if let Some(tokens) = total_tokens {
            let threshold =
                (context_window as f64 * COMPACT_THRESHOLD) as usize;
            if tokens > threshold {
                channel
                    .send("[guard] Approaching context limit, compacting...")
                    .await;
                self.compact().await?;
            }
        }

        Ok(())
    }

    /// Compact history by summarizing older messages via LLM
    ///
    /// Keeps the most recent 20% (min 4) messages intact.
    /// Summarizes the first 50% into a single user/assistant pair.
    async fn compact(&mut self) -> Result<()> {
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

        let old_messages = &self.store.context.history[..compress_count];
        let mut old_text = String::new();
        for msg in old_messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            if let Some(content) = &msg.content {
                old_text.push_str(&format!("[{role}]: {content}\n"));
            }
        }

        let summary_prompt = format!(
            "Summarize the following conversation concisely, \
             preserving key facts and decisions. \
             Output only the summary, no preamble.\n\n{old_text}"
        );

        let original_history =
            std::mem::take(&mut self.store.context.history);
        let original_prompt = self.store.context.system_prompt.clone();

        self.store.context.system_prompt =
            "You are a conversation summarizer. Be concise and factual."
                .into();
        self.store.context.history = vec![Message {
            role: Role::User,
            content: Some(summary_prompt),
            tool_calls: None,
            tool_call_id: None,
        }];

        let llm = LLMCall {
            channel: Arc::new(SilentChannel),
            http: self.http.clone(),
        };
        let response = llm.run(&mut self.store).await;

        self.store.context.system_prompt = original_prompt;

        let summary_text = match response {
            Ok(resp) => resp.content.unwrap_or_default(),
            Err(_) => {
                self.store.context.history =
                    original_history[compress_count..].to_vec();
                return Ok(());
            }
        };

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
        compacted.extend_from_slice(&original_history[compress_count..]);
        self.store.context.history = compacted;

        Ok(())
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
                         Resilience\n\
                         \x20 /profiles               Show API key profiles\n\
                         \x20 /lanes                   Show lane stats\n\
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
                            &meta.last_active[..19],
                        ));
                    }
                    channel.send(&output).await;
                }
            }
            "/compact" => {
                if self.store.context.history.len() <= 4 {
                    channel
                        .send("Too few messages to compact.")
                        .await;
                } else {
                    let before = self.store.context.history.len();
                    self.compact().await?;
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
                    self.resilience
                        .run(&mut self.store, channel, &self.http)
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
