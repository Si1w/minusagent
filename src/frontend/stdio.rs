use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

use crate::frontend::{Channel, UserMessage};
use crate::frontend::gateway::Gateway;
use crate::routing::protocol::{
    ControlEvent, ControlMessage, ProtocolChannel, SessionControl,
};

/// Run the stdio transport: read `ControlMessage` from stdin, write `ControlEvent` to stdout
///
/// This enables external SDKs to drive the agent via structured JSON protocol
/// over stdin/stdout (one JSON object per line).
pub async fn run(gateway: Arc<Gateway>) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let (protocol_channel, mut events_rx) = ProtocolChannel::new();
    let protocol_channel = Arc::new(protocol_channel);

    // Event writer: drain events_rx → stdout
    let out = stdout.clone();
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            emit(&out, &event).await;
        }
    });

    let mut lines = stdin.lines();
    let mut session_key: Option<String> = None;
    let mut active_dispatch: Option<oneshot::Receiver<()>> = None;

    loop {
        // If a dispatch is in progress, select between stdin and completion
        if let Some(ref mut done) = active_dispatch {
            tokio::select! {
                line = lines.next_line() => {
                    match line {
                        Ok(Some(line)) => {
                            handle_mid_turn(
                                &line,
                                &gateway,
                                &protocol_channel,
                                session_key.as_deref(),
                                &stdout,
                            ).await;
                        }
                        _ => break,
                    }
                }
                _ = done => {
                    active_dispatch = None;
                    emit(&stdout, &ControlEvent::TurnComplete { text: None }).await;
                }
            }
            continue;
        }

        // Idle: read next message
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            _ => break,
        };

        let msg: ControlMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                emit(&stdout, &ControlEvent::Error {
                    code: -32700,
                    message: format!("Parse error: {e}"),
                })
                .await;
                continue;
            }
        };

        match msg {
            ControlMessage::Init {
                agent_id,
                model: _model,
                permission_mode,
            } => {
                // Dispatch an empty-ish message to create the session
                let um = UserMessage {
                    text: String::new(),
                    sender_id: "stdio".into(),
                    channel: "stdio".into(),
                    account_id: String::new(),
                    guild_id: String::new(),
                };

                let result = gateway
                    .dispatch(
                        um,
                        protocol_channel.clone() as Arc<dyn Channel>,
                        agent_id.as_deref(),
                    )
                    .await;

                match result {
                    Ok(res) => {
                        // Wait for empty turn to finish
                        let _ = res.done.await;
                        session_key = Some(res.session_key.clone());

                        // Apply permission mode if requested
                        if let Some(mode) = permission_mode {
                            let _ = gateway
                                .send_control(
                                    &res.session_key,
                                    SessionControl::SetPermissionMode { mode },
                                )
                                .await;
                        }

                        let model = {
                            let s = gateway.state().read().await;
                            s.router.shared_agents().effective_model(&res.agent_id)
                        };

                        emit(&stdout, &ControlEvent::SessionReady {
                            session_key: res.session_key,
                            agent_id: res.agent_id,
                            model,
                        })
                        .await;
                    }
                    Err(e) => {
                        emit(&stdout, &ControlEvent::Error {
                            code: -1,
                            message: e.to_string(),
                        })
                        .await;
                    }
                }
            }

            ControlMessage::UserMessage {
                text,
                channel,
                peer_id,
                account_id,
                guild_id,
            } => {
                let um = UserMessage {
                    text,
                    sender_id: peer_id.unwrap_or_else(|| "stdio".into()),
                    channel: channel.unwrap_or_else(|| "stdio".into()),
                    account_id: account_id.unwrap_or_default(),
                    guild_id: guild_id.unwrap_or_default(),
                };

                match gateway
                    .dispatch(
                        um,
                        protocol_channel.clone() as Arc<dyn Channel>,
                        None,
                    )
                    .await
                {
                    Ok(res) => {
                        session_key = Some(res.session_key);
                        active_dispatch = Some(res.done);
                    }
                    Err(e) => {
                        emit(&stdout, &ControlEvent::Error {
                            code: -1,
                            message: e.to_string(),
                        })
                        .await;
                    }
                }
            }

            ControlMessage::ToolResponse { request_id, allow } => {
                protocol_channel.resolve_tool(&request_id, allow).await;
            }

            ControlMessage::Interrupt => {
                if let Some(sk) = &session_key {
                    match gateway.interrupt(sk).await {
                        Ok(()) => emit(&stdout, &ControlEvent::TurnComplete {
                            text: Some("interrupted".into()),
                        }).await,
                        Err(e) => emit(&stdout, &ControlEvent::Error {
                            code: -1,
                            message: e.to_string(),
                        }).await,
                    }
                }
            }

            // Session-level control messages
            ControlMessage::ContextUsage => {
                let ctrl = SessionControl::ContextUsage;
                dispatch_session_control(&gateway, session_key.as_deref(), ctrl, &stdout).await;
            }
            ControlMessage::Rewind { count } => {
                let ctrl = SessionControl::Rewind { count };
                dispatch_session_control(&gateway, session_key.as_deref(), ctrl, &stdout).await;
            }
            ControlMessage::ModelSwitch { model } => {
                let ctrl = SessionControl::ModelSwitch { model };
                dispatch_session_control(&gateway, session_key.as_deref(), ctrl, &stdout).await;
            }
            ControlMessage::SetPermissionMode { mode } => {
                let ctrl = SessionControl::SetPermissionMode { mode };
                dispatch_session_control(&gateway, session_key.as_deref(), ctrl, &stdout).await;
            }
        }
    }

    Ok(())
}

/// Handle messages that arrive during an active agent turn
async fn handle_mid_turn(
    line: &str,
    gateway: &Arc<Gateway>,
    protocol_channel: &Arc<ProtocolChannel>,
    session_key: Option<&str>,
    stdout: &Arc<Mutex<tokio::io::Stdout>>,
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
            emit(stdout, &ControlEvent::Error {
                code: -32600,
                message: "Agent is busy. Only tool_response and interrupt accepted.".into(),
            })
            .await;
        }
    }
}

/// Route a session-level control message and emit the response
async fn dispatch_session_control(
    gateway: &Arc<Gateway>,
    session_key: Option<&str>,
    ctrl: SessionControl,
    stdout: &Arc<Mutex<tokio::io::Stdout>>,
) {
    let sk = match session_key {
        Some(sk) => sk,
        None => {
            emit(stdout, &ControlEvent::Error {
                code: -1,
                message: "No active session. Send init first.".into(),
            })
            .await;
            return;
        }
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

/// Write a ControlEvent as JSON line to stdout
async fn emit(stdout: &Arc<Mutex<tokio::io::Stdout>>, event: &ControlEvent) {
    if let Ok(line) = serde_json::to_string(event) {
        let mut out = stdout.lock().await;
        let _ = out.write_all(line.as_bytes()).await;
        let _ = out.write_all(b"\n").await;
        let _ = out.flush().await;
    }
}
