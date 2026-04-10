use std::sync::Arc;

use serde_json::{Value, json};

use super::Gateway;
use super::args::ServiceControlRequest;

pub(super) async fn delivery_stats(gateway: &Arc<Gateway>) -> std::result::Result<Value, String> {
    match gateway.delivery().stats().await {
        Some(stats) => Ok(json!({
            "total_attempted": stats.total_attempted,
            "total_succeeded": stats.total_succeeded,
            "total_failed": stats.total_failed,
            "pending": stats.pending,
        })),
        None => Err("delivery runner not available".to_string()),
    }
}

pub(super) async fn service_control(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = ServiceControlRequest::from_params(params)?;
    let result = gateway
        .services()
        .control(gateway, request.service, request.command)
        .await;
    Ok(json!({
        "changed": result.changed(),
        "message": result.to_string(),
    }))
}

pub(super) async fn status(gateway: &Arc<Gateway>) -> std::result::Result<Value, String> {
    let (agents, bindings, sessions, uptime_secs) = {
        let state = gateway.state().read().await;
        (
            state.router.shared_agents().list().len(),
            state.router.table().list().len(),
            state.sessions.len(),
            state.start_time.elapsed().as_secs(),
        )
    };
    let services = gateway.services().status_snapshot(gateway).await;
    Ok(json!({
        "agents": agents,
        "bindings": bindings,
        "sessions": sessions,
        "uptime_secs": uptime_secs,
        "services": services,
    }))
}
