use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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

        // task 2 and 3 depend on task 1
        mgr.update(2, None, Some(vec![1]), None).unwrap();
        mgr.update(3, None, Some(vec![1]), None).unwrap();

        // Verify dependencies are set
        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        assert_eq!(t2.blocked_by, vec![1]);

        // Complete task 1 — should clear dependencies
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

        // Transform blocked_by Parse
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

        // Parse blocks Transform
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

        // Build: task1 -> (task2, task3) -> task4
        mgr.create("Task 1", "").unwrap();
        mgr.create("Task 2", "").unwrap();
        mgr.create("Task 3", "").unwrap();
        mgr.create("Task 4", "").unwrap();

        mgr.update(2, None, Some(vec![1]), None).unwrap();
        mgr.update(3, None, Some(vec![1]), None).unwrap();
        mgr.update(4, None, Some(vec![2, 3]), None).unwrap();

        // Complete task 1 — unblocks 2 and 3
        mgr.update(1, Some(TaskStatus::Completed), None, None)
            .unwrap();

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        let t3: Task = serde_json::from_str(&mgr.get(3).unwrap()).unwrap();
        let t4: Task = serde_json::from_str(&mgr.get(4).unwrap()).unwrap();

        assert!(t2.blocked_by.is_empty());
        assert!(t3.blocked_by.is_empty());
        assert_eq!(t4.blocked_by, vec![2, 3]); // still blocked

        // Complete task 2 and 3 — unblocks 4
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
        mgr.update(2, None, Some(vec![1]), None).unwrap(); // duplicate

        let t2: Task = serde_json::from_str(&mgr.get(2).unwrap()).unwrap();
        assert_eq!(t2.blocked_by.len(), 1);
    }
}
