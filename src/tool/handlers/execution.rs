use anyhow::Result;
use serde::Deserialize;

use crate::team::{TodoItem, TodoWrite};

use super::super::{ToolContext, exec, search};
use super::common::{parse_request, run_node};

#[derive(Default, Deserialize)]
struct BashRequest {
    command: String,
    timeout: Option<u64>,
    #[serde(default)]
    dangerously_disable_sandbox: bool,
}

#[derive(Default, Deserialize)]
struct ReadFileRequest {
    path: String,
}

#[derive(Default, Deserialize)]
struct WriteFileRequest {
    path: String,
    content: String,
}

#[derive(Default, Deserialize)]
struct EditFileRequest {
    path: String,
    old_string: String,
    new_string: String,
}

#[derive(Default, Deserialize)]
struct GlobRequest {
    pattern: String,
    directory: Option<String>,
}

#[derive(Default, Deserialize)]
struct GrepRequest {
    pattern: String,
    path: Option<String>,
    include: Option<String>,
}

#[derive(Default, Deserialize)]
struct TodoRequest {
    items: Vec<TodoItem>,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "bash" => {
            let Some(request) = parse_request::<BashRequest>(ctx) else {
                return Ok(true);
            };
            handle_bash(ctx, request).await;
        }
        "read_file" => {
            let Some(request) = parse_request::<ReadFileRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                exec::ReadFile {
                    call_id: ctx.call_id.to_string(),
                    path: request.path,
                },
                ctx,
            )
            .await;
        }
        "write_file" => {
            let Some(request) = parse_request::<WriteFileRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                exec::WriteFile {
                    call_id: ctx.call_id.to_string(),
                    path: request.path,
                    content: request.content,
                },
                ctx,
            )
            .await;
        }
        "edit_file" => {
            let Some(request) = parse_request::<EditFileRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                exec::EditFile {
                    call_id: ctx.call_id.to_string(),
                    path: request.path,
                    old_string: request.old_string,
                    new_string: request.new_string,
                },
                ctx,
            )
            .await;
        }
        "glob" => {
            let Some(request) = parse_request::<GlobRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                search::GlobFile {
                    call_id: ctx.call_id.to_string(),
                    pattern: request.pattern,
                    directory: request.directory,
                },
                ctx,
            )
            .await;
        }
        "grep" => {
            let Some(request) = parse_request::<GrepRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                search::GrepFile {
                    call_id: ctx.call_id.to_string(),
                    pattern: request.pattern,
                    path: request.path,
                    include: request.include,
                },
                ctx,
            )
            .await;
        }
        "todo" => {
            let Some(request) = parse_request::<TodoRequest>(ctx) else {
                return Ok(true);
            };
            run_node(
                TodoWrite {
                    call_id: ctx.call_id.to_string(),
                    items: request.items,
                },
                ctx,
            )
            .await;
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_bash(ctx: &mut ToolContext<'_>, request: BashRequest) {
    if !ctx.channel.confirm(&request.command).await {
        ctx.push_result("User denied execution.");
        return;
    }

    run_node(
        exec::BashExec {
            call_id: ctx.call_id.to_string(),
            command: request.command,
            sandbox: !request.dangerously_disable_sandbox,
            timeout_secs: request.timeout,
            current_dir: None,
        },
        ctx,
    )
    .await;
}
