use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::args::{AgentRegisterRequest, BindingRemoveRequest, BindingSetRequest};
use crate::intelligence::manager::{AgentConfig, normalize_agent_id};
use crate::routing::router::Binding;

pub(super) async fn bindings_set(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = BindingSetRequest::from_params(params)?;
    let mut state = gateway.state().write().await;
    let binding = Binding {
        agent_id: normalize_agent_id(&request.agent_id),
        tier: request.tier,
        match_key: request.match_key,
        match_value: request.match_value,
        priority: request.priority,
    };
    state.router.table_mut().add(binding);
    Ok(json!({"ok": true}))
}

pub(super) async fn bindings_remove(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = BindingRemoveRequest::from_params(params);
    let mut state = gateway.state().write().await;
    let removed = state.router.table_mut().remove(
        &request.agent_id,
        &request.match_key,
        &request.match_value,
    );
    Ok(json!({"removed": removed}))
}

pub(super) async fn bindings_list(gateway: &Arc<Gateway>) -> std::result::Result<Value, String> {
    let state = gateway.state().read().await;
    let bindings: Vec<Value> = state
        .router
        .table()
        .list()
        .iter()
        .map(|binding| {
            json!({
                "agent_id": binding.agent_id,
                "tier": binding.tier,
                "match_key": binding.match_key,
                "match_value": binding.match_value,
                "priority": binding.priority,
            })
        })
        .collect();
    Ok(json!(bindings))
}

pub(super) async fn agents_list(gateway: &Arc<Gateway>) -> std::result::Result<Value, String> {
    let state = gateway.state().read().await;
    let shared = state.router.shared_agents();
    let list = shared.list();
    let agents: Vec<Value> = list
        .iter()
        .map(|agent| {
            json!({
                "id": agent.id,
                "name": agent.name,
                "model": shared.effective_model(&agent.id),
                "dm_scope": agent.dm_scope.as_str(),
            })
        })
        .collect();
    Ok(json!(agents))
}

pub(super) async fn agents_register(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = AgentRegisterRequest::from_params(params)?;
    let mut state = gateway.state().write().await;
    let config = AgentConfig {
        id: request.id,
        name: request.name,
        system_prompt: request.system_prompt,
        model: request.model,
        dm_scope: request.dm_scope,
        workspace_dir: request.workspace_dir,
        denied_tools: request.denied_tools,
    };
    let id = normalize_agent_id(&config.id);
    state.router.manager_mut().register(config);
    Ok(json!({"ok": true, "id": id}))
}

pub(super) async fn sessions_list(gateway: &Arc<Gateway>) -> std::result::Result<Value, String> {
    let state = gateway.state().read().await;
    let sessions: Vec<&String> = state.sessions.iter().collect();
    Ok(json!(sessions))
}
