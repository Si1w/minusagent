use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::RoutedMessage;
use crate::intelligence::manager::{AgentConfig, normalize_agent_id};
use crate::routing::router::{Binding, BindingRouter, Router};
use crate::frontend::{Channel, UserMessage};

/// Shared application state between main loop and gateway
pub struct AppState {
    pub router: BindingRouter,
    pub sessions: HashSet<String>,
    pub start_time: Instant,
}

/// Thread-safe shared state handle
pub type SharedState = Arc<RwLock<AppState>>;

/// Channel implementation that buffers all output for JSON-RPC response
struct GatewayReply {
    buffer: Mutex<String>,
}

impl GatewayReply {
    fn new() -> Self {
        Self {
            buffer: Mutex::new(String::new()),
        }
    }

    /// Take accumulated buffer content
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

/// Start the WebSocket gateway server
///
/// # Arguments
///
/// * `state` - Shared application state
/// * `tx` - Channel to route messages into the main loop
/// * `host` - Bind address
/// * `port` - Bind port
///
/// # Errors
///
/// Returns error if the TCP listener fails to bind.
pub async fn start_gateway(
    state: SharedState,
    tx: mpsc::Sender<RoutedMessage>,
    host: &str,
    port: u16,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{host}:{port}")).await?;
    log::info!("Gateway started ws://{host}:{port}");

    loop {
        let (stream, addr) = listener.accept().await?;
        log::debug!("Gateway: new connection from {addr}");

        let state = state.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    log::error!("Gateway: WebSocket handshake failed: {e}");
                    return;
                }
            };

            let (write, mut read) = ws.split();
            let write = Arc::new(Mutex::new(write));

            while let Some(Ok(ws_msg)) = read.next().await {
                let text = match ws_msg {
                    WsMessage::Text(t) => t,
                    WsMessage::Close(_) => break,
                    _ => continue,
                };

                let resp = dispatch(&state, &tx, &text).await;
                if let Some(resp) = resp {
                    let msg = WsMessage::Text(resp.to_string().into());
                    if write.lock().await.send(msg).await.is_err() {
                        break;
                    }
                }
            }

            log::debug!("Gateway: connection from {addr} closed");
        });
    }
}

async fn dispatch(
    state: &SharedState,
    tx: &mpsc::Sender<RoutedMessage>,
    raw: &str,
) -> Option<Value> {
    let req: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
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
        "send" => m_send(state, tx, &params).await,
        "bindings.set" => m_bindings_set(state, &params),
        "bindings.remove" => m_bindings_remove(state, &params),
        "bindings.list" => m_bindings_list(state),
        "agents.list" => m_agents_list(state),
        "agents.register" => m_agents_register(state, &params),
        "sessions.list" => m_sessions_list(state),
        "status" => m_status(state),
        _ => Err(format!("Unknown method: {method}")),
    };

    Some(match result {
        Ok(val) => json!({"jsonrpc": "2.0", "result": val, "id": id}),
        Err(msg) => json!({
            "jsonrpc": "2.0",
            "error": {"code": -32000, "message": msg},
            "id": id
        }),
    })
}

async fn m_send(
    state: &SharedState,
    tx: &mpsc::Sender<RoutedMessage>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let text = params["text"]
        .as_str()
        .ok_or("text is required")?
        .to_string();
    let channel = params["channel"]
        .as_str()
        .unwrap_or("websocket")
        .to_string();
    let peer_id = params["peer_id"]
        .as_str()
        .unwrap_or("ws-client")
        .to_string();

    let account_id = params["account_id"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let guild_id = params["guild_id"]
        .as_str()
        .unwrap_or("")
        .to_string();

    let msg = UserMessage {
        text,
        sender_id: peer_id,
        channel,
        account_id,
        guild_id,
    };

    let (agent_id, session_key) = {
        let s = state.read().map_err(|e| e.to_string())?;
        if let Some(explicit) = params["agent_id"].as_str() {
            let result = s.router.resolve_explicit(explicit, &msg);
            (result.agent_id, result.session_key)
        } else {
            let result = s.router.resolve(&msg);
            (result.agent_id, result.session_key)
        }
    };

    // Create reply channel
    let reply = Arc::new(GatewayReply::new());
    let (done_tx, done_rx) = oneshot::channel();

    tx.send(RoutedMessage {
        msg,
        frontend: reply.clone() as Arc<dyn Channel>,
        done: Some(done_tx),
        agent_override: None,
    })
    .await
    .map_err(|_| "Main loop closed")?;

    let _ = done_rx.await;
    let reply_text = reply.take_buffer().await;

    Ok(json!({
        "agent_id": agent_id,
        "session_key": session_key,
        "reply": reply_text,
    }))
}

fn m_bindings_set(
    state: &SharedState,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s = state.write().map_err(|e| e.to_string())?;
    let binding = Binding {
        agent_id: normalize_agent_id(
            params["agent_id"].as_str().unwrap_or("mandeven"),
        ),
        tier: params["tier"].as_u64().unwrap_or(5) as u8,
        match_key: params["match_key"]
            .as_str()
            .unwrap_or("default")
            .to_string(),
        match_value: params["match_value"]
            .as_str()
            .unwrap_or("*")
            .to_string(),
        priority: params["priority"].as_i64().unwrap_or(0) as i32,
    };
    s.router.table_mut().add(binding);
    Ok(json!({"ok": true}))
}

fn m_bindings_remove(
    state: &SharedState,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s = state.write().map_err(|e| e.to_string())?;
    let removed = s.router.table_mut().remove(
        params["agent_id"].as_str().unwrap_or(""),
        params["match_key"].as_str().unwrap_or(""),
        params["match_value"].as_str().unwrap_or(""),
    );
    Ok(json!({"removed": removed}))
}

fn m_bindings_list(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    let bindings: Vec<Value> = s
        .router
        .table()
        .list()
        .iter()
        .map(|b| {
            json!({
                "agent_id": b.agent_id,
                "tier": b.tier,
                "match_key": b.match_key,
                "match_value": b.match_value,
                "priority": b.priority,
            })
        })
        .collect();
    Ok(json!(bindings))
}

fn m_agents_list(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    let agents: Vec<Value> = s
        .router
        .manager()
        .list()
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "model": s.router.manager().effective_model(&a.id),
                "dm_scope": a.dm_scope,
            })
        })
        .collect();
    Ok(json!(agents))
}

fn m_agents_register(
    state: &SharedState,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s = state.write().map_err(|e| e.to_string())?;
    let config = AgentConfig {
        id: params["id"]
            .as_str()
            .ok_or("id is required")?
            .to_string(),
        name: params["name"]
            .as_str()
            .ok_or("name is required")?
            .to_string(),
        system_prompt: params["system_prompt"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        model: params["model"].as_str().unwrap_or("").to_string(),
        dm_scope: params["dm_scope"]
            .as_str()
            .unwrap_or("per-peer")
            .to_string(),
        workspace_dir: params["workspace_dir"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    };
    let id = normalize_agent_id(&config.id);
    s.router.manager_mut().register(config);
    Ok(json!({"ok": true, "id": id}))
}

fn m_sessions_list(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    let sessions: Vec<&String> = s.sessions.iter().collect();
    Ok(json!(sessions))
}

fn m_status(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    Ok(json!({
        "agents": s.router.manager().list().len(),
        "bindings": s.router.table().list().len(),
        "sessions": s.sessions.len(),
        "uptime_secs": s.start_time.elapsed().as_secs(),
    }))
}