use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::core::node::Node;
use crate::core::store::SharedStore;
use crate::intelligence::utils::discover_files;

/// A discovered memory entry (frontmatter only, body not loaded)
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub name: String,
    pub tldr: String,
    pub path: PathBuf,
}

/// Progressive memory store
///
/// Each memory is a `.md` file with YAML frontmatter containing a `tldr`.
/// At startup only frontmatter is parsed (lightweight index).
/// Full content is loaded on demand via `load_full()`.
pub struct MemoryStore {
    memory_dir: PathBuf,
    pub entries: Vec<MemoryEntry>,
}

impl MemoryStore {
    /// Create a new memory store for the given directory
    pub fn new(memory_dir: &Path) -> Self {
        Self {
            memory_dir: memory_dir.to_path_buf(),
            entries: Vec::new(),
        }
    }

    /// Return the memory directory path
    pub fn dir(&self) -> &Path {
        &self.memory_dir
    }

    /// Scan memory directory and load frontmatter only
    pub fn discover(&mut self) {
        let mut seen: HashMap<String, MemoryEntry> = HashMap::new();
        for f in discover_files(&self.memory_dir, "md") {
            let tldr = match f.meta.get("tldr") {
                Some(t) if !t.is_empty() => t.clone(),
                _ => continue,
            };
            let name = f.meta
                .get("id")
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or(f.name);

            seen.insert(name.clone(), MemoryEntry { name, tldr, path: f.path });
        }
        self.entries = seen.into_values().collect();
    }

}


/// Node that saves a memory with an LLM-generated TLDR
///
/// Pipeline:
/// - prep: extract LLM config from store
/// - exec: call LLM to generate TLDR, write `.md` file with frontmatter
/// - post: append new entry to memory index (hot update)
pub struct MemoryWrite {
    pub content: String,
    pub name: String,
    pub memory_dir: PathBuf,
    pub http: reqwest::Client,
}

/// Prepared inputs for MemoryWrite execution
#[derive(Clone)]
pub struct MemoryWritePrep {
    content: String,
    name: String,
    memory_dir: PathBuf,
    llm_url: String,
    llm_api_key: String,
    llm_model: String,
}

/// Result of MemoryWrite execution
#[derive(Clone)]
pub struct MemoryWriteResult {
    pub name: String,
    pub tldr: String,
    pub path: PathBuf,
}

impl Node for MemoryWrite {
    type PrepRes = MemoryWritePrep;
    type ExecRes = MemoryWriteResult;

    async fn prep(&self, store: &SharedStore) -> Result<MemoryWritePrep> {
        let config = &store.state.config.llm;
        Ok(MemoryWritePrep {
            content: self.content.clone(),
            name: self.name.clone(),
            memory_dir: self.memory_dir.clone(),
            llm_url: format!(
                "{}/chat/completions",
                config.base_url.trim_end_matches('/')
            ),
            llm_api_key: config.api_key.clone(),
            llm_model: config.model.clone(),
        })
    }

    async fn exec(&self, prep: MemoryWritePrep) -> Result<MemoryWriteResult> {
        // Call LLM to generate TLDR
        let tldr = generate_tldr(
            &self.http,
            &prep.llm_url,
            &prep.llm_api_key,
            &prep.llm_model,
            &prep.content,
        )
        .await?;

        // Write .md file with frontmatter
        std::fs::create_dir_all(&prep.memory_dir)?;
        // Sanitize name to prevent path traversal
        let safe_name = prep.name.replace(['/', '\\', '.'], "_");
        let path = prep.memory_dir.join(format!("{safe_name}.md"));
        // Sanitize TLDR to single line
        let tldr = tldr
            .lines()
            .next()
            .unwrap_or(&tldr)
            .trim()
            .to_string();

        let escaped_tldr = tldr.replace(':', "：");
        let file_content = format!(
            "---\nid: {safe_name}\ntldr: {escaped_tldr}\n---\n\n{}",
            prep.content,
        );
        tokio::fs::write(&path, &file_content).await?;

        Ok(MemoryWriteResult {
            name: safe_name,
            tldr,
            path,
        })
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep: MemoryWritePrep,
        exec: MemoryWriteResult,
    ) -> Result<()> {
        if let Some(intel) = &mut store.state.intelligence {
            intel.memory.entries.push(MemoryEntry {
                name: exec.name,
                tldr: exec.tldr,
                path: exec.path,
            });
        }
        Ok(())
    }
}

/// Non-streaming LLM call to generate a one-line TLDR
async fn generate_tldr(
    http: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    content: &str,
) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content":
                    "Generate a concise one-line TLDR summary. \
                     Output only the summary, nothing else."
            },
            {
                "role": "user",
                "content": content
            }
        ],
        "stream": false
    });

    let resp: serde_json::Value = http
        .post(url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let tldr = resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No summary generated")
        .trim()
        .to_string();

    Ok(tldr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_discover_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MemoryStore::new(dir.path());
        store.discover();
        assert!(store.entries.is_empty());
    }

    #[test]
    fn test_discover_with_entries() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("dark_mode.md"),
            "---\nid: dark_mode\ntldr: User prefers dark mode\n---\nDetailed info.",
        )
        .unwrap();
        fs::write(
            dir.path().join("no_frontmatter.md"),
            "Just plain text.",
        )
        .unwrap();

        let mut store = MemoryStore::new(dir.path());
        store.discover();

        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].name, "dark_mode");
        assert_eq!(store.entries[0].tldr, "User prefers dark mode");
    }

#[test]
    fn test_dir() {
        let dir = tempfile::tempdir().unwrap();
        let store = MemoryStore::new(dir.path());
        assert_eq!(store.dir(), dir.path());
    }
}