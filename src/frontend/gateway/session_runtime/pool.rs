use std::sync::Arc;

use tokio::sync::mpsc;

use super::super::Gateway;
use super::resolve::{ResolvedSession, build_session_intelligence};
use super::store::{SessionStoreConfig, build_session_store};
use super::worker::{SessionHandle, SessionMessage, spawn_session_task};
use crate::config::LLMConfig;
use crate::scheduler::LaneLock;
use crate::scheduler::heartbeat::HeartbeatHandle;
use crate::scheduler::lane::CommandQueue;

pub(super) async fn record_active_session(gateway: &Gateway, session_key: &str) {
    let mut state = gateway.state.write().await;
    state.sessions.insert(session_key.to_string());
}

pub(super) async fn get_or_spawn_session_sender(
    gateway: &Gateway,
    resolved: &ResolvedSession,
) -> mpsc::Sender<SessionMessage> {
    {
        let mut txs = gateway.session_txs.lock().await;
        prune_closed_session(&mut txs, &resolved.session_key);
        if let Some(handle) = txs.get(&resolved.session_key) {
            return handle.sender();
        }
    }

    let handle = spawn_session_handle(gateway, resolved);
    let tx = handle.sender();
    let mut txs = gateway.session_txs.lock().await;
    prune_closed_session(&mut txs, &resolved.session_key);
    if let Some(existing) = txs.get(&resolved.session_key) {
        return existing.sender();
    }
    txs.insert(resolved.session_key.clone(), handle);
    tx
}

fn spawn_session_handle(gateway: &Gateway, resolved: &ResolvedSession) -> SessionHandle {
    let (initial_prompt, intelligence) = build_session_intelligence(resolved);
    let cron = gateway.services.cron_handle();
    let store = build_session_store(
        &gateway.config,
        SessionStoreConfig {
            system_prompt: initial_prompt,
            model: resolved.model.clone(),
            intelligence,
            agents: resolved.shared_agents.clone(),
            workspace_dir: resolved.workspace_dir.as_deref(),
            cron,
            denied_tools: &resolved.denied_tools,
        },
    );
    let lane_lock: LaneLock = Arc::new(CommandQueue::new());
    let heartbeat = build_heartbeat_handle(gateway, resolved, &lane_lock);
    let extra_profiles = gateway
        .config
        .extra_profiles()
        .iter()
        .map(LLMConfig::to_auth_profile)
        .collect();
    spawn_session_task(
        store,
        lane_lock,
        heartbeat,
        extra_profiles,
        gateway.config.fallback_models().to_vec(),
    )
}

fn build_heartbeat_handle(
    gateway: &Gateway,
    resolved: &ResolvedSession,
    lane_lock: &LaneLock,
) -> Option<HeartbeatHandle> {
    let workspace_dir = resolved.workspace_dir.as_ref()?;
    workspace_dir.join("HEARTBEAT.md").exists().then(|| {
        let mut llm_config = gateway.config.primary_llm().clone();
        llm_config.model.clone_from(&resolved.model);
        crate::scheduler::heartbeat::spawn(
            workspace_dir,
            lane_lock.clone(),
            llm_config,
            resolved.system_prompt.clone(),
            gateway.services.delivery().clone(),
            "bg".to_string(),
            String::new(),
        )
    })
}

fn prune_closed_session(
    txs: &mut std::collections::HashMap<String, SessionHandle>,
    session_key: &str,
) {
    if txs.get(session_key).is_some_and(SessionHandle::is_closed) {
        txs.remove(session_key);
    }
}
