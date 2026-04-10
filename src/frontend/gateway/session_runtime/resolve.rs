use std::path::{Path, PathBuf};

use super::Gateway;
use crate::frontend::UserMessage;
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::routing::router::Router;

pub(super) struct ResolvedSession {
    pub(super) session_key: String,
    pub(super) system_prompt: String,
    pub(super) model: String,
    pub(super) agent_id: String,
    pub(super) channel_name: String,
    pub(super) workspace_dir: Option<PathBuf>,
    pub(super) shared_agents: SharedAgents,
    pub(super) denied_tools: Vec<String>,
}

pub(super) async fn resolve_dispatch(
    gateway: &Gateway,
    msg: &UserMessage,
    agent_override: Option<&str>,
) -> ResolvedSession {
    let state = gateway.state.read().await;
    let result = match agent_override {
        Some(agent_id) => state.router.resolve_explicit(agent_id, msg),
        None => state.router.resolve(msg),
    };
    let shared_agents = state.router.shared_agents();
    let agent = shared_agents.get(&result.agent_id);
    let workspace_dir = agent
        .as_ref()
        .map(|entry| entry.workspace_dir.clone())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| gateway.config.workspace_dir().map(Path::to_path_buf));

    ResolvedSession {
        session_key: result.session_key,
        system_prompt: agent
            .as_ref()
            .map(|entry| entry.system_prompt.clone())
            .unwrap_or_default(),
        model: shared_agents.effective_model(&result.agent_id),
        agent_id: result.agent_id,
        channel_name: msg.channel.clone(),
        workspace_dir,
        shared_agents,
        denied_tools: agent
            .as_ref()
            .map(|entry| entry.denied_tools.clone())
            .unwrap_or_default(),
    }
}

pub(super) fn build_session_intelligence(
    resolved: &ResolvedSession,
) -> (String, Option<Intelligence>) {
    let intelligence = resolved.workspace_dir.as_ref().map(|workspace_dir| {
        Intelligence::new(
            workspace_dir,
            &resolved.system_prompt,
            resolved.agent_id.clone(),
            resolved.channel_name.clone(),
            resolved.model.clone(),
        )
    });
    let initial_prompt = intelligence.as_ref().map_or_else(
        || resolved.system_prompt.clone(),
        Intelligence::build_prompt,
    );
    (initial_prompt, intelligence)
}
