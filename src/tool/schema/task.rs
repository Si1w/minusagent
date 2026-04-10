use crate::tool::ToolDefinition;

use super::builder::{
    array, integer, integer_item, no_args_tool, object, string, string_enum, tool,
};

pub(super) fn primary_tools() -> Vec<ToolDefinition> {
    vec![task_tool()]
}

pub(super) fn graph_tools() -> Vec<ToolDefinition> {
    vec![
        task_create_tool(),
        task_update_tool(),
        task_list_tool(),
        task_get_tool(),
        claim_task_tool(),
    ]
}

fn task_create_tool() -> ToolDefinition {
    tool(
        "task_create",
        "Create a new persistent task in the task graph.",
        object(
            vec![
                ("subject", string("Short task title.")),
                ("description", string("Detailed task description.")),
            ],
            &["subject"],
        ),
    )
}

fn task_update_tool() -> ToolDefinition {
    tool(
        "task_update",
        "Update a task's status or dependencies. Setting status to 'completed' auto-unblocks dependent tasks.",
        object(
            vec![
                ("task_id", integer("Task ID to update.")),
                (
                    "status",
                    string_enum("New task status.", &["pending", "in_progress", "completed"]),
                ),
                (
                    "blocked_by",
                    array("Task IDs this task depends on.", &integer_item()),
                ),
                (
                    "blocks",
                    array("Task IDs this task blocks.", &integer_item()),
                ),
            ],
            &["task_id"],
        ),
    )
}

fn task_list_tool() -> ToolDefinition {
    no_args_tool(
        "task_list",
        "List all tasks with their status and dependencies.",
    )
}

fn task_get_tool() -> ToolDefinition {
    tool(
        "task_get",
        "Get details of a specific task by ID.",
        object(
            vec![("task_id", integer("Task ID to retrieve."))],
            &["task_id"],
        ),
    )
}

fn task_tool() -> ToolDefinition {
    tool(
        "task",
        "Spawn an agent with fresh context to handle a subtask. The agent runs in isolation and only the final summary is returned.",
        object(
            vec![
                ("prompt", string("The task description.")),
                ("agent", string("Agent ID to handle the task.")),
            ],
            &["prompt", "agent"],
        ),
    )
}

fn claim_task_tool() -> ToolDefinition {
    tool(
        "claim_task",
        "Claim an unclaimed task from the task board. Sets you as owner and marks it in_progress.",
        object(
            vec![("task_id", integer("ID of the task to claim."))],
            &["task_id"],
        ),
    )
}
