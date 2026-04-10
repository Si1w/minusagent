use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::engine::store::Message;

const SESSIONS_DIR: &str = "sessions";

/// Index entry for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SessionMeta {
    pub(super) label: String,
    pub(super) created_at: String,
    pub(super) last_active: String,
    pub(super) message_count: usize,
}

/// JSONL-based session persistence.
pub(super) struct SessionStore {
    base_dir: PathBuf,
    index_path: PathBuf,
    index: HashMap<String, SessionMeta>,
    current_id: Option<String>,
}

impl SessionStore {
    pub(super) fn new(base_dir: &Path) -> Result<Self> {
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

    pub(super) fn new_default() -> Result<Self> {
        Self::new(Path::new(SESSIONS_DIR))
    }

    pub(super) fn create(&mut self, label: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        let now = Utc::now().to_rfc3339();

        self.index.insert(
            id.clone(),
            SessionMeta {
                label: label.to_string(),
                created_at: now.clone(),
                last_active: now,
                message_count: 0,
            },
        );
        self.save_index()?;
        self.current_id = Some(id.clone());
        Ok(id)
    }

    pub(super) fn save(&mut self, history: &[Message]) -> Result<()> {
        let id = self
            .current_id
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No active session. Use /new first."))?;

        let path = self.session_path(id);
        let mut content = String::new();
        for message in history {
            let line = serde_json::to_string(message)?;
            content.push_str(&line);
            content.push('\n');
        }
        std::fs::write(path, content)?;

        if let Some(meta) = self.index.get_mut(id) {
            meta.last_active = Utc::now().to_rfc3339();
            meta.message_count = history.len();
        }
        self.save_index()?;
        Ok(())
    }

    pub(super) fn load(&mut self, label: &str) -> Result<Vec<Message>> {
        let matched = self.match_label(label)?;
        let path = self.session_path(&matched);
        if !path.exists() {
            self.current_id = Some(matched);
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(path)?;
        let mut history = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            history.push(serde_json::from_str(line)?);
        }
        self.current_id = Some(matched);
        Ok(history)
    }

    pub(super) fn list(&self) -> Vec<(String, SessionMeta)> {
        let mut items = self
            .index
            .iter()
            .map(|(id, meta)| (id.clone(), meta.clone()))
            .collect::<Vec<_>>();
        items.sort_by(|left, right| right.1.last_active.cmp(&left.1.last_active));
        items
    }

    pub(super) fn current_id(&self) -> Option<&str> {
        self.current_id.as_deref()
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
        let matches = self
            .index
            .iter()
            .filter(|(_, meta)| meta.label.starts_with(prefix))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();

        match matches.len() {
            0 => Err(anyhow::anyhow!("Session not found: {prefix}")),
            1 => Ok(matches[0].clone()),
            _ => {
                let labels = matches
                    .iter()
                    .filter_map(|id| self.index.get(id))
                    .map(|meta| meta.label.as_str())
                    .collect::<Vec<_>>();
                Err(anyhow::anyhow!(
                    "Ambiguous prefix, matches: {}",
                    labels.join(", ")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::store::Role;

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

        assert_eq!(store.current_id(), Some(id.as_str()));

        let sessions = store.list();
        assert!(
            sessions
                .iter()
                .any(|(sid, meta)| sid == &id && meta.label == "test-label")
        );
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
            result
                .unwrap_err()
                .to_string()
                .contains("No active session")
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
