use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::mpsc;

use crate::core::agent::{CotOptions, cot_loop};
use crate::core::store::{
    Config, Context, LLMConfig, Message, Role, SharedStore, SystemState,
};
use crate::core::task::{BackgroundManager, TaskManager};
use crate::core::todo::TodoManager;
use crate::frontend::SilentChannel;
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::scheduler::now_secs;

use tokio::time::Duration;

const MAX_TEAMMATE_TURNS: usize = 50;
const REQUEST_ID_LEN: usize = 8;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

// ── Message Bus ──────────────────────────────────────────

/// A message in a teammate's inbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub from: String,
    pub content: String,
    pub timestamp: f64,
    /// Protocol metadata (request_id, approve, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// File-based JSONL inbox system for inter-agent communication
///
/// Each agent has an append-only JSONL file as its inbox.
/// `send()` appends a line; `read_inbox()` reads all and drains.
#[derive(Clone)]
pub struct MessageBus {
    dir: PathBuf,
}

impl MessageBus {
    /// Create a new message bus, ensuring the inbox directory exists
    pub fn new(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    /// Append a message to a recipient's inbox
    ///
    /// # Arguments
    ///
    /// * `from` - Sender name
    /// * `to` - Recipient name (used as filename)
    /// * `content` - Message body
    /// * `msg_type` - Message type (e.g. "message", "status")
    /// * `extra` - Optional protocol metadata
    pub fn send(
        &self,
        from: &str,
        to: &str,
        content: &str,
        msg_type: &str,
        extra: Option<serde_json::Value>,
    ) -> Result<()> {
        let msg = InboxMessage {
            msg_type: msg_type.into(),
            from: from.into(),
            content: content.into(),
            timestamp: now_secs(),
            extra,
        };
        let path = self.dir.join(format!("{to}.jsonl"));
        let line = serde_json::to_string(&msg)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(f, "{line}")?;
        Ok(())
    }

    /// Read and drain all messages from an inbox
    ///
    /// Opens the file once, reads content, then truncates while
    /// still holding the handle — prevents a concurrent reader
    /// from seeing the same messages.
    pub fn read_inbox(&self, name: &str) -> Vec<InboxMessage> {
        let path = self.dir.join(format!("{name}.jsonl"));
        let mut f = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let mut content = String::new();
        if std::io::Read::read_to_string(&mut f, &mut content).is_err() {
            return Vec::new();
        }
        // Truncate while still holding the handle
        let _ = f.set_len(0);
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

// ── Teammate Manager ─────────────────────────────────────

/// Teammate lifecycle status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeammateStatus {
    Working,
    Idle,
    Shutdown,
}

/// A team member entry persisted in config.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeammateEntry {
    pub name: String,
    pub role: String,
    pub status: TeammateStatus,
    pub agent_id: String,
}

// ── Protocols ────────────────────────────────────────────

/// Request-response FSM status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Approved,
    Rejected,
}

impl fmt::Display for RequestStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Approved => write!(f, "approved"),
            Self::Rejected => write!(f, "rejected"),
        }
    }
}

/// Tracked shutdown request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRequest {
    pub target: String,
    pub status: RequestStatus,
}

/// Tracked plan approval request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    pub from: String,
    pub plan: String,
    pub status: RequestStatus,
}

// ── Team Inner ───────────────────────────────────────────

struct TeamInner {
    dir: PathBuf,
    members: Vec<TeammateEntry>,
    wake_txs: HashMap<String, mpsc::Sender<()>>,
    shutdown_requests: HashMap<String, ShutdownRequest>,
    plan_requests: HashMap<String, PlanRequest>,
}

/// Manages team roster and inter-agent communication
///
/// Persists team config to `config.json`. Each teammate gets a
/// JSONL inbox in `inbox/`. Teammates run as background tokio
/// tasks with wake-on-message support.
#[derive(Clone)]
pub struct TeammateManager {
    inner: Arc<Mutex<TeamInner>>,
    bus: MessageBus,
}

impl TeammateManager {
    /// Create a new team manager at the given directory
    ///
    /// Loads existing config.json if present.
    pub fn new(team_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(team_dir)?;
        let bus = MessageBus::new(&team_dir.join("inbox"))?;

        let config_path = team_dir.join("config.json");
        let members: Vec<TeammateEntry> =
            match std::fs::read_to_string(&config_path) {
                Ok(content) => {
                    serde_json::from_str(&content).unwrap_or_default()
                }
                Err(_) => Vec::new(),
            };

        Ok(Self {
            inner: Arc::new(Mutex::new(TeamInner {
                dir: team_dir.to_path_buf(),
                members,
                wake_txs: HashMap::new(),
                shutdown_requests: HashMap::new(),
                plan_requests: HashMap::new(),
            })),
            bus,
        })
    }

    /// Access the underlying message bus
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }

    /// Spawn a new teammate or re-awaken an idle one
    ///
    /// # Arguments
    ///
    /// * `name` - Teammate name (used as inbox address)
    /// * `role` - Short role description
    /// * `prompt` - Initial task/instruction
    /// * `agent_id` - Agent ID from registry for identity
    /// * `llm_config` - LLM configuration
    /// * `agents` - Shared agent registry
    /// * `tasks` - Shared task graph for autonomous claiming
    ///
    /// # Returns
    ///
    /// Status message.
    ///
    /// # Errors
    ///
    /// Returns error if teammate exists and is not idle.
    pub fn spawn(
        &self,
        name: &str,
        role: &str,
        prompt: &str,
        agent_id: &str,
        llm_config: LLMConfig,
        agents: SharedAgents,
        tasks: Option<TaskManager>,
    ) -> Result<String> {
        // Check for existing teammate
        {
            let mut inner = self.inner.lock().unwrap();
            let existing = inner
                .members
                .iter()
                .position(|m| m.name == name);
            if let Some(idx) = existing {
                if inner.members[idx].status == TeammateStatus::Idle
                {
                    if let Some(tx) = inner.wake_txs.get(name) {
                        let _ = tx.try_send(());
                    }
                    inner.members[idx].status =
                        TeammateStatus::Working;
                    save_config(&inner)?;
                    return Ok(format!(
                        "Teammate '{name}' re-awakened"
                    ));
                }
                let status = &inner.members[idx].status;
                return Err(anyhow::anyhow!(
                    "Teammate '{name}' already exists \
                     (status: {status:?})",
                ));
            }
        }

        let (wake_tx, wake_rx) = mpsc::channel::<()>(8);

        {
            let mut inner = self.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: name.into(),
                role: role.into(),
                status: TeammateStatus::Working,
                agent_id: agent_id.into(),
            });
            inner.wake_txs.insert(name.into(), wake_tx);
            save_config(&inner)?;
        }

        let msg =
            format!("Spawned teammate '{name}' (role: {role})");

        let team = self.clone();
        let name_owned = name.to_string();
        let role_owned = role.to_string();
        let prompt_owned = prompt.to_string();
        let agent_id_owned = agent_id.to_string();

        tokio::spawn(async move {
            if let Err(e) = teammate_loop(
                &name_owned,
                &role_owned,
                &prompt_owned,
                &agent_id_owned,
                &team,
                llm_config,
                agents,
                tasks,
                wake_rx,
            )
            .await
            {
                log::error!(
                    "Teammate '{}' error: {e}",
                    name_owned
                );
            }
            team.set_status(&name_owned, TeammateStatus::Shutdown);
        });

        Ok(msg)
    }

    /// List all team members
    pub fn list(&self) -> Vec<TeammateEntry> {
        let inner = self.inner.lock().unwrap();
        inner.members.clone()
    }

    /// Send a message and wake the recipient if idle
    ///
    /// # Errors
    ///
    /// Returns error if recipient is unknown.
    pub fn send_message(
        &self,
        from: &str,
        to: &str,
        content: &str,
    ) -> Result<String> {
        {
            let inner = self.inner.lock().unwrap();
            if to != "lead"
                && !inner.members.iter().any(|m| m.name == to)
            {
                return Err(anyhow::anyhow!(
                    "Unknown recipient: {to}"
                ));
            }
        }

        self.bus.send(from, to, content, "message", None)?;

        {
            let inner = self.inner.lock().unwrap();
            if let Some(tx) = inner.wake_txs.get(to) {
                let _ = tx.try_send(());
            }
        }

        Ok(format!("Sent to '{to}'"))
    }

    /// Read and drain an inbox, returning formatted text
    pub fn read_inbox(&self, name: &str) -> String {
        let msgs = self.bus.read_inbox(name);
        if msgs.is_empty() {
            return "No messages.".into();
        }
        serde_json::to_string_pretty(&msgs)
            .unwrap_or_else(|_| "[]".into())
    }

    // ── Protocol: Shutdown ─────────────────────────────────

    /// Send a shutdown request to a teammate
    ///
    /// # Errors
    ///
    /// Returns error if teammate is unknown.
    pub fn request_shutdown(
        &self,
        teammate: &str,
    ) -> Result<String> {
        {
            let inner = self.inner.lock().unwrap();
            if !inner.members.iter().any(|m| m.name == teammate) {
                return Err(anyhow::anyhow!(
                    "Unknown teammate: {teammate}"
                ));
            }
        }

        let req_id =
            uuid::Uuid::new_v4().to_string()[..REQUEST_ID_LEN].to_string();

        {
            let mut inner = self.inner.lock().unwrap();
            inner.shutdown_requests.insert(
                req_id.clone(),
                ShutdownRequest {
                    target: teammate.to_string(),
                    status: RequestStatus::Pending,
                },
            );
        }

        self.bus.send(
            "lead",
            teammate,
            "Please shut down gracefully.",
            "shutdown_request",
            Some(json!({"request_id": req_id})),
        )?;

        // Wake teammate so it sees the request
        {
            let inner = self.inner.lock().unwrap();
            if let Some(tx) = inner.wake_txs.get(teammate) {
                let _ = tx.try_send(());
            }
        }

        Ok(format!(
            "Shutdown request {req_id} sent to '{teammate}' \
             (status: pending)"
        ))
    }

    /// Respond to a shutdown request
    ///
    /// If approved, the teammate's wake channel is dropped so it
    /// exits after the current work cycle.
    ///
    /// # Errors
    ///
    /// Returns error if request is unknown or already resolved.
    pub fn respond_shutdown(
        &self,
        req_id: &str,
        approve: bool,
        reason: &str,
        sender: &str,
    ) -> Result<String> {
        let target = {
            let mut inner = self.inner.lock().unwrap();
            let req = inner
                .shutdown_requests
                .get_mut(req_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown shutdown request: {req_id}"
                    )
                })?;
            if req.status != RequestStatus::Pending {
                return Err(anyhow::anyhow!(
                    "Request {req_id} already {:?}",
                    req.status
                ));
            }
            req.status = if approve {
                RequestStatus::Approved
            } else {
                RequestStatus::Rejected
            };
            req.target.clone()
        };

        let status = if approve { "approved" } else { "rejected" };
        let content = if reason.is_empty() {
            format!("Shutdown {status}.")
        } else {
            format!("Shutdown {status}. Reason: {reason}")
        };
        self.bus.send(
            sender,
            "lead",
            &content,
            "shutdown_response",
            Some(json!({
                "request_id": req_id,
                "approve": approve,
            })),
        )?;

        if approve {
            self.set_status(&target, TeammateStatus::Shutdown);
        }

        Ok(format!("Shutdown {req_id}: {status}"))
    }

    // ── Protocol: Plan Approval ──────────────────────────

    /// Submit a plan for lead review
    ///
    /// # Returns
    ///
    /// Status message with the generated request_id.
    pub fn submit_plan(
        &self,
        from: &str,
        plan: &str,
    ) -> Result<String> {
        let req_id =
            uuid::Uuid::new_v4().to_string()[..REQUEST_ID_LEN].to_string();

        {
            let mut inner = self.inner.lock().unwrap();
            inner.plan_requests.insert(
                req_id.clone(),
                PlanRequest {
                    from: from.to_string(),
                    plan: plan.to_string(),
                    status: RequestStatus::Pending,
                },
            );
        }

        self.bus.send(
            from,
            "lead",
            plan,
            "plan_request",
            Some(json!({"request_id": req_id})),
        )?;

        Ok(format!(
            "Plan submitted (request_id: {req_id}, \
             status: pending)"
        ))
    }

    /// Respond to a plan submission
    ///
    /// Sends the decision to the submitter's inbox and wakes
    /// them if idle.
    ///
    /// # Errors
    ///
    /// Returns error if request is unknown or already resolved.
    pub fn respond_plan(
        &self,
        req_id: &str,
        approve: bool,
        feedback: &str,
    ) -> Result<String> {
        let from = {
            let mut inner = self.inner.lock().unwrap();
            let req = inner
                .plan_requests
                .get_mut(req_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown plan request: {req_id}"
                    )
                })?;
            if req.status != RequestStatus::Pending {
                return Err(anyhow::anyhow!(
                    "Plan {req_id} already {:?}",
                    req.status
                ));
            }
            req.status = if approve {
                RequestStatus::Approved
            } else {
                RequestStatus::Rejected
            };
            req.from.clone()
        };

        let status = if approve { "approved" } else { "rejected" };
        let content = if feedback.is_empty() {
            format!("Plan {status}.")
        } else {
            format!("Plan {status}. Feedback: {feedback}")
        };

        self.bus.send(
            "lead",
            &from,
            &content,
            "plan_response",
            Some(json!({
                "request_id": req_id,
                "approve": approve,
            })),
        )?;

        // Wake the teammate to process the response
        {
            let inner = self.inner.lock().unwrap();
            if let Some(tx) = inner.wake_txs.get(&from) {
                let _ = tx.try_send(());
            }
        }

        Ok(format!("Plan {req_id}: {status}"))
    }

    // ── Queries ──────────────────────────────────────────

    /// Check if a teammate has been shut down via protocol
    pub fn is_shutdown(&self, name: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .members
            .iter()
            .any(|m| m.name == name && m.status == TeammateStatus::Shutdown)
    }

    /// Format pending protocol requests for display
    pub fn list_requests(&self) -> String {
        let inner = self.inner.lock().unwrap();
        let mut lines = Vec::new();
        for (id, req) in &inner.shutdown_requests {
            lines.push(format!(
                "  shutdown {id} -> {} ({})",
                req.target, req.status
            ));
        }
        for (id, req) in &inner.plan_requests {
            lines.push(format!(
                "  plan {id} from {} ({})",
                req.from, req.status
            ));
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("\nRequests:\n{}", lines.join("\n"))
        }
    }

    fn set_status(&self, name: &str, status: TeammateStatus) {
        let is_shutdown = status == TeammateStatus::Shutdown;
        let mut inner = self.inner.lock().unwrap();
        let mut changed = false;
        if let Some(m) =
            inner.members.iter_mut().find(|m| m.name == name)
        {
            if m.status != status {
                m.status = status;
                changed = true;
            }
        }
        if is_shutdown {
            inner.wake_txs.remove(name);
        }
        if changed {
            let _ = save_config(&inner);
        }
    }
}

fn save_config(inner: &TeamInner) -> Result<()> {
    let path = inner.dir.join("config.json");
    let content = serde_json::to_string_pretty(&inner.members)?;
    std::fs::write(path, content)?;
    Ok(())
}

// ── Teammate Loop ────────────────────────────────────────

/// Run a teammate's agent loop with inbox integration
///
/// Each cycle: cot_loop (with auto inbox drain) → idle → wait
/// for wake. Repeats until the wake channel is closed.
async fn teammate_loop(
    name: &str,
    role: &str,
    initial_prompt: &str,
    agent_id: &str,
    team: &TeammateManager,
    llm_config: LLMConfig,
    agents: SharedAgents,
    tasks: Option<TaskManager>,
    mut wake_rx: mpsc::Receiver<()>,
) -> Result<()> {
    let (system_prompt, intelligence) = build_teammate_identity(
        agent_id,
        name,
        role,
        &agents,
        &llm_config,
    );

    let mut store = SharedStore {
        context: Context {
            system_prompt,
            history: vec![Message {
                role: Role::User,
                content: Some(initial_prompt.to_string()),
                tool_calls: None,
                tool_call_id: None,
            }],
        },
        state: SystemState {
            config: Config { llm: llm_config },
            intelligence,
            todo: TodoManager::new(),
            is_subagent: true,
            agents,
            tasks,
            background: BackgroundManager::new(),
            team: Some(team.clone()),
            team_name: Some(name.to_string()),
            worktrees: None,
            idle_requested: false,
        },
    };

    let channel: Arc<dyn crate::frontend::Channel> =
        Arc::new(SilentChannel);
    let http = reqwest::Client::new();

    loop {
        // Identity re-injection after context compression
        if store.context.history.len() <= 3 {
            store.context.history.insert(
                0,
                Message {
                    role: Role::User,
                    content: Some(format!(
                        "<identity>You are teammate '{name}' \
                         (role: {role}). Continue your work.\
                         </identity>"
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                },
            );
            store.context.history.insert(
                1,
                Message {
                    role: Role::Assistant,
                    content: Some(format!(
                        "I am {name}. Continuing."
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                },
            );
        }

        store.state.idle_requested = false;
        cot_loop(
            &mut store,
            &channel,
            &http,
            &CotOptions {
                max_turns: Some(MAX_TEAMMATE_TURNS),
                nag_reminder: false,
                flush_on_done: false,
            },
        )
        .await?;

        // Check if shutdown was approved during this cycle
        if team.is_shutdown(name) {
            break;
        }

        // Go idle, notify lead
        team.set_status(name, TeammateStatus::Idle);
        let _ = team.bus.send(
            name,
            "lead",
            &format!("{name} finished current task"),
            "status",
            None,
        );

        // Idle polling: inbox + task board every 5s for 60s
        let resumed =
            idle_poll(name, team, &mut store, &mut wake_rx).await;
        if !resumed {
            break; // timeout or channel closed -> shutdown
        }
        team.set_status(name, TeammateStatus::Working);
    }

    Ok(())
}

/// Poll for work during idle phase
///
/// Checks inbox and task board every 5s. Returns `true` if work
/// was found, `false` on timeout or channel close.
async fn idle_poll(
    name: &str,
    team: &TeammateManager,
    store: &mut SharedStore,
    wake_rx: &mut mpsc::Receiver<()>,
) -> bool {
    let deadline =
        tokio::time::Instant::now() + IDLE_TIMEOUT;

    loop {
        let poll = tokio::time::timeout(
            IDLE_POLL_INTERVAL,
            wake_rx.recv(),
        )
        .await;

        match poll {
            Ok(Some(())) => return true,
            Ok(None) => return false,
            Err(_) => {}
        }

        // Check inbox
        let msgs = team.bus().read_inbox(name);
        if !msgs.is_empty() {
            let inbox_json =
                serde_json::to_string(&msgs).unwrap_or_default();
            store.context.history.push(Message {
                role: Role::User,
                content: Some(format!(
                    "<inbox>\n{inbox_json}\n</inbox>"
                )),
                tool_calls: None,
                tool_call_id: None,
            });
            store.context.history.push(Message {
                role: Role::Assistant,
                content: Some(
                    "Noted inbox messages.".into(),
                ),
                tool_calls: None,
                tool_call_id: None,
            });
            return true;
        }

        // Check unclaimed tasks
        if let Some(ref tasks) = store.state.tasks {
            if let Ok(unclaimed) = tasks.scan_unclaimed() {
                if let Some(task) = unclaimed.first() {
                    let task_id = task.id;
                    let subject = task.subject.clone();
                    if tasks.claim(task_id, name).is_ok() {
                        store.context.history.push(Message {
                            role: Role::User,
                            content: Some(format!(
                                "<auto-claimed>\
                                 Task #{task_id}: {subject}\n\
                                 You have been assigned this \
                                 task. Work on it now.\
                                 </auto-claimed>"
                            )),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                        return true;
                    }
                }
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return false;
        }
    }
}

fn build_teammate_identity(
    agent_id: &str,
    name: &str,
    role: &str,
    agents: &SharedAgents,
    llm_config: &LLMConfig,
) -> (String, Option<Intelligence>) {
    let teammate_ctx = format!(
        "\n\n---\n\
         You are teammate '{name}' (role: {role}).\n\
         Use team_send to message other teammates or 'lead'.\n\
         Your inbox is checked automatically before each response."
    );

    if !agent_id.is_empty() {
        if let Some(config) = agents.get(agent_id) {
            let ws_dir = if config.workspace_dir.is_empty() {
                None
            } else {
                Some(PathBuf::from(&config.workspace_dir))
            };

            let intelligence = ws_dir.as_ref().map(|ws| {
                Intelligence::new(
                    ws,
                    config.system_prompt.clone(),
                    agent_id.to_string(),
                    "team".into(),
                    llm_config.model.clone(),
                )
            });

            let base = intelligence
                .as_ref()
                .map(|i| i.build_prompt())
                .unwrap_or(config.system_prompt.clone());

            return (format!("{base}{teammate_ctx}"), intelligence);
        }
    }

    (
        format!("You are a helpful assistant.{teammate_ctx}"),
        None,
    )
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_bus_send_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let bus = MessageBus::new(dir.path()).unwrap();

        bus.send("alice", "bob", "hello", "message", None).unwrap();
        bus.send("alice", "bob", "world", "message", None).unwrap();

        let msgs = bus.read_inbox("bob");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[1].content, "world");
        assert_eq!(msgs[0].from, "alice");

        // Drain: second read should be empty
        let msgs = bus.read_inbox("bob");
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_message_bus_empty_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let bus = MessageBus::new(dir.path()).unwrap();
        assert!(bus.read_inbox("nobody").is_empty());
    }

    #[test]
    fn test_message_bus_multiple_recipients() {
        let dir = tempfile::tempdir().unwrap();
        let bus = MessageBus::new(dir.path()).unwrap();

        bus.send("lead", "alice", "task A", "message", None).unwrap();
        bus.send("lead", "bob", "task B", "message", None).unwrap();

        let alice = bus.read_inbox("alice");
        let bob = bus.read_inbox("bob");
        assert_eq!(alice.len(), 1);
        assert_eq!(bob.len(), 1);
        assert_eq!(alice[0].content, "task A");
        assert_eq!(bob[0].content, "task B");
    }

    #[test]
    fn test_teammate_manager_new_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();
        assert!(team.list().is_empty());
    }

    #[test]
    fn test_teammate_manager_send_to_lead() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();
        assert!(team.send_message("alice", "lead", "hi").is_ok());
    }

    #[test]
    fn test_teammate_manager_send_to_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();
        let err = team.send_message("alice", "unknown", "hi");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("Unknown"));
    }

    #[test]
    fn test_teammate_manager_read_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        team.bus()
            .send("alice", "lead", "report", "message", None)
            .unwrap();
        let result = team.read_inbox("lead");
        assert!(result.contains("report"));

        // Drained
        assert_eq!(team.read_inbox("lead"), "No messages.");
    }

    #[test]
    fn test_teammate_manager_config_persistence() {
        let dir = tempfile::tempdir().unwrap();

        {
            let team = TeammateManager::new(dir.path()).unwrap();
            let mut inner = team.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: "alice".into(),
                role: "coder".into(),
                status: TeammateStatus::Idle,
                agent_id: String::new(),
            });
            save_config(&inner).unwrap();
        }

        let team = TeammateManager::new(dir.path()).unwrap();
        let members = team.list();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "alice");
        assert_eq!(members[0].status, TeammateStatus::Idle);
    }

    // ── Protocol tests ────────────────────────────────────

    #[test]
    fn test_request_shutdown_unknown_teammate() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();
        let err = team.request_shutdown("nobody");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("Unknown"));
    }

    #[test]
    fn test_shutdown_request_response_flow() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        // Register a teammate manually
        {
            let mut inner = team.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: "alice".into(),
                role: "coder".into(),
                status: TeammateStatus::Working,
                agent_id: String::new(),
            });
        }

        // Request shutdown
        let result = team.request_shutdown("alice").unwrap();
        assert!(result.contains("pending"));
        let req_id = result.split_whitespace().nth(2).unwrap();

        // Verify message in alice's inbox
        let msgs = team.bus().read_inbox("alice");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, "shutdown_request");

        // Approve shutdown
        let result = team
            .respond_shutdown(req_id, true, "done", "alice")
            .unwrap();
        assert!(result.contains("approved"));

        // Teammate status should be Shutdown
        assert!(team.is_shutdown("alice"));
    }

    #[test]
    fn test_shutdown_reject() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        {
            let mut inner = team.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: "bob".into(),
                role: "tester".into(),
                status: TeammateStatus::Working,
                agent_id: String::new(),
            });
        }

        let result = team.request_shutdown("bob").unwrap();
        let req_id = result.split_whitespace().nth(2).unwrap();

        let result = team
            .respond_shutdown(req_id, false, "busy", "bob")
            .unwrap();
        assert!(result.contains("rejected"));

        // Should NOT be shutdown
        assert!(!team.is_shutdown("bob"));
    }

    #[test]
    fn test_plan_submit_and_approve() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result =
            team.submit_plan("alice", "Refactor auth module").unwrap();
        assert!(result.contains("pending"));
        let req_id = result
            .split("request_id: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();

        // Lead's inbox has the plan
        let msgs = team.bus().read_inbox("lead");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, "plan_request");
        assert!(msgs[0].content.contains("Refactor"));

        // Approve
        let result =
            team.respond_plan(req_id, true, "looks good").unwrap();
        assert!(result.contains("approved"));
    }

    #[test]
    fn test_plan_reject() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result =
            team.submit_plan("bob", "Delete everything").unwrap();
        let req_id = result
            .split("request_id: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        team.bus().read_inbox("lead"); // drain

        let result =
            team.respond_plan(req_id, false, "too risky").unwrap();
        assert!(result.contains("rejected"));
    }

    #[test]
    fn test_double_respond_fails() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result =
            team.submit_plan("alice", "some plan").unwrap();
        let req_id = result
            .split("request_id: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();

        team.respond_plan(req_id, true, "").unwrap();
        let err = team.respond_plan(req_id, false, "");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("already"));
    }

    #[test]
    fn test_list_requests() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        // Empty
        assert!(team.list_requests().is_empty());

        // Add members and create requests
        {
            let mut inner = team.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: "alice".into(),
                role: "coder".into(),
                status: TeammateStatus::Working,
                agent_id: String::new(),
            });
        }
        team.request_shutdown("alice").unwrap();
        team.submit_plan("alice", "plan A").unwrap();

        let output = team.list_requests();
        assert!(output.contains("shutdown"));
        assert!(output.contains("plan"));
        assert!(output.contains("alice"));
    }

    #[test]
    fn test_teammate_manager_set_status() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        {
            let mut inner = team.inner.lock().unwrap();
            inner.members.push(TeammateEntry {
                name: "bob".into(),
                role: "tester".into(),
                status: TeammateStatus::Working,
                agent_id: String::new(),
            });
        }

        team.set_status("bob", TeammateStatus::Idle);
        let members = team.list();
        assert_eq!(members[0].status, TeammateStatus::Idle);

        team.set_status("bob", TeammateStatus::Shutdown);
        let members = team.list();
        assert_eq!(members[0].status, TeammateStatus::Shutdown);
    }
}
