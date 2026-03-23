use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

mod core;
mod frontend;
mod intelligence;
mod logger;

use crate::core::manager::{AgentConfig, AgentManager};
use crate::core::router::BindingTable;
use crate::core::session::Session;
use crate::core::store::{Config, Context, LLMConfig, SharedStore, SystemState};
use crate::frontend::Channel;
use crate::frontend::UserMessage;
use crate::frontend::cli::Cli;
use crate::frontend::gateway::{AppState, SharedState};
use crate::intelligence::Intelligence;

/// A message routed from a frontend to the main loop
pub struct RoutedMessage {
    pub msg: UserMessage,
    pub frontend: Arc<dyn Channel>,
    pub done: Option<oneshot::Sender<()>>,
}

/// Global LLM provider config (shared across all agents)
struct ProviderConfig {
    base_url: String,
    api_key: String,
    context_window: usize,
    default_model: String,
    workspace_dir: Option<std::path::PathBuf>,
}

impl ProviderConfig {
    fn from_env() -> Self {
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
                .map(std::path::PathBuf::from)
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

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    logger::TuiLogger::init();

    let (tx, mut rx) = mpsc::channel::<RoutedMessage>(32);

    // Provider config
    let provider = ProviderConfig::from_env();

    let default_prompt = std::fs::read_to_string("prompts/system.md")
        .expect("Failed to read prompts/system.md");

    // Shared state: agent manager + binding table
    let mut mgr = AgentManager::new(provider.default_model.clone());
    mgr.register(AgentConfig {
        id: "main".into(),
        name: "main".into(),
        personality: String::new(),
        system_prompt: default_prompt,
        model: String::new(),
        dm_scope: "per-peer".into(),
        workspace_dir: String::new(),
    });

    let state: SharedState = Arc::new(RwLock::new(AppState {
        mgr,
        table: BindingTable::new(),
        sessions: Default::default(),
        start_time: Instant::now(),
    }));

    // CLI always starts; /discord and /gateway spawn at runtime
    let cli: Arc<dyn Channel> = Arc::new(Cli::new());
    let cli_clone = cli.clone();
    let cli_tx = tx.clone();
    let dc_tx = tx.clone();
    let gw_tx = tx.clone();
    let gw_state = state.clone();
    drop(tx);

    tokio::spawn(async move {
        let mut discord_started = false;
        let mut gateway_started = false;
        loop {
            let msg = match cli_clone.receive().await {
                Some(msg) => msg,
                None => continue,
            };

            if msg.text == "/exit" {
                frontend::cli::cleanup_terminal();
                std::process::exit(0);
            }

            if msg.text == "/discord" {
                if discord_started {
                    cli_clone
                        .send("Discord gateway already running")
                        .await;
                    continue;
                }
                match std::env::var("DISCORD_BOT_TOKEN") {
                    Ok(token) if !token.is_empty() => {
                        let gateway_tx = dc_tx.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                frontend::discord::start_gateway(
                                    token, gateway_tx,
                                )
                                .await
                            {
                                log::error!(
                                    "Discord gateway error: {e}"
                                );
                            }
                        });
                        discord_started = true;
                        cli_clone
                            .send("Discord gateway started")
                            .await;
                    }
                    _ => {
                        cli_clone
                            .send("DISCORD_BOT_TOKEN not set")
                            .await;
                    }
                }
                continue;
            }

            if msg.text == "/gateway" {
                if gateway_started {
                    cli_clone
                        .send("WebSocket gateway already running")
                        .await;
                    continue;
                }
                let gs = gw_state.clone();
                let gt = gw_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        frontend::gateway::start_gateway(
                            gs,
                            gt,
                            "localhost",
                            8765,
                        )
                        .await
                    {
                        log::error!("WebSocket gateway error: {e}");
                    }
                });
                gateway_started = true;
                cli_clone
                    .send("WebSocket gateway started on ws://localhost:8765")
                    .await;
                continue;
            }

            let (done_tx, done_rx) = oneshot::channel();
            let _ = cli_tx
                .send(RoutedMessage {
                    msg,
                    frontend: cli_clone.clone(),
                    done: Some(done_tx),
                })
                .await;
            let _ = done_rx.await;
        }
    });

    // Main loop: route messages to per-session tasks
    let mut session_txs =
        HashMap::<String, mpsc::Sender<RoutedMessage>>::new();

    let prompts_dir = std::path::Path::new("prompts");

    while let Some(routed) = rx.recv().await {
        let (session_key, system_prompt, model, agent_id, channel_name, ws_dir) =
        {
            let s = state.read().expect("State lock poisoned");
            let (agent_id, sk) = s.resolve_route(&routed.msg);
            let agent = s.mgr.get(&agent_id);
            let prompt = agent
                .map(|a| a.effective_system_prompt())
                .unwrap_or_default();
            let model = s.mgr.effective_model(&agent_id);
            let ch = routed.msg.channel.clone();
            // Per-agent workspace_dir > global WORKSPACE_DIR
            let ws: Option<PathBuf> = agent
                .map(|a| a.workspace_dir.clone())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or(provider.workspace_dir.clone());
            (sk, prompt, model, agent_id, ch, ws)
        };
        {
            let mut s = state.write().expect("State lock poisoned");
            s.sessions.insert(session_key.clone());
        }

        let session_tx = session_txs
            .entry(session_key)
            .or_insert_with(|| {
                let intelligence =
                    ws_dir.as_ref().map(|ws| {
                        Intelligence::new(
                            ws,
                            prompts_dir,
                            agent_id,
                            channel_name,
                            model.clone(),
                        )
                    });
                // If intelligence is configured, use its initial prompt
                let initial_prompt = intelligence
                    .as_ref()
                    .map(|i| i.build_prompt())
                    .unwrap_or(system_prompt);
                let store = provider.build_store(
                    initial_prompt,
                    model,
                    intelligence,
                );
                let (stx, mut srx) = mpsc::channel::<RoutedMessage>(8);
                tokio::spawn(async move {
                    let mut session = Session::new(store)
                        .expect("Failed to create session");
                    while let Some(msg) = srx.recv().await {
                        if let Err(e) = session
                            .turn(&msg.msg.text, &msg.frontend)
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
                stx
            });

        if session_tx.send(routed).await.is_err() {
            log::error!("Session task unexpectedly closed");
        }
    }
}
