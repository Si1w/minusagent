use std::collections::HashMap;
use std::path::{Path, PathBuf};

const MAX_FILE_CHARS: usize = 20_000;
const MAX_TOTAL_CHARS: usize = 150_000;

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
    /// # Arguments
    ///
    /// * `mode` - "full", "minimal", or "none"
    ///
    /// # Returns
    ///
    /// Map of filename to (possibly truncated) content.
    pub fn load_all(&self, mode: &str) -> HashMap<String, String> {
        if mode == "none" {
            return HashMap::new();
        }

        let names: &[&str] = if mode == "minimal" {
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

            let remaining = MAX_TOTAL_CHARS.saturating_sub(total);
            if remaining == 0 {
                break;
            }

            let budget = remaining.min(MAX_FILE_CHARS);
            let truncated = Self::truncate(&raw, budget);
            total += truncated.len();
            result.insert(name.to_string(), truncated);
        }

        result
    }

    fn load_file(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace_dir.join(name)).ok()
    }

    fn truncate(content: &str, max_chars: usize) -> String {
        if content.len() <= max_chars {
            return content.to_string();
        }
        let cut = content[..max_chars].rfind('\n').unwrap_or(max_chars);
        format!(
            "{}\n\n[... truncated ({} chars total, showing first {}) ...]",
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
        let result = loader.load_all("none");
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_all_full() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("TOOLS.md"), "Use tools wisely.").unwrap();
        fs::write(dir.path().join("USER.md"), "User info.").unwrap();

        let loader = BootstrapLoader::new(dir.path());
        let result = loader.load_all("full");
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
        let result = loader.load_all("minimal");
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