use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::core::session::Session;
use crate::core::store::{Config, Context, LLMConfig, SharedStore, SystemState};
use crate::frontend::{Channel, UserMessage};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::{AgentConfig, normalize_agent_id};
use crate::routing::delivery::DeliveryHandle;
use crate::routing::router::{Binding, BindingRouter, Router};
use crate::scheduler::LaneLock;
use crate::scheduler::cron::{CronHandle, CronJobStatus};
use crate::scheduler::heartbeat::HeartbeatHandle;

/// Global LLM provider config (shared across all agents)
pub struct ProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub context_window: usize,
    pub default_model: String,
    pub workspace_dir: Option<PathBuf>,
}

impl ProviderConfig {
    /// Load provider config from environment variables
    ///
    /// # Panics
    ///
    /// Panics if required env vars (`LLM_MODEL`, `LLM_BASE_URL`, `LLM_API_KEY`,
    /// `LLM_CONTEXT_WINDOW`) are missing or malformed.
    pub fn from_env() -> Self {
        Self {
            default_model: std::env::var("LLM_MODEL")
                .expect("LLM_MODEL not set"),
            base_url: std::env::var("LLM_BASE_URL")
                .expect("LLM_BASE_URL not set"),
            api_key: std::env::var("LLM_API_KEY")
                .expect("LLM_API_KEY not set"),
            context_window: std::env::var("LLM_CONTEXT_WINDOW")
                .expect("LLM_CONTEXT_WINDOW not set")
                .parse()
                .expect("LLM_CONTEXT_WINDOW must be a number"),
            workspace_dir: std::env::var("WORKSPACE_DIR")
                .ok()
                .map(PathBuf::from)
                .or_else(|| Some(PathBuf::from("./workspace")))
                .filter(|p| p.is_dir()),
        }
    }

    fn build_store(
        &self,
        system_prompt: String,
        model: String,
        intelligence: Option<Intelligence>,
    ) -> SharedStore {
        SharedStore {
            context: Context {
                system_prompt,
                history: Vec::new(),
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model,
                        base_url: self.base_url.clone(),
                        api_key: self.api_key.clone(),
                        context_window: self.context_window,
                    },
                },
                intelligence,
            },
        }
    }
}

/// Shared application state between gateway and frontends
pub struct AppState {
    pub router: BindingRouter,
    pub sessions: HashSet<String>,
    pub start_time: Instant,
}

/// Thread-safe shared state handle
pub type SharedState = Arc<RwLock<AppState>>;

/// Message passed to a session task
struct SessionMessage {
    text: String,
    frontend: Arc<dyn Channel>,
    done: Option<oneshot::Sender<()>>,
}

/// Result of a dispatch operation
pub struct DispatchResult {
    pub agent_id: String,
    pub session_key: String,
    pub done: oneshot::Receiver<()>,
}

/// Central dispatcher: routes messages and manages session lifecycle
///
/// Owns the shared state (router + agent manager), provider config,
/// and the per-session task pool.
pub struct Gateway {
    state: SharedState,
    provider: ProviderConfig,
    session_txs: Mutex<HashMap<String, mpsc::Sender<SessionMessage>>>,
    heartbeat_handles: Mutex<HashMap<String, HeartbeatHandle>>,
    cron_handle: Mutex<Option<CronHandle>>,
    delivery: DeliveryHandle,
}

impl Gateway {
    /// Create a new gateway
    pub fn new(state: SharedState, provider: ProviderConfig) -> Self {
        // Outbound sinks are shared between router and delivery runner
        let outbound = {
            let s = state.read().expect("State lock poisoned");
            s.router.outbound().clone()
        };

        // Start delivery runner
        let delivery_dir = provider
            .workspace_dir
            .as_ref()
            .map(|ws| ws.join(".delivery"))
            .unwrap_or_else(|| PathBuf::from(".delivery"));
        let delivery = crate::routing::delivery::spawn(
            &delivery_dir,
            outbound,
        )
        .expect("Failed to start delivery runner");

        // Start cron service if CRON.json exists
        let cron_handle = provider
            .workspace_dir
            .as_ref()
            .map(|ws| {
                let cron_file = ws.join("CRON.json");
                if cron_file.exists() {
                    let llm_config = LLMConfig {
                        model: provider.default_model.clone(),
                        base_url: provider.base_url.clone(),
                        api_key: provider.api_key.clone(),
                        context_window: provider.context_window,
                    };
                    Some(crate::scheduler::cron::spawn(
                        cron_file,
                        llm_config,
                        delivery.clone(),
                    ))
                } else {
                    None
                }
            })
            .flatten();

        Self {
            state,
            provider,
            session_txs: Mutex::new(HashMap::new()),
            heartbeat_handles: Mutex::new(HashMap::new()),
            cron_handle: Mutex::new(cron_handle),
            delivery,
        }
    }

    /// Read access to the shared state
    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Get the delivery handle
    pub fn delivery(&self) -> &DeliveryHandle {
        &self.delivery
    }

    /// Get the cron handle
    pub async fn cron_handle(&self) -> Option<CronHandle> {
        self.cron_handle.lock().await.clone()
    }

    /// List cron jobs
    pub async fn cron_list_jobs(&self) -> Vec<CronJobStatus> {
        match self.cron_handle.lock().await.as_ref() {
            Some(h) => h.list_jobs().await,
            None => Vec::new(),
        }
    }

    /// Dispatch a user message: route → find/create session → forward
    ///
    /// # Arguments
    ///
    /// * `msg` - The user message
    /// * `frontend` - Channel to send responses through
    /// * `agent_override` - Force routing to a specific agent
    ///
    /// # Returns
    ///
    /// Dispatch result with agent_id, session_key, and completion receiver.
    ///
    /// # Errors
    ///
    /// Returns error if the session task has unexpectedly closed.
    pub async fn dispatch(
        &self,
        msg: UserMessage,
        frontend: Arc<dyn Channel>,
        agent_override: Option<&str>,
    ) -> Result<DispatchResult> {
        // 1. Resolve routing
        let (session_key, system_prompt, model, agent_id, channel_name, ws_dir) =
        {
            let s = self.state.read().expect("State lock poisoned");
            let result = if let Some(ov) = agent_override {
                s.router.resolve_explicit(ov, &msg)
            } else {
                s.router.resolve(&msg)
            };
            let agent = s.router.manager().get(&result.agent_id);
            let prompt = agent
                .map(|a| a.effective_system_prompt())
                .unwrap_or_default();
            let model =
                s.router.manager().effective_model(&result.agent_id);
            let ch = msg.channel.clone();
            let ws: Option<PathBuf> = agent
                .map(|a| a.workspace_dir.clone())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or(self.provider.workspace_dir.clone());
            (result.session_key, prompt, model, result.agent_id, ch, ws)
        };

        // 2. Track session
        {
            let mut s =
                self.state.write().expect("State lock poisoned");
            s.sessions.insert(session_key.clone());
        }

        // 3. Get or create session task
        let (done_tx, done_rx) = oneshot::channel();
        let text = msg.text;

        let session_tx = {
            let mut txs = self.session_txs.lock().await;
            if let Some(tx) = txs.get(&session_key) {
                if tx.is_closed() {
                    txs.remove(&session_key);
                }
            }

            if let Some(tx) = txs.get(&session_key) {
                tx.clone()
            } else {
                let intelligence = ws_dir.as_ref().map(|ws| {
                    Intelligence::new(
                        ws,
                        system_prompt.clone(),
                        agent_id.clone(),
                        channel_name,
                        model.clone(),
                    )
                });
                let initial_prompt = intelligence
                    .as_ref()
                    .map(|i| i.build_prompt())
                    .unwrap_or(system_prompt.clone());
                let store = self.provider.build_store(
                    initial_prompt,
                    model.clone(),
                    intelligence,
                );

                let lane_lock =
                    LaneLock::new(tokio::sync::Mutex::new(()));

                // Spawn heartbeat before session so we can pass
                // the handle into Session
                let hb_handle = ws_dir
                    .as_ref()
                    .filter(|ws| ws.join("HEARTBEAT.md").exists())
                    .map(|ws| {
                        let llm_config = LLMConfig {
                            model: model.clone(),
                            base_url: self
                                .provider
                                .base_url
                                .clone(),
                            api_key: self
                                .provider
                                .api_key
                                .clone(),
                            context_window: self
                                .provider
                                .context_window,
                        };
                        crate::scheduler::heartbeat::spawn(
                            ws.clone(),
                            lane_lock.clone(),
                            llm_config,
                            system_prompt,
                            tokio::time::Duration::from_secs(1800),
                            (9, 22),
                            self.delivery.clone(),
                            "bg".to_string(),
                            String::new(),
                        )
                    });

                // Store handle in gateway for REPL access
                if let Some(ref h) = hb_handle {
                    self.heartbeat_handles
                        .lock()
                        .await
                        .insert(session_key.clone(), h.clone());
                }

                let lock = lane_lock.clone();
                let (stx, mut srx) =
                    mpsc::channel::<SessionMessage>(8);
                tokio::spawn(async move {
                    let mut session =
                        Session::new(store, lock, hb_handle)
                            .expect("Failed to create session");
                    while let Some(msg) = srx.recv().await {
                        if let Err(e) = session
                            .turn(&msg.text, &msg.frontend)
                            .await
                        {
                            msg.frontend
                                .send(&format!("Error: {e}"))
                                .await;
                        }
                        if let Some(done) = msg.done {
                            let _ = done.send(());
                        }
                    }
                });
                txs.insert(session_key.clone(), stx.clone());
                stx
            }
        };

        // 4. Send to session
        session_tx
            .send(SessionMessage {
                text,
                frontend,
                done: Some(done_tx),
            })
            .await
            .map_err(|_| anyhow::anyhow!("Session task closed"))?;

        Ok(DispatchResult {
            agent_id,
            session_key,
            done: done_rx,
        })
    }
}

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

/// Start the WebSocket server
///
/// # Arguments
///
/// * `gateway` - Shared gateway for dispatch and state access
/// * `host` - Bind address
/// * `port` - Bind port
///
/// # Errors
///
/// Returns error if the TCP listener fails to bind.
pub async fn start_ws(
    gateway: Arc<Gateway>,
    host: &str,
    port: u16,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{host}:{port}")).await?;
    log::info!("Gateway started ws://{host}:{port}");

    loop {
        let (stream, addr) = listener.accept().await?;
        log::debug!("Gateway: new connection from {addr}");

        let gateway = gateway.clone();

        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(stream).await
            {
                Ok(ws) => ws,
                Err(e) => {
                    log::error!(
                        "Gateway: WebSocket handshake failed: {e}"
                    );
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

                let resp = handle_rpc(&gateway, &text).await;
                if let Some(resp) = resp {
                    let msg =
                        WsMessage::Text(resp.to_string().into());
                    if write.lock().await.send(msg).await.is_err() {
                        break;
                    }
                }
            }

            log::debug!("Gateway: connection from {addr} closed");
        });
    }
}

async fn handle_rpc(
    gateway: &Arc<Gateway>,
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
        "send" => m_send(gateway, &params).await,
        "bindings.set" => m_bindings_set(gateway, &params),
        "bindings.remove" => m_bindings_remove(gateway, &params),
        "bindings.list" => m_bindings_list(gateway),
        "agents.list" => m_agents_list(gateway),
        "agents.register" => m_agents_register(gateway, &params),
        "sessions.list" => m_sessions_list(gateway),
        "delivery.stats" => m_delivery_stats(gateway).await,
        "status" => m_status(gateway),
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
    gateway: &Arc<Gateway>,
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

    let agent_override = params["agent_id"].as_str();
    let reply = Arc::new(GatewayReply::new());

    let result = gateway
        .dispatch(msg, reply.clone() as Arc<dyn Channel>, agent_override)
        .await
        .map_err(|e| e.to_string())?;

    let _ = result.done.await;
    let reply_text = reply.take_buffer().await;

    Ok(json!({
        "agent_id": result.agent_id,
        "session_key": result.session_key,
        "reply": reply_text,
    }))
}

fn m_bindings_set(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().map_err(|e| e.to_string())?;
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
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().map_err(|e| e.to_string())?;
    let removed = s.router.table_mut().remove(
        params["agent_id"].as_str().unwrap_or(""),
        params["match_key"].as_str().unwrap_or(""),
        params["match_value"].as_str().unwrap_or(""),
    );
    Ok(json!({"removed": removed}))
}

fn m_bindings_list(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().map_err(|e| e.to_string())?;
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
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().map_err(|e| e.to_string())?;
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
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().map_err(|e| e.to_string())?;
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
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().map_err(|e| e.to_string())?;
    let sessions: Vec<&String> = s.sessions.iter().collect();
    Ok(json!(sessions))
}

async fn m_delivery_stats(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    match gateway.delivery().stats().await {
        Some(st) => Ok(json!({
            "total_attempted": st.total_attempted,
            "total_succeeded": st.total_succeeded,
            "total_failed": st.total_failed,
            "pending": st.pending,
        })),
        None => Err("delivery runner not available".to_string()),
    }
}

fn m_status(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().map_err(|e| e.to_string())?;
    Ok(json!({
        "agents": s.router.manager().list().len(),
        "bindings": s.router.table().list().len(),
        "sessions": s.sessions.len(),
        "uptime_secs": s.start_time.elapsed().as_secs(),
    }))
}