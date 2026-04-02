use std::path::Path;

use anyhow::Result;
use tokio::fs;
use tokio::process::Command;
use tokio::time::Duration;

use crate::core::node::Node;
use crate::core::store::SharedStore;
use crate::config::tuning;
use crate::tool::push_tool_result;

/// Execute a shell command and capture output
pub struct BashExec {
    pub call_id: String,
    pub command: String,
}

impl Node for BashExec {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        let dangerous = ["rm -rf /", "sudo", "shutdown", "reboot", "> /dev/"];
        for pattern in dangerous {
            if self.command.contains(pattern) {
                return Err(anyhow::anyhow!(
                    "command blocked: contains dangerous pattern '{}'",
                    pattern
                ));
            }
        }
        Ok(self.command.clone())
    }

    async fn exec(&self, command: String) -> Result<String> {
        let timeout = Duration::from_secs(tuning().bash_timeout_secs);
        let output = tokio::time::timeout(
            timeout,
            Command::new("sh").arg("-c").arg(&command).output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("command timed out after {}s", timeout.as_secs()))??;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if output.status.success() {
            Ok(stdout.to_string())
        } else {
            Ok(format!("stdout:\n{stdout}\nstderr:\n{stderr}"))
        }
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

/// Read a file and return line-numbered content
pub struct ReadFile {
    pub call_id: String,
    pub path: String,
}

impl Node for ReadFile {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        safe_path(&self.path)
    }

    async fn exec(&self, path: String) -> Result<String> {
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {path}: {e}"))?;
        let numbered: String = content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:4}\t{line}\n", i + 1))
            .collect();
        Ok(numbered)
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

/// Write content to a file, creating parent directories if needed
pub struct WriteFile {
    pub call_id: String,
    pub path: String,
    pub content: String,
}

impl Node for WriteFile {
    type PrepRes = (String, String);
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<(String, String)> {
        let path = safe_path(&self.path)?;
        Ok((path, self.content.clone()))
    }

    async fn exec(&self, prep_res: (String, String)) -> Result<String> {
        let (path, content) = prep_res;
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&path, &content).await?;
        Ok(format!("Written {} bytes to {path}", content.len()))
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: (String, String),
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

/// Edit a file by replacing a unique string match
pub struct EditFile {
    pub call_id: String,
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

impl Node for EditFile {
    type PrepRes = (String, String, String);
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<(String, String, String)> {
        let path = safe_path(&self.path)?;
        Ok((path, self.old_string.clone(), self.new_string.clone()))
    }

    async fn exec(&self, prep_res: (String, String, String)) -> Result<String> {
        let (path, old_string, new_string) = prep_res;
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {path}: {e}"))?;

        let count = content.matches(&old_string).count();
        if count == 0 {
            return Err(anyhow::anyhow!("old_string not found in {path}"));
        }
        if count > 1 {
            return Err(anyhow::anyhow!(
                "old_string found {count} times in {path}, must be unique"
            ));
        }

        let new_content = content.replacen(&old_string, &new_string, 1);
        fs::write(&path, &new_content).await?;
        Ok(format!("Edited {path}"))
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: (String, String, String),
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

/// Resolve a path relative to workdir with traversal protection
pub fn safe_path(raw: &str) -> Result<String> {
    let workdir = std::env::current_dir()?;
    let target = workdir.join(raw).canonicalize().or_else(|_| {
        let p = workdir.join(raw);
        if let Some(parent) = p.parent() {
            parent
                .canonicalize()
                .map(|resolved| resolved.join(p.file_name().unwrap_or_default()))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "invalid path",
            ))
        }
    })?;

    if !target.starts_with(&workdir) {
        return Err(anyhow::anyhow!(
            "Path traversal blocked: {raw} resolves outside workdir"
        ));
    }

    Ok(target.to_string_lossy().into_owned())
}
