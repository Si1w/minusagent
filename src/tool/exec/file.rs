use std::fmt::Write as _;
use std::path::Path;
use std::time::SystemTime;

use anyhow::Result;
use tokio::fs;

use crate::config::tuning;
use crate::engine::node::Node;
use crate::engine::store::SharedStore;
use crate::tool::push_tool_result;

#[derive(Clone)]
struct GuardedPath {
    path: String,
    warning: Option<String>,
}

#[derive(Clone)]
pub struct ReadResult {
    content: String,
    mtime: SystemTime,
}

#[derive(Clone)]
pub struct WritePrep {
    pub(crate) path: String,
    pub(crate) content: String,
    pub(crate) warning: Option<String>,
}

#[derive(Clone)]
pub struct EditPrep {
    pub(crate) path: String,
    pub(crate) old_string: String,
    pub(crate) new_string: String,
    pub(crate) warning: Option<String>,
}

/// Read a file and return line-numbered content
pub struct ReadFile {
    pub call_id: String,
    pub path: String,
}

impl Node for ReadFile {
    type PrepRes = String;
    type ExecRes = ReadResult;

    async fn prep(&self, _store: &SharedStore) -> Result<String> {
        safe_path(&self.path)
    }

    async fn exec(&self, path: String) -> Result<ReadResult> {
        let mtime = fs::metadata(&path)
            .await
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {path}: {e}"))?;

        Ok(ReadResult {
            content: number_lines(&content),
            mtime,
        })
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        prep_res: String,
        exec_res: ReadResult,
    ) -> Result<()> {
        track_read_file(store, prep_res, exec_res.mtime);
        push_tool_result(store, &self.call_id, exec_res.content);
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
    type PrepRes = WritePrep;
    type ExecRes = String;

    async fn prep(&self, store: &SharedStore) -> Result<WritePrep> {
        let guarded_path = guarded_path(store, &self.path)?;
        Ok(WritePrep {
            path: guarded_path.path,
            content: self.content.clone(),
            warning: guarded_path.warning,
        })
    }

    async fn exec(&self, prep: WritePrep) -> Result<String> {
        ensure_parent_dir(&prep.path).await?;
        fs::write(&prep.path, &prep.content).await?;

        Ok(format_file_result(
            prep.warning,
            format!("Written {} bytes to {}", prep.content.len(), prep.path),
        ))
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
        let guarded_path = guarded_path(store, &self.path)?;
        Ok(EditPrep {
            path: guarded_path.path,
            old_string: self.old_string.clone(),
            new_string: self.new_string.clone(),
            warning: guarded_path.warning,
        })
    }

    async fn exec(&self, prep: EditPrep) -> Result<String> {
        let content = fs::read_to_string(&prep.path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", prep.path))?;
        let new_content = replace_unique(&content, &prep.old_string, &prep.new_string, &prep.path)?;

        fs::write(&prep.path, new_content).await?;
        Ok(format_file_result(
            prep.warning,
            format!("Edited {}", prep.path),
        ))
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
        let path = workdir.join(raw);
        if let Some(parent) = path.parent() {
            parent
                .canonicalize()
                .map(|resolved| resolved.join(path.file_name().unwrap_or_default()))
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

fn guarded_path(store: &SharedStore, raw_path: &str) -> Result<GuardedPath> {
    let path = safe_path(raw_path)?;
    let warning = check_write_guard(store, &path)?;
    Ok(GuardedPath { path, warning })
}

fn check_write_guard(store: &SharedStore, path: &str) -> Result<Option<String>> {
    let metadata = match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) => metadata,
    };

    match store.state.read_file_state.get(path) {
        None => Err(anyhow::anyhow!("Must read_file before writing: {path}")),
        Some(recorded_mtime) => {
            let current_mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if current_mtime == *recorded_mtime {
                Ok(None)
            } else {
                Ok(Some(format!(
                    "Warning: {path} was modified since last read"
                )))
            }
        }
    }
}

fn number_lines(content: &str) -> String {
    let mut numbered = String::new();
    for (index, line) in content.lines().enumerate() {
        let _ = writeln!(&mut numbered, "{:4}\t{line}", index + 1);
    }
    numbered
}

fn track_read_file(store: &mut SharedStore, path: String, mtime: SystemTime) {
    if store.state.read_file_state.len() >= tuning().limits.max_tracked_files {
        store.state.read_file_state.clear();
    }
    store.state.read_file_state.insert(path, mtime);
}

async fn ensure_parent_dir(path: &str) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent).await?;
    }
    Ok(())
}

fn format_file_result(warning: Option<String>, result: String) -> String {
    match warning {
        Some(warning) => format!("{warning}\n{result}"),
        None => result,
    }
}

fn replace_unique(content: &str, old: &str, new: &str, path: &str) -> Result<String> {
    let count = content.matches(old).count();
    if count == 0 {
        return Err(anyhow::anyhow!("old_string not found in {path}"));
    }
    if count > 1 {
        return Err(anyhow::anyhow!(
            "old_string found {count} times in {path}, must be unique"
        ));
    }

    Ok(content.replacen(old, new, 1))
}
