use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config::{AppConfig, LLMConfig};
use crate::engine::session::Session;
use crate::engine::store::{Config, Context, SharedStore, SystemState};
use crate::team::{BackgroundManager, TaskManager, TeammateManager, TodoManager, WorktreeManager};
use crate::frontend::{Channel, UserMessage};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::{AgentConfig, SharedAgents, normalize_agent_id};
use crate::routing::delivery::DeliveryHandle;
use crate::routing::protocol::{ControlEvent, SessionControl, ToolPolicy};
use crate::routing::router::{Binding, BindingRouter, Router};
use crate::scheduler::LaneLock;
use crate::scheduler::cron::{CronHandle, CronJobStatus};
use crate::scheduler::lane::CommandQueue;

impl AppConfig {
    fn build_store(
        &self,
        system_prompt: String,
        model: String,
        intelligence: Option<Intelligence>,
        agents: SharedAgents,
        workspace_dir: Option<&PathBuf>,
        cron: Option<CronHandle>,
        denied_tools: &[String],
    ) -> SharedStore {
        let tasks = workspace_dir
            .map(|ws| TaskManager::new(ws.join(".tasks")))
            .and_then(|r| r.ok());

        let team = workspace_dir
            .map(|ws| TeammateManager::new(&ws.join(".team")))
            .and_then(|r| r.ok());

        let worktrees = workspace_dir
            .map(|ws| {
                WorktreeManager::new(
                    ws.join(".worktrees"),
                    ws.clone(),
                )
            })
            .and_then(|r| r.ok());

        SharedStore {
            context: Context {
                system_prompt,
                history: Vec::new(),
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model,
                        base_url: self.primary_llm().base_url.clone(),
                        api_key: self.primary_llm().api_key.clone(),
                        context_window: self.primary_llm().context_window,
                    },
                },
                intelligence,
                todo: TodoManager::new(),
                is_subagent: false,
                agents,
                tasks,
                background: BackgroundManager::new(),
                team,
                team_name: None,
                worktrees,
                tool_policy: ToolPolicy::from_denied(denied_tools),
                idle_requested: false,
                plan_mode: false,
                cron,
                read_file_state: HashMap::new(),
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
enum SessionMessage {
    /// Regular user turn
    Turn {
        text: String,
        frontend: Arc<dyn Channel>,
        done: Option<oneshot::Sender<()>>,
    },
    /// Control message requiring session state
    Control {
        ctrl: SessionControl,
        reply: oneshot::Sender<ControlEvent>,
    },
}

/// Per-session handle stored in the gateway
struct SessionHandle {
    tx: mpsc::Sender<SessionMessage>,
    interrupted: Arc<AtomicBool>,
}

/// Result of a dispatch operation
pub struct DispatchResult {
    pub agent_id: String,
    pub session_key: String,
    pub done: oneshot::Receiver<()>,
}

/// Central dispatcher: routes messages and manages session lifecycle
///
/// Owns the shared state (router + agent manager), app config,
/// and the per-session task pool.
pub struct Gateway {
    state: SharedState,
    config: AppConfig,
    session_txs: Mutex<HashMap<String, SessionHandle>>,
    cron_handle: Mutex<Option<CronHandle>>,
    delivery: DeliveryHandle,
}

impl Gateway {
    /// Create a new gateway
    pub async fn new(state: SharedState, config: AppConfig) -> Self {
        // Outbound sinks are shared between router and delivery runner
        let outbound = {
            let s = state.read().await;
            s.router.outbound().clone()
        };

        // Start delivery runner
        let delivery_dir = config
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
        let cron_handle = config
            .workspace_dir
            .as_ref()
            .map(|ws| {
                let cron_file = ws.join("CRON.json");
                if cron_file.exists() {
                    Some(crate::scheduler::cron::spawn(
                        cron_file,
                        config.primary_llm().clone(),
                        delivery.clone(),
                    ))
                } else {
                    None
                }
            })
            .flatten();

        Self {
            state,
            config,
            session_txs: Mutex::new(HashMap::new()),
            cron_handle: Mutex::new(cron_handle),
            delivery,
        }
    }

    /// Read access to the shared state
    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Read access to the app config
    pub fn config(&self) -> &AppConfig {
        &self.config
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

    /// Send a control message to an existing session
    ///
    /// # Arguments
    ///
    /// * `session_key` - Target session
    /// * `ctrl` - Control message
    ///
    /// # Returns
    ///
    /// The control event response from the session.
    ///
    /// # Errors
    ///
    /// Returns error if session not found or channel closed.
    /// Interrupt a running session by setting its AtomicBool flag
    ///
    /// # Errors
    ///
    /// Returns error if session not found.
    pub async fn interrupt(&self, session_key: &str) -> Result<()> {
        let txs = self.session_txs.lock().await;
        let handle = txs
            .get(session_key)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
        handle.interrupted.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Send a control message to an existing session
    ///
    /// # Arguments
    ///
    /// * `session_key` - Target session
    /// * `ctrl` - Control message
    ///
    /// # Returns
    ///
    /// The control event response from the session.
    ///
    /// # Errors
    ///
    /// Returns error if session not found or channel closed.
    pub async fn send_control(
        &self,
        session_key: &str,
        ctrl: SessionControl,
    ) -> Result<ControlEvent> {
        let txs = self.session_txs.lock().await;
        let handle = txs
            .get(session_key)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;

        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .tx
            .send(SessionMessage::Control { ctrl, reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("Session task closed"))?;

        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Session did not respond"))
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
        let (session_key, system_prompt, model, agent_id, channel_name, ws_dir, shared_agents, denied_tools) =
        {
            let s = self.state.read().await;
            let result = if let Some(ov) = agent_override {
                s.router.resolve_explicit(ov, &msg)
            } else {
                s.router.resolve(&msg)
            };
            let agents = s.router.shared_agents();
            let agent = agents.get(&result.agent_id);
            let prompt = agent
                .as_ref()
                .map(|a| a.system_prompt.clone())
                .unwrap_or_default();
            let denied = agent
                .as_ref()
                .map(|a| a.denied_tools.clone())
                .unwrap_or_default();
            let model = agents.effective_model(&result.agent_id);
            let ch = msg.channel.clone();
            let ws: Option<PathBuf> = agent
                .as_ref()
                .map(|a| a.workspace_dir.clone())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or(self.config.workspace_dir.clone());
            (result.session_key, prompt, model, result.agent_id, ch, ws, agents, denied)
        };

        // 2. Track session
        {
            let mut s =
                self.state.write().await;
            s.sessions.insert(session_key.clone());
        }

        // 3. Get or create session task
        let (done_tx, done_rx) = oneshot::channel();
        let text = msg.text;

        let session_tx = {
            let mut txs = self.session_txs.lock().await;
            if let Some(h) = txs.get(&session_key) {
                if h.tx.is_closed() {
                    txs.remove(&session_key);
                }
            }

            if let Some(h) = txs.get(&session_key) {
                h.tx.clone()
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
                let cron_handle =
                    self.cron_handle.lock().await.clone();
                let store = self.config.build_store(
                    initial_prompt,
                    model.clone(),
                    intelligence,
                    shared_agents.clone(),
                    ws_dir.as_ref(),
                    cron_handle,
                    &denied_tools,
                );

                let lane_lock: LaneLock =
                    Arc::new(CommandQueue::new());

                // Spawn heartbeat before session so we can pass
                // the handle into Session
                let hb_handle = ws_dir
                    .as_ref()
                    .filter(|ws| ws.join("HEARTBEAT.md").exists())
                    .map(|ws| {
                        let mut llm_config = self.config.primary_llm().clone();
                        llm_config.model = model.clone();
                        crate::scheduler::heartbeat::spawn(
                            ws.clone(),
                            lane_lock.clone(),
                            llm_config,
                            system_prompt,
                            self.delivery.clone(),
                            "bg".to_string(),
                            String::new(),
                        )
                    });

                let extra_profiles = self.config.extra_profiles()
                    .iter()
                    .map(|p| p.to_auth_profile())
                    .collect::<Vec<_>>();
                let fallback_models = self.config.fallback_models.clone();

                let lock = lane_lock.clone();
                let interrupted = Arc::new(AtomicBool::new(false));
                let interrupted_clone = interrupted.clone();
                let (stx, mut srx) =
                    mpsc::channel::<SessionMessage>(8);
                tokio::spawn(async move {
                    let mut session = match Session::new(store, lock, hb_handle, extra_profiles, fallback_models, interrupted_clone) {
                        Ok(s) => s,
                        Err(e) => {
                            log::error!("Failed to create session: {e}");
                            if let Some(msg) = srx.recv().await {
                                if let SessionMessage::Turn { frontend, done, .. } = msg {
                                    frontend.send(&format!("Error: {e}")).await;
                                    if let Some(done) = done {
                                        let _ = done.send(());
                                    }
                                }
                            }
                            return;
                        }
                    };
                    while let Some(msg) = srx.recv().await {
                        match msg {
                            SessionMessage::Turn { text, frontend, done } => {
                                if let Err(e) = session
                                    .turn(&text, &frontend)
                                    .await
                                {
                                    frontend
                                        .send(&format!("Error: {e}"))
                                        .await;
                                }
                                if let Some(done) = done {
                                    let _ = done.send(());
                                }
                            }
                            SessionMessage::Control { ctrl, reply } => {
                                let event = session.handle_control(ctrl);
                                let _ = reply.send(event);
                            }
                        }
                    }
                });
                let tx = stx.clone();
                txs.insert(session_key.clone(), SessionHandle {
                    tx: stx,
                    interrupted,
                });
                tx
            }
        };

        // 4. Send to session
        session_tx
            .send(SessionMessage::Turn {
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
        // User message
        "send" => m_send(gateway, &params).await,
        // Session control
        "context_usage" => m_control(gateway, &params, SessionControl::ContextUsage).await,
        "rewind" => {
            let count = params["count"].as_u64().unwrap_or(1) as usize;
            m_control(gateway, &params, SessionControl::Rewind { count }).await
        }
        "model" => {
            let model = params["model"].as_str().unwrap_or("").to_string();
            m_control(gateway, &params, SessionControl::ModelSwitch { model }).await
        }
        "interrupt" => m_interrupt(gateway, &params).await,
        "permission_mode" => {
            let mode: crate::routing::protocol::PermissionMode = params
                .get("mode")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            m_control(gateway, &params, SessionControl::SetPermissionMode { mode }).await
        }
        // Admin
        "bindings.set" => m_bindings_set(gateway, &params).await,
        "bindings.remove" => m_bindings_remove(gateway, &params).await,
        "bindings.list" => m_bindings_list(gateway).await,
        "agents.list" => m_agents_list(gateway).await,
        "agents.register" => m_agents_register(gateway, &params).await,
        "sessions.list" => m_sessions_list(gateway).await,
        "delivery.stats" => m_delivery_stats(gateway).await,
        "status" => m_status(gateway).await,
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

async fn m_bindings_set(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().await;
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

async fn m_bindings_remove(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().await;
    let removed = s.router.table_mut().remove(
        params["agent_id"].as_str().unwrap_or(""),
        params["match_key"].as_str().unwrap_or(""),
        params["match_value"].as_str().unwrap_or(""),
    );
    Ok(json!({"removed": removed}))
}

async fn m_bindings_list(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().await;
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

async fn m_agents_list(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().await;
    let shared = s.router.shared_agents();
    let list = shared.list();
    let agents: Vec<Value> = list
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "model": shared.effective_model(&a.id),
                "dm_scope": a.dm_scope,
            })
        })
        .collect();
    Ok(json!(agents))
}

async fn m_agents_register(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let mut s =
        gateway.state().write().await;
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
        denied_tools: params["denied_tools"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    };
    let id = normalize_agent_id(&config.id);
    s.router.manager_mut().register(config);
    Ok(json!({"ok": true, "id": id}))
}

async fn m_sessions_list(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().await;
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

async fn m_control(
    gateway: &Arc<Gateway>,
    params: &Value,
    ctrl: SessionControl,
) -> std::result::Result<Value, String> {
    let session_key = params["session_key"]
        .as_str()
        .ok_or("session_key is required")?;

    let event = gateway
        .send_control(session_key, ctrl)
        .await
        .map_err(|e| e.to_string())?;

    serde_json::to_value(&event).map_err(|e| e.to_string())
}

async fn m_interrupt(
    gateway: &Arc<Gateway>,
    params: &Value,
) -> std::result::Result<Value, String> {
    let session_key = params["session_key"]
        .as_str()
        .ok_or("session_key is required")?;

    gateway
        .interrupt(session_key)
        .await
        .map_err(|e| e.to_string())?;

    Ok(json!({"interrupted": true}))
}

async fn m_status(
    gateway: &Arc<Gateway>,
) -> std::result::Result<Value, String> {
    let s = gateway.state().read().await;
    Ok(json!({
        "agents": s.router.shared_agents().list().len(),
        "bindings": s.router.table().list().len(),
        "sessions": s.sessions.len(),
        "uptime_secs": s.start_time.elapsed().as_secs(),
    }))
}