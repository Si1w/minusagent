use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use crate::frontend::gateway::Gateway;
use crate::frontend::{Channel, UserMessage};
use crate::routing::protocol::{ControlEvent, ControlMessage, ProtocolChannel, SessionControl};

type SharedStdout = Arc<Mutex<tokio::io::Stdout>>;

/// Run the stdio transport: read `ControlMessage` from stdin, write `ControlEvent` to stdout
///
/// This enables external SDKs to drive the agent via structured JSON protocol
/// over stdin/stdout (one JSON object per line).
///
/// # Errors
///
/// Returns an error if reading from stdin fails.
pub async fn run(gateway: Arc<Gateway>) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let (protocol_channel, mut events_rx) = ProtocolChannel::new();
    let protocol_channel = Arc::new(protocol_channel);
    spawn_event_writer(stdout.clone(), &mut events_rx);

    let mut lines = stdin.lines();
    let mut state = StdioState::default();

    loop {
        if state.active_dispatch.is_some() {
            if !poll_active_dispatch(&mut state, &mut lines, &gateway, &protocol_channel, &stdout)
                .await?
            {
                break;
            }
            continue;
        }

        let Some(line) = read_next_line(&mut lines).await? else {
            break;
        };
        let Some(msg) = parse_control_message(&line, &stdout).await else {
            continue;
        };

        handle_idle_message(msg, &gateway, &protocol_channel, &stdout, &mut state).await;
    }

    Ok(())
}

#[derive(Default)]
struct StdioState {
    session_key: Option<String>,
    active_dispatch: Option<oneshot::Receiver<()>>,
}

fn spawn_event_writer(
    stdout: SharedStdout,
    events_rx: &mut tokio::sync::mpsc::Receiver<ControlEvent>,
) {
    let mut events = std::mem::replace(events_rx, tokio::sync::mpsc::channel(1).1);
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            emit(&stdout, &event).await;
        }
    });
}

async fn read_next_line(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<Option<String>> {
    lines.next_line().await.map_err(Into::into)
}

async fn parse_control_message(line: &str, stdout: &SharedStdout) -> Option<ControlMessage> {
    match serde_json::from_str(line) {
        Ok(message) => Some(message),
        Err(e) => {
            emit(
                stdout,
                &ControlEvent::Error {
                    code: -32700,
                    message: format!("Parse error: {e}"),
                },
            )
            .await;
            None
        }
    }
}

async fn poll_active_dispatch(
    state: &mut StdioState,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    stdout: &SharedStdout,
) -> Result<bool> {
    let Some(mut done) = state.active_dispatch.take() else {
        return Ok(true);
    };

    tokio::select! {
        line = lines.next_line() => {
            match line? {
                Some(line) => {
                    handle_mid_turn(
                        &line,
                        gateway,
                        protocol_channel,
                        state.session_key.as_deref(),
                        stdout,
                    ).await;
                    state.active_dispatch = Some(done);
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        _ = &mut done => {
            emit(stdout, &ControlEvent::TurnComplete { text: None }).await;
            Ok(true)
        }
    }
}

async fn handle_idle_message(
    msg: ControlMessage,
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    stdout: &SharedStdout,
    state: &mut StdioState,
) {
    match msg {
        ControlMessage::Init {
            agent_id,
            model: _,
            permission_mode,
        } => {
            handle_init(
                gateway,
                protocol_channel,
                stdout,
                state,
                agent_id,
                permission_mode,
            )
            .await;
        }
        ControlMessage::UserMessage {
            text,
            channel,
            peer_id,
            account_id,
            guild_id,
        } => {
            let user_msg = UserMessage {
                text,
                sender_id: peer_id.unwrap_or_else(|| "stdio".into()),
                channel: channel.unwrap_or_else(|| "stdio".into()),
                account_id: account_id.unwrap_or_default(),
                guild_id: guild_id.unwrap_or_default(),
            };
            handle_user_message(gateway, protocol_channel, stdout, state, user_msg).await;
        }
        ControlMessage::ToolResponse { request_id, allow } => {
            protocol_channel.resolve_tool(&request_id, allow).await;
        }
        ControlMessage::Interrupt => {
            handle_interrupt(gateway, stdout, state.session_key.as_deref()).await;
        }
        ControlMessage::ContextUsage => {
            dispatch_session_control(
                gateway,
                state.session_key.as_deref(),
                SessionControl::ContextUsage,
                stdout,
            )
            .await;
        }
        ControlMessage::Rewind { count } => {
            dispatch_session_control(
                gateway,
                state.session_key.as_deref(),
                SessionControl::Rewind { count },
                stdout,
            )
            .await;
        }
        ControlMessage::ModelSwitch { model } => {
            dispatch_session_control(
                gateway,
                state.session_key.as_deref(),
                SessionControl::ModelSwitch { model },
                stdout,
            )
            .await;
        }
        ControlMessage::SetPermissionMode { mode } => {
            dispatch_session_control(
                gateway,
                state.session_key.as_deref(),
                SessionControl::SetPermissionMode { mode },
                stdout,
            )
            .await;
        }
    }
}

async fn handle_init(
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    stdout: &SharedStdout,
    state: &mut StdioState,
    agent_id: Option<String>,
    permission_mode: Option<crate::routing::protocol::PermissionMode>,
) {
    let user_msg = UserMessage {
        text: String::new(),
        sender_id: "stdio".into(),
        channel: "stdio".into(),
        account_id: String::new(),
        guild_id: String::new(),
    };

    match gateway
        .dispatch(
            user_msg,
            protocol_channel.clone() as Arc<dyn Channel>,
            agent_id.as_deref(),
        )
        .await
    {
        Ok(result) => {
            let _ = result.done.await;
            state.session_key = Some(result.session_key.clone());

            if let Some(mode) = permission_mode {
                let _ = gateway
                    .send_control(
                        &result.session_key,
                        SessionControl::SetPermissionMode { mode },
                    )
                    .await;
            }

            let model = {
                let s = gateway.state().read().await;
                s.router.shared_agents().effective_model(&result.agent_id)
            };
            emit(
                stdout,
                &ControlEvent::SessionReady {
                    session_key: result.session_key,
                    agent_id: result.agent_id,
                    model,
                },
            )
            .await;
        }
        Err(e) => emit_error(stdout, e.to_string()).await,
    }
}

async fn handle_user_message(
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    stdout: &SharedStdout,
    state: &mut StdioState,
    user_msg: UserMessage,
) {
    match gateway
        .dispatch(user_msg, protocol_channel.clone() as Arc<dyn Channel>, None)
        .await
    {
        Ok(result) => {
            state.session_key = Some(result.session_key);
            state.active_dispatch = Some(result.done);
        }
        Err(e) => emit_error(stdout, e.to_string()).await,
    }
}

async fn handle_interrupt(
    gateway: &Arc<Gateway>,
    stdout: &SharedStdout,
    session_key: Option<&str>,
) {
    let Some(session_key) = session_key else {
        return;
    };

    match gateway.interrupt(session_key).await {
        Ok(()) => {
            emit(
                stdout,
                &ControlEvent::TurnComplete {
                    text: Some("interrupted".into()),
                },
            )
            .await;
        }
        Err(e) => {
            emit(
                stdout,
                &ControlEvent::Error {
                    code: -1,
                    message: e.to_string(),
                },
            )
            .await;
        }
    }
}

async fn emit_error(stdout: &SharedStdout, message: String) {
    emit(stdout, &ControlEvent::Error { code: -1, message }).await;
}

/// Handle messages that arrive during an active agent turn
async fn handle_mid_turn(
    line: &str,
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    session_key: Option<&str>,
    stdout: &SharedStdout,
) {
    let msg: ControlMessage = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(_) => return,
    };

    match msg {
        // Tool response resolves pending permission request
        ControlMessage::ToolResponse { request_id, allow } => {
            protocol_channel.resolve_tool(&request_id, allow).await;
        }
        ControlMessage::Interrupt => {
            if let Some(sk) = session_key {
                let _ = gateway.interrupt(sk).await;
            }
        }
        // Other messages during active turn → error
        _ => {
            emit(
                stdout,
                &ControlEvent::Error {
                    code: -32600,
                    message: "Agent is busy. Only tool_response and interrupt accepted.".into(),
                },
            )
            .await;
        }
    }
}

/// Route a session-level control message and emit the response
async fn dispatch_session_control(
    gateway: &Arc<Gateway>,
    session_key: Option<&str>,
    ctrl: SessionControl,
    stdout: &SharedStdout,
) {
    let Some(sk) = session_key else {
        emit(
            stdout,
            &ControlEvent::Error {
                code: -1,
                message: "No active session. Send init first.".into(),
            },
        )
        .await;
        return;
    };

    let event = gateway
        .send_control(sk, ctrl)
        .await
        .unwrap_or_else(|e| ControlEvent::Error {
            code: -1,
            message: e.to_string(),
        });

    emit(stdout, &event).await;
}

/// Write a `ControlEvent` as JSON line to stdout.
async fn emit(stdout: &SharedStdout, event: &ControlEvent) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut out = stdout.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.write_all(b"\n").await;
        let _ = out.flush().await;
    }
}
