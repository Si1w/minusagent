use anyhow::Result;
use serde::Deserialize;

use crate::engine::agent::{SubagentSpec, run_subagent};
use crate::intelligence::manager::normalize_agent_id;
use crate::team::TaskStatus;

use super::super::{ToolContext, llm_config_for_agent};
use super::common::{parse_request, push_manager_result, require_tasks};

#[derive(Default, Deserialize)]
struct TaskCreateRequest {
    subject: String,
    #[serde(default)]
    description: String,
}

#[derive(Default, Deserialize)]
struct TaskUpdateRequest {
    task_id: usize,
    status: Option<TaskStatus>,
    blocked_by: Option<Vec<usize>>,
    blocks: Option<Vec<usize>>,
}

#[derive(Default, Deserialize)]
struct TaskGetRequest {
    task_id: usize,
}

#[derive(Default, Deserialize)]
struct SubagentTaskRequest {
    prompt: String,
    agent: String,
}

#[derive(Default, Deserialize)]
struct ClaimTaskRequest {
    task_id: usize,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "task_create" => {
            let Some(request) = parse_request::<TaskCreateRequest>(ctx) else {
                return Ok(true);
            };
            handle_task_create(ctx, &request);
        }
        "task_update" => {
            let Some(request) = parse_request::<TaskUpdateRequest>(ctx) else {
                return Ok(true);
            };
            handle_task_update(ctx, request);
        }
        "task_list" => handle_task_list(ctx),
        "task_get" => {
            let Some(request) = parse_request::<TaskGetRequest>(ctx) else {
                return Ok(true);
            };
            handle_task_get(ctx, &request);
        }
        "task" => {
            let Some(request) = parse_request::<SubagentTaskRequest>(ctx) else {
                return Ok(true);
            };
            handle_subagent_task(ctx, request).await?;
        }
        "claim_task" => {
            let Some(request) = parse_request::<ClaimTaskRequest>(ctx) else {
                return Ok(true);
            };
            handle_claim_task(ctx, &request);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

fn handle_task_create(ctx: &mut ToolContext<'_>, request: &TaskCreateRequest) {
    let Some(tasks) = require_tasks(ctx) else {
        return;
    };
    push_manager_result(ctx, tasks.create(&request.subject, &request.description));
}

fn handle_task_update(ctx: &mut ToolContext<'_>, request: TaskUpdateRequest) {
    let Some(tasks) = require_tasks(ctx) else {
        return;
    };
    push_manager_result(
        ctx,
        tasks.update(
            request.task_id,
            request.status,
            request.blocked_by,
            request.blocks,
        ),
    );
}

fn handle_task_list(ctx: &mut ToolContext<'_>) {
    let Some(tasks) = require_tasks(ctx) else {
        return;
    };
    push_manager_result(ctx, tasks.list_all());
}

fn handle_task_get(ctx: &mut ToolContext<'_>, request: &TaskGetRequest) {
    let Some(tasks) = require_tasks(ctx) else {
        return;
    };
    push_manager_result(ctx, tasks.get(request.task_id));
}

async fn handle_subagent_task(
    ctx: &mut ToolContext<'_>,
    request: SubagentTaskRequest,
) -> Result<()> {
    if ctx.store.state.is_subagent {
        ctx.push_result("Error: task tool is not available here");
        return Ok(());
    }

    let agent_id = normalize_agent_id(&request.agent);

    if let Some(config) = ctx.store.state.agents.get(&agent_id) {
        let workspace_dir = (!config.workspace_dir.is_empty())
            .then(|| std::path::PathBuf::from(&config.workspace_dir));
        let summary = run_subagent(SubagentSpec {
            prompt: request.prompt,
            system_prompt: config.system_prompt.clone(),
            llm_config: llm_config_for_agent(ctx.store, &agent_id),
            workspace_dir,
            agent_id,
            agents: ctx.store.state.agents.clone(),
            tasks: ctx.store.state.tasks.clone(),
            denied_tools: config.denied_tools.clone(),
        })
        .await?;
        ctx.push_result(summary);
        return Ok(());
    }

    let available = ctx
        .store
        .state
        .agents
        .list()
        .iter()
        .map(|agent| agent.id.clone())
        .collect::<Vec<_>>();
    ctx.push_result(format!(
        "Error: unknown agent '{}'. Available: {}",
        agent_id,
        available.join(", ")
    ));

    Ok(())
}

fn handle_claim_task(ctx: &mut ToolContext<'_>, request: &ClaimTaskRequest) {
    let Some(tasks) = require_tasks(ctx) else {
        return;
    };
    let owner = ctx.store.state.sender_name().to_string();
    push_manager_result(ctx, tasks.claim(request.task_id, &owner));
}
