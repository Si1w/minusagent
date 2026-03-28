use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Worktree lifecycle status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeStatus {
    Active,
    Removed,
    Kept,
}

/// A worktree entry in index.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub task_id: Option<usize>,
    pub status: WorktreeStatus,
}

/// Manages git worktrees for task isolation
///
/// Each worktree gets its own branch (`wt/{name}`) and directory.
/// Tracked in `index.json`; lifecycle events in `events.jsonl`.
#[derive(Clone)]
pub struct WorktreeManager {
    dir: PathBuf,
    repo_root: PathBuf,
}

impl WorktreeManager {
    /// Create a new worktree manager
    ///
    /// # Arguments
    ///
    /// * `dir` - Directory to store worktrees (e.g. `.worktrees/`)
    /// * `repo_root` - Git repository root for running git commands
    pub fn new(dir: PathBuf, repo_root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, repo_root })
    }

    /// Create a git worktree with optional task binding
    ///
    /// # Returns
    ///
    /// The created entry as pretty-printed JSON.
    pub fn create(
        &self,
        name: &str,
        task_id: Option<usize>,
    ) -> Result<String> {
        if self.get(name).is_some() {
            return Err(anyhow::anyhow!(
                "Worktree '{name}' already exists"
            ));
        }

        let wt_path = self.dir.join(name);
        let abs_path = self
            .dir
            .canonicalize()
            .unwrap_or_else(|_| self.dir.clone())
            .join(name);
        let branch = format!("wt/{name}");

        self.emit("worktree.create.before", name, task_id);

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch,
                &abs_path.to_string_lossy(),
                "HEAD",
            ])
            .current_dir(&self.repo_root)
            .output()?;

        if !output.status.success() {
            let stderr =
                String::from_utf8_lossy(&output.stderr);
            self.emit("worktree.create.failed", name, task_id);
            return Err(anyhow::anyhow!(
                "git worktree add failed: {}",
                stderr.trim()
            ));
        }

        // Use the actual path (may differ from abs_path if
        // canonicalize wasn't available before creation)
        let real_path = wt_path
            .canonicalize()
            .unwrap_or(abs_path)
            .to_string_lossy()
            .into_owned();

        let entry = WorktreeEntry {
            name: name.into(),
            path: real_path,
            branch,
            task_id,
            status: WorktreeStatus::Active,
        };

        let mut index = self.load_index();
        index.push(entry.clone());
        self.save_index(&index)?;

        self.emit("worktree.create.after", name, task_id);

        Ok(serde_json::to_string_pretty(&entry)?)
    }

    /// Remove a git worktree
    ///
    /// # Returns
    ///
    /// The removed entry (with updated status).
    pub fn remove(
        &self,
        name: &str,
        force: bool,
    ) -> Result<WorktreeEntry> {
        let mut index = self.load_index();
        let idx = index
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| {
                anyhow::anyhow!("Worktree '{name}' not found")
            })?;

        if index[idx].status == WorktreeStatus::Removed {
            return Err(anyhow::anyhow!(
                "Worktree '{name}' already removed"
            ));
        }

        let task_id = index[idx].task_id;
        self.emit("worktree.remove.before", name, task_id);

        let path = index[idx].path.clone();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(&path);

        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.repo_root)
            .output()?;

        if !output.status.success() {
            let stderr =
                String::from_utf8_lossy(&output.stderr);
            self.emit("worktree.remove.failed", name, task_id);
            return Err(anyhow::anyhow!(
                "git worktree remove failed: {}",
                stderr.trim()
            ));
        }

        index[idx].status = WorktreeStatus::Removed;
        self.save_index(&index)?;

        self.emit("worktree.remove.after", name, task_id);

        Ok(index[idx].clone())
    }

    /// Mark a worktree as kept (preserved for later use)
    pub fn keep(&self, name: &str) -> Result<String> {
        let mut index = self.load_index();
        let entry = index
            .iter_mut()
            .find(|e| e.name == name)
            .ok_or_else(|| {
                anyhow::anyhow!("Worktree '{name}' not found")
            })?;
        entry.status = WorktreeStatus::Kept;
        let task_id = entry.task_id;
        self.save_index(&index)?;
        self.emit("worktree.keep", name, task_id);
        Ok(format!("Worktree '{name}' marked as kept"))
    }

    /// List all worktree entries
    pub fn list(&self) -> Vec<WorktreeEntry> {
        self.load_index()
    }

    /// Get a worktree entry by name
    pub fn get(&self, name: &str) -> Option<WorktreeEntry> {
        self.load_index().into_iter().find(|e| e.name == name)
    }

    /// Format worktrees for display
    pub fn list_formatted(&self) -> String {
        let entries = self.load_index();
        if entries.is_empty() {
            return "No worktrees.".into();
        }
        let mut output = String::from("Worktrees:\n");
        for e in &entries {
            let status = match e.status {
                WorktreeStatus::Active => "active",
                WorktreeStatus::Removed => "removed",
                WorktreeStatus::Kept => "kept",
            };
            let task = match e.task_id {
                Some(id) => format!("task=#{id}"),
                None => "task=none".into(),
            };
            output.push_str(&format!(
                "  {} [{}] {} branch={}\n",
                e.name, status, task, e.branch,
            ));
        }
        output
    }

    /// Read the events log
    pub fn events(&self) -> String {
        let path = self.dir.join("events.jsonl");
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| "No events.".into())
    }

    fn load_index(&self) -> Vec<WorktreeEntry> {
        let path = self.dir.join("index.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                serde_json::from_str(&content).unwrap_or_default()
            }
            Err(_) => Vec::new(),
        }
    }

    fn save_index(
        &self,
        index: &[WorktreeEntry],
    ) -> Result<()> {
        let path = self.dir.join("index.json");
        let content = serde_json::to_string_pretty(index)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn emit(
        &self,
        event: &str,
        name: &str,
        task_id: Option<usize>,
    ) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let entry = serde_json::json!({
            "event": event,
            "name": name,
            "task_id": task_id,
            "ts": ts,
        });
        let path = self.dir.join("events.jsonl");
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn test_index_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr =
            WorktreeManager::new(dir.path().into(), dir.path().into())
                .unwrap();

        assert!(mgr.list().is_empty());

        let entries = vec![WorktreeEntry {
            name: "test".into(),
            path: "/tmp/test".into(),
            branch: "wt/test".into(),
            task_id: Some(1),
            status: WorktreeStatus::Active,
        }];
        mgr.save_index(&entries).unwrap();

        let loaded = mgr.list();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "test");
    }

    #[test]
    fn test_events_log() {
        let dir = tempfile::tempdir().unwrap();
        let mgr =
            WorktreeManager::new(dir.path().into(), dir.path().into())
                .unwrap();

        mgr.emit("test.event", "wt1", Some(1));
        mgr.emit("test.event2", "wt2", None);

        let events = mgr.events();
        assert!(events.contains("test.event"));
        assert!(events.contains("wt1"));
        assert!(events.contains("test.event2"));
    }

    #[test]
    fn test_create_and_list() {
        let repo = init_test_repo();
        let wt_dir = repo.path().join(".worktrees");
        let mgr =
            WorktreeManager::new(wt_dir, repo.path().into())
                .unwrap();

        let result = mgr.create("feat-a", Some(1));
        assert!(result.is_ok(), "create failed: {:?}", result);

        let entries = mgr.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "feat-a");
        assert_eq!(entries[0].task_id, Some(1));
        assert_eq!(entries[0].status, WorktreeStatus::Active);
        assert_eq!(entries[0].branch, "wt/feat-a");
    }

    #[test]
    fn test_create_duplicate() {
        let repo = init_test_repo();
        let wt_dir = repo.path().join(".worktrees");
        let mgr =
            WorktreeManager::new(wt_dir, repo.path().into())
                .unwrap();

        mgr.create("dup", None).unwrap();
        let err = mgr.create("dup", None);
        assert!(err.is_err());
        assert!(
            err.unwrap_err().to_string().contains("already exists")
        );
    }

    #[test]
    fn test_remove() {
        let repo = init_test_repo();
        let wt_dir = repo.path().join(".worktrees");
        let mgr =
            WorktreeManager::new(wt_dir, repo.path().into())
                .unwrap();

        mgr.create("rm-me", None).unwrap();
        let entry = mgr.remove("rm-me", false).unwrap();
        assert_eq!(entry.status, WorktreeStatus::Removed);

        let entries = mgr.list();
        assert_eq!(entries[0].status, WorktreeStatus::Removed);
    }

    #[test]
    fn test_keep() {
        let repo = init_test_repo();
        let wt_dir = repo.path().join(".worktrees");
        let mgr =
            WorktreeManager::new(wt_dir, repo.path().into())
                .unwrap();

        mgr.create("keep-me", None).unwrap();
        mgr.keep("keep-me").unwrap();

        let entry = mgr.get("keep-me").unwrap();
        assert_eq!(entry.status, WorktreeStatus::Kept);
    }

    #[test]
    fn test_list_formatted() {
        let dir = tempfile::tempdir().unwrap();
        let mgr =
            WorktreeManager::new(dir.path().into(), dir.path().into())
                .unwrap();

        let entries = vec![
            WorktreeEntry {
                name: "a".into(),
                path: "/a".into(),
                branch: "wt/a".into(),
                task_id: Some(1),
                status: WorktreeStatus::Active,
            },
            WorktreeEntry {
                name: "b".into(),
                path: "/b".into(),
                branch: "wt/b".into(),
                task_id: None,
                status: WorktreeStatus::Kept,
            },
        ];
        mgr.save_index(&entries).unwrap();

        let output = mgr.list_formatted();
        assert!(output.contains("a [active] task=#1"));
        assert!(output.contains("b [kept] task=none"));
    }
}
