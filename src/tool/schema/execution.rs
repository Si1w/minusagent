use serde_json::json;

use crate::tool::ToolDefinition;

use super::builder::{array, boolean, integer, object, string, string_enum, tool, with_default};

pub(super) fn tools() -> Vec<ToolDefinition> {
    vec![
        bash_tool(),
        read_file_tool(),
        write_file_tool(),
        edit_file_tool(),
        glob_tool(),
        grep_tool(),
        todo_tool(),
    ]
}

fn bash_tool() -> ToolDefinition {
    tool(
        "bash",
        "Run a shell command.",
        object(
            vec![
                ("command", string("The shell command to execute.")),
                (
                    "timeout",
                    integer("Timeout in seconds (default: from config)."),
                ),
                (
                    "dangerously_disable_sandbox",
                    with_default(
                        boolean(
                            "Disable OS sandbox. Only when sandbox blocks a legitimate operation.",
                        ),
                        json!(false),
                    ),
                ),
            ],
            &["command"],
        ),
    )
}

fn read_file_tool() -> ToolDefinition {
    tool(
        "read_file",
        "Read the contents of a file. Returns line-numbered content.",
        object(vec![("path", string("The file path to read."))], &["path"]),
    )
}

fn write_file_tool() -> ToolDefinition {
    tool(
        "write_file",
        "Write content to a file. Creates parent directories if needed.",
        object(
            vec![
                ("path", string("The file path to write to.")),
                ("content", string("The content to write.")),
            ],
            &["path", "content"],
        ),
    )
}

fn edit_file_tool() -> ToolDefinition {
    tool(
        "edit_file",
        "Edit a file by replacing a unique string with a new string.",
        object(
            vec![
                ("path", string("The file path to edit.")),
                (
                    "old_string",
                    string("The exact string to find (must be unique in the file)."),
                ),
                ("new_string", string("The replacement string.")),
            ],
            &["path", "old_string", "new_string"],
        ),
    )
}

fn glob_tool() -> ToolDefinition {
    tool(
        "glob",
        "Find files matching a glob pattern. Returns paths sorted by modification time (newest first). Patterns without '**/' or '/' are auto-prefixed with '**/'.",
        object(
            vec![
                (
                    "pattern",
                    string("Glob pattern (e.g. '**/*.rs', '*.toml')."),
                ),
                (
                    "directory",
                    string("Directory to search in (default: current directory)."),
                ),
            ],
            &["pattern"],
        ),
    )
}

fn grep_tool() -> ToolDefinition {
    tool(
        "grep",
        "Search file contents using regex. Uses ripgrep if available, otherwise falls back to regex crate. Returns matching lines with file path and line number.",
        object(
            vec![
                ("pattern", string("Regex pattern to search for.")),
                (
                    "path",
                    string("File or directory to search (default: current directory)."),
                ),
                ("include", string("Glob to filter files (e.g. '*.rs').")),
            ],
            &["pattern"],
        ),
    )
}

fn todo_tool() -> ToolDefinition {
    tool(
        "todo",
        "Update the task plan. Use this to track progress on multi-step tasks. Only one task can be in_progress at a time.",
        object(
            vec![(
                "items",
                array(
                    "Full list of todo items. Replaces all existing items.",
                    &object(
                        vec![
                            ("id", integer("Unique task ID.")),
                            ("text", string("Task description.")),
                            (
                                "status",
                                string_enum(
                                    "Task status.",
                                    &["pending", "in_progress", "completed"],
                                ),
                            ),
                        ],
                        &["id", "text", "status"],
                    ),
                ),
            )],
            &["items"],
        ),
    )
}
