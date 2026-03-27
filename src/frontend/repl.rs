use std::sync::Arc;

use crate::frontend::gateway::Gateway;
use crate::frontend::{Channel, UserMessage, cli};
use crate::routing::router::Router;

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

        if msg.text == "/exit" {
            cli::cleanup_terminal();
            std::process::exit(0);
        }

        if msg.text == "/help" {
            cli.send(
                "Sessions\n\
                 \x20 /new <label>            New session\n\
                 \x20 /save                   Save session\n\
                 \x20 /load <label>           Load session\n\
                 \x20 /list                   List sessions\n\
                 \x20 /compact                Compact history\n\
                 \n\
                 Intelligence\n\
                 \x20 /prompt                 Show system prompt\n\
                 \x20 /remember <name> <txt>  Save memory\n\
                 \x20 /<skill> [args]         Invoke skill\n\
                 \n\
                 Agents & Routing\n\
                 \x20 /agents                 List agents\n\
                 \x20 /switch <agent>         Switch agent\n\
                 \x20 /switch off             Default routing\n\
                 \x20 /bindings               List route bindings\n\
                 \x20 /route <ch> <peer>      Test route resolution\n\
                 \n\
                 Scheduler\n\
                 \x20 /heartbeat              Heartbeat status\n\
                 \x20 /heartbeat stop         Stop heartbeat\n\
                 \x20 /trigger                Manual heartbeat\n\
                 \x20 /cron                   List cron jobs\n\
                 \x20 /cron stop              Stop cron service\n\
                 \x20 /delivery               Delivery queue stats\n\
                 \x20 /delivery stop          Stop delivery runner\n\
                 \n\
                 Gateways\n\
                 \x20 /discord                Discord bot\n\
                 \x20 /gateway                WebSocket API\n\
                 \n\
                 /help  /exit",
            )
            .await;
            continue;
        }

        // /agents — list all registered agents
        if msg.text == "/agents" {
            let text = {
                let s = gateway
                    .state()
                    .read()
                    .await;
                let agents = s.router.manager().list();
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
            cli.send(&text).await;
            continue;
        }

        // /switch <agent> | /switch off
        if msg.text.starts_with("/switch") {
            let arg = msg
                .text
                .strip_prefix("/switch")
                .unwrap_or("")
                .trim()
                .to_string();
            if arg.is_empty() {
                let current = agent_override
                    .as_deref()
                    .unwrap_or("(default routing)");
                cli.send(&format!("Current agent: {current}"))
                    .await;
            } else if arg == "off" {
                agent_override = None;
                cli.send("Switched to default routing.").await;
            } else {
                let found = gateway
                    .state()
                    .read()
                    .await
                    .router
                    .manager()
                    .get(&arg)
                    .is_some();
                if found {
                    cli.send(&format!("Switched to agent: {arg}"))
                        .await;
                    agent_override = Some(arg);
                } else {
                    cli.send(&format!(
                        "Agent '{arg}' not found. \
                         Use /agents to list."
                    ))
                    .await;
                }
            }
            continue;
        }

        // /bindings — list all route bindings
        if msg.text == "/bindings" {
            let text = {
                let s = gateway
                    .state()
                    .read()
                    .await;
                let bindings = s.router.table().list();
                if bindings.is_empty() {
                    "No bindings.".to_string()
                } else {
                    let mut lines = vec![format!(
                        "Route Bindings ({}):",
                        bindings.len()
                    )];
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
            cli.send(&text).await;
            continue;
        }

        // /route <channel> <peer_id> [account_id] [guild_id]
        // Test route resolution without sending a message
        if msg.text.starts_with("/route") {
            let args: Vec<&str> = msg
                .text
                .strip_prefix("/route")
                .unwrap_or("")
                .split_whitespace()
                .collect();
            if args.len() < 2 {
                cli.send(
                    "Usage: /route <channel> <peer_id> \
                     [account_id] [guild_id]",
                )
                .await;
            } else {
                let test_msg = UserMessage {
                    text: String::new(),
                    channel: args[0].to_string(),
                    sender_id: args[1].to_string(),
                    account_id: args
                        .get(2)
                        .unwrap_or(&"")
                        .to_string(),
                    guild_id: args
                        .get(3)
                        .unwrap_or(&"")
                        .to_string(),
                };
                let text = {
                    let s = gateway
                        .state()
                        .read()
                        .await;
                    let result = s.router.resolve(&test_msg);
                    let agent =
                        s.router.manager().get(&result.agent_id);
                    let name = agent
                        .map(|a| a.name.as_str())
                        .unwrap_or("?");
                    format!(
                        "Route Resolution:\n\
                         \x20 Agent:   {} ({})\n\
                         \x20 Session: {}",
                        result.agent_id, name, result.session_key,
                    )
                };
                cli.send(&text).await;
            }
            continue;
        }

        // /cron — list cron jobs
        if msg.text.starts_with("/cron") {
            let arg = msg
                .text
                .strip_prefix("/cron")
                .unwrap_or("")
                .trim()
                .to_string();

            if arg.starts_with("trigger ") {
                let job_id = arg.strip_prefix("trigger ").unwrap().trim();
                if let Some(h) = gateway.cron_handle().await {
                    let result = h.trigger_job(job_id).await;
                    cli.send(&result).await;
                } else {
                    cli.send("Cron service not running.").await;
                }
            } else if arg == "stop" {
                if let Some(h) = gateway.cron_handle().await {
                    h.stop();
                    cli.send("Cron service stopped.").await;
                } else {
                    cli.send("Cron service not running.").await;
                }
            } else if arg == "reload" {
                if let Some(h) = gateway.cron_handle().await {
                    let result = h.reload().await;
                    cli.send(&result).await;
                } else {
                    cli.send("Cron service not running.").await;
                }
            } else {
                let jobs = gateway.cron_list_jobs().await;
                if jobs.is_empty() {
                    cli.send("No cron jobs.").await;
                } else {
                    let mut lines = vec![format!(
                        "Cron Jobs ({}):",
                        jobs.len()
                    )];
                    for j in &jobs {
                        let en = if j.enabled { "on" } else { "OFF" };
                        lines.push(format!(
                            "  {} ({}) [{en}] kind={} \
                             errors={} last={} next={}",
                            j.name,
                            j.id,
                            j.kind,
                            j.errors,
                            j.last_run,
                            j.next_run,
                        ));
                    }
                    cli.send(&lines.join("\n")).await;
                }
        }
            continue;
        }

        // /delivery — delivery queue stats / stop
        if msg.text.starts_with("/delivery") {
            let arg = msg
                .text
                .strip_prefix("/delivery")
                .unwrap_or("")
                .trim();
            if arg == "stop" {
                gateway.delivery().stop();
                cli.send("Delivery runner stopped.").await;
            } else if let Some(st) =
                gateway.delivery().stats().await
            {
                cli.send(&format!(
                    "Delivery Queue:\n\
                     \x20 attempted={}  succeeded={}  \
                     failed={}  pending={}",
                    st.total_attempted,
                    st.total_succeeded,
                    st.total_failed,
                    st.pending,
                ))
                .await;
            } else {
                cli.send("Delivery runner not available.").await;
            }
            continue;
        }

        if msg.text == "/discord" {
            if discord_started {
                cli.send("Discord gateway already running").await;
                continue;
            }
            match std::env::var("DISCORD_BOT_TOKEN") {
                Ok(token) if !token.is_empty() => {
                    let gw = gateway.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            crate::frontend::discord::start_gateway(
                                token, gw,
                            )
                            .await
                        {
                            log::error!(
                                "Discord gateway error: {e}"
                            );
                        }
                    });
                    discord_started = true;
                    cli.send("Discord gateway started").await;
                }
                _ => {
                    cli.send("DISCORD_BOT_TOKEN not set").await;
                }
            }
            continue;
        }

        if msg.text == "/gateway" {
            if ws_started {
                cli.send("WebSocket gateway already running").await;
                continue;
            }
            let gw = gateway.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    crate::frontend::gateway::start_ws(
                        gw,
                        "localhost",
                        8765,
                    )
                    .await
                {
                    log::error!("WebSocket gateway error: {e}");
                }
            });
            ws_started = true;
            cli.send(
                "WebSocket gateway started on ws://localhost:8765",
            )
            .await;
            continue;
        }

        // Everything else → dispatch through gateway
        // Session-level commands (/new, /save, /compact, /prompt, etc.)
        // are handled by Session::turn()
        match gateway
            .dispatch(msg, cli.clone(), agent_override.as_deref())
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
}