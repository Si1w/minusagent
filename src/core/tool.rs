use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::fs;
use tokio::process::Command;
use tokio::time::Duration;

use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::core::subagent::run_subagent;
use crate::core::todo::{TodoItem, TodoWrite};
use crate::frontend::Channel;
use crate::intelligence::manager::normalize_agent_id;

const BASH_TIMEOUT: Duration = Duration::from_secs(120);

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
        let output = tokio::time::timeout(
            BASH_TIMEOUT,
            Command::new("sh").arg("-c").arg(&command).output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("command timed out after {}s", BASH_TIMEOUT.as_secs()))??;

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

pub fn todo_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "todo".into(),
            description: "Update the task plan. Use this to track progress on multi-step \
                          tasks. Only one task can be in_progress at a time."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {
                                    "type": "integer",
                                    "description": "Unique task ID."
                                },
                                "text": {
                                    "type": "string",
                                    "description": "Task description."
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Task status."
                                }
                            },
                            "required": ["id", "text", "status"]
                        },
                        "description": "Full list of todo items. Replaces all existing items."
                    }
                },
                "required": ["items"]
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

/// Task tool: spawn an agent with isolated context (parent-only)
pub fn task_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "task".into(),
            description: "Spawn an agent with fresh context to handle a \
                          subtask. The agent runs in isolation and only \
                          the final summary is returned."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The task description."
                    },
                    "agent": {
                        "type": "string",
                        "description": "Agent ID to handle the task."
                    }
                },
                "required": ["prompt", "agent"]
            }),
        },
    }
}

/// All built-in tool definitions for LLM registration
pub fn all_tools(is_subagent: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        bash_tool(),
        read_file_tool(),
        write_file_tool(),
        edit_file_tool(),
        todo_tool(),
    ];
    if !is_subagent {
        tools.push(task_tool());
    }
    tools
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
        "todo" => {
            let items: Vec<TodoItem> = match serde_json::from_value(
                args["items"].clone(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("todo: failed to parse items: {e}");
                    push_tool_result(
                        store,
                        &call_id,
                        format!("Error: invalid items JSON: {e}"),
                    );
                    return Ok(true);
                }
            };
            let node = TodoWrite {
                call_id: call_id.clone(),
                items,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(store, &call_id, format!("Error: {e}"));
            }
        }
        "task" => {
            if store.state.is_subagent {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: task tool is not available here".into(),
                );
            } else {
                let prompt = args["prompt"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let agent_id = normalize_agent_id(
                    args["agent"].as_str().unwrap_or_default(),
                );
                let agent_config = store.state.agents.get(&agent_id);
                match agent_config {
                    Some(config) => {
                        let ws_dir = if config.workspace_dir.is_empty() {
                            None
                        } else {
                            Some(PathBuf::from(&config.workspace_dir))
                        };
                        let llm_config = store.state.config.llm.clone();
                        let summary = run_subagent(
                            prompt,
                            config.system_prompt.clone(),
                            llm_config,
                            ws_dir,
                            agent_id,
                            store.state.agents.clone(),
                        )
                        .await?;
                        push_tool_result(store, &call_id, summary);
                    }
                    None => {
                        let available: Vec<String> = store
                            .state
                            .agents
                            .list()
                            .iter()
                            .map(|a| a.id.clone())
                            .collect();
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: unknown agent '{}'. Available: {}",
                                agent_id,
                                available.join(", ")
                            ),
                        );
                    }
                }
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

// Helpers

pub(crate) fn push_tool_result(store: &mut SharedStore, call_id: &str, content: String) {
    store.context.history.push(Message {
        role: Role::Tool,
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
    });
}

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

    if !target.starts_with(&workdir) {
        return Err(anyhow::anyhow!(
            "Path traversal blocked: {raw} resolves outside workdir"
        ));
    }

    Ok(target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bash_exec() {
        let mut store = SharedStore::test_default();
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
        let store = SharedStore::test_default();
        let node = BashExec {
            call_id: "call_456".into(),
            command: "sudo rm -rf /".into(),
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let mut store = SharedStore::test_default();
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
        let mut store = SharedStore::test_default();
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
        let result = node.run(&mut SharedStore::test_default()).await;
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
        let result = node.run(&mut SharedStore::test_default()).await;
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

    #[tokio::test]
    async fn test_task_rejects_unknown_agent() {
        let mut store = SharedStore::test_default();
        let args = r#"{"prompt":"do something","agent":"nonexistent"}"#;
        let result =
            dispatch_tool("task", "t1".into(), args, &mut store, &silent())
                .await
                .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        let content = last.content.as_ref().unwrap();
        assert!(content.contains("Error: unknown agent"));
        assert!(content.contains("Available:"));
    }

    #[tokio::test]
    async fn test_task_blocked_in_subagent() {
        let mut store = SharedStore::test_default();
        store.state.is_subagent = true;

        let args = r#"{"prompt":"do something","agent":"any"}"#;
        let result =
            dispatch_tool("task", "t1".into(), args, &mut store, &silent())
                .await
                .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        assert!(last
            .content
            .as_ref()
            .unwrap()
            .contains("not available here"));
    }

    #[test]
    fn test_all_tools_excludes_task_for_subagent() {
        let parent_tools = all_tools(false);
        let sub_tools = all_tools(true);

        assert!(parent_tools.iter().any(|t| t.function.name == "task"));
        assert!(!sub_tools.iter().any(|t| t.function.name == "task"));
        // Both have base tools
        assert!(parent_tools.iter().any(|t| t.function.name == "bash"));
        assert!(sub_tools.iter().any(|t| t.function.name == "bash"));
    }

    fn silent() -> Arc<dyn Channel> {
        Arc::new(crate::frontend::SilentChannel)
    }
}
