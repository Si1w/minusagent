use crate::tool::ToolDefinition;

use super::builder::{boolean, no_args_tool, object, string, tool};

pub(super) fn primary_tools() -> Vec<ToolDefinition> {
    vec![
        team_spawn_tool(),
        shutdown_request_tool(),
        plan_response_tool(),
    ]
}

pub(super) fn shared_tools() -> Vec<ToolDefinition> {
    vec![
        team_send_tool(),
        team_read_inbox_tool(),
        shutdown_response_tool(),
        plan_submit_tool(),
    ]
}

pub(super) fn subagent_tools() -> Vec<ToolDefinition> {
    vec![idle_tool()]
}

fn team_spawn_tool() -> ToolDefinition {
    tool(
        "team_spawn",
        "Spawn a persistent teammate with its own agent loop. The teammate has an inbox for receiving messages and runs until idle.",
        object(
            vec![
                ("name", string("Teammate name (used as inbox address).")),
                (
                    "role",
                    string("Short role description (e.g. 'coder', 'tester')."),
                ),
                (
                    "prompt",
                    string("Initial task/instruction for the teammate."),
                ),
                (
                    "agent",
                    string("Agent ID from registry for identity. Optional."),
                ),
            ],
            &["name", "role", "prompt"],
        ),
    )
}

fn team_send_tool() -> ToolDefinition {
    tool(
        "team_send",
        "Send a message to a teammate or 'lead'. Wakes idle teammates automatically.",
        object(
            vec![
                ("to", string("Recipient name (teammate name or 'lead').")),
                ("content", string("Message content.")),
            ],
            &["to", "content"],
        ),
    )
}

fn team_read_inbox_tool() -> ToolDefinition {
    tool(
        "team_read_inbox",
        "Read and drain messages from an inbox. Defaults to your own inbox.",
        object(
            vec![("name", string("Inbox to read. Defaults to own inbox."))],
            &[],
        ),
    )
}

fn shutdown_request_tool() -> ToolDefinition {
    tool(
        "shutdown_request",
        "Request a teammate to shut down gracefully. The teammate can approve or reject.",
        object(
            vec![("teammate", string("Name of the teammate to shut down."))],
            &["teammate"],
        ),
    )
}

fn shutdown_response_tool() -> ToolDefinition {
    tool(
        "shutdown_response",
        "Respond to a shutdown request. If approved, you will shut down after finishing current work.",
        object(
            vec![
                ("request_id", string("The shutdown request ID.")),
                ("approve", boolean("Whether to approve the shutdown.")),
                ("reason", string("Reason for the decision.")),
            ],
            &["request_id", "approve"],
        ),
    )
}

fn plan_submit_tool() -> ToolDefinition {
    tool(
        "plan_submit",
        "Submit a plan for lead review. Wait for approval before executing.",
        object(
            vec![("plan", string("Description of the proposed plan."))],
            &["plan"],
        ),
    )
}

fn plan_response_tool() -> ToolDefinition {
    tool(
        "plan_response",
        "Approve or reject a submitted plan.",
        object(
            vec![
                ("request_id", string("The plan request ID.")),
                ("approve", boolean("Whether to approve the plan.")),
                ("feedback", string("Feedback on the plan.")),
            ],
            &["request_id", "approve"],
        ),
    )
}

fn idle_tool() -> ToolDefinition {
    no_args_tool(
        "idle",
        "Signal that you have no more work. You will enter idle state and poll for new messages or unclaimed tasks.",
    )
}
