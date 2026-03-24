use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

mod core;
mod frontend;
mod intelligence;
mod logger;
mod routing;

use crate::intelligence::manager::{AgentConfig, AgentManager};
use crate::routing::router::{Binding, BindingTable};
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
    /// Force routing to a specific agent (set by CLI /switch)
    pub agent_override: Option<String>,
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
    // Auto-discover agents from WORKSPACE_DIR/.agents/
    if let Some(ws) = &provider.workspace_dir {
        mgr.discover_workspace(&ws.join(".agents"));
    }

    // Load routing bindings from WORKSPACE_DIR/routes.json
    let mut table = BindingTable::new();
    if let Some(ws) = &provider.workspace_dir {
        table.load_file(&ws.join("routes.json"));
    }

    let state: SharedState = Arc::new(RwLock::new(AppState {
        mgr,
        table,
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
    let cli_state = state.clone();
    let routes_path = provider
        .workspace_dir
        .as_ref()
        .map(|ws| ws.join("routes.json"));
    drop(tx);

    tokio::spawn(async move {
        let mut discord_started = false;
        let mut gateway_started = false;
        let mut agent_override: Option<String> = None;
        loop {
            let msg = match cli_clone.receive().await {
                Some(msg) => msg,
                None => continue,
            };

            if msg.text == "/exit" {
                frontend::cli::cleanup_terminal();
                std::process::exit(0);
            }

            // /agents — list all registered agents
            if msg.text == "/agents" {
                let text = {
                    let s = cli_state.read()
                        .expect("State lock poisoned");
                    let agents = s.mgr.list();
                    if agents.is_empty() {
                        "No agents registered.".to_string()
                    } else {
                        let mut lines =
                            vec!["Registered agents:".to_string()];
                        for a in &agents {
                            let ws = if a.workspace_dir.is_empty() {
                                "(default)"
                            } else {
                                &a.workspace_dir
                            };
                            let active =
                                if agent_override.as_deref()
                                    == Some(&*a.id)
                                {
                                    " ← active"
                                } else {
                                    ""
                                };
                            lines.push(format!(
                                "  {} — workspace: {ws}{active}",
                                a.id,
                            ));
                        }
                        lines.join("\n")
                    }
                };
                cli_clone.send(&text).await;
                continue;
            }

            // /switch <agent> | /switch off
            if msg.text.starts_with("/switch") {
                let arg = msg.text.strip_prefix("/switch")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if arg.is_empty() {
                    let current = agent_override
                        .as_deref()
                        .unwrap_or("(default routing)");
                    cli_clone
                        .send(&format!("Current agent: {current}"))
                        .await;
                } else if arg == "off" {
                    agent_override = None;
                    cli_clone
                        .send("Switched to default routing.")
                        .await;
                } else {
                    let found = cli_state
                        .read()
                        .expect("State lock poisoned")
                        .mgr
                        .get(&arg)
                        .is_some();
                    if found {
                        cli_clone
                            .send(&format!("Switched to agent: {arg}"))
                            .await;
                        agent_override = Some(arg);
                    } else {
                        cli_clone
                            .send(&format!(
                                "Agent '{arg}' not found. Use /agents to list."
                            ))
                            .await;
                    }
                }
                continue;
            }

            // /route — manage routing bindings
            // /route                          list all
            // /route <channel> <agent>        add tier-4 binding
            // /route rm <channel>             remove tier-4 binding
            if msg.text.starts_with("/route") {
                let args: Vec<&str> = msg.text
                    .strip_prefix("/route")
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                let default_path = PathBuf::from("routes.json");
                let bindings_path = routes_path
                    .as_deref()
                    .unwrap_or(&default_path);
                match args.as_slice() {
                    // /route — list
                    [] => {
                        let text = {
                            let s = cli_state.read()
                                .expect("State lock poisoned");
                            let bindings = s.table.list();
                            if bindings.is_empty() {
                                "No bindings. Use: /route <channel> <agent>"
                                    .to_string()
                            } else {
                                let mut lines =
                                    vec!["Bindings:".to_string()];
                                for b in bindings {
                                    lines.push(format!(
                                        "  T{} {} = {} → {}",
                                        b.tier,
                                        b.match_key,
                                        b.match_value,
                                        b.agent_id,
                                    ));
                                }
                                lines.join("\n")
                            }
                        };
                        cli_clone.send(&text).await;
                    }
                    // /route rm <channel>
                    ["rm", channel] => {
                        let removed = {
                            let mut s = cli_state.write()
                                .expect("State lock poisoned");
                            let before = s.table.list().len();
                            s.table.remove_by_key("channel", channel);
                            s.table.save_file(bindings_path);
                            before != s.table.list().len()
                        };
                        if removed {
                            cli_clone
                                .send(&format!(
                                    "Removed binding for channel: {channel}"
                                ))
                                .await;
                        } else {
                            cli_clone
                                .send(&format!(
                                    "No binding found for channel: {channel}"
                                ))
                                .await;
                        }
                    }
                    // /route <channel> <agent>
                    [channel, agent] => {
                        let msg = {
                            let s = cli_state.read()
                                .expect("State lock poisoned");
                            if s.mgr.get(agent).is_none() {
                                format!(
                                    "Agent '{agent}' not found. \
                                     Use /agents to list."
                                )
                            } else {
                                drop(s);
                                let mut s = cli_state.write()
                                    .expect("State lock poisoned");
                                s.table
                                    .remove_by_key("channel", channel);
                                s.table.add(Binding {
                                    agent_id: agent.to_string(),
                                    tier: 4,
                                    match_key: "channel".into(),
                                    match_value: channel.to_string(),
                                    priority: 0,
                                });
                                s.table.save_file(bindings_path);
                                format!(
                                    "Bound channel '{channel}' \
                                     → agent '{agent}'"
                                )
                            }
                        };
                        cli_clone.send(&msg).await;
                    }
                    _ => {
                        cli_clone
                            .send(
                                "Usage:\n\
                                 \x20 /route                  List bindings\n\
                                 \x20 /route <channel> <agent>  Bind channel to agent\n\
                                 \x20 /route rm <channel>       Remove binding",
                            )
                            .await;
                    }
                }
                continue;
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
                    agent_override: agent_override.clone(),
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
            // /switch override > normal routing
            let (agent_id, sk) =
                if let Some(ref ov) = routed.agent_override {
                    let aid = ov.clone();
                    let sk = routing::router::build_session_key(
                        &aid,
                        &routed.msg.channel,
                        &routed.msg.account_id,
                        &routed.msg.sender_id,
                        s.mgr
                            .get(&aid)
                            .map(|a| a.dm_scope.as_str())
                            .unwrap_or("per-peer"),
                    );
                    (aid, sk)
                } else {
                    s.resolve_route(&routed.msg)
                };
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
                            system_prompt.clone(),
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
