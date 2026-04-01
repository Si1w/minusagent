use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config;
use crate::frontend::gateway::Gateway;
use crate::frontend::{Channel, UserMessage, cli};
use crate::routing::router::Router;

// ── CLI command definitions ────────────────────────────────

#[derive(Parser)]
#[command(name = "/", no_binary_name = true, disable_help_flag = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Exit the application
    Exit,
    /// Show available commands
    Help,

    // ── Config ──
    /// Manage LLM profiles
    Llm {
        #[command(subcommand)]
        action: Option<LlmAction>,
    },

    // ── Agents & Routing ──
    /// List registered agents
    Agents,
    /// Switch agent or show current
    Switch {
        /// Agent ID, or "off" to restore default routing
        agent: Option<String>,
    },
    /// List route bindings
    Bindings,
    /// Test route resolution
    Route {
        channel: String,
        peer_id: String,
        account_id: Option<String>,
        guild_id: Option<String>,
    },

    // ── Scheduler ──
    /// Manage cron jobs
    Cron {
        #[command(subcommand)]
        action: Option<CronAction>,
    },
    /// Delivery queue stats
    Delivery {
        #[command(subcommand)]
        action: Option<DeliveryAction>,
    },

    // ── Gateways ──
    /// Start Discord bot
    Discord,
    /// Start WebSocket gateway
    Gateway,
}

#[derive(Subcommand)]
enum LlmAction {
    /// Add a new LLM profile (interactive)
    Add,
    /// Remove an LLM profile by model name
    Rm { model: String },
    /// Set an LLM profile as primary
    Primary { model: String },
}

#[derive(Subcommand)]
enum CronAction {
    /// Stop the cron service
    Stop,
    /// Reload CRON.json
    Reload,
    /// Manually trigger a cron job
    Trigger { id: String },
}

#[derive(Subcommand)]
enum DeliveryAction {
    /// Stop the delivery runner
    Stop,
}

// ── REPL ───────────────────────────────────────────────────

/// Try to parse a `/`-prefixed input as a clap command.
/// Returns `None` if the input is not a known repl-level command
/// (i.e. it should be passed through to the session).
fn parse_cmd(input: &str) -> Option<Result<Cmd, String>> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }

    let tokens: Vec<&str> = trimmed[1..].split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    match Cli::try_parse_from(&tokens) {
        Ok(cli) => Some(Ok(cli.cmd)),
        Err(e) => {
            // clap returns DisplayHelp / DisplayVersion for --help etc.
            // For unknown subcommands, let the input fall through to session.
            if e.kind() == clap::error::ErrorKind::InvalidSubcommand
                || e.kind() == clap::error::ErrorKind::UnknownArgument
            {
                None
            } else {
                Some(Err(e.render().to_string()))
            }
        }
    }
}

/// Run the CLI REPL
///
/// Handles routing-level commands and dispatches normal messages
/// through the gateway. Session-level commands (`/new`, `/save`,
/// `/compact`, `/prompt`, etc.) pass through to the session.
pub async fn run(gateway: Arc<Gateway>, cli: Arc<dyn Channel>) {
    let mut discord_started = false;
    let mut ws_started = false;
    let mut agent_override: Option<String> = None;

    loop {
        let msg = match cli.receive().await {
            Some(msg) => msg,
            None => continue,
        };

        match parse_cmd(&msg.text) {
            // Not a repl command → pass through to gateway/session
            None => {
                match gateway
                    .dispatch(msg, cli.clone(), agent_override.as_deref())
                    .await
                {
                    Ok(result) => { let _ = result.done.await; }
                    Err(e) => { cli.send(&format!("Error: {e}")).await; }
                }
                continue;
            }
            // Parse error from clap
            Some(Err(e)) => {
                cli.send(&e).await;
                continue;
            }
            Some(Ok(cmd)) => {
                dispatch(cmd, &gateway, &cli, &mut agent_override, &mut discord_started, &mut ws_started).await;
            }
        }
    }
}

async fn dispatch(
    cmd: Cmd,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
    agent_override: &mut Option<String>,
    discord_started: &mut bool,
    ws_started: &mut bool,
) {
    match cmd {
        Cmd::Exit => {
            cli::cleanup_terminal();
            std::process::exit(0);
        }

        Cmd::Help => {
            cli.send(HELP_TEXT).await;
        }

        // ── LLM ──

        Cmd::Llm { action: None } => {
            match config::list_llm_models() {
                Ok(models) if models.is_empty() => {
                    cli.send("No LLM profiles.").await;
                }
                Ok(models) => {
                    let mut lines = vec!["LLM Profiles:".to_string()];
                    for (i, m) in models.iter().enumerate() {
                        let tag = if i == 0 { " (primary)" } else { "" };
                        lines.push(format!("  {m}{tag}"));
                    }
                    cli.send(&lines.join("\n")).await;
                }
                Err(e) => cli.send(&format!("Error: {e}")).await,
            }
        }

        Cmd::Llm { action: Some(LlmAction::Add) } => {
            cli.send("model name:").await;
            let model = match cli.receive().await {
                Some(m) if !m.text.trim().is_empty() => m.text.trim().to_string(),
                _ => { cli.send("Cancelled.").await; return; }
            };
            cli.send("base_url:").await;
            let base_url = match cli.receive().await {
                Some(m) if !m.text.trim().is_empty() => m.text.trim().to_string(),
                _ => { cli.send("Cancelled.").await; return; }
            };
            cli.send("api_key (or $ENV_VAR):").await;
            let api_key = match cli.receive().await {
                Some(m) if !m.text.trim().is_empty() => m.text.trim().to_string(),
                _ => { cli.send("Cancelled.").await; return; }
            };
            cli.send("context_window:").await;
            let context_window = match cli.receive().await {
                Some(m) => match m.text.trim().parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => { cli.send("Invalid number. Cancelled.").await; return; }
                },
                None => { cli.send("Cancelled.").await; return; }
            };
            let entry = config::LLMConfig {
                model: model.clone(),
                base_url,
                api_key,
                context_window,
            };
            match config::add_llm(&entry) {
                Ok(()) => cli.send(&format!("Added: {model}")).await,
                Err(e) => cli.send(&format!("Error: {e}")).await,
            }
        }

        Cmd::Llm { action: Some(LlmAction::Rm { model }) } => {
            match config::remove_llm(&model) {
                Ok(()) => cli.send(&format!("Removed: {model}")).await,
                Err(e) => cli.send(&format!("Error: {e}")).await,
            }
        }

        Cmd::Llm { action: Some(LlmAction::Primary { model }) } => {
            match config::set_primary_llm(&model) {
                Ok(()) => cli.send(&format!("Primary set to: {model}")).await,
                Err(e) => cli.send(&format!("Error: {e}")).await,
            }
        }

        // ── Agents & Routing ──

        Cmd::Agents => {
            let text = {
                let s = gateway.state().read().await;
                let agents = s.router.shared_agents().list();
                if agents.is_empty() {
                    "No agents registered.".to_string()
                } else {
                    let mut lines = vec!["Registered agents:".to_string()];
                    for a in &agents {
                        let ws = if a.workspace_dir.is_empty() {
                            "(default)"
                        } else {
                            &a.workspace_dir
                        };
                        let active = if agent_override.as_deref() == Some(&*a.id) {
                            " ← active"
                        } else {
                            ""
                        };
                        lines.push(format!("  {} — workspace: {ws}{active}", a.id));
                    }
                    lines.join("\n")
                }
            };
            cli.send(&text).await;
        }

        Cmd::Switch { agent: None } => {
            let current = agent_override.as_deref().unwrap_or("(default routing)");
            cli.send(&format!("Current agent: {current}")).await;
        }

        Cmd::Switch { agent: Some(ref name) } if name == "off" => {
            *agent_override = None;
            cli.send("Switched to default routing.").await;
        }

        Cmd::Switch { agent: Some(name) } => {
            let found = gateway.state().read().await
                .router.shared_agents().get(&name).is_some();
            if found {
                cli.send(&format!("Switched to agent: {name}")).await;
                *agent_override = Some(name);
            } else {
                cli.send(&format!("Agent '{name}' not found. Use /agents to list.")).await;
            }
        }

        Cmd::Bindings => {
            let text = {
                let s = gateway.state().read().await;
                let bindings = s.router.table().list();
                if bindings.is_empty() {
                    "No bindings.".to_string()
                } else {
                    let mut lines = vec![format!("Route Bindings ({}):", bindings.len())];
                    for b in bindings {
                        lines.push(format!(
                            "  T{} {} = {} → {}",
                            b.tier, b.match_key, b.match_value, b.agent_id,
                        ));
                    }
                    lines.join("\n")
                }
            };
            cli.send(&text).await;
        }

        Cmd::Route { channel, peer_id, account_id, guild_id } => {
            let test_msg = UserMessage {
                text: String::new(),
                channel,
                sender_id: peer_id,
                account_id: account_id.unwrap_or_default(),
                guild_id: guild_id.unwrap_or_default(),
            };
            let text = {
                let s = gateway.state().read().await;
                let result = s.router.resolve(&test_msg);
                let agent = s.router.shared_agents().get(&result.agent_id);
                let name = agent.as_ref().map(|a| a.name.as_str()).unwrap_or("?");
                format!(
                    "Route Resolution:\n  Agent:   {} ({})\n  Session: {}",
                    result.agent_id, name, result.session_key,
                )
            };
            cli.send(&text).await;
        }

        // ── Scheduler ──

        Cmd::Cron { action: None } => {
            let jobs = gateway.cron_list_jobs().await;
            if jobs.is_empty() {
                cli.send("No cron jobs.").await;
            } else {
                let mut lines = vec![format!("Cron Jobs ({}):", jobs.len())];
                for j in &jobs {
                    let en = if j.enabled { "on" } else { "OFF" };
                    lines.push(format!(
                        "  {} ({}) [{en}] kind={} errors={} last={} next={}",
                        j.name, j.id, j.kind, j.errors, j.last_run, j.next_run,
                    ));
                }
                cli.send(&lines.join("\n")).await;
            }
        }

        Cmd::Cron { action: Some(CronAction::Stop) } => {
            if let Some(h) = gateway.cron_handle().await {
                h.stop();
                cli.send("Cron service stopped.").await;
            } else {
                cli.send("Cron service not running.").await;
            }
        }

        Cmd::Cron { action: Some(CronAction::Reload) } => {
            if let Some(h) = gateway.cron_handle().await {
                let result = h.reload().await;
                cli.send(&result).await;
            } else {
                cli.send("Cron service not running.").await;
            }
        }

        Cmd::Cron { action: Some(CronAction::Trigger { id }) } => {
            if let Some(h) = gateway.cron_handle().await {
                let result = h.trigger_job(&id).await;
                cli.send(&result).await;
            } else {
                cli.send("Cron service not running.").await;
            }
        }

        Cmd::Delivery { action: None } => {
            if let Some(st) = gateway.delivery().stats().await {
                cli.send(&format!(
                    "Delivery Queue:\n  attempted={}  succeeded={}  failed={}  pending={}",
                    st.total_attempted, st.total_succeeded, st.total_failed, st.pending,
                )).await;
            } else {
                cli.send("Delivery runner not available.").await;
            }
        }

        Cmd::Delivery { action: Some(DeliveryAction::Stop) } => {
            gateway.delivery().stop();
            cli.send("Delivery runner stopped.").await;
        }

        // ── Gateways ──

        Cmd::Discord => {
            if *discord_started {
                cli.send("Discord gateway already running").await;
                return;
            }
            match &gateway.config().discord_token {
                Some(token) => {
                    let token = token.clone();
                    let gw = gateway.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::frontend::discord::start_gateway(token, gw).await
                        {
                            log::error!("Discord gateway error: {e}");
                        }
                    });
                    *discord_started = true;
                    cli.send("Discord gateway started").await;
                }
                _ => cli.send("DISCORD_BOT_TOKEN not set").await,
            }
        }

        Cmd::Gateway => {
            if *ws_started {
                cli.send("WebSocket gateway already running").await;
                return;
            }
            let gw = gateway.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    crate::frontend::gateway::start_ws(gw, "localhost", 8765).await
                {
                    log::error!("WebSocket gateway error: {e}");
                }
            });
            *ws_started = true;
            cli.send("WebSocket gateway started on ws://localhost:8765").await;
        }
    }
}

const HELP_TEXT: &str = "\
Sessions
  /new <label>            New session
  /save                   Save session
  /load <label>           Load session
  /list                   List sessions
  /compact                Compact history

Intelligence
  /prompt                 Show system prompt
  /remember <name> <txt>  Save memory
  /<skill> [args]         Invoke skill

Config
  /llm                    List LLM profiles
  /llm add                Add profile (interactive)
  /llm rm <model>         Remove profile
  /llm primary <model>    Set primary

Agents & Routing
  /agents                 List agents
  /switch <agent>         Switch agent
  /switch off             Default routing
  /bindings               List route bindings
  /route <ch> <peer>      Test route resolution

Scheduler
  /heartbeat              Heartbeat status
  /heartbeat stop         Stop heartbeat
  /trigger                Manual heartbeat
  /cron                   List cron jobs
  /cron stop              Stop cron service
  /delivery               Delivery queue stats
  /delivery stop          Stop delivery runner

Gateways
  /discord                Discord bot
  /gateway                WebSocket API

/help  /exit";
