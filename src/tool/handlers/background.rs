use anyhow::Result;
use serde::Deserialize;

use super::super::{ToolContext, exec};
use super::common::{background_task_list, format_background_task, parse_request};

#[derive(Default, Deserialize)]
struct BackgroundRunRequest {
    command: String,
}

#[derive(Default, Deserialize)]
struct BackgroundCheckRequest {
    task_id: Option<String>,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "background_run" => {
            let Some(request) = parse_request::<BackgroundRunRequest>(ctx) else {
                return Ok(true);
            };
            handle_background_run(ctx, request).await;
        }
        "background_check" => {
            let Some(request) = parse_request::<BackgroundCheckRequest>(ctx) else {
                return Ok(true);
            };
            handle_background_check(ctx, &request);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_background_run(ctx: &mut ToolContext<'_>, request: BackgroundRunRequest) {
    if exec::is_dangerous_command(&request.command) {
        ctx.push_result("Error: command blocked (dangerous pattern)");
    } else if !ctx.channel.confirm(&request.command).await {
        ctx.push_result("User denied execution.");
    } else {
        let task_id = ctx.store.state.background.run(&request.command);
        ctx.push_result(format!(
            "Background task {task_id} started: {}",
            request.command
        ));
    }
}

fn handle_background_check(ctx: &mut ToolContext<'_>, request: &BackgroundCheckRequest) {
    if let Some(task_id) = request.task_id.as_deref() {
        match ctx.store.state.background.get(task_id) {
            Some(task) => ctx.push_result(format_background_task(task_id, &task)),
            None => ctx.push_result(format!("Error: background task '{task_id}' not found")),
        }
        return;
    }

    let tasks = ctx.store.state.background.list();
    ctx.push_result(background_task_list(&tasks));
}
