use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config;
use crate::frontend::gateway::{Gateway, ManagedService, ServiceCommand, ServiceStatus};
use crate::frontend::{Channel, UserMessage, cli};
use crate::routing::router::Router;

// ── CLI command definitions ────────────────────────────────

#[derive(Parser)]
#[command(
    name = "/",
    no_binary_name = true,
    disable_help_flag = true,
    disable_help_subcommand = true
)]
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
    /// Show runtime service status
    Services,

    // ── Gateways ──
    /// Manage Discord bot runtime
    Discord {
        #[command(subcommand)]
        action: Option<FrontendAction>,
    },
    /// Manage WebSocket gateway runtime
    Gateway {
        #[command(subcommand)]
        action: Option<FrontendAction>,
    },
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
    /// Start the cron service
    Start,
    /// Stop the cron service
    Stop,
    /// Restart the cron service
    Restart,
    /// Reload CRON.json
    Reload,
    /// Manually trigger a cron job
    Trigger { id: String },
}

#[derive(Subcommand)]
enum DeliveryAction {
    /// Start the delivery runner
    Start,
    /// Stop the delivery runner
    Stop,
    /// Restart the delivery runner
    Restart,
}

#[derive(Subcommand)]
enum FrontendAction {
    /// Start the service
    Start,
    /// Stop the service
    Stop,
    /// Restart the service
    Restart,
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
    let mut state = ReplState::default();

    loop {
        let Some(msg) = cli.receive().await else {
            continue;
        };

        match parse_cmd(&msg.text) {
            None => {
                handle_session_passthrough(&gateway, &cli, &state, msg).await;
            }
            Some(Err(e)) => {
                cli.send(&e).await;
            }
            Some(Ok(cmd)) => {
                dispatch_command(cmd, &gateway, &cli, &mut state).await;
            }
        }
    }
}

#[derive(Default)]
struct ReplState {
    agent_override: Option<String>,
}

async fn handle_session_passthrough(
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
    state: &ReplState,
    msg: UserMessage,
) {
    match gateway
        .dispatch(msg, cli.clone(), state.agent_override.as_deref())
        .await
    {
        Ok(result) => {
            let _ = result.done.await;
        }
        Err(e) => {
            cli.send(&format!("Error: {e}")).await;
        }
    }
}

async fn dispatch_command(
    cmd: Cmd,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
    state: &mut ReplState,
) {
    match cmd {
        Cmd::Exit => {
            cli::cleanup_terminal();
            std::process::exit(0);
        }

        Cmd::Help => {
            cli.send(HELP_TEXT).await;
        }

        Cmd::Llm { action } => handle_llm_command(action, cli).await,
        Cmd::Agents | Cmd::Switch { .. } | Cmd::Bindings | Cmd::Route { .. } => {
            handle_routing_command(cmd, gateway, cli, state).await;
        }
        Cmd::Cron { action } => handle_cron_command(action, gateway, cli).await,
        Cmd::Delivery { action } => handle_delivery_command(action, gateway, cli).await,
        Cmd::Services => handle_services_command(gateway, cli).await,
        Cmd::Discord { .. } | Cmd::Gateway { .. } => {
            handle_gateway_command(cmd, gateway, cli).await;
        }
    }
}

async fn handle_llm_command(action: Option<LlmAction>, cli: &Arc<dyn Channel>) {
    match action {
        None => show_llm_profiles(cli).await,
        Some(LlmAction::Add) => add_llm_profile(cli).await,
        Some(LlmAction::Rm { model }) => match config::remove_llm(&model) {
            Ok(()) => cli.send(&format!("Removed: {model}")).await,
            Err(e) => cli.send(&format!("Error: {e}")).await,
        },
        Some(LlmAction::Primary { model }) => match config::set_primary_llm(&model) {
            Ok(()) => cli.send(&format!("Primary set to: {model}")).await,
            Err(e) => cli.send(&format!("Error: {e}")).await,
        },
    }
}

async fn show_llm_profiles(cli: &Arc<dyn Channel>) {
    match config::list_llm_profiles() {
        Ok(profiles) if profiles.is_empty() => cli.send("No LLM profiles.").await,
        Ok(profiles) => {
            let mut lines = vec![format!("LLM Profiles ({}):", profiles.len())];
            for (i, profile) in profiles.iter().enumerate() {
                let tag = if i == 0 { " ← primary" } else { "" };
                lines.push(format!(
                    "  {} │ {} │ ctx={}k{}",
                    profile.model,
                    profile.base_url.trim_end_matches('/'),
                    profile.context_window / 1000,
                    tag,
                ));
            }
            cli.send(&lines.join("\n")).await;
        }
        Err(e) => cli.send(&format!("Error: {e}")).await,
    }
}

async fn add_llm_profile(cli: &Arc<dyn Channel>) {
    let Some(model) = prompt_required(cli, "model name:").await else {
        cli.send("Cancelled.").await;
        return;
    };
    let Some(base_url) = prompt_required(cli, "base_url:").await else {
        cli.send("Cancelled.").await;
        return;
    };
    let Some(api_key) = prompt_required(cli, "api_key (or $ENV_VAR):").await else {
        cli.send("Cancelled.").await;
        return;
    };
    let Some(context_window) = prompt_usize(cli, "context_window:").await else {
        return;
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

async fn prompt_required(cli: &Arc<dyn Channel>, prompt: &str) -> Option<String> {
    cli.send(prompt).await;
    let msg = cli.receive().await?;
    let text = msg.text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

async fn prompt_usize(cli: &Arc<dyn Channel>, prompt: &str) -> Option<usize> {
    cli.send(prompt).await;
    let Some(message) = cli.receive().await else {
        cli.send("Cancelled.").await;
        return None;
    };

    if let Ok(value) = message.text.trim().parse::<usize>() {
        Some(value)
    } else {
        cli.send("Invalid number. Cancelled.").await;
        None
    }
}

async fn handle_routing_command(
    cmd: Cmd,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
    state: &mut ReplState,
) {
    match cmd {
        Cmd::Agents => {
            let text = {
                let s = gateway.state().read().await;
                let agents = s.router.shared_agents().list();
                if agents.is_empty() {
                    "No agents registered.".to_string()
                } else {
                    let mut lines = vec!["Registered agents:".to_string()];
                    for agent in &agents {
                        let workspace = if agent.workspace_dir.is_empty() {
                            "(default)"
                        } else {
                            &agent.workspace_dir
                        };
                        let active = if state.agent_override.as_deref() == Some(&agent.id) {
                            " ← active"
                        } else {
                            ""
                        };
                        lines.push(format!("  {} — workspace: {workspace}{active}", agent.id));
                    }
                    lines.join("\n")
                }
            };
            cli.send(&text).await;
        }
        Cmd::Switch { agent } => handle_switch_command(agent, gateway, cli, state).await,
        Cmd::Bindings => {
            let text = {
                let s = gateway.state().read().await;
                let bindings = s.router.table().list();
                if bindings.is_empty() {
                    "No bindings.".to_string()
                } else {
                    let mut lines = vec![format!("Route Bindings ({}):", bindings.len())];
                    for binding in bindings {
                        lines.push(format!(
                            "  T{} {} = {} → {}",
                            binding.tier, binding.match_key, binding.match_value, binding.agent_id,
                        ));
                    }
                    lines.join("\n")
                }
            };
            cli.send(&text).await;
        }
        Cmd::Route {
            channel,
            peer_id,
            account_id,
            guild_id,
        } => {
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
                let name = agent.as_ref().map_or("?", |entry| entry.name.as_str());
                format!(
                    "Route Resolution:\n  Agent:   {} ({})\n  Session: {}",
                    result.agent_id, name, result.session_key,
                )
            };
            cli.send(&text).await;
        }
        _ => {}
    }
}

async fn handle_switch_command(
    agent: Option<String>,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
    state: &mut ReplState,
) {
    match agent {
        None => {
            let current = state
                .agent_override
                .as_deref()
                .unwrap_or("(default routing)");
            cli.send(&format!("Current agent: {current}")).await;
        }
        Some(name) if name == "off" => {
            state.agent_override = None;
            cli.send("Switched to default routing.").await;
        }
        Some(name) => {
            let found = gateway
                .state()
                .read()
                .await
                .router
                .shared_agents()
                .get(&name)
                .is_some();
            if found {
                cli.send(&format!("Switched to agent: {name}")).await;
                state.agent_override = Some(name);
            } else {
                cli.send(&format!("Agent '{name}' not found. Use /agents to list."))
                    .await;
            }
        }
    }
}

async fn handle_cron_command(
    action: Option<CronAction>,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
) {
    match action {
        None => {
            let jobs = gateway.cron_list_jobs().await;
            if jobs.is_empty() {
                cli.send("No cron jobs.").await;
            } else {
                let mut lines = vec![format!("Cron Jobs ({}):", jobs.len())];
                for job in &jobs {
                    let enabled = if job.enabled { "on" } else { "OFF" };
                    lines.push(format!(
                        "  {} ({}) [{enabled}] kind={} errors={} last={} next={}",
                        job.name, job.id, job.kind, job.errors, job.last_run, job.next_run,
                    ));
                }
                cli.send(&lines.join("\n")).await;
            }
        }
        Some(CronAction::Stop) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Cron, ServiceCommand::Stop)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(CronAction::Start) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Cron, ServiceCommand::Start)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(CronAction::Restart) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Cron, ServiceCommand::Restart)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(CronAction::Reload) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Cron, ServiceCommand::Reload)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(CronAction::Trigger { id }) => {
            with_cron_handle(gateway, cli, |handle| async move {
                handle.trigger_job(&id).await
            })
            .await;
        }
    }
}

async fn handle_services_command(gateway: &Arc<Gateway>, cli: &Arc<dyn Channel>) {
    let statuses = gateway.services().status_snapshot(gateway).await;
    cli.send(&render_service_statuses(&statuses)).await;
}

async fn with_cron_handle<F, Fut>(gateway: &Arc<Gateway>, cli: &Arc<dyn Channel>, f: F)
where
    F: FnOnce(crate::scheduler::cron::CronHandle) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    if let Some(handle) = gateway.cron_handle() {
        let result = f(handle).await;
        cli.send(&result).await;
    } else {
        cli.send("Cron service not running.").await;
    }
}

async fn handle_delivery_command(
    action: Option<DeliveryAction>,
    gateway: &Arc<Gateway>,
    cli: &Arc<dyn Channel>,
) {
    match action {
        None => {
            if let Some(stats) = gateway.delivery().stats().await {
                cli.send(&format!(
                    "Delivery Queue:\n  attempted={}  succeeded={}  failed={}  pending={}",
                    stats.total_attempted, stats.total_succeeded, stats.total_failed, stats.pending,
                ))
                .await;
            } else {
                cli.send("Delivery runner not available.").await;
            }
        }
        Some(DeliveryAction::Stop) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Delivery, ServiceCommand::Stop)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(DeliveryAction::Start) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Delivery, ServiceCommand::Start)
                    .await
                    .to_string(),
            )
            .await;
        }
        Some(DeliveryAction::Restart) => {
            cli.send(
                &gateway
                    .services()
                    .control(gateway, ManagedService::Delivery, ServiceCommand::Restart)
                    .await
                    .to_string(),
            )
            .await;
        }
    }
}

async fn handle_gateway_command(cmd: Cmd, gateway: &Arc<Gateway>, cli: &Arc<dyn Channel>) {
    let (service, command) = match cmd {
        Cmd::Discord { action } => (ManagedService::Discord, map_frontend_action(action)),
        Cmd::Gateway { action } => (ManagedService::Websocket, map_frontend_action(action)),
        _ => return,
    };
    cli.send(
        &gateway
            .services()
            .control(gateway, service, command)
            .await
            .to_string(),
    )
    .await;
}

fn map_frontend_action(action: Option<FrontendAction>) -> ServiceCommand {
    match action.unwrap_or(FrontendAction::Start) {
        FrontendAction::Start => ServiceCommand::Start,
        FrontendAction::Stop => ServiceCommand::Stop,
        FrontendAction::Restart => ServiceCommand::Restart,
    }
}

fn render_service_statuses(statuses: &[ServiceStatus]) -> String {
    let mut lines = vec!["Runtime Services:".to_string()];
    for status in statuses {
        let marker = if status.running { "on " } else { "OFF" };
        lines.push(format!(
            "  {:9} [{marker}] {}",
            status.service.label(),
            status.summary
        ));
    }
    lines.join("\n")
}

const HELP_TEXT: &str = "\
## Sessions

- `/new <label>` — new session
- `/save` — save current session
- `/load <label>` — load session
- `/list` — list sessions
- `/compact` — compact history

## Intelligence

- `/prompt` — show system prompt
- `/remember <name> <txt>` — save memory
- `/<skill> [args]` — invoke skill

## Agents

- `/agents` — list agents
- `/switch <agent|off>` — switch agent (`off` = default routing)
- `/bindings` — list route bindings
- `/route <ch> <peer>` — test route resolution

## Team

- `/team` — show team roster
- `/inbox` — check lead inbox
- `/tasks` — show task board
- `/worktrees` — list worktrees
- `/events` — worktree event log

## Config

- `/llm [add|rm <model>|primary <model>]` — manage LLM profiles

## Resilience

- `/profiles` — show API key profiles
- `/lanes` — show lane stats

## Scheduler

- `/heartbeat` — heartbeat status
- `/trigger` — manual heartbeat
- `/cron [start|stop|restart|trigger <id>|reload]` — cron service
- `/delivery [start|stop|restart]` — delivery runner
- `/services` — runtime service status

## Gateways

- `/discord [start|stop|restart]` — Discord bot
- `/gateway [start|stop|restart]` — WebSocket API

## Misc

- `/help` — show this help
- `/exit` — quit
";
