use crate::tool::ToolDefinition;

use super::builder::{boolean, integer, object, string, tool};

pub(super) fn tools() -> Vec<ToolDefinition> {
    vec![web_fetch_tool(), web_search_tool(), plan_mode_tool()]
}

fn web_fetch_tool() -> ToolDefinition {
    tool(
        "web_fetch",
        "Fetch the contents of a URL. Returns the response body (truncated if too large).",
        object(
            vec![
                ("url", string("The URL to fetch.")),
                (
                    "max_length",
                    integer("Max response chars (default: from config)."),
                ),
            ],
            &["url"],
        ),
    )
}

fn web_search_tool() -> ToolDefinition {
    tool(
        "web_search",
        "Search the web via DuckDuckGo. Returns up to 10 results with title, URL, and snippet.",
        object(vec![("query", string("Search query."))], &["query"]),
    )
}

fn plan_mode_tool() -> ToolDefinition {
    tool(
        "plan_mode",
        "Toggle plan mode. In plan mode you should only research and plan, not execute changes. Exit plan mode when ready to execute.",
        object(
            vec![("active", boolean("true to enter plan mode, false to exit."))],
            &["active"],
        ),
    )
}
