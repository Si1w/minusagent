use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::frontend::{Channel, UserMessage};

// ── Permission ──────────────────────────────────────────────

/// Permission mode for tool execution
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Always ask before tool execution
    #[default]
    Ask,
    /// Auto-approve read-only tools, ask for write/exec
    Auto,
    /// Never ask, approve all
    Trust,
}

/// Read-only tools that are auto-approved in `Auto` mode
const AUTO_APPROVE_TOOLS: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "todo",
    "task_list",
    "task_get",
    "team_read_inbox",
    "worktree_list",
    "background_check",
];

/// Per-session tool permission policy
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    pub mode: PermissionMode,
    /// Per-tool overrides (tool_name → allow)
    pub overrides: HashMap<String, bool>,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            mode: PermissionMode::default(),
            overrides: HashMap::new(),
        }
    }
}

impl ToolPolicy {
    /// Check if a tool should be auto-approved without asking
    ///
    /// Returns `Some(true)` to allow, `Some(false)` to deny,
    /// `None` to defer to `channel.confirm()`.
    pub fn auto_approve(&self, tool: &str) -> Option<bool> {
        if let Some(&allow) = self.overrides.get(tool) {
            return Some(allow);
        }
        match self.mode {
            PermissionMode::Trust => Some(true),
            PermissionMode::Auto => {
                if AUTO_APPROVE_TOOLS.contains(&tool) {
                    Some(true)
                } else {
                    None
                }
            }
            PermissionMode::Ask => None,
        }
    }
}

// ── Client → Server ─────────────────────────────────────────

/// Control message sent from client to server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    /// Initialize or resume a session
    Init {
        #[serde(default)]
        agent_id: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        permission_mode: Option<PermissionMode>,
    },
    /// Send user text to the agent
    UserMessage {
        text: String,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        peer_id: Option<String>,
        #[serde(default)]
        account_id: Option<String>,
        #[serde(default)]
        guild_id: Option<String>,
    },
    /// Interrupt current agent execution
    Interrupt,
    /// Switch LLM model
    ModelSwitch {
        model: String,
    },
    /// Query context window usage
    ContextUsage,
    /// Remove last N messages from history
    Rewind {
        count: usize,
    },
    /// Respond to a tool permission request
    ToolResponse {
        request_id: String,
        allow: bool,
    },
    /// Set permission mode for tool execution
    SetPermissionMode {
        mode: PermissionMode,
    },
}

// ── Server → Client ─────────────────────────────────────────

/// Control event sent from server to client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    /// Session initialized
    SessionReady {
        session_key: String,
        agent_id: String,
        model: String,
    },
    /// Streaming LLM output chunk
    StreamDelta {
        text: String,
    },
    /// Tool permission request (can_use_tool)
    ToolRequest {
        request_id: String,
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    /// Tool execution completed
    ToolResult {
        tool: String,
        success: bool,
        output: String,
    },
    /// Agent turn completed
    TurnComplete {
        #[serde(default)]
        text: Option<String>,
    },
    /// Context usage information
    ContextInfo {
        used_tokens: usize,
        total_tokens: usize,
        history_messages: usize,
    },
    /// Messages rewound
    Rewound {
        removed: usize,
        remaining: usize,
    },
    /// Error
    Error {
        code: i32,
        message: String,
    },
}

// ── Session-level control ───────────────────────────────────

/// Control message subset routed to a specific session
///
/// These require access to session state and are sent via the
/// session's mpsc channel alongside regular turns.
/// Note: Interrupt is handled at the gateway level (sets AtomicBool
/// directly) and is not part of this enum.
#[derive(Debug)]
pub enum SessionControl {
    /// Query context usage
    ContextUsage,
    /// Remove last N messages
    Rewind { count: usize },
    /// Switch model
    ModelSwitch { model: String },
    /// Update permission mode
    SetPermissionMode { mode: PermissionMode },
}

// ── Protocol-aware Channel ───────────────────────────────────

/// Channel implementation that communicates via `ControlEvent` stream
///
/// Used by protocol-aware frontends (stdio, SDK). Sends structured events
/// instead of raw text, and supports async tool permission via
/// `ToolRequest` → `ToolResponse` flow.
pub struct ProtocolChannel {
    events_tx: mpsc::Sender<ControlEvent>,
    pending_tools: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl ProtocolChannel {
    /// Create a new protocol channel
    ///
    /// Returns the channel and an event receiver for consuming outbound events.
    pub fn new() -> (Self, mpsc::Receiver<ControlEvent>) {
        let (events_tx, events_rx) = mpsc::channel(256);
        let channel = Self {
            events_tx,
            pending_tools: Arc::new(Mutex::new(HashMap::new())),
        };
        (channel, events_rx)
    }

    /// Resolve a pending tool permission request
    ///
    /// Called when the client sends a `ToolResponse`. Returns `false`
    /// if no pending request matches the given `request_id`.
    pub async fn resolve_tool(&self, request_id: &str, allow: bool) -> bool {
        if let Some(tx) = self.pending_tools.lock().await.remove(request_id) {
            let _ = tx.send(allow);
            true
        } else {
            false
        }
    }
}

#[async_trait::async_trait]
impl Channel for ProtocolChannel {
    async fn receive(&self) -> Option<UserMessage> {
        None
    }

    async fn send(&self, text: &str) {
        if !text.is_empty() {
            let _ = self.events_tx.send(ControlEvent::TurnComplete {
                text: Some(text.to_string()),
            }).await;
        }
    }

    async fn confirm(&self, command: &str) -> bool {
        self.can_use_tool("confirm", &serde_json::json!({"command": command}))
            .await
    }

    async fn can_use_tool(
        &self,
        tool: &str,
        args: &serde_json::Value,
    ) -> bool {
        let request_id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        let (tx, rx) = oneshot::channel();

        self.pending_tools
            .lock()
            .await
            .insert(request_id.clone(), tx);

        let _ = self.events_tx.send(ControlEvent::ToolRequest {
            request_id,
            tool: tool.to_string(),
            args: args.clone(),
        }).await;

        // Wait for client response; deny on channel drop
        rx.await.unwrap_or(false)
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        let _ = self.events_tx.send(ControlEvent::StreamDelta {
            text: chunk.to_string(),
        }).await;
    }

    async fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_policy_trust_approves_all() {
        let policy = ToolPolicy {
            mode: PermissionMode::Trust,
            overrides: HashMap::new(),
        };
        assert_eq!(policy.auto_approve("bash"), Some(true));
        assert_eq!(policy.auto_approve("write_file"), Some(true));
    }

    #[test]
    fn tool_policy_auto_approves_readonly() {
        let policy = ToolPolicy {
            mode: PermissionMode::Auto,
            overrides: HashMap::new(),
        };
        assert_eq!(policy.auto_approve("read_file"), Some(true));
        assert_eq!(policy.auto_approve("glob"), Some(true));
        assert_eq!(policy.auto_approve("bash"), None);
        assert_eq!(policy.auto_approve("write_file"), None);
    }

    #[test]
    fn tool_policy_ask_defers_all() {
        let policy = ToolPolicy::default();
        assert_eq!(policy.auto_approve("read_file"), None);
        assert_eq!(policy.auto_approve("bash"), None);
    }

    #[test]
    fn tool_policy_override_takes_precedence() {
        let mut policy = ToolPolicy {
            mode: PermissionMode::Trust,
            overrides: HashMap::new(),
        };
        policy.overrides.insert("bash".into(), false);
        assert_eq!(policy.auto_approve("bash"), Some(false));
        assert_eq!(policy.auto_approve("read_file"), Some(true));
    }

    #[test]
    fn control_message_serde_roundtrip() {
        let msg = ControlMessage::Init {
            agent_id: Some("test".into()),
            model: None,
            permission_mode: Some(PermissionMode::Auto),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ControlMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ControlMessage::Init { .. }));
    }

    #[test]
    fn control_event_serde_roundtrip() {
        let event = ControlEvent::ToolRequest {
            request_id: "r1".into(),
            tool: "bash".into(),
            args: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: ControlEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ControlEvent::ToolRequest { .. }));
    }
}
