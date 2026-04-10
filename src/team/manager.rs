use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[path = "manager/bus.rs"]
mod bus;
#[path = "manager/protocol.rs"]
mod protocol;
#[path = "manager/runtime.rs"]
mod runtime;

pub(super) const LEAD_NAME: &str = "lead";
pub(super) const REQUEST_ID_LEN: usize = 8;
pub use self::bus::{InboxMessage, MessageBus};
pub use self::protocol::{PlanRequest, RequestStatus, ShutdownRequest};
pub use self::runtime::TeammateSpawn;

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

struct TeamState {
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
    state: Arc<Mutex<TeamState>>,
    bus: MessageBus,
}

impl TeammateManager {
    /// Create a new team manager at the given directory
    ///
    /// Loads existing config.json if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the team directory or inbox cannot be created.
    pub fn new(team_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(team_dir)?;
        let bus = MessageBus::new(&team_dir.join("inbox"))?;

        let config_path = team_dir.join("config.json");
        let members: Vec<TeammateEntry> = match std::fs::read_to_string(&config_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        Ok(Self {
            state: Arc::new(Mutex::new(TeamState {
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
    #[must_use]
    pub fn bus(&self) -> &MessageBus {
        &self.bus
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, TeamState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wake_teammate(&self, name: &str) {
        let state = self.lock_state();
        if let Some(tx) = state.wake_txs.get(name) {
            let _ = tx.try_send(());
        }
    }

    pub(super) fn next_request_id() -> String {
        uuid::Uuid::new_v4().to_string()[..REQUEST_ID_LEN].to_string()
    }

    pub(super) fn has_member(&self, name: &str) -> bool {
        self.lock_state()
            .members
            .iter()
            .any(|member| member.name == name)
    }

    /// List all team members
    #[must_use]
    pub fn list(&self) -> Vec<TeammateEntry> {
        let state = self.lock_state();
        state.members.clone()
    }

    /// Send a message and wake the recipient if idle
    ///
    /// # Errors
    ///
    /// Returns error if recipient is unknown.
    pub fn send_message(&self, from: &str, to: &str, content: &str) -> Result<String> {
        if to != LEAD_NAME && !self.has_member(to) {
            return Err(anyhow::anyhow!("Unknown recipient: {to}"));
        }

        self.bus.send(from, to, content, "message", None)?;
        self.wake_teammate(to);

        Ok(format!("Sent to '{to}'"))
    }

    /// Read and drain an inbox, returning formatted text
    #[must_use]
    pub fn read_inbox(&self, name: &str) -> String {
        let msgs = self.bus.read_inbox(name);
        if msgs.is_empty() {
            return "No messages.".into();
        }
        serde_json::to_string_pretty(&msgs).unwrap_or_else(|_| "[]".into())
    }

    // ── Queries ──────────────────────────────────────────

    /// Check if a teammate has been shut down via protocol
    #[must_use]
    pub fn is_shutdown(&self, name: &str) -> bool {
        let state = self.lock_state();
        state
            .members
            .iter()
            .any(|m| m.name == name && m.status == TeammateStatus::Shutdown)
    }

    fn set_status(&self, name: &str, status: TeammateStatus) {
        let is_shutdown = status == TeammateStatus::Shutdown;
        let mut state = self.lock_state();
        let mut changed = false;
        if let Some(member) = state.members.iter_mut().find(|member| member.name == name)
            && member.status != status
        {
            member.status = status;
            changed = true;
        }
        if is_shutdown {
            state.wake_txs.remove(name);
        }
        if changed {
            let _ = persist_roster(&state);
        }
    }
}

fn persist_roster(state: &TeamState) -> Result<()> {
    let path = state.dir.join("config.json");
    let content = serde_json::to_string_pretty(&state.members)?;
    std::fs::write(path, content)?;
    Ok(())
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

        bus.send(LEAD_NAME, "alice", "task A", "message", None)
            .unwrap();
        bus.send(LEAD_NAME, "bob", "task B", "message", None)
            .unwrap();

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
        assert!(team.send_message("alice", LEAD_NAME, "hi").is_ok());
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
            .send("alice", LEAD_NAME, "report", "message", None)
            .unwrap();
        let result = team.read_inbox(LEAD_NAME);
        assert!(result.contains("report"));

        // Drained
        assert_eq!(team.read_inbox(LEAD_NAME), "No messages.");
    }

    #[test]
    fn test_teammate_manager_config_persistence() {
        let dir = tempfile::tempdir().unwrap();

        {
            let team = TeammateManager::new(dir.path()).unwrap();
            let mut state = team.lock_state();
            state.members.push(TeammateEntry {
                name: "alice".into(),
                role: "coder".into(),
                status: TeammateStatus::Idle,
                agent_id: String::new(),
            });
            persist_roster(&state).unwrap();
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
            let mut state = team.lock_state();
            state.members.push(TeammateEntry {
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
            let mut state = team.lock_state();
            state.members.push(TeammateEntry {
                name: "bob".into(),
                role: "tester".into(),
                status: TeammateStatus::Working,
                agent_id: String::new(),
            });
        }

        let result = team.request_shutdown("bob").unwrap();
        let req_id = result.split_whitespace().nth(2).unwrap();

        let result = team.respond_shutdown(req_id, false, "busy", "bob").unwrap();
        assert!(result.contains("rejected"));

        // Should NOT be shutdown
        assert!(!team.is_shutdown("bob"));
    }

    #[test]
    fn test_plan_submit_and_approve() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result = team.submit_plan("alice", "Refactor auth module").unwrap();
        assert!(result.contains("pending"));
        let req_id = result
            .split("request_id: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();

        // Lead's inbox has the plan
        let msgs = team.bus().read_inbox(LEAD_NAME);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].msg_type, "plan_request");
        assert!(msgs[0].content.contains("Refactor"));

        // Approve
        let result = team.respond_plan(req_id, true, "looks good").unwrap();
        assert!(result.contains("approved"));
    }

    #[test]
    fn test_plan_reject() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result = team.submit_plan("bob", "Delete everything").unwrap();
        let req_id = result
            .split("request_id: ")
            .nth(1)
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        let _ = team.bus().read_inbox(LEAD_NAME); // drain

        let result = team.respond_plan(req_id, false, "too risky").unwrap();
        assert!(result.contains("rejected"));
    }

    #[test]
    fn test_double_respond_fails() {
        let dir = tempfile::tempdir().unwrap();
        let team = TeammateManager::new(dir.path()).unwrap();

        let result = team.submit_plan("alice", "some plan").unwrap();
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
            let mut state = team.lock_state();
            state.members.push(TeammateEntry {
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
            let mut state = team.lock_state();
            state.members.push(TeammateEntry {
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
