use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::Gateway;
use crate::frontend::{Channel, UserMessage};
use crate::routing::protocol::{PermissionMode, SessionControl};

#[path = "rpc/admin.rs"]
mod admin;
#[path = "rpc/args.rs"]
mod args;
#[path = "rpc/services.rs"]
mod services;
#[path = "rpc/session.rs"]
mod session;

pub(crate) async fn handle_rpc(gateway: &Arc<Gateway>, raw: &str) -> Option<Value> {
    let req: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => {
            return Some(json!({
                "jsonrpc": "2.0",
                "error": {"code": -32700, "message": "Parse error"},
                "id": null
            }));
        }
    };

    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req["method"].as_str().unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let result = match method {
        "send" => session::send(gateway, &params).await,
        "context_usage" => session::control(gateway, &params, SessionControl::ContextUsage).await,
        "rewind" => match args::rewind_count(&params) {
            Ok(count) => session::control(gateway, &params, SessionControl::Rewind { count }).await,
            Err(error) => Err(error),
        },
        "model" => {
            let model = args::model_name(&params);
            session::control(gateway, &params, SessionControl::ModelSwitch { model }).await
        }
        "interrupt" => session::interrupt(gateway, &params).await,
        "permission_mode" => {
            let mode: PermissionMode = args::permission_mode(&params);
            session::control(gateway, &params, SessionControl::SetPermissionMode { mode }).await
        }
        "bindings.set" => admin::bindings_set(gateway, &params).await,
        "bindings.remove" => admin::bindings_remove(gateway, &params).await,
        "bindings.list" => admin::bindings_list(gateway).await,
        "agents.list" => admin::agents_list(gateway).await,
        "agents.register" => admin::agents_register(gateway, &params).await,
        "sessions.list" => admin::sessions_list(gateway).await,
        "delivery.stats" => services::delivery_stats(gateway).await,
        "service.control" => services::service_control(gateway, &params).await,
        "status" => services::status(gateway).await,
        _ => Err(format!("Unknown method: {method}")),
    };

    Some(match result {
        Ok(value) => json!({"jsonrpc": "2.0", "result": value, "id": id}),
        Err(message) => json!({
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": message},
            "id": id
        }),
    })
}

struct GatewayReply {
    buffer: Mutex<String>,
}

impl GatewayReply {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(String::new()),
        }
    }

    async fn take_buffer(&self) -> String {
        std::mem::take(&mut *self.buffer.lock().await)
    }
}

#[async_trait::async_trait]
impl Channel for GatewayReply {
    async fn receive(&self) -> Option<UserMessage> {
        None
    }

    async fn send(&self, text: &str) {
        if !text.is_empty() {
            let mut buf = self.buffer.lock().await;
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }

    async fn confirm(&self, _command: &str) -> bool {
        true
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        self.buffer.lock().await.push_str(chunk);
    }

    async fn flush(&self) {}
}
