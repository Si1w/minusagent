use std::path::Path;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::fs;
use tokio::process::Command;

use std::sync::Arc;

use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::frontend::Channel;

/// Tool definition for LLM function calling registration
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: ToolFunction,
}

/// Function schema within a tool definition
#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

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
        let output = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .await?;

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
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(exec_res),
            tool_calls: None,
            tool_call_id: Some(self.call_id.clone()),
        });
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
        // Add line numbers for LLM to reference specific lines
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
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(exec_res),
            tool_calls: None,
            tool_call_id: Some(self.call_id.clone()),
        });
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
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(exec_res),
            tool_calls: None,
            tool_call_id: Some(self.call_id.clone()),
        });
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
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(exec_res),
            tool_calls: None,
            tool_call_id: Some(self.call_id.clone()),
        });
        Ok(())
    }
}

// Tool definitions

pub fn bash_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "bash".into(),
            description: "Run a shell command.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    }
                },
                "required": ["command"]
            }),
        },
    }
}

pub fn read_file_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "read_file".into(),
            description: "Read the contents of a file. Returns line-numbered content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read."
                    }
                },
                "required": ["path"]
            }),
        },
    }
}

pub fn write_file_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "write_file".into(),
            description: "Write content to a file. Creates parent directories if needed."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to write to."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write."
                    }
                },
                "required": ["path", "content"]
            }),
        },
    }
}

pub fn edit_file_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "edit_file".into(),
            description: "Edit a file by replacing a unique string with a new string.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to edit."
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to find (must be unique in the file)."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement string."
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
    }
}

/// All built-in tool definitions for LLM registration
pub fn all_tools() -> Vec<ToolDefinition> {
    vec![bash_tool(), read_file_tool(), write_file_tool(), edit_file_tool()]
}

/// Dispatch a tool call by name
///
/// # Arguments
///
/// * `name` - Tool name from LLM response
/// * `call_id` - Unique tool call ID
/// * `arguments` - JSON string of tool arguments
/// * `store` - Shared store for reading/writing context
/// * `channel` - Channel for user confirmation (bash only)
///
/// # Returns
///
/// `true` if the tool was found and executed, `false` if unknown.
///
/// # Errors
///
/// Returns error on argument parsing or tool execution failure.
pub async fn dispatch_tool(
    name: &str,
    call_id: String,
    arguments: &str,
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    let args: serde_json::Value = serde_json::from_str(arguments)?;

    match name {
        "bash" => {
            let command = args["command"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            if !channel.confirm(&command).await {
                store.context.history.push(Message {
                    role: Role::Tool,
                    content: Some("User denied execution.".into()),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
                return Ok(true);
            }

            let node = BashExec { call_id, command };
            node.run(store).await?;
        }
        "read_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = ReadFile { call_id, path };
            node.run(store).await?;
        }
        "write_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let content = args["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = WriteFile { call_id, path, content };
            node.run(store).await?;
        }
        "edit_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let old_string = args["old_string"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let new_string = args["new_string"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = EditFile {
                call_id,
                path,
                old_string,
                new_string,
            };
            node.run(store).await?;
        }
        _ => return Ok(false),
    }

    Ok(true)
}

// Helpers

fn safe_path(raw: &str) -> Result<String> {
    let workdir = std::env::current_dir()?;
    let target = workdir.join(raw).canonicalize().or_else(|_| {
        // File may not exist yet (write); resolve parent instead
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

    let target_str = target.to_string_lossy();
    let workdir_str = workdir.to_string_lossy();
    if !target_str.starts_with(workdir_str.as_ref()) {
        return Err(anyhow::anyhow!(
            "Path traversal blocked: {raw} resolves outside workdir"
        ));
    }

    Ok(target_str.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::store::{Config, Context, LLMConfig, SystemState};

    fn empty_store() -> SharedStore {
        SharedStore {
            context: Context {
                system_prompt: String::new(),
                history: Vec::new(),
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model: String::new(),
                        base_url: String::new(),
                        api_key: String::new(),
                        context_window: 256_000,
                    },
                },
            },
        }
    }

    #[tokio::test]
    async fn test_bash_exec() {
        let mut store = empty_store();
        let node = BashExec {
            call_id: "call_123".into(),
            command: "echo hello".into(),
        };

        let prep_res = node.prep(&store).await.expect("prep failed");
        let exec_res = node.exec(prep_res.clone()).await.expect("exec failed");
        assert_eq!(exec_res.trim(), "hello");

        node.post(&mut store, prep_res, exec_res)
            .await
            .expect("post failed");

        let last = store.context.history.last().expect("history empty");
        assert!(matches!(last.role, Role::Tool));
        assert_eq!(last.tool_call_id.as_deref(), Some("call_123"));
        assert!(last.content.as_ref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exec_dangerous() {
        let store = empty_store();
        let node = BashExec {
            call_id: "call_456".into(),
            command: "sudo rm -rf /".into(),
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let mut store = empty_store();
        let test_path = "test_rw_output.txt";

        // Write
        let write_node = WriteFile {
            call_id: "w1".into(),
            path: test_path.into(),
            content: "hello world".into(),
        };
        write_node.run(&mut store).await.expect("write failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("Written"));

        // Read
        let read_node = ReadFile {
            call_id: "r1".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.expect("read failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("hello world"));

        // Cleanup
        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file() {
        let mut store = empty_store();
        let test_path = "test_edit_output.txt";
        std::fs::write(test_path, "foo bar baz").unwrap();

        let node = EditFile {
            call_id: "e1".into(),
            path: test_path.into(),
            old_string: "bar".into(),
            new_string: "qux".into(),
        };
        node.run(&mut store).await.expect("edit failed");

        let content = std::fs::read_to_string(test_path).unwrap();
        assert_eq!(content, "foo qux baz");

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_string_not_found() {
        let test_path = "test_edit_notfound.txt";
        std::fs::write(test_path, "some content").unwrap();

        let node = EditFile {
            call_id: "e2".into(),
            path: test_path.into(),
            old_string: "nonexistent".into(),
            new_string: "replacement".into(),
        };
        let result = node.run(&mut empty_store()).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_not_unique() {
        let test_path = "test_edit_dup.txt";
        std::fs::write(test_path, "aaa aaa").unwrap();

        let node = EditFile {
            call_id: "e3".into(),
            path: test_path.into(),
            old_string: "aaa".into(),
            new_string: "bbb".into(),
        };
        let result = node.run(&mut empty_store()).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[test]
    fn test_safe_path_blocks_traversal() {
        let result = safe_path("../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_safe_path_allows_relative() {
        // A file within the workdir should be allowed
        std::fs::write("test_safe_path.txt", "").unwrap();
        let result = safe_path("test_safe_path.txt");
        assert!(result.is_ok());
        std::fs::remove_file("test_safe_path.txt").ok();
    }
}
