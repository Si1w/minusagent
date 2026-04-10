use serde::Deserialize;

use crate::intelligence::manager::normalize_agent_id;
use crate::team::manager::TeammateSpawn;

use super::super::{ToolContext, llm_config_for_agent};
use super::common::{parse_request, push_manager_result, require_team};

#[derive(Default, Deserialize)]
struct TeamSpawnRequest {
    name: String,
    role: String,
    prompt: String,
    agent: Option<String>,
}

#[derive(Default, Deserialize)]
struct TeamSendRequest {
    to: String,
    content: String,
}

#[derive(Default, Deserialize)]
struct ShutdownRequestArgs {
    teammate: String,
}

#[derive(Default, Deserialize)]
struct ShutdownResponseRequest {
    request_id: String,
    approve: bool,
    #[serde(default)]
    reason: String,
}

#[derive(Default, Deserialize)]
struct PlanSubmitRequest {
    plan: String,
}

#[derive(Default, Deserialize)]
struct PlanResponseRequest {
    request_id: String,
    approve: bool,
    #[serde(default)]
    feedback: String,
}

#[derive(Default, Deserialize)]
struct TeamReadInboxRequest {
    name: Option<String>,
}

pub(super) fn handle(ctx: &mut ToolContext<'_>) -> bool {
    match ctx.name {
        "idle" => {
            ctx.store.state.idle_requested = true;
            ctx.push_result("Entering idle state. Will resume when new work arrives.");
        }
        "team_spawn" => {
            let Some(request) = parse_request::<TeamSpawnRequest>(ctx) else {
                return true;
            };
            handle_team_spawn(ctx, &request);
        }
        "team_send" => {
            let Some(request) = parse_request::<TeamSendRequest>(ctx) else {
                return true;
            };
            handle_team_send(ctx, &request);
        }
        "shutdown_request" => {
            let Some(request) = parse_request::<ShutdownRequestArgs>(ctx) else {
                return true;
            };
            handle_shutdown_request(ctx, &request);
        }
        "shutdown_response" => {
            let Some(request) = parse_request::<ShutdownResponseRequest>(ctx) else {
                return true;
            };
            handle_shutdown_response(ctx, &request);
        }
        "plan_submit" => {
            let Some(request) = parse_request::<PlanSubmitRequest>(ctx) else {
                return true;
            };
            handle_plan_submit(ctx, &request);
        }
        "plan_response" => {
            let Some(request) = parse_request::<PlanResponseRequest>(ctx) else {
                return true;
            };
            handle_plan_response(ctx, &request);
        }
        "team_read_inbox" => {
            let Some(request) = parse_request::<TeamReadInboxRequest>(ctx) else {
                return true;
            };
            handle_team_read_inbox(ctx, &request);
        }
        _ => return false,
    }

    true
}

fn handle_team_spawn(ctx: &mut ToolContext<'_>, request: &TeamSpawnRequest) {
    if ctx.store.state.is_subagent {
        ctx.push_result("Error: team_spawn not available for teammates");
        return;
    }

    let Some(team) = require_team(ctx) else {
        return;
    };

    let agent_id = normalize_agent_id(request.agent.as_deref().unwrap_or_default());
    let llm_config = llm_config_for_agent(ctx.store, &agent_id);
    push_manager_result(
        ctx,
        team.spawn(
            TeammateSpawn {
                name: &request.name,
                role: &request.role,
                prompt: &request.prompt,
                agent_id: &agent_id,
            },
            llm_config,
            ctx.store.state.agents.clone(),
            ctx.store.state.tasks.clone(),
        ),
    );
}

fn handle_team_send(ctx: &mut ToolContext<'_>, request: &TeamSendRequest) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    let sender = ctx.store.state.sender_name().to_string();
    push_manager_result(
        ctx,
        team.send_message(&sender, &request.to, &request.content),
    );
}

fn handle_shutdown_request(ctx: &mut ToolContext<'_>, request: &ShutdownRequestArgs) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    push_manager_result(ctx, team.request_shutdown(&request.teammate));
}

fn handle_shutdown_response(ctx: &mut ToolContext<'_>, request: &ShutdownResponseRequest) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    let sender = ctx.store.state.sender_name().to_string();
    push_manager_result(
        ctx,
        team.respond_shutdown(
            &request.request_id,
            request.approve,
            &request.reason,
            &sender,
        ),
    );
}

fn handle_plan_submit(ctx: &mut ToolContext<'_>, request: &PlanSubmitRequest) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    let sender = ctx.store.state.sender_name().to_string();
    push_manager_result(ctx, team.submit_plan(&sender, &request.plan));
}

fn handle_plan_response(ctx: &mut ToolContext<'_>, request: &PlanResponseRequest) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    push_manager_result(
        ctx,
        team.respond_plan(&request.request_id, request.approve, &request.feedback),
    );
}

fn handle_team_read_inbox(ctx: &mut ToolContext<'_>, request: &TeamReadInboxRequest) {
    let Some(team) = require_team(ctx) else {
        return;
    };
    let default_name = ctx.store.state.sender_name().to_string();
    let name = request.name.as_deref().unwrap_or(&default_name);
    ctx.push_result(team.read_inbox(name));
}
