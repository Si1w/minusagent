use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;
use tokio::fs;
use tokio::process::Command;
use tokio::time::Duration;

use crate::core::node::Node;
use crate::core::store::SharedStore;
use crate::config::tuning;
use crate::tool::push_tool_result;

/// Check if a file was previously read and whether its mtime still matches.
///
/// Returns:
/// - `Ok(None)` — file was read and mtime matches, or file does not exist yet (new file)
/// - `Ok(Some(warning))` — file was read but mtime changed since
/// - `Err` — file exists but was never read
fn check_write_guard(store: &SharedStore, path: &str) -> Result<Option<String>> {
    let meta = match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(m) => m,
    };

    match store.state.read_file_state.get(path) {
        None => Err(anyhow::anyhow!(
            "Must read_file before writing: {path}"
        )),
        Some(recorded_mtime) => {
            let current_mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if current_mtime != *recorded_mtime {
                Ok(Some(format!(
                    "Warning: {path} was modified since last read"
                )))
            } else {
                Ok(None)
            }
        }
    }
}

/// Check if a command contains dangerous patterns
pub fn is_dangerous_command(command: &str) -> bool {
    const DANGEROUS: &[&str] = &["rm -rf /", "sudo", "shutdown", "reboot", "> /dev/"];
    DANGEROUS.iter().any(|p| command.contains(p))
}

/// macOS sandbox-exec policy: allow most operations, deny writes to system paths
const SANDBOX_POLICY: &str = r#"(version 1)
(allow default)
(deny file-write*
    (subpath "/System")
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/Library"))
(deny process-exec
    (subpath "/System")
    (subpath "/usr/sbin"))
"#;

/// Execute a shell command and capture output
pub struct BashExec {
    pub call_id: String,
    pub command: String,
    /// Run inside macOS sandbox-exec (default: true from config)
    pub sandbox: bool,
    /// Override timeout in seconds (None = use config default)
    pub timeout_secs: Option<u64>,
    /// Working directory override (None = inherit current dir)
    pub current_dir: Option<std::path::PathBuf>,
}

impl Node for BashExec {
    type PrepRes = String;
    type ExecRes = String;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        if is_dangerous_command(&self.command) {
            return Err(anyhow::anyhow!(
                "command blocked: contains dangerous pattern"
            ));
        }
        Ok(self.command.clone())
    }

    async fn exec(&self, command: String) -> Result<String> {
        let timeout_secs = self.timeout_secs.unwrap_or(tuning().bash_timeout_secs);
        let timeout = Duration::from_secs(timeout_secs);

        let mut cmd = if self.sandbox && cfg!(target_os = "macos") {
            let mut c = Command::new("sandbox-exec");
            c.arg("-p").arg(SANDBOX_POLICY).arg("sh").arg("-c").arg(&command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&command);
            c
        };
        if let Some(dir) = &self.current_dir {
            cmd.current_dir(dir);
        }

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| anyhow::anyhow!("command timed out after {timeout_secs}s"))??;

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
    type ExecRes = (String, SystemTime);

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        safe_path(&self.path)
    }

    async fn exec(&self, path: String) -> Result<(String, SystemTime)> {
        let mtime = fs::metadata(&path)
            .await
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {path}: {e}"))?;
        let numbered: String = content
            .lines()
            .enumerate()
            .map(|(i, line)| format!("{:4}\t{line}\n", i + 1))
            .collect();
        Ok((numbered, mtime))
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        prep_res: String,
        exec_res: (String, SystemTime),
    ) -> Result<()> {
        let (content, mtime) = exec_res;
        // Cap to prevent unbounded growth in long sessions
        if store.state.read_file_state.len() >= tuning().max_tracked_files {
            store.state.read_file_state.clear();
        }
        store.state.read_file_state.insert(prep_res, mtime);
        push_tool_result(store, &self.call_id, content);
        Ok(())
    }
}

#[derive(Clone)]
pub struct WritePrep {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) warning: Option<String>,
}

/// Write content to a file, creating parent directories if needed
pub struct WriteFile {
    pub call_id: String,
    pub path: String,
    pub content: String,
}

impl Node for WriteFile {
    type PrepRes = WritePrep;
    type ExecRes = String;

    async fn prep(&self, store: &SharedStore) -> Result<WritePrep> {
        let path = safe_path(&self.path)?;
        let warning = check_write_guard(store, &path)?;
        Ok(WritePrep { path, content: self.content.clone(), warning })
    }

    async fn exec(&self, prep: WritePrep) -> Result<String> {
        if let Some(parent) = Path::new(&prep.path).parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&prep.path, &prep.content).await?;
        let mut result = format!("Written {} bytes to {}", prep.content.len(), prep.path);
        if let Some(w) = prep.warning {
            result = format!("{w}\n{result}");
        }
        Ok(result)
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: WritePrep,
        exec_res: String,
    ) -> Result<()> {
        push_tool_result(store, &self.call_id, exec_res);
        Ok(())
    }
}

#[derive(Clone)]
pub struct EditPrep {
    pub(crate) path: String,
    pub(crate) old_string: String,
    pub(crate) new_string: String,
    pub(crate) warning: Option<String>,
}

/// Edit a file by replacing a unique string match
pub struct EditFile {
    pub call_id: String,
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

impl Node for EditFile {
    type PrepRes = EditPrep;
    type ExecRes = String;

    async fn prep(&self, store: &SharedStore) -> Result<EditPrep> {
        let path = safe_path(&self.path)?;
        let warning = check_write_guard(store, &path)?;
        Ok(EditPrep {
            path,
            old_string: self.old_string.clone(),
            new_string: self.new_string.clone(),
            warning,
        })
    }

    async fn exec(&self, prep: EditPrep) -> Result<String> {
        let content = fs::read_to_string(&prep.path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", prep.path))?;

        let count = content.matches(&prep.old_string).count();
        if count == 0 {
            return Err(anyhow::anyhow!("old_string not found in {}", prep.path));
        }
        if count > 1 {
            return Err(anyhow::anyhow!(
                "old_string found {count} times in {}, must be unique",
                prep.path
            ));
        }

        let new_content = content.replacen(&prep.old_string, &prep.new_string, 1);
        fs::write(&prep.path, &new_content).await?;
        let mut result = format!("Edited {}", prep.path);
        if let Some(w) = prep.warning {
            result = format!("{w}\n{result}");
        }
        Ok(result)
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: EditPrep,
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
