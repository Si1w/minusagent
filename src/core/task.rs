use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::time::Duration;

// ── Task Graph ──────────────────────────────────────────────

/// Task status in the dependency graph
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
}

/// A persistent task node in the task graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: usize,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    pub status: TaskStatus,
    #[serde(default, rename = "blockedBy")]
    pub blocked_by: Vec<usize>,
    #[serde(default)]
    pub blocks: Vec<usize>,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub worktree: String,
}

/// File-based task graph manager
///
/// Each task is a JSON file (`task_{id}.json`) in the tasks directory.
/// Supports CRUD operations and automatic dependency resolution
/// when tasks are completed.
#[derive(Clone)]
pub struct TaskManager {
    dir: PathBuf,
}

impl TaskManager {
    /// Create a new manager, ensuring the tasks directory exists
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Create a new task with the given subject and description
    ///
    /// # Returns
    ///
    /// The created task as pretty-printed JSON.
    pub fn create(&self, subject: &str, description: &str) -> Result<String> {
        let id = self.next_id()?;
        let task = Task {
            id,
            subject: subject.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            blocked_by: Vec::new(),
            blocks: Vec::new(),
            owner: String::new(),
            worktree: String::new(),
        };
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    /// Get a task by ID
    ///
    /// # Returns
    ///
    /// The task as pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns error if the task file does not exist.
    pub fn get(&self, id: usize) -> Result<String> {
        let task = self.load(id)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    /// Update a task's status and/or dependencies
    ///
    /// When status is set to `completed`, the task's ID is automatically
    /// removed from the `blockedBy` list of all other tasks, unblocking
    /// any that were waiting on it.
    ///
    /// Dependency edges are bidirectional: adding `blocked_by` on task A
    /// also adds A to the `blocks` list of the blocker, and vice versa.
    ///
    /// # Returns
    ///
    /// The updated task as pretty-printed JSON.
    pub fn update(
        &self,
        id: usize,
        status: Option<TaskStatus>,
        add_blocked_by: Option<Vec<usize>>,
        add_blocks: Option<Vec<usize>>,
    ) -> Result<String> {
        let mut task = self.load(id)?;

        if let Some(deps) = add_blocked_by {
            for dep_id in deps {
                if !task.blocked_by.contains(&dep_id) {
                    task.blocked_by.push(dep_id);
                }
                let mut blocker = self.load(dep_id)?;
                if !blocker.blocks.contains(&id) {
                    blocker.blocks.push(id);
                    self.save(&blocker)?;
                }
            }
        }

        if let Some(blocked) = add_blocks {
            for blocked_id in blocked {
                if !task.blocks.contains(&blocked_id) {
                    task.blocks.push(blocked_id);
                }
                let mut downstream = self.load(blocked_id)?;
                if !downstream.blocked_by.contains(&id) {
                    downstream.blocked_by.push(id);
                    self.save(&downstream)?;
                }
            }
        }

        if let Some(s) = status {
            task.status = s.clone();
            if s == TaskStatus::Completed {
                self.clear_dependency(id)?;
            }
        }

        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    /// List all tasks sorted by ID
    ///
    /// # Returns
    ///
    /// All tasks as a pretty-printed JSON array.
    pub fn list_all(&self) -> Result<String> {
        let tasks = self.list()?;
        Ok(serde_json::to_string_pretty(&tasks)?)
    }

    fn list(&self) -> Result<Vec<Task>> {
        let mut tasks = Vec::new();
        let entries = std::fs::read_dir(&self.dir);
        let entries = match entries {
            Ok(e) => e,
            Err(_) => return Ok(tasks),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = std::fs::read_to_string(&path)?;
                let task: Task = serde_json::from_str(&content)?;
                tasks.push(task);
            }
        }
        tasks.sort_by_key(|t| t.id);
        Ok(tasks)
    }

    fn next_id(&self) -> Result<usize> {
        let tasks = self.list()?;
        Ok(tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1)
    }

    fn save(&self, task: &Task) -> Result<()> {
        let path = self.dir.join(format!("task_{}.json", task.id));
        let content = serde_json::to_string_pretty(task)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn load(&self, id: usize) -> Result<Task> {
        let path = self.dir.join(format!("task_{id}.json"));
        let content = std::fs::read_to_string(&path)
            .map_err(|_| anyhow::anyhow!("Task {id} not found"))?;
        let task: Task = serde_json::from_str(&content)?;
        Ok(task)
    }

    /// Bind a worktree to a task
    ///
    /// Also advances status from `Pending` to `InProgress`.
    pub fn bind_worktree(
        &self,
        task_id: usize,
        worktree: &str,
    ) -> Result<()> {
        let mut task = self.load(task_id)?;
        task.worktree = worktree.to_string();
        if task.status == TaskStatus::Pending {
            task.status = TaskStatus::InProgress;
        }
        self.save(&task)?;
        Ok(())
    }

    /// Remove worktree binding from a task
    pub fn unbind_worktree(&self, task_id: usize) -> Result<()> {
        let mut task = self.load(task_id)?;
        task.worktree = String::new();
        self.save(&task)?;
        Ok(())
    }

    /// List unclaimed tasks (pending, no owner, not blocked)
    pub fn scan_unclaimed(&self) -> Result<Vec<Task>> {
        let tasks = self.list()?;
        Ok(tasks
            .into_iter()
            .filter(|t| {
                t.status == TaskStatus::Pending
                    && t.owner.is_empty()
                    && t.blocked_by.is_empty()
            })
            .collect())
    }

    /// Claim an unclaimed task
    ///
    /// Sets the owner and changes status to `InProgress`.
    /// Not atomic under concurrent access — file-based storage
    /// has a read-modify-write window between `load` and `save`.
    ///
    /// # Errors
    ///
    /// Returns error if the task is not pending, already owned,
    /// or blocked.
    pub fn claim(
        &self,
        task_id: usize,
        owner: &str,
    ) -> Result<String> {
        let mut task = self.load(task_id)?;
        if task.status != TaskStatus::Pending {
            return Err(anyhow::anyhow!(
                "Task {task_id} is not pending"
            ));
        }
        if !task.owner.is_empty() {
            return Err(anyhow::anyhow!(
                "Task {task_id} already owned by '{}'",
                task.owner
            ));
        }
        if !task.blocked_by.is_empty() {
            return Err(anyhow::anyhow!(
                "Task {task_id} is blocked"
            ));
        }
        task.owner = owner.to_string();
        task.status = TaskStatus::InProgress;
        self.save(&task)?;
        Ok(serde_json::to_string_pretty(&task)?)
    }

    /// Format all tasks for display with owner info
    pub fn list_formatted(&self) -> Result<String> {
        let tasks = self.list()?;
        if tasks.is_empty() {
            return Ok("No tasks.".into());
        }
        let mut output = String::from("Tasks:\n");
        for t in &tasks {
            let status = match t.status {
                TaskStatus::Pending => "pending",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Completed => "completed",
            };
            let owner = if t.owner.is_empty() {
                "unassigned"
            } else {
                &t.owner
            };
            let mut line = format!(
                "  #{} [{}] owner={} {}",
                t.id, status, owner, t.subject
            );
            if !t.blocked_by.is_empty() {
                line.push_str(&format!(
                    " blocked_by={:?}",
                    t.blocked_by
                ));
            }
            line.push('\n');
            output.push_str(&line);
        }
        Ok(output)
    }

    /// Remove `completed_id` from all tasks' `blockedBy` lists
    fn clear_dependency(&self, completed_id: usize) -> Result<()> {
        let tasks = self.list()?;
        for mut task in tasks {
            if task.blocked_by.contains(&completed_id) {
                task.blocked_by.retain(|&id| id != completed_id);
                self.save(&task)?;
            }
        }
        Ok(())
    }
}

// ── Background Tasks ────────────────────────────────────────

const BG_TIMEOUT: Duration = Duration::from_secs(300);
const NOTIFICATION_MAX_LEN: usize = 500;
const OUTPUT_MAX_LEN: usize = 50_000;

/// Status of a background task
#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundStatus {
    Running,
    Completed,
    Failed,
}

/// A background task entry
#[derive(Debug, Clone)]
pub struct BackgroundTask {
    pub id: String,
    pub command: String,
    pub status: BackgroundStatus,
    pub output: Option<String>,
}

struct Notification {
    task_id: String,
    result: String,
}

struct BgInner {
    tasks: HashMap<String, BackgroundTask>,
    notifications: Vec<Notification>,
}

/// Thread-safe background task manager
///
/// Spawns shell commands as tokio tasks and collects results
/// in a notification queue. Notifications are drained before
/// each LLM call and injected into the conversation.
#[derive(Clone)]
pub struct BackgroundManager {
    inner: Arc<Mutex<BgInner>>,
}

impl BackgroundManager {
    /// Create a new background manager with empty task pool
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BgInner {
                tasks: HashMap::new(),
                notifications: Vec::new(),
            })),
        }
    }

    /// Spawn a command in the background
    ///
    /// Returns the task ID immediately. The command runs in a
    /// separate tokio task; results are pushed to the notification
    /// queue on completion.
    pub fn run(&self, command: &str) -> String {
        let task_id = uuid::Uuid::new_v4().to_string()[..8].to_string();

        {
            let mut inner = self.inner.lock().unwrap();
            inner.tasks.insert(
                task_id.clone(),
                BackgroundTask {
                    id: task_id.clone(),
                    command: command.to_string(),
                    status: BackgroundStatus::Running,
                    output: None,
                },
            );
        }

        let inner = self.inner.clone();
        let cmd = command.to_string();
        let tid = task_id.clone();

        tokio::spawn(async move {
            let result = tokio::time::timeout(
                BG_TIMEOUT,
                Command::new("sh").arg("-c").arg(&cmd).output(),
            )
            .await;

            let (output, failed) = match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if out.status.success() {
                        (stdout.to_string(), false)
                    } else {
                        (
                            format!(
                                "stdout:\n{stdout}\nstderr:\n{stderr}"
                            ),
                            true,
                        )
                    }
                }
                Ok(Err(e)) => (format!("Error: {e}"), true),
                Err(_) => (
                    format!(
                        "Error: Timeout ({}s)",
                        BG_TIMEOUT.as_secs()
                    ),
                    true,
                ),
            };

            let output = truncate(&output, OUTPUT_MAX_LEN);
            let notification = truncate(&output, NOTIFICATION_MAX_LEN);

            let mut inner = inner.lock().unwrap();
            if let Some(task) = inner.tasks.get_mut(&tid) {
                task.status = if failed {
                    BackgroundStatus::Failed
                } else {
                    BackgroundStatus::Completed
                };
                task.output = Some(output);
            }
            inner.notifications.push(Notification {
                task_id: tid,
                result: notification,
            });
        });

        task_id
    }

    /// Drain all pending notifications
    ///
    /// Returns `(task_id, result)` pairs and clears the queue.
    pub fn drain_notifications(&self) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .notifications
            .drain(..)
            .map(|n| (n.task_id, n.result))
            .collect()
    }

    /// List all background tasks
    pub fn list(&self) -> Vec<BackgroundTask> {
        let inner = self.inner.lock().unwrap();
        let mut tasks: Vec<_> = inner.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        tasks
    }

    /// Get a specific background task by ID
    pub fn get(&self, task_id: &str) -> Option<BackgroundTask> {
        let inner = self.inner.lock().unwrap();
        inner.tasks.get(task_id).cloned()
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(truncated)", &s[..end])
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task Graph Tests ────────────────────────────────────

    fn test_manager() -> (tempfile::TempDir, TaskManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = TaskManager::new(dir.path().join(".tasks")).unwrap();
        (dir, mgr)
    }

    #[test]
    fn test_create_and_get() {
        let (_dir, mgr) = test_manager();
        let json = mgr.create("Setup project", "Initialize repo").unwrap();
        let task: Task = serde_json::from_str(&json).unwrap();

        assert_eq!(task.id, 1);
        assert_eq!(task.subject, "Setup project");
        assert_eq!(task.status, TaskStatus::Pending);

        let got = mgr.get(1).unwrap();
        let got_task: Task = serde_json::from_str(&got).unwrap();
        assert_eq!(got_task.subject, "Setup project");
    }

    #[test]
    fn test_auto_increment_id() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task 1", "").unwrap();
        mgr.create("Task 2", "").unwrap();
        let json = mgr.create("Task 3", "").unwrap();
        let task: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task.id, 3);
    }

    #[test]
    fn test_update_status() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task A", "").unwrap();

        let json = mgr
            .update(1, Some(TaskStatus::InProgress), None, None)
            .unwrap();
        let task: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_dependency_clearing_on_complete() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task 1", "").unwrap();
        mgr.create("Task 2", "").unwrap();
        mgr.create("Task 3", "").unwrap();

        mgr.update(2, None, Some(vec![1]), None).unwrap();
        mgr.update(3, None, Some(vec![1]), None).unwrap();

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        assert_eq!(t2.blocked_by, vec![1]);

        mgr.update(1, Some(TaskStatus::Completed), None, None)
            .unwrap();

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        let t3: Task = serde_json::from_str(&mgr.get(3).unwrap()).unwrap();
        assert!(t2.blocked_by.is_empty());
        assert!(t3.blocked_by.is_empty());
    }

    #[test]
    fn test_bidirectional_edges() {
        let (_dir, mgr) = test_manager();
        mgr.create("Parse", "").unwrap();
        mgr.create("Transform", "").unwrap();

        mgr.update(2, None, Some(vec![1]), None).unwrap();

        let t1: Task = serde_json::from_str(&mgr.get(1).unwrap()).unwrap();
        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();

        assert!(t1.blocks.contains(&2));
        assert!(t2.blocked_by.contains(&1));
    }

    #[test]
    fn test_add_blocks_edge() {
        let (_dir, mgr) = test_manager();
        mgr.create("Parse", "").unwrap();
        mgr.create("Transform", "").unwrap();

        mgr.update(1, None, None, Some(vec![2])).unwrap();

        let t1: Task = serde_json::from_str(&mgr.get(1).unwrap()).unwrap();
        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();

        assert!(t1.blocks.contains(&2));
        assert!(t2.blocked_by.contains(&1));
    }

    #[test]
    fn test_list_sorted() {
        let (_dir, mgr) = test_manager();
        mgr.create("C", "").unwrap();
        mgr.create("A", "").unwrap();
        mgr.create("B", "").unwrap();

        let json = mgr.list_all().unwrap();
        let tasks: Vec<Task> = serde_json::from_str(&json).unwrap();

        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[0].id, 1);
        assert_eq!(tasks[1].id, 2);
        assert_eq!(tasks[2].id, 3);
    }

    #[test]
    fn test_get_nonexistent() {
        let (_dir, mgr) = test_manager();
        let result = mgr.get(999);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_dag_workflow() {
        let (_dir, mgr) = test_manager();

        mgr.create("Task 1", "").unwrap();
        mgr.create("Task 2", "").unwrap();
        mgr.create("Task 3", "").unwrap();
        mgr.create("Task 4", "").unwrap();

        mgr.update(2, None, Some(vec![1]), None).unwrap();
        mgr.update(3, None, Some(vec![1]), None).unwrap();
        mgr.update(4, None, Some(vec![2, 3]), None).unwrap();

        mgr.update(1, Some(TaskStatus::Completed), None, None)
            .unwrap();

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        let t3: Task = serde_json::from_str(&mgr.get(3).unwrap()).unwrap();
        let t4: Task = serde_json::from_str(&mgr.get(4).unwrap()).unwrap();

        assert!(t2.blocked_by.is_empty());
        assert!(t3.blocked_by.is_empty());
        assert_eq!(t4.blocked_by, vec![2, 3]);

        mgr.update(2, Some(TaskStatus::Completed), None, None)
            .unwrap();
        mgr.update(3, Some(TaskStatus::Completed), None, None)
            .unwrap();

        let t4: Task = serde_json::from_str(&mgr.get(4).unwrap()).unwrap();
        assert!(t4.blocked_by.is_empty());
    }

    #[test]
    fn test_duplicate_dependency_ignored() {
        let (_dir, mgr) = test_manager();
        mgr.create("A", "").unwrap();
        mgr.create("B", "").unwrap();

        mgr.update(2, None, Some(vec![1]), None).unwrap();
        mgr.update(2, None, Some(vec![1]), None).unwrap();

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        assert_eq!(t2.blocked_by.len(), 1);
    }

    // ── Autonomous Tests ────────────────────────────────────

    #[test]
    fn test_scan_unclaimed() {
        let (_dir, mgr) = test_manager();
        mgr.create("Free task", "").unwrap();
        mgr.create("Blocked task", "").unwrap();
        mgr.update(2, None, Some(vec![1]), None).unwrap();

        let unclaimed = mgr.scan_unclaimed().unwrap();
        assert_eq!(unclaimed.len(), 1);
        assert_eq!(unclaimed[0].subject, "Free task");
    }

    #[test]
    fn test_scan_unclaimed_skips_owned() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task A", "").unwrap();
        mgr.claim(1, "alice").unwrap();

        let unclaimed = mgr.scan_unclaimed().unwrap();
        assert!(unclaimed.is_empty());
    }

    #[test]
    fn test_claim_success() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task 1", "").unwrap();

        let json = mgr.claim(1, "bob").unwrap();
        let task: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(task.owner, "bob");
        assert_eq!(task.status, TaskStatus::InProgress);
    }

    #[test]
    fn test_claim_already_owned() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task 1", "").unwrap();
        mgr.claim(1, "alice").unwrap();

        let err = mgr.claim(1, "bob");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("not pending"));
    }

    #[test]
    fn test_claim_blocked() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task 1", "").unwrap();
        mgr.create("Task 2", "").unwrap();
        mgr.update(2, None, Some(vec![1]), None).unwrap();

        let err = mgr.claim(2, "alice");
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("blocked"));
    }

    #[test]
    fn test_list_formatted() {
        let (_dir, mgr) = test_manager();
        mgr.create("Task A", "").unwrap();
        mgr.create("Task B", "").unwrap();
        mgr.claim(1, "alice").unwrap();

        let output = mgr.list_formatted().unwrap();
        assert!(output.contains("#1"));
        assert!(output.contains("owner=alice"));
        assert!(output.contains("owner=unassigned"));
    }

    // ── Background Tests ────────────────────────────────────

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello...(truncated)");
    }

    #[tokio::test]
    async fn test_bg_run_and_drain() {
        let mgr = BackgroundManager::new();
        let task_id = mgr.run("echo background_test_output");

        let task = mgr.get(&task_id).unwrap();
        assert_eq!(task.status, BackgroundStatus::Running);

        tokio::time::sleep(Duration::from_millis(500)).await;

        let notifs = mgr.drain_notifications();
        assert_eq!(notifs.len(), 1);
        assert_eq!(notifs[0].0, task_id);
        assert!(notifs[0].1.contains("background_test_output"));

        let notifs2 = mgr.drain_notifications();
        assert!(notifs2.is_empty());
    }

    #[tokio::test]
    async fn test_bg_completed_status() {
        let mgr = BackgroundManager::new();
        let task_id = mgr.run("echo done");

        tokio::time::sleep(Duration::from_millis(500)).await;

        let task = mgr.get(&task_id).unwrap();
        assert_eq!(task.status, BackgroundStatus::Completed);
        assert!(task.output.as_ref().unwrap().contains("done"));
    }

    #[tokio::test]
    async fn test_bg_failed_status() {
        let mgr = BackgroundManager::new();
        let task_id = mgr.run("exit 1");

        tokio::time::sleep(Duration::from_millis(500)).await;

        let task = mgr.get(&task_id).unwrap();
        assert_eq!(task.status, BackgroundStatus::Failed);
    }

    #[tokio::test]
    async fn test_bg_multiple_tasks() {
        let mgr = BackgroundManager::new();
        mgr.run("echo a");
        mgr.run("echo b");
        mgr.run("echo c");

        assert_eq!(mgr.list().len(), 3);

        tokio::time::sleep(Duration::from_millis(500)).await;

        let notifs = mgr.drain_notifications();
        assert_eq!(notifs.len(), 3);
    }

    #[test]
    fn test_bg_get_nonexistent() {
        let mgr = BackgroundManager::new();
        assert!(mgr.get("nope").is_none());
    }

    #[test]
    fn test_bg_empty_drain() {
        let mgr = BackgroundManager::new();
        assert!(mgr.drain_notifications().is_empty());
    }

    #[test]
    fn test_bg_clone_shares_state() {
        let mgr1 = BackgroundManager::new();
        let mgr2 = mgr1.clone();

        {
            let mut inner = mgr1.inner.lock().unwrap();
            inner.tasks.insert(
                "t1".into(),
                BackgroundTask {
                    id: "t1".into(),
                    command: "test".into(),
                    status: BackgroundStatus::Running,
                    output: None,
                },
            );
        }
        assert!(mgr2.get("t1").is_some());
    }
}
