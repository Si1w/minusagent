use serde_json::json;

use crate::tool::{ToolDefinition, ToolFunction};

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
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: from config)."
                    },
                    "dangerously_disable_sandbox": {
                        "type": "boolean",
                        "description": "Disable OS sandbox. Only when sandbox blocks a legitimate operation."
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

pub fn glob_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Returns paths sorted by \
                modification time (newest first). Patterns without '**/' or '/' are \
                auto-prefixed with '**/'.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g. '**/*.rs', '*.toml')."
                    },
                    "directory": {
                        "type": "string",
                        "description": "Directory to search in (default: current directory)."
                    }
                },
                "required": ["pattern"]
            }),
        },
    }
}

pub fn grep_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "grep".into(),
            description: "Search file contents using regex. Uses ripgrep if available, \
                otherwise falls back to regex crate. Returns matching lines with \
                file path and line number.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for."
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search (default: current directory)."
                    },
                    "include": {
                        "type": "string",
                        "description": "Glob to filter files (e.g. '*.rs')."
                    }
                },
                "required": ["pattern"]
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

pub fn web_fetch_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "web_fetch".into(),
            description: "Fetch the contents of a URL. Returns the \
                          response body (truncated if too large)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch."
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Max response chars (default: from config)."
                    }
                },
                "required": ["url"]
            }),
        },
    }
}

pub fn web_search_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "web_search".into(),
            description: "Search the web via DuckDuckGo. Returns \
                          up to 10 results with title, URL, and snippet."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query."
                    }
                },
                "required": ["query"]
            }),
        },
    }
}

pub fn plan_mode_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "plan_mode".into(),
            description: "Toggle plan mode. In plan mode you should \
                          only research and plan, not execute changes. \
                          Exit plan mode when ready to execute."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "active": {
                        "type": "boolean",
                        "description": "true to enter plan mode, false to exit."
                    }
                },
                "required": ["active"]
            }),
        },
    }
}

pub fn cron_list_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "cron_list".into(),
            description: "List all cron jobs with their status."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {}
            }),
        },
    }
}

pub fn cron_create_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "cron_create".into(),
            description: "Create a new cron job. Persists to CRON.json."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Unique job ID."
                    },
                    "name": {
                        "type": "string",
                        "description": "Human-readable job name."
                    },
                    "schedule_kind": {
                        "type": "string",
                        "enum": ["at", "every", "cron"],
                        "description": "Schedule type."
                    },
                    "at": {
                        "type": "string",
                        "description": "RFC3339 timestamp (for 'at' kind)."
                    },
                    "every_seconds": {
                        "type": "integer",
                        "description": "Interval in seconds (for 'every' kind, default 3600)."
                    },
                    "cron_expr": {
                        "type": "string",
                        "description": "Cron expression (for 'cron' kind, e.g. '0 0 9 * * * *')."
                    },
                    "payload_kind": {
                        "type": "string",
                        "enum": ["agent_turn", "system_event"],
                        "description": "What to execute. Default: agent_turn."
                    },
                    "message": {
                        "type": "string",
                        "description": "Message for agent_turn or text for system_event."
                    },
                    "channel": {
                        "type": "string",
                        "description": "Delivery channel (default: 'bg')."
                    },
                    "delete_after_run": {
                        "type": "boolean",
                        "description": "Auto-delete after execution (for 'at' schedules)."
                    }
                },
                "required": ["id", "name", "schedule_kind", "message"]
            }),
        },
    }
}

pub fn cron_delete_tool() -> ToolDefinition {
    ToolDefinition {
        r#type: "function".into(),
        function: ToolFunction {
            name: "cron_delete".into(),
            description: "Delete a cron job by ID."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "ID of the job to delete."
                    }
                },
                "required": ["job_id"]
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
    has_cron: bool,
) -> Vec<ToolDefinition> {
    let mut tools = vec![
        bash_tool(),
        read_file_tool(),
        write_file_tool(),
        edit_file_tool(),
        glob_tool(),
        grep_tool(),
        todo_tool(),
        background_run_tool(),
        background_check_tool(),
        web_fetch_tool(),
        web_search_tool(),
        plan_mode_tool(),
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
    if has_cron {
        tools.push(cron_list_tool());
        tools.push(cron_create_tool());
        tools.push(cron_delete_tool());
    }
    tools
}
