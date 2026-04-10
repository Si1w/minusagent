use crate::tool::ToolDefinition;

use super::builder::{object, string, tool};

pub(super) fn tools() -> Vec<ToolDefinition> {
    vec![background_run_tool(), background_check_tool()]
}

fn background_run_tool() -> ToolDefinition {
    tool(
        "background_run",
        "Run a shell command in the background. Returns immediately with a task ID. Results are automatically delivered before your next response.",
        object(
            vec![(
                "command",
                string("The shell command to execute in the background."),
            )],
            &["command"],
        ),
    )
}

fn background_check_tool() -> ToolDefinition {
    tool(
        "background_check",
        "Check status of background tasks. Returns all tasks if no task_id given.",
        object(
            vec![(
                "task_id",
                string("Specific background task ID to check. Omit to list all."),
            )],
            &[],
        ),
    )
}
