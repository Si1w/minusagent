use serde_json::json;

use crate::tool::ToolDefinition;

use super::builder::{
    boolean, integer, no_args_tool, object, string, string_enum, tool, with_default,
};

pub(super) fn tools() -> Vec<ToolDefinition> {
    vec![cron_list_tool(), cron_create_tool(), cron_delete_tool()]
}

fn cron_list_tool() -> ToolDefinition {
    no_args_tool("cron_list", "List all cron jobs with their status.")
}

fn cron_create_tool() -> ToolDefinition {
    tool(
        "cron_create",
        "Create a new cron job. Persists to CRON.json.",
        object(
            vec![
                ("id", string("Unique job ID.")),
                ("name", string("Human-readable job name.")),
                (
                    "schedule_kind",
                    with_default(
                        string_enum("Schedule type.", &["at", "every", "cron"]),
                        json!("every"),
                    ),
                ),
                ("at", string("RFC3339 timestamp (for 'at' kind).")),
                (
                    "every_seconds",
                    with_default(
                        integer("Interval in seconds (for 'every' kind)."),
                        json!(3600),
                    ),
                ),
                (
                    "cron_expr",
                    string("Cron expression (for 'cron' kind, e.g. '0 0 9 * * * *')."),
                ),
                (
                    "payload_kind",
                    with_default(
                        string_enum("What to execute.", &["agent_turn", "system_event"]),
                        json!("agent_turn"),
                    ),
                ),
                (
                    "message",
                    string("Message for agent_turn or text for system_event."),
                ),
                (
                    "channel",
                    with_default(string("Delivery channel."), json!("bg")),
                ),
                (
                    "delete_after_run",
                    with_default(
                        boolean("Auto-delete after execution (for 'at' schedules)."),
                        json!(false),
                    ),
                ),
            ],
            &["id", "name", "message"],
        ),
    )
}

fn cron_delete_tool() -> ToolDefinition {
    tool(
        "cron_delete",
        "Delete a cron job by ID.",
        object(
            vec![("job_id", string("ID of the job to delete."))],
            &["job_id"],
        ),
    )
}
