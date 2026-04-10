use crate::tool::ToolDefinition;

use super::builder::{boolean, integer, no_args_tool, object, string, tool};

pub(super) fn tools() -> Vec<ToolDefinition> {
    vec![
        worktree_create_tool(),
        worktree_remove_tool(),
        worktree_keep_tool(),
        worktree_list_tool(),
        worktree_exec_tool(),
    ]
}

fn worktree_create_tool() -> ToolDefinition {
    tool(
        "worktree_create",
        "Create a git worktree. Optionally bind to a task (auto-sets task to in_progress).",
        object(
            vec![
                (
                    "name",
                    string("Worktree name (used as directory and branch suffix)."),
                ),
                ("task_id", integer("Task ID to bind to this worktree.")),
            ],
            &["name"],
        ),
    )
}

fn worktree_remove_tool() -> ToolDefinition {
    tool(
        "worktree_remove",
        "Remove a git worktree. Set complete_task=true to also mark the bound task as completed.",
        object(
            vec![
                ("name", string("Worktree name to remove.")),
                (
                    "force",
                    boolean("Force removal even with uncommitted changes."),
                ),
                (
                    "complete_task",
                    boolean("Complete the bound task after removal."),
                ),
            ],
            &["name"],
        ),
    )
}

fn worktree_keep_tool() -> ToolDefinition {
    tool(
        "worktree_keep",
        "Mark a worktree as kept for later use.",
        object(vec![("name", string("Worktree name to keep."))], &["name"]),
    )
}

fn worktree_list_tool() -> ToolDefinition {
    no_args_tool(
        "worktree_list",
        "List all worktrees with status and task bindings.",
    )
}

fn worktree_exec_tool() -> ToolDefinition {
    tool(
        "worktree_exec",
        "Run a shell command inside a worktree directory (isolated cwd).",
        object(
            vec![
                ("name", string("Worktree name.")),
                ("command", string("Shell command to execute.")),
            ],
            &["name", "command"],
        ),
    )
}
