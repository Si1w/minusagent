use anyhow::Result;
use serde::Deserialize;

use crate::team::{TaskStatus, WorktreeStatus};

use super::super::{ToolContext, exec};
use super::common::{parse_request, push_manager_result, require_worktrees, run_node};

#[derive(Default, Deserialize)]
struct WorktreeCreateRequest {
    name: String,
    task_id: Option<usize>,
}

#[derive(Default, Deserialize)]
struct WorktreeRemoveRequest {
    name: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    complete_task: bool,
}

#[derive(Default, Deserialize)]
struct WorktreeKeepRequest {
    name: String,
}

#[derive(Default, Deserialize)]
struct WorktreeExecRequest {
    name: String,
    command: String,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "worktree_create" => {
            let Some(request) = parse_request::<WorktreeCreateRequest>(ctx) else {
                return Ok(true);
            };
            handle_worktree_create(ctx, &request);
        }
        "worktree_remove" => {
            let Some(request) = parse_request::<WorktreeRemoveRequest>(ctx) else {
                return Ok(true);
            };
            handle_worktree_remove(ctx, &request);
        }
        "worktree_keep" => {
            let Some(request) = parse_request::<WorktreeKeepRequest>(ctx) else {
                return Ok(true);
            };
            handle_worktree_keep(ctx, &request);
        }
        "worktree_list" => handle_worktree_list(ctx),
        "worktree_exec" => {
            let Some(request) = parse_request::<WorktreeExecRequest>(ctx) else {
                return Ok(true);
            };
            handle_worktree_exec(ctx, request).await;
        }
        _ => return Ok(false),
    }

    Ok(true)
}

fn handle_worktree_create(ctx: &mut ToolContext<'_>, request: &WorktreeCreateRequest) {
    let Some(worktrees) = require_worktrees(ctx) else {
        return;
    };

    match worktrees.create(&request.name, request.task_id) {
        Ok(json) => {
            if let (Some(task_id), Some(tasks)) = (request.task_id, &ctx.store.state.tasks) {
                let _ = tasks.bind_worktree(task_id, &request.name);
            }
            ctx.push_result(json);
        }
        Err(error) => ctx.push_result(format!("Error: {error}")),
    }
}

fn handle_worktree_remove(ctx: &mut ToolContext<'_>, request: &WorktreeRemoveRequest) {
    let Some(worktrees) = require_worktrees(ctx) else {
        return;
    };

    match worktrees.remove(&request.name, request.force) {
        Ok(entry) => {
            if request.complete_task
                && let (Some(task_id), Some(tasks)) = (entry.task_id, &ctx.store.state.tasks)
            {
                let _ = tasks.update(task_id, Some(TaskStatus::Completed), None, None);
                let _ = tasks.unbind_worktree(task_id);
            }
            ctx.push_result(format!("Worktree '{}' removed", request.name));
        }
        Err(error) => ctx.push_result(format!("Error: {error}")),
    }
}

fn handle_worktree_keep(ctx: &mut ToolContext<'_>, request: &WorktreeKeepRequest) {
    let Some(worktrees) = require_worktrees(ctx) else {
        return;
    };
    push_manager_result(ctx, worktrees.keep(&request.name));
}

fn handle_worktree_list(ctx: &mut ToolContext<'_>) {
    let Some(worktrees) = require_worktrees(ctx) else {
        return;
    };
    ctx.push_result(worktrees.list_formatted());
}

async fn handle_worktree_exec(ctx: &mut ToolContext<'_>, request: WorktreeExecRequest) {
    let Some(worktrees) = require_worktrees(ctx) else {
        return;
    };

    match worktrees.get(&request.name) {
        Some(entry)
            if entry.status == WorktreeStatus::Active || entry.status == WorktreeStatus::Kept =>
        {
            if ctx.channel.confirm(&request.command).await {
                run_node(
                    exec::BashExec {
                        call_id: ctx.call_id.to_string(),
                        command: request.command,
                        sandbox: true,
                        timeout_secs: None,
                        current_dir: Some(entry.path.clone().into()),
                    },
                    ctx,
                )
                .await;
            } else {
                ctx.push_result("User denied execution.");
            }
        }
        Some(_) => ctx.push_result(format!("Error: worktree '{}' is not active", request.name)),
        None => ctx.push_result(format!("Error: worktree '{}' not found", request.name)),
    }
}
