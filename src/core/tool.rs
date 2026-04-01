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
use crate::core::task::{BackgroundStatus, TaskStatus};
use crate::core::todo::{TodoItem, TodoWrite};
use crate::frontend::Channel;
use crate::core::worktree::WorktreeStatus;
use crate::config::tuning;
use crate::intelligence::manager::normalize_agent_id;

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

/// Create a persistent task in the task graph
pub fn task_create_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "task_create".into(),
            description: "Create a new persistent task in the task graph.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "Short task title."
                    },
                    "description": {
                        "type": "string",
                        "description": "Detailed task description."
                    }
                },
                "required": ["subject"]
            }),
        },
    }
}

/// Update a task's status or dependencies
pub fn task_update_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "task_update".into(),
            description: "Update a task's status or dependencies. \
                          Setting status to 'completed' auto-unblocks \
                          dependent tasks."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "integer",
                        "description": "Task ID to update."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "in_progress", "completed"],
                        "description": "New task status."
                    },
                    "blocked_by": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Task IDs this task depends on (adds to existing)."
                    },
                    "blocks": {
                        "type": "array",
                        "items": {"type": "integer"},
                        "description": "Task IDs this task blocks (adds to existing)."
                    }
                },
                "required": ["task_id"]
            }),
        },
    }
}

/// List all tasks in the task graph
pub fn task_list_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "task_list".into(),
            description: "List all tasks with their status and dependencies."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
    }
}

/// Get a specific task by ID
pub fn task_get_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "task_get".into(),
            description: "Get details of a specific task by ID.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "integer",
                        "description": "Task ID to retrieve."
                    }
                },
                "required": ["task_id"]
            }),
        },
    }
}

/// Run a shell command in the background
pub fn background_run_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "background_run".into(),
            description: "Run a shell command in the background. \
                          Returns immediately with a task ID. \
                          Results are automatically delivered \
                          before your next response."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute in the background."
                    }
                },
                "required": ["command"]
            }),
        },
    }
}

/// Check status of background tasks
pub fn background_check_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "background_check".into(),
            description: "Check status of background tasks. \
                          Returns all tasks if no task_id given."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Specific background task ID to check. Omit to list all."
                    }
                }
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

/// Spawn a persistent teammate with its own agent loop
pub fn team_spawn_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "team_spawn".into(),
            description: "Spawn a persistent teammate with its own \
                          agent loop. The teammate has an inbox for \
                          receiving messages and runs until idle."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Teammate name (used as inbox address)."
                    },
                    "role": {
                        "type": "string",
                        "description": "Short role description (e.g. 'coder', 'tester')."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Initial task/instruction for the teammate."
                    },
                    "agent": {
                        "type": "string",
                        "description": "Agent ID from registry for identity. Optional."
                    }
                },
                "required": ["name", "role", "prompt"]
            }),
        },
    }
}

/// Send a message to a teammate's inbox
pub fn team_send_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "team_send".into(),
            description: "Send a message to a teammate or 'lead'. \
                          Wakes idle teammates automatically."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Recipient name (teammate name or 'lead')."
                    },
                    "content": {
                        "type": "string",
                        "description": "Message content."
                    }
                },
                "required": ["to", "content"]
            }),
        },
    }
}

/// Read and drain an inbox
pub fn team_read_inbox_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "team_read_inbox".into(),
            description: "Read and drain messages from an inbox. \
                          Defaults to your own inbox."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Inbox to read. Defaults to own inbox."
                    }
                }
            }),
        },
    }
}

/// Request graceful shutdown of a teammate
pub fn shutdown_request_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "shutdown_request".into(),
            description: "Request a teammate to shut down \
                          gracefully. The teammate can approve \
                          or reject."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "teammate": {
                        "type": "string",
                        "description": "Name of the teammate to shut down."
                    }
                },
                "required": ["teammate"]
            }),
        },
    }
}

/// Respond to a shutdown request (approve/reject)
pub fn shutdown_response_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "shutdown_response".into(),
            description: "Respond to a shutdown request. \
                          If approved, you will shut down after \
                          finishing current work."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "request_id": {
                        "type": "string",
                        "description": "The shutdown request ID."
                    },
                    "approve": {
                        "type": "boolean",
                        "description": "Whether to approve the shutdown."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Reason for the decision."
                    }
                },
                "required": ["request_id", "approve"]
            }),
        },
    }
}

/// Submit a plan for lead review before execution
pub fn plan_submit_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "plan_submit".into(),
            description: "Submit a plan for lead review. \
                          Wait for approval before executing."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "Description of the proposed plan."
                    }
                },
                "required": ["plan"]
            }),
        },
    }
}

/// Respond to a plan submission (approve/reject)
pub fn plan_response_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "plan_response".into(),
            description: "Approve or reject a submitted plan."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "request_id": {
                        "type": "string",
                        "description": "The plan request ID."
                    },
                    "approve": {
                        "type": "boolean",
                        "description": "Whether to approve the plan."
                    },
                    "feedback": {
                        "type": "string",
                        "description": "Feedback on the plan."
                    }
                },
                "required": ["request_id", "approve"]
            }),
        },
    }
}

/// Claim an unclaimed task from the task board
pub fn claim_task_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "claim_task".into(),
            description: "Claim an unclaimed task from the task \
                          board. Sets you as owner and marks it \
                          in_progress."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "integer",
                        "description": "ID of the task to claim."
                    }
                },
                "required": ["task_id"]
            }),
        },
    }
}

/// Signal idle state to wait for new work
pub fn idle_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "idle".into(),
            description: "Signal that you have no more work. \
                          You will enter idle state and poll for \
                          new messages or unclaimed tasks."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
    }
}

// ── Worktree tools ───────────────────────────────────────

/// Create a git worktree for task isolation
pub fn worktree_create_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "worktree_create".into(),
            description: "Create a git worktree. Optionally bind \
                          to a task (auto-sets task to in_progress)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Worktree name (used as directory and branch suffix)."
                    },
                    "task_id": {
                        "type": "integer",
                        "description": "Task ID to bind to this worktree."
                    }
                },
                "required": ["name"]
            }),
        },
    }
}

/// Remove a git worktree
pub fn worktree_remove_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "worktree_remove".into(),
            description: "Remove a git worktree. Set \
                          complete_task=true to also mark the \
                          bound task as completed."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Worktree name to remove."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "Force removal even with uncommitted changes."
                    },
                    "complete_task": {
                        "type": "boolean",
                        "description": "Complete the bound task after removal."
                    }
                },
                "required": ["name"]
            }),
        },
    }
}

/// Keep a worktree (mark as preserved)
pub fn worktree_keep_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "worktree_keep".into(),
            description: "Mark a worktree as kept for later use."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Worktree name to keep."
                    }
                },
                "required": ["name"]
            }),
        },
    }
}

/// List all worktrees
pub fn worktree_list_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "worktree_list".into(),
            description: "List all worktrees with status and \
                          task bindings."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
    }
}

/// Execute a command in a worktree directory
pub fn worktree_exec_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "worktree_exec".into(),
            description: "Run a shell command inside a worktree \
                          directory (isolated cwd)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Worktree name."
                    },
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute."
                    }
                },
                "required": ["name", "command"]
            }),
        },
    }
}

/// All built-in tool definitions for LLM registration
pub fn all_tools(
    is_subagent: bool,
    has_tasks: bool,
    has_team: bool,
    has_worktrees: bool,
) -> Vec<ToolDefinition> {
    let mut tools = vec![
        bash_tool(),
        read_file_tool(),
        write_file_tool(),
        edit_file_tool(),
        todo_tool(),
        background_run_tool(),
        background_check_tool(),
    ];
    if !is_subagent {
        tools.push(task_tool());
    }
    if has_tasks {
        tools.push(task_create_tool());
        tools.push(task_update_tool());
        tools.push(task_list_tool());
        tools.push(task_get_tool());
        tools.push(claim_task_tool());
    }
    if has_team {
        if !is_subagent {
            tools.push(team_spawn_tool());
            tools.push(shutdown_request_tool());
            tools.push(plan_response_tool());
        }
        tools.push(team_send_tool());
        tools.push(team_read_inbox_tool());
        tools.push(shutdown_response_tool());
        tools.push(plan_submit_tool());
        if is_subagent {
            tools.push(idle_tool());
        }
    }
    if has_worktrees {
        tools.push(worktree_create_tool());
        tools.push(worktree_remove_tool());
        tools.push(worktree_keep_tool());
        tools.push(worktree_list_tool());
        tools.push(worktree_exec_tool());
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

            let node = BashExec {
                call_id: call_id.clone(),
                command,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "read_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = ReadFile {
                call_id: call_id.clone(),
                path,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
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
            let node = WriteFile {
                call_id: call_id.clone(),
                path,
                content,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
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
                call_id: call_id.clone(),
                path,
                old_string,
                new_string,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "todo" => {
            let raw = args.get("items").cloned()
                .unwrap_or(serde_json::Value::Array(Vec::new()));
            let items: Vec<TodoItem> = match serde_json::from_value(
                if raw.is_null() {
                    serde_json::Value::Array(Vec::new())
                } else {
                    raw
                },
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
        "background_run" => {
            let command = args["command"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            let dangerous =
                ["rm -rf /", "sudo", "shutdown", "reboot", "> /dev/"];
            let blocked = dangerous.iter().any(|p| command.contains(p));
            if blocked {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: command blocked (dangerous pattern)".into(),
                );
            } else if !channel.confirm(&command).await {
                push_tool_result(
                    store,
                    &call_id,
                    "User denied execution.".into(),
                );
            } else {
                let task_id = store.state.background.run(&command);
                push_tool_result(
                    store,
                    &call_id,
                    format!(
                        "Background task {task_id} started: {command}"
                    ),
                );
            }
        }
        "background_check" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str());
            match task_id {
                Some(id) => match store.state.background.get(id) {
                    Some(task) => {
                        let status = match task.status {
                            BackgroundStatus::Running => "running",
                            BackgroundStatus::Completed => "completed",
                            BackgroundStatus::Failed => "failed",
                        };
                        let output = task
                            .output
                            .as_deref()
                            .unwrap_or("(still running)");
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Task {id}: {status}\n\
                                 Command: {}\n\
                                 Output:\n{output}",
                                task.command,
                            ),
                        );
                    }
                    None => {
                        push_tool_result(
                            store,
                            &call_id,
                            format!("Error: background task '{id}' not found"),
                        );
                    }
                },
                None => {
                    let tasks = store.state.background.list();
                    if tasks.is_empty() {
                        push_tool_result(
                            store,
                            &call_id,
                            "No background tasks.".into(),
                        );
                    } else {
                        let lines: Vec<String> = tasks
                            .iter()
                            .map(|t| {
                                let status = match t.status {
                                    BackgroundStatus::Running => {
                                        "running"
                                    }
                                    BackgroundStatus::Completed => {
                                        "completed"
                                    }
                                    BackgroundStatus::Failed => "failed",
                                };
                                format!(
                                    "  {} [{}] {}",
                                    t.id, status, t.command,
                                )
                            })
                            .collect();
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Background tasks:\n{}",
                                lines.join("\n")
                            ),
                        );
                    }
                }
            }
        }
        "task_create" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let subject = args["subject"]
                        .as_str()
                        .unwrap_or_default();
                    let description = args["description"]
                        .as_str()
                        .unwrap_or_default();
                    match mgr.create(subject, description) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_update" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id = args["task_id"].as_u64().unwrap_or(0) as usize;
                    let status = args["status"].as_str().and_then(|s| {
                        serde_json::from_value::<TaskStatus>(
                            serde_json::Value::String(s.to_string()),
                        )
                        .ok()
                    });
                    let blocked_by: Option<Vec<usize>> = args
                        .get("blocked_by")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_u64().map(|n| n as usize))
                                .collect()
                        });
                    let blocks: Option<Vec<usize>> = args
                        .get("blocks")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_u64().map(|n| n as usize))
                                .collect()
                        });
                    match mgr.update(task_id, status, blocked_by, blocks) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_list" => {
            match &store.state.tasks {
                Some(mgr) => match mgr.list_all() {
                    Ok(json) => push_tool_result(store, &call_id, json),
                    Err(e) => push_tool_result(
                        store,
                        &call_id,
                        format!("Error: {e}"),
                    ),
                },
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_get" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id = args["task_id"].as_u64().unwrap_or(0) as usize;
                    match mgr.get(task_id) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
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
                            store.state.tasks.clone(),
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
        "claim_task" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id =
                        args["task_id"].as_u64().unwrap_or(0)
                            as usize;
                    let owner = store
                        .state
                        .team_name
                        .as_deref()
                        .unwrap_or("lead")
                        .to_string();
                    match mgr.claim(task_id, &owner) {
                        Ok(json) => {
                            push_tool_result(store, &call_id, json)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "idle" => {
            store.state.idle_requested = true;
            push_tool_result(
                store,
                &call_id,
                "Entering idle state. Will resume when new \
                 work arrives."
                    .into(),
            );
        }
        "team_spawn" => {
            if store.state.is_subagent {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: team_spawn not available for teammates"
                        .into(),
                );
            } else {
                let team = store.state.team.clone();
                match team {
                    Some(team) => {
                        let name = args["name"]
                            .as_str()
                            .unwrap_or_default();
                        let role = args["role"]
                            .as_str()
                            .unwrap_or_default();
                        let prompt = args["prompt"]
                            .as_str()
                            .unwrap_or_default();
                        let agent_id = normalize_agent_id(
                            args.get("agent")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                        );
                        let llm_config =
                            store.state.config.llm.clone();
                        let agents = store.state.agents.clone();
                        let tasks =
                            store.state.tasks.clone();
                        match team.spawn(
                            name,
                            role,
                            prompt,
                            &agent_id,
                            llm_config,
                            agents,
                            tasks,
                        ) {
                            Ok(msg) => push_tool_result(
                                store, &call_id, msg,
                            ),
                            Err(e) => push_tool_result(
                                store,
                                &call_id,
                                format!("Error: {e}"),
                            ),
                        }
                    }
                    None => {
                        push_tool_result(
                            store,
                            &call_id,
                            "Error: team not available".into(),
                        );
                    }
                }
            }
        }
        "team_send" => {
            let sender = store.state.sender_name().to_string();
            let to = args["to"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let content = args["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    match team.send_message(&sender, &to, &content)
                    {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "shutdown_request" => {
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let teammate = args["teammate"]
                        .as_str()
                        .unwrap_or_default();
                    match team.request_shutdown(teammate) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "shutdown_response" => {
            let sender = store.state.sender_name().to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let req_id = args["request_id"]
                        .as_str()
                        .unwrap_or_default();
                    let approve = args["approve"]
                        .as_bool()
                        .unwrap_or(false);
                    let reason = args
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match team.respond_shutdown(
                        req_id, approve, reason, &sender,
                    ) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "plan_submit" => {
            let sender = store.state.sender_name().to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let plan = args["plan"]
                        .as_str()
                        .unwrap_or_default();
                    match team.submit_plan(&sender, plan) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "plan_response" => {
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let req_id = args["request_id"]
                        .as_str()
                        .unwrap_or_default();
                    let approve = args["approve"]
                        .as_bool()
                        .unwrap_or(false);
                    let feedback = args
                        .get("feedback")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match team.respond_plan(
                        req_id, approve, feedback,
                    ) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "team_read_inbox" => {
            let default_name = store.state.sender_name().to_string();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&default_name);
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let result = team.read_inbox(name);
                    push_tool_result(store, &call_id, result);
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "worktree_create" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let task_id = args
                        .get("task_id")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);
                    match wt.create(name, task_id) {
                        Ok(json) => {
                            if let (Some(tid), Some(tasks)) =
                                (task_id, &store.state.tasks)
                            {
                                let _ = tasks
                                    .bind_worktree(tid, name);
                            }
                            push_tool_result(
                                store, &call_id, json,
                            );
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_remove" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let force = args
                        .get("force")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let complete_task = args
                        .get("complete_task")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    match wt.remove(name, force) {
                        Ok(entry) => {
                            if complete_task {
                                if let (Some(tid), Some(tasks)) =
                                    (
                                        entry.task_id,
                                        &store.state.tasks,
                                    )
                                {
                                    let _ = tasks.update(
                                        tid,
                                        Some(
                                            TaskStatus::Completed,
                                        ),
                                        None,
                                        None,
                                    );
                                    let _ = tasks
                                        .unbind_worktree(tid);
                                }
                            }
                            push_tool_result(
                                store,
                                &call_id,
                                format!(
                                    "Worktree '{name}' removed"
                                ),
                            );
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_keep" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    match wt.keep(name) {
                        Ok(msg) => {
                            push_tool_result(
                                store, &call_id, msg,
                            )
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_list" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    push_tool_result(
                        store,
                        &call_id,
                        wt.list_formatted(),
                    );
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_exec" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let command = args["command"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    match wt.get(name) {
                        Some(entry)
                            if entry.status
                                == WorktreeStatus::Active
                                || entry.status
                                    == WorktreeStatus::Kept =>
                        {
                            if !channel.confirm(&command).await {
                                push_tool_result(
                                    store,
                                    &call_id,
                                    "User denied execution."
                                        .into(),
                                );
                            } else {
                                let bash_timeout = Duration::from_secs(tuning().bash_timeout_secs);
                                let result =
                                    tokio::time::timeout(
                                        bash_timeout,
                                        Command::new("sh")
                                            .arg("-c")
                                            .arg(&command)
                                            .current_dir(
                                                &entry.path,
                                            )
                                            .output(),
                                    )
                                    .await;
                                let text = match result {
                                    Ok(Ok(out)) => {
                                        let stdout =
                                            String::from_utf8_lossy(
                                                &out.stdout,
                                            );
                                        let stderr =
                                            String::from_utf8_lossy(
                                                &out.stderr,
                                            );
                                        if out.status.success() {
                                            stdout.to_string()
                                        } else {
                                            format!(
                                                "stdout:\n{stdout}\n\
                                                 stderr:\n{stderr}"
                                            )
                                        }
                                    }
                                    Ok(Err(e)) => {
                                        format!("Error: {e}")
                                    }
                                    Err(_) => format!(
                                        "Error: timeout ({}s)",
                                        bash_timeout.as_secs()
                                    ),
                                };
                                push_tool_result(
                                    store, &call_id, text,
                                );
                            }
                        }
                        Some(_) => push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: worktree '{name}' \
                                 is not active"
                            ),
                        ),
                        None => push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: worktree '{name}' \
                                 not found"
                            ),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
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
        let parent_tools = all_tools(false, false, false, false);
        let sub_tools = all_tools(true, false, false, false);

        assert!(parent_tools.iter().any(|t| t.function.name == "task"));
        assert!(!sub_tools.iter().any(|t| t.function.name == "task"));
        // Both have base tools
        assert!(parent_tools.iter().any(|t| t.function.name == "bash"));
        assert!(sub_tools.iter().any(|t| t.function.name == "bash"));
    }

    #[test]
    fn test_all_tools_includes_task_graph_tools() {
        let without = all_tools(false, false, false, false);
        let with = all_tools(false, true, false, false);

        assert!(!without.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_update"));
        assert!(with.iter().any(|t| t.function.name == "task_list"));
        assert!(with.iter().any(|t| t.function.name == "task_get"));
    }

    #[test]
    fn test_all_tools_includes_team_tools() {
        let without = all_tools(false, false, false, false);
        let with = all_tools(false, false, true, false);
        let sub_with = all_tools(true, false, true, false);

        assert!(
            !without.iter().any(|t| t.function.name == "team_spawn")
        );
        // Lead has spawn, shutdown_request, plan_response
        assert!(
            with.iter().any(|t| t.function.name == "team_spawn")
        );
        assert!(
            with.iter().any(|t| t.function.name == "team_send")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "team_read_inbox")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "shutdown_request")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "plan_response")
        );

        // Subagent (teammate) can send/read/respond but not
        // spawn/request-shutdown/respond-plan
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "team_spawn")
        );
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_request")
        );
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "plan_response")
        );
        assert!(
            sub_with.iter().any(|t| t.function.name == "team_send")
        );
        assert!(
            sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_response")
        );
        assert!(
            sub_with
                .iter()
                .any(|t| t.function.name == "plan_submit")
        );
    }

    fn silent() -> Arc<dyn Channel> {
        Arc::new(crate::frontend::SilentChannel)
    }
}
