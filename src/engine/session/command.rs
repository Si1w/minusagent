use std::fmt::Write;
use std::sync::Arc;

use anyhow::Result;

use crate::engine::node::Node;
use crate::engine::session::Session;
use crate::engine::store::Message;
use crate::frontend::Channel;
use crate::intelligence::memory::MemoryWrite;
use crate::team::TeammateStatus;

use super::compact;

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

## Team

- `/team` — show team roster
- `/inbox` — check lead inbox
- `/tasks` — show task board
- `/worktrees` — list worktrees
- `/events` — worktree event log

## Resilience

- `/profiles` — show API key profiles
- `/lanes` — show lane stats

## Scheduler

- `/heartbeat` — heartbeat status
- `/trigger` — manual heartbeat

## Misc

- `/help` — show this help
";

pub(super) async fn handle_command(
    session: &mut Session,
    input: &str,
    channel: &Arc<dyn Channel>,
) -> Result<()> {
    let (cmd, arg) = split_command(input);

    if handle_session_command(session, cmd, arg, channel).await? {
        return Ok(());
    }
    if handle_runtime_command(session, cmd, arg, channel).await? {
        return Ok(());
    }
    if handle_workspace_command(session, cmd, channel).await? {
        return Ok(());
    }
    if handle_intelligence_command(session, cmd, arg, channel).await? {
        return Ok(());
    }

    handle_skill_or_unknown(session, cmd, arg, channel).await
}

async fn handle_session_command(
    session: &mut Session,
    cmd: &str,
    arg: &str,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    match cmd {
        "/help" => channel.send(HELP_TEXT).await,
        "/new" => {
            let id = session.sessions.create(arg)?;
            session.store.context.history.clear();
            channel.send(&format!("Created session: {id}")).await;
        }
        "/save" => {
            session.sessions.save(&session.store.context.history)?;
            let id = session.sessions.current_id().unwrap_or("none");
            channel.send(&format!("Saved session: {id}")).await;
        }
        "/load" => {
            if arg.is_empty() {
                channel.send("Usage: /load <session_id>").await;
                return Ok(true);
            }
            let history = session.sessions.load(arg)?;
            let id = session.sessions.current_id().unwrap_or("none");
            channel
                .send(&format!(
                    "Loaded session: {id} ({} messages)",
                    history.len()
                ))
                .await;
            session.store.context.history = history;
        }
        "/list" => {
            let sessions = session.sessions.list();
            if sessions.is_empty() {
                channel.send("No sessions found.").await;
            } else {
                channel.send(&format_sessions(session, &sessions)).await;
            }
        }
        "/compact" => {
            if session.store.context.history.len() <= 2 {
                channel.send("Too few messages to compact.").await;
            } else {
                let (before, after) = compact::compact_now(session).await?;
                channel
                    .send(&format!("Compacted: {before} -> {after} messages"))
                    .await;
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_runtime_command(
    session: &mut Session,
    cmd: &str,
    arg: &str,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    match cmd {
        "/heartbeat" => {
            if let Some(handle) = &session.heartbeat {
                if arg == "stop" {
                    handle.stop();
                    channel.send("Heartbeat stopped.").await;
                } else if let Some(status) = handle.status().await {
                    channel
                        .send(&format!(
                            "Heartbeat:\n\
                             \x20 enabled={}  running={}  should_run={}\n\
                             \x20 reason: {}\n\
                             \x20 last_run={}  next_in={}  interval={}s\n\
                             \x20 active_hours={}:00-{}:00  outputs={}",
                            status.enabled,
                            status.running,
                            status.should_run,
                            status.reason,
                            status.last_run,
                            status.next_in,
                            status.interval_secs,
                            status.active_hours.0,
                            status.active_hours.1,
                            status.queue_size,
                        ))
                        .await;
                }
            } else {
                channel.send("No heartbeat (HEARTBEAT.md not found)").await;
            }
        }
        "/trigger" => {
            if let Some(handle) = &session.heartbeat {
                channel.send(&handle.trigger().await).await;
            } else {
                channel.send("No heartbeat (HEARTBEAT.md not found)").await;
            }
        }
        "/profiles" => {
            let lines = session.resilience.profile_status();
            channel
                .send(&format!(
                    "Profiles ({}):\n{}",
                    lines.len(),
                    lines.join("\n")
                ))
                .await;
        }
        "/lanes" => {
            let stats = session.lane_lock.all_stats().await;
            if stats.is_empty() {
                channel.send("No lanes.").await;
            } else {
                let mut output = String::from("Lanes:\n");
                for stat in &stats {
                    let _ = writeln!(output, "  {:<14} active={}", stat.name, stat.active);
                }
                channel.send(&output).await;
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_workspace_command(
    session: &mut Session,
    cmd: &str,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    match cmd {
        "/team" => {
            if let Some(team) = &session.store.state.team {
                let members = team.list();
                if members.is_empty() {
                    channel.send("No teammates.").await;
                } else {
                    channel.send(&format_team(team, &members)).await;
                }
            } else {
                channel.send("Team not available (set WORKSPACE_DIR)").await;
            }
        }
        "/tasks" => {
            if let Some(tasks) = &session.store.state.tasks {
                match tasks.list_formatted() {
                    Ok(output) => channel.send(&output).await,
                    Err(error) => channel.send(&format!("Error: {error}")).await,
                }
            } else {
                channel.send("Task system not available").await;
            }
        }
        "/worktrees" => {
            if let Some(worktrees) = &session.store.state.worktrees {
                channel.send(&worktrees.list_formatted()).await;
            } else {
                channel.send("Worktree system not available").await;
            }
        }
        "/events" => {
            if let Some(worktrees) = &session.store.state.worktrees {
                channel.send(&worktrees.events()).await;
            } else {
                channel.send("Worktree system not available").await;
            }
        }
        "/inbox" => {
            if let Some(team) = &session.store.state.team {
                channel.send(&team.read_inbox("lead")).await;
            } else {
                channel.send("Team not available (set WORKSPACE_DIR)").await;
            }
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_intelligence_command(
    session: &mut Session,
    cmd: &str,
    arg: &str,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    match cmd {
        "/prompt" => channel.send(&current_prompt(session)).await,
        "/remember" => remember(session, arg, channel).await?,
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_skill_or_unknown(
    session: &mut Session,
    cmd: &str,
    arg: &str,
    channel: &Arc<dyn Channel>,
) -> Result<()> {
    let skill_name = cmd.strip_prefix('/').unwrap_or(cmd);
    let skill_body = session
        .store
        .state
        .intelligence
        .as_ref()
        .and_then(|intelligence| intelligence.find_skill(skill_name))
        .and_then(crate::intelligence::skills::Skill::load_body);

    if let Some(body) = skill_body {
        let content = if arg.is_empty() {
            format!("[Skill: {cmd}]\n\n{body}")
        } else {
            format!("[Skill: {cmd}]\n\n{body}\n\nUser input: {arg}")
        };
        session.store.context.history.push(Message {
            role: crate::engine::store::Role::User,
            content: Some(content),
            tool_calls: None,
            tool_call_id: None,
        });
        let interrupted = Some(session.interrupted.clone());
        session
            .resilience
            .run(
                &mut session.store,
                channel,
                &session.http,
                interrupted.as_ref(),
            )
            .await?;
        return Ok(());
    }

    channel.send(&format!("Unknown command: {cmd}")).await;
    Ok(())
}

async fn remember(session: &mut Session, arg: &str, channel: &Arc<dyn Channel>) -> Result<()> {
    let parts = arg.splitn(2, ' ').collect::<Vec<_>>();
    if parts.len() < 2 || parts[0].is_empty() {
        channel.send("Usage: /remember <name> <content>").await;
        return Ok(());
    }

    let name = parts[0];
    let content = parts[1];
    let memory_dir = session
        .store
        .state
        .intelligence
        .as_ref()
        .map(|intelligence| intelligence.memory.dir().to_path_buf());

    if let Some(dir) = memory_dir {
        let node = MemoryWrite {
            content: content.to_string(),
            name: name.to_string(),
            memory_dir: dir,
            http: session.http.clone(),
        };
        node.run(&mut session.store).await?;
        channel.send(&format!("Memory saved: {name}")).await;
    } else {
        channel
            .send("Intelligence not configured (set WORKSPACE_DIR)")
            .await;
    }

    Ok(())
}

fn split_command(input: &str) -> (&str, &str) {
    let parts = input.splitn(2, ' ').collect::<Vec<_>>();
    let cmd = parts[0];
    let arg = parts.get(1).copied().unwrap_or("").trim();
    (cmd, arg)
}

fn current_prompt(session: &Session) -> String {
    session.store.state.intelligence.as_ref().map_or_else(
        || session.store.context.system_prompt.clone(),
        crate::intelligence::Intelligence::build_prompt,
    )
}

fn format_sessions(session: &Session, sessions: &[(String, super::store::SessionMeta)]) -> String {
    let mut output = String::from("Sessions:\n");
    for (id, meta) in sessions {
        let current = if session.sessions.current_id() == Some(id.as_str()) {
            " <-- current"
        } else {
            ""
        };
        let label = if meta.label.is_empty() {
            String::new()
        } else {
            format!(" ({})", meta.label)
        };
        let _ = writeln!(
            output,
            "  {id}{label}  msgs={}  last={}{current}",
            meta.message_count,
            meta.last_active.get(..19).unwrap_or(&meta.last_active),
        );
    }
    output
}

fn format_team(
    team: &crate::team::TeammateManager,
    members: &[crate::team::manager::TeammateEntry],
) -> String {
    let mut output = String::from("Team:\n");
    for member in members {
        let status = match member.status {
            TeammateStatus::Working => "working",
            TeammateStatus::Idle => "idle",
            TeammateStatus::Shutdown => "shutdown",
        };
        let _ = writeln!(
            output,
            "  {} [{}] role={}",
            member.name, status, member.role
        );
    }

    let requests = team.list_requests();
    if !requests.is_empty() {
        output.push_str(&requests);
    }
    output
}
