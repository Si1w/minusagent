use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::fs;
use tokio::process::Command;
use tokio::time::Duration;

use crate::config::tuning;
use crate::engine::node::Node;
use crate::engine::store::SharedStore;
use crate::tool::push_tool_result;

/// Resolve an optional path relative to workdir with traversal protection
fn resolve_safe_dir(raw: Option<&str>) -> Result<PathBuf> {
    let workdir = std::env::current_dir()?;
    match raw {
        Some(p) => {
            let resolved = workdir
                .join(p)
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("path not found: {p}: {e}"))?;
            if !resolved.starts_with(&workdir) {
                return Err(anyhow::anyhow!(
                    "Path traversal blocked: {p} resolves outside workdir"
                ));
            }
            Ok(resolved)
        }
        None => Ok(workdir),
    }
}

/// Auto-prefix glob patterns without path separators
fn normalize_glob_pattern(pattern: &str) -> String {
    if !pattern.starts_with("**/") && !pattern.contains('/') {
        format!("**/{pattern}")
    } else {
        pattern.to_string()
    }
}

/// Find files matching a glob pattern
pub struct GlobFile {
    pub call_id: String,
    pub pattern: String,
    pub directory: Option<String>,
}

impl Node for GlobFile {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        let base = resolve_safe_dir(self.directory.as_deref())?;
        let pattern = normalize_glob_pattern(&self.pattern);
        Ok(base.join(pattern).to_string_lossy().into_owned())
    }

    async fn exec(&self, full_pattern: String) -> Result<String> {
        let max_results = tuning().limits.glob_max_results;
        let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for path in glob::glob(&full_pattern)
            .map_err(|e| anyhow::anyhow!("invalid glob pattern: {e}"))?
            .flatten()
        {
            let Ok(meta) = path.metadata() else {
                continue;
            };
            if !meta.file_type().is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((path, mtime));
        }

        entries.sort_by(|a, b| b.1.cmp(&a.1));

        if entries.is_empty() {
            return Ok("No files matched.".into());
        }

        let total = entries.len();
        let lines: Vec<String> = entries
            .iter()
            .take(max_results)
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();

        let mut result = lines.join("\n");
        if total > max_results {
            result.push_str("\n... and ");
            result.push_str(&(total - max_results).to_string());
            result.push_str(" more");
        }
        Ok(result)
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: String,
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

/// Search file contents using regex
pub struct GrepFile {
    pub call_id: String,
    pub pattern: String,
    pub path: Option<String>,
    pub include: Option<String>,
}

impl Node for GrepFile {
    type PrepRes = (String, PathBuf, Option<String>);
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<(String, PathBuf, Option<String>)> {
        let search_path = resolve_safe_dir(self.path.as_deref())?;
        Ok((self.pattern.clone(), search_path, self.include.clone()))
    }

    async fn exec(&self, prep_res: (String, PathBuf, Option<String>)) -> Result<String> {
        let (pattern, search_path, include) = prep_res;
        let max = tuning().limits.grep_max_results;

        if has_rg() {
            return rg_search(&pattern, &search_path, include.as_deref(), max).await;
        }

        regex_search(&pattern, &search_path, include.as_deref(), max).await
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: (String, PathBuf, Option<String>),
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

fn has_rg() -> bool {
    static RG_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RG_AVAILABLE.get_or_init(|| {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

async fn rg_search(
    pattern: &str,
    path: &Path,
    include: Option<&str>,
    max: usize,
) -> Result<String> {
    let mut cmd = Command::new("rg");
    cmd.arg("--no-heading")
        .arg("--line-number")
        .arg("--max-count")
        .arg(max.to_string())
        .arg(pattern)
        .arg(path);
    if let Some(glob_pat) = include {
        cmd.arg("--glob").arg(glob_pat);
    }

    let output = tokio::time::timeout(
        Duration::from_secs(tuning().timeouts.search_timeout_secs),
        cmd.output(),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "grep timed out after {}s",
            tuning().timeouts.search_timeout_secs
        )
    })??;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = stdout.trim();
    if text.is_empty() {
        return Ok("No matches found.".into());
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > max {
        let mut result: String = lines[..max].join("\n");
        result.push_str("\n... (truncated at ");
        result.push_str(&max.to_string());
        result.push_str(" matches)");
        Ok(result)
    } else {
        Ok(lines.join("\n"))
    }
}

async fn regex_search(
    pattern: &str,
    path: &Path,
    include: Option<&str>,
    max: usize,
) -> Result<String> {
    let re = regex::Regex::new(pattern)?;
    let include_glob = include.and_then(|g| glob::Pattern::new(&normalize_glob_pattern(g)).ok());

    let mut matches = Vec::new();

    if path.is_file() {
        search_file(&re, path, &mut matches, max).await;
    } else {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(mut entries) = fs::read_dir(&dir).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let Ok(ft) = entry.file_type().await else {
                    continue;
                };
                if ft.is_dir() {
                    stack.push(entry.path());
                } else if ft.is_file() {
                    let p = entry.path();
                    if include_glob
                        .as_ref()
                        .is_some_and(|pat| !pat.matches_path(&p))
                    {
                        continue;
                    }
                    search_file(&re, &p, &mut matches, max).await;
                    if matches.len() >= max {
                        matches.push(format!("... (truncated at {max} matches)"));
                        return Ok(matches.join("\n"));
                    }
                }
            }
        }
    }

    if matches.is_empty() {
        Ok("No matches found.".into())
    } else {
        Ok(matches.join("\n"))
    }
}

async fn search_file(re: &regex::Regex, path: &Path, matches: &mut Vec<String>, max: usize) {
    let Ok(content) = fs::read_to_string(path).await else {
        return;
    };
    for (i, line) in content.lines().enumerate() {
        if re.is_match(line) {
            matches.push(format!("{}:{}:{}", path.display(), i + 1, line.trim_end()));
            if matches.len() >= max {
                return;
            }
        }
    }
}
