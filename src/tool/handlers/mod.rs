mod background;
mod common;
mod cron;
mod execution;
mod task;
mod team;
mod web;
mod worktree;

use anyhow::Result;

use super::ToolContext;

pub(super) async fn handle_tool(ctx: &mut ToolContext<'_>) -> Result<bool> {
    if execution::handle(ctx).await? {
        return Ok(true);
    }
    if background::handle(ctx).await? {
        return Ok(true);
    }
    if task::handle(ctx).await? {
        return Ok(true);
    }
    if team::handle(ctx) {
        return Ok(true);
    }
    if worktree::handle(ctx).await? {
        return Ok(true);
    }
    if web::handle(ctx).await? {
        return Ok(true);
    }
    if cron::handle(ctx).await? {
        return Ok(true);
    }
    Ok(false)
}
