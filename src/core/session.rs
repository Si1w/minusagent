use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::agent::Agent;
use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::frontend::{Channel, SilentChannel};

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
    fn new() -> Result<Self> {
        let base_dir = Path::new(SESSIONS_DIR).to_path_buf();
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
    /// * `id` - Session ID or unique prefix
    ///
    /// # Returns
    ///
    /// The reconstructed message history. Returns empty `Vec` if file does not exist.
    ///
    /// # Errors
    ///
    /// Returns error if `id` matches zero or multiple sessions, or if JSONL is malformed.
    fn load(&mut self, id: &str) -> Result<Vec<Message>> {
        let matched = self.match_id(id)?;
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

    fn match_id(&self, prefix: &str) -> Result<String> {
        let matched: Vec<_> = self.index.keys()
            .filter(|k| k.starts_with(prefix))
            .collect();

        match matched.len() {
            0 => Err(anyhow::anyhow!("Session not found: {prefix}")),
            1 => Ok(matched[0].clone()),
            _ => Err(anyhow::anyhow!(
                "Ambiguous prefix, matches: {}",
                matched.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

/// Session orchestrator between Frontend and Agent
///
/// Manages user turns, persistence, and `/` commands.
/// Delegates CoT reasoning to Agent.
pub struct Session {
    store: SharedStore,
    session_store: SessionStore,
    agent: Agent,
    channel: Arc<dyn Channel>,
}

impl Session {
    /// Create a new session orchestrator
    pub fn new(store: SharedStore, channel: Arc<dyn Channel>) -> Result<Self> {
        let session_store = SessionStore::new()?;
        let agent = Agent::new(channel.clone());
        Ok(Self { store, session_store, agent, channel })
    }

    /// Handle one user input: dispatch `/` commands or run agent turn
    pub async fn turn(&mut self, input: &str) -> Result<()> {
        if input.starts_with('/') {
            return self.handle_command(input).await;
        }

        self.store.context.history.push(Message {
            role: Role::User,
            content: Some(input.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        let prompt_tokens = self.agent.run(&mut self.store).await?;

        // Check if compaction is needed
        let context_window = self.store.state.config.llm.context_window;
        if let Some(tokens) = prompt_tokens {
            let threshold =
                (context_window as f64 * COMPACT_THRESHOLD) as usize;
            if tokens > threshold {
                self.channel
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

        // Serialize old messages for summarization
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

        // Temporarily swap store for summarization LLM call
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
        };
        let response = llm.run(&mut self.store).await;

        // Restore original prompt
        self.store.context.system_prompt = original_prompt;

        let summary_text = match response {
            Ok(resp) => resp.content.unwrap_or_default(),
            Err(_) => {
                // Summarization failed, just drop old messages
                self.store.context.history =
                    original_history[compress_count..].to_vec();
                return Ok(());
            }
        };

        // Build compacted history
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

    async fn handle_command(&mut self, input: &str) -> Result<()> {
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let cmd = parts[0];
        let arg = parts.get(1).unwrap_or(&"").trim();

        match cmd {
            "/new" => {
                let id = self.session_store.create(arg)?;
                self.store.context.history.clear();
                self.channel.send(&format!("Created session: {id}")).await;
            }
            "/save" => {
                self.session_store.save(&self.store.context.history)?;
                let id = self.session_store.current_id.as_deref()
                    .unwrap_or("none");
                self.channel.send(&format!("Saved session: {id}")).await;
            }
            "/load" => {
                if arg.is_empty() {
                    self.channel.send("Usage: /load <session_id>").await;
                    return Ok(());
                }
                let history = self.session_store.load(arg)?;
                let id = self.session_store.current_id.as_deref()
                    .unwrap_or("none");
                self.channel
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
                    self.channel.send("No sessions found.").await;
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
                    self.channel.send(&output).await;
                }
            }
            "/compact" => {
                if self.store.context.history.len() <= 4 {
                    self.channel
                        .send("Too few messages to compact.")
                        .await;
                } else {
                    let before = self.store.context.history.len();
                    self.compact().await?;
                    let after = self.store.context.history.len();
                    self.channel
                        .send(&format!(
                            "Compacted: {before} -> {after} messages"
                        ))
                        .await;
                }
            }
            _ => {
                self.channel
                    .send(&format!("Unknown command: {cmd}"))
                    .await;
            }
        }

        Ok(())
    }
}
