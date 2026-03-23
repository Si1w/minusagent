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
use crate::core::manager::{AgentManager, AgentConfig, normalize_agent_id};
use crate::core::router::{Binding, BindingTable, build_session_key};
use crate::frontend::{Channel, UserMessage};

/// Shared application state between main loop and gateway
pub struct AppState {
    pub mgr: AgentManager,
    pub table: BindingTable,
    pub sessions: HashSet<String>,
    pub start_time: Instant,
}

impl AppState {
    /// Resolve routing for a message
    ///
    /// # Arguments
    ///
    /// * `msg` - Inbound user message
    ///
    /// # Returns
    ///
    /// `(agent_id, session_key)` tuple.
    pub fn resolve_route(&self, msg: &UserMessage) -> (String, String) {
        let agent_id = self
            .table
            .resolve_msg(
                &msg.channel,
                &msg.account_id,
                &msg.guild_id,
                &msg.sender_id,
            )
            .map(|b| b.agent_id.clone())
            .unwrap_or_else(|| "main".into());

        let dm_scope = self
            .mgr
            .get(&agent_id)
            .map(|a| a.dm_scope.as_str())
            .unwrap_or("per-peer");

        let session_key = build_session_key(
            &agent_id,
            &msg.channel,
            &msg.account_id,
            &msg.sender_id,
            dm_scope,
        );

        (agent_id, session_key)
    }
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
            let aid = normalize_agent_id(explicit);
            let dm_scope = s
                .mgr
                .get(&aid)
                .map(|a| a.dm_scope.as_str())
                .unwrap_or("per-peer");
            let sk = build_session_key(
                &aid,
                &msg.channel,
                &msg.account_id,
                &msg.sender_id,
                dm_scope,
            );
            (aid, sk)
        } else {
            s.resolve_route(&msg)
        }
    };

    // Create reply channel
    let reply = Arc::new(GatewayReply::new());
    let (done_tx, done_rx) = oneshot::channel();

    tx.send(RoutedMessage {
        msg,
        frontend: reply.clone() as Arc<dyn Channel>,
        done: Some(done_tx),
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
            params["agent_id"].as_str().unwrap_or("main"),
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
    s.table.add(binding);
    Ok(json!({"ok": true}))
}

fn m_bindings_remove(
    state: &SharedState,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s = state.write().map_err(|e| e.to_string())?;
    let removed = s.table.remove(
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
        .table
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
        .mgr
        .list()
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "model": s.mgr.effective_model(&a.id),
                "dm_scope": a.dm_scope,
                "personality": a.personality,
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
        personality: params["personality"]
            .as_str()
            .unwrap_or("")
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
    };
    let id = normalize_agent_id(&config.id);
    s.mgr.register(config);
    Ok(json!({"ok": true, "id": id}))
}

fn m_sessions_list(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    let sessions: Vec<&str> =
        s.sessions.iter().map(|s| s.as_str()).collect();
    Ok(json!(sessions))
}

fn m_status(
    state: &SharedState,
) -> std::result::Result<Value, String> {
    let s = state.read().map_err(|e| e.to_string())?;
    Ok(json!({
        "running": true,
        "uptime_seconds": s.start_time.elapsed().as_secs(),
        "agent_count": s.mgr.list().len(),
        "binding_count": s.table.list().len(),
        "session_count": s.sessions.len(),
    }))
}
