use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::process::Command;
use tokio::time::Duration;

use crate::config::tuning;

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

impl Default for BackgroundManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundManager {
    /// Create a new background manager with empty task pool
    #[must_use]
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
    ///
    /// # Panics
    ///
    /// Panics if the background task state mutex is poisoned.
    #[must_use]
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
        let command = command.to_string();
        let task_id_clone = task_id.clone();

        tokio::spawn(async move {
            let tuning = tuning();
            let timeout = Duration::from_secs(tuning.timeouts.bg_timeout_secs);
            let result =
                tokio::time::timeout(timeout, Command::new("sh").arg("-c").arg(&command).output())
                    .await;

            let (output, failed) = match result {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if out.status.success() {
                        (stdout.to_string(), false)
                    } else {
                        (format!("stdout:\n{stdout}\nstderr:\n{stderr}"), true)
                    }
                }
                Ok(Err(error)) => (format!("Error: {error}"), true),
                Err(_) => (format!("Error: Timeout ({}s)", timeout.as_secs()), true),
            };

            let output = truncate(&output, tuning.limits.output_max_len);
            let notification = truncate(&output, tuning.limits.notification_max_len);

            let mut inner = inner.lock().unwrap();
            if let Some(task) = inner.tasks.get_mut(&task_id_clone) {
                task.status = if failed {
                    BackgroundStatus::Failed
                } else {
                    BackgroundStatus::Completed
                };
                task.output = Some(output);
            }
            inner.notifications.push(Notification {
                task_id: task_id_clone,
                result: notification,
            });
        });

        task_id
    }

    /// Drain all pending notifications
    ///
    /// Returns `(task_id, result)` pairs and clears the queue.
    ///
    /// # Panics
    ///
    /// Panics if the background task state mutex is poisoned.
    #[must_use]
    pub fn drain_notifications(&self) -> Vec<(String, String)> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .notifications
            .drain(..)
            .map(|notification| (notification.task_id, notification.result))
            .collect()
    }

    /// List all background tasks
    ///
    /// # Panics
    ///
    /// Panics if the background task state mutex is poisoned.
    #[must_use]
    pub fn list(&self) -> Vec<BackgroundTask> {
        let inner = self.inner.lock().unwrap();
        let mut tasks: Vec<_> = inner.tasks.values().cloned().collect();
        tasks.sort_by(|left, right| left.id.cmp(&right.id));
        tasks
    }

    /// Get a specific background task by ID
    ///
    /// # Panics
    ///
    /// Panics if the background task state mutex is poisoned.
    #[must_use]
    pub fn get(&self, task_id: &str) -> Option<BackgroundTask> {
        let inner = self.inner.lock().unwrap();
        inner.tasks.get(task_id).cloned()
    }
}

#[must_use]
fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...(truncated)", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let notifications = mgr.drain_notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].0, task_id);
        assert!(notifications[0].1.contains("background_test_output"));

        let notifications2 = mgr.drain_notifications();
        assert!(notifications2.is_empty());
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
        let _ = mgr.run("echo a");
        let _ = mgr.run("echo b");
        let _ = mgr.run("echo c");

        assert_eq!(mgr.list().len(), 3);

        tokio::time::sleep(Duration::from_millis(500)).await;

        let notifications = mgr.drain_notifications();
        assert_eq!(notifications.len(), 3);
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
