use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::scheduler::now_secs;

/// A message in a teammate's inbox
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub from: String,
    pub content: String,
    pub timestamp: f64,
    /// Protocol metadata (`request_id`, `approve`, etc.)
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
    ///
    /// # Errors
    ///
    /// Returns an error if the inbox directory cannot be created.
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
    ///
    /// # Errors
    ///
    /// Returns an error if the inbox file cannot be opened or the message
    /// cannot be serialized and written.
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
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Read and drain all messages from an inbox
    ///
    /// Opens the file once, reads content, then truncates while
    /// still holding the handle — prevents a concurrent reader
    /// from seeing the same messages.
    #[must_use]
    pub fn read_inbox(&self, name: &str) -> Vec<InboxMessage> {
        let path = self.dir.join(format!("{name}.jsonl"));
        let Ok(mut file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        else {
            return Vec::new();
        };
        let mut content = String::new();
        if std::io::Read::read_to_string(&mut file, &mut content).is_err() {
            return Vec::new();
        }
        let _ = file.set_len(0);
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}
