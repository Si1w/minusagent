use std::fmt::Write;

use serde::de::DeserializeOwned;
use serde_json::from_value;

use crate::engine::node::Node;
use crate::scheduler::cron::CronHandle;
use crate::team::task::BackgroundTask;
use crate::team::{BackgroundStatus, TaskManager, TeammateManager, WorktreeManager};

use super::super::ToolContext;

pub(super) const TASK_SYSTEM_UNAVAILABLE: &str = "Error: task system not available";
pub(super) const TEAM_UNAVAILABLE: &str = "Error: team not available";
pub(super) const WORKTREE_UNAVAILABLE: &str = "Error: worktree system not available";
pub(super) const CRON_UNAVAILABLE: &str = "Error: cron service not available";

pub(super) async fn run_node<N>(node: N, ctx: &mut ToolContext<'_>)
where
    N: Node,
{
    if let Err(error) = node.run(ctx.store).await {
        ctx.push_result(format!("Error: {error}"));
    }
}

pub(super) fn push_manager_result(ctx: &mut ToolContext<'_>, result: anyhow::Result<String>) {
    match result {
        Ok(output) => ctx.push_result(output),
        Err(error) => ctx.push_result(format!("Error: {error}")),
    }
}

pub(super) fn parse_request<T>(ctx: &mut ToolContext<'_>) -> Option<T>
where
    T: DeserializeOwned,
{
    match from_value(ctx.args.clone()) {
        Ok(request) => Some(request),
        Err(error) => {
            ctx.push_result(format!("Error: invalid arguments: {error}"));
            None
        }
    }
}

pub(super) fn require_tasks(ctx: &mut ToolContext<'_>) -> Option<TaskManager> {
    let tasks = ctx.store.state.tasks.clone();
    if tasks.is_none() {
        ctx.push_result(TASK_SYSTEM_UNAVAILABLE);
    }
    tasks
}

pub(super) fn require_team(ctx: &mut ToolContext<'_>) -> Option<TeammateManager> {
    let team = ctx.store.state.team.clone();
    if team.is_none() {
        ctx.push_result(TEAM_UNAVAILABLE);
    }
    team
}

pub(super) fn require_worktrees(ctx: &mut ToolContext<'_>) -> Option<WorktreeManager> {
    let worktrees = ctx.store.state.worktrees.clone();
    if worktrees.is_none() {
        ctx.push_result(WORKTREE_UNAVAILABLE);
    }
    worktrees
}

pub(super) fn require_cron(ctx: &mut ToolContext<'_>) -> Option<CronHandle> {
    let cron = ctx.store.state.cron.clone();
    if cron.is_none() {
        ctx.push_result(CRON_UNAVAILABLE);
    }
    cron
}

pub(super) fn format_background_task(task_id: &str, task: &BackgroundTask) -> String {
    let status = background_status(&task.status);
    let output = task.output.as_deref().unwrap_or("(still running)");
    format!(
        "Task {task_id}: {status}\n\
         Command: {}\n\
         Output:\n{output}",
        task.command,
    )
}

pub(super) fn background_task_list(tasks: &[BackgroundTask]) -> String {
    if tasks.is_empty() {
        return "No background tasks.".to_string();
    }

    let mut output = String::from("Background tasks:\n");
    for task in tasks {
        let _ = writeln!(
            output,
            "  {} [{}] {}",
            task.id,
            background_status(&task.status),
            task.command
        );
    }
    output
}

pub(super) fn background_status(status: &BackgroundStatus) -> &'static str {
    match status {
        BackgroundStatus::Running => "running",
        BackgroundStatus::Completed => "completed",
        BackgroundStatus::Failed => "failed",
    }
}
