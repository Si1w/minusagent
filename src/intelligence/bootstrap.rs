use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::tuning;
use crate::intelligence::PromptMode;

/// Bootstrap file names loaded at agent startup
pub const BOOTSTRAP_FILES: &[&str] = &[
    "TOOLS.md",
    "USER.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
    "AGENTS.md",
];

/// Loads workspace bootstrap files at agent startup
///
/// Different modes for different scenarios:
/// - `full`: main agent (all files)
/// - `minimal`: sub-agent / cron (AGENTS.md + TOOLS.md only)
/// - `none`: empty
pub struct BootstrapLoader {
    workspace_dir: PathBuf,
}

impl BootstrapLoader {
    /// Create a new loader for the given workspace directory
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Load all bootstrap files for the given mode
    ///
    /// # Returns
    ///
    /// Map of filename to (possibly truncated) content.
    pub fn load_all(&self, mode: PromptMode) -> HashMap<String, String> {
        if mode == PromptMode::None {
            return HashMap::new();
        }

        let names: &[&str] = if mode == PromptMode::Minimal {
            &["AGENTS.md", "TOOLS.md"]
        } else {
            BOOTSTRAP_FILES
        };

        let mut result = HashMap::new();
        let mut total = 0;

        for &name in names {
            let raw = match self.load_file(name) {
                Some(content) if !content.is_empty() => content,
                _ => continue,
            };

            let remaining = tuning().bootstrap_max_total_chars.saturating_sub(total);
            if remaining == 0 {
                break;
            }

            let budget = remaining.min(tuning().bootstrap_max_file_chars);
            let truncated = Self::truncate(&raw, budget);
            total += truncated.len();
            result.insert(name.to_string(), truncated);
        }

        result
    }

    fn load_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace_dir.join(name)).ok()
    }

    fn truncate(content: &str, max_bytes: usize) -> String {
        if content.len() <= max_bytes {
            return content.to_string();
        }
        // Find a char-safe boundary at or before max_bytes
        let safe = match content.get(..max_bytes) {
            Some(s) => s.len(),
            None => content
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= max_bytes)
                .last()
                .unwrap_or(0),
        };
        let cut = content[..safe].rfind('\n').unwrap_or(safe);
        format!(
            "{}\n\n[... truncated ({} bytes total, showing first {}) ...]",
            &content[..cut],
            content.len(),
            cut,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_load_all_none() {
        let loader = BootstrapLoader::new(Path::new("/nonexistent"));
        let result = loader.load_all(PromptMode::None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_all_full() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("TOOLS.md"), "Use tools wisely.").unwrap();
        fs::write(dir.path().join("USER.md"), "User info.").unwrap();

        let loader = BootstrapLoader::new(dir.path());
        let result = loader.load_all(PromptMode::Full);
        assert_eq!(result.get("TOOLS.md").unwrap(), "Use tools wisely.");
        assert_eq!(result.get("USER.md").unwrap(), "User info.");
        assert!(!result.contains_key("SOUL.md"));
    }

    #[test]
    fn test_load_all_minimal() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("TOOLS.md"), "Use tools wisely.").unwrap();
        fs::write(dir.path().join("USER.md"), "User info.").unwrap();

        let loader = BootstrapLoader::new(dir.path());
        let result = loader.load_all(PromptMode::Minimal);
        assert_eq!(result.get("TOOLS.md").unwrap(), "Use tools wisely.");
        assert!(!result.contains_key("USER.md"));
    }

    #[test]
    fn test_truncate() {
        let short = "hello";
        assert_eq!(BootstrapLoader::truncate(short, 100), "hello");

        let long = "line1\nline2\nline3\nline4";
        let truncated = BootstrapLoader::truncate(long, 12);
        assert!(truncated.starts_with("line1\nline2"));
        assert!(truncated.contains("truncated"));
    }
}