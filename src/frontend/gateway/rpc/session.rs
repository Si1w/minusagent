use std::sync::Arc;

use serde_json::{Value, json};

use super::args::{SendRequest, SessionKeyRequest};
use super::{Gateway, GatewayReply};
use crate::frontend::{Channel, UserMessage};
use crate::routing::protocol::SessionControl;

pub(super) async fn send(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = SendRequest::from_params(params)?;
    let message = UserMessage {
        text: request.text,
        sender_id: request.peer_id,
        channel: request.channel,
        account_id: request.account_id,
        guild_id: request.guild_id,
    };
    let reply = Arc::new(GatewayReply::new());

    let result = gateway
        .dispatch(
            message,
            reply.clone() as Arc<dyn Channel>,
            request.agent_override.as_deref(),
        )
        .await
        .map_err(|error| error.to_string())?;

    let _ = result.done.await;
    let reply_text = reply.take_buffer().await;

    Ok(json!({
        "agent_id": result.agent_id,
        "session_key": result.session_key,
        "reply": reply_text,
    }))
}

pub(super) async fn control(
    gateway: &Arc<Gateway>,
    params: &Value,
    ctrl: SessionControl,
) -> std::result::Result<Value, String> {
    let request = SessionKeyRequest::from_params(params)?;
    let event = gateway
        .send_control(&request.session_key, ctrl)
        .await
        .map_err(|error| error.to_string())?;

    serde_json::to_value(&event).map_err(|error| error.to_string())
}

pub(super) async fn interrupt(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let request = SessionKeyRequest::from_params(params)?;

    gateway
        .interrupt(&request.session_key)
        .await
        .map_err(|error| error.to_string())?;

    Ok(json!({"interrupted": true}))
}
