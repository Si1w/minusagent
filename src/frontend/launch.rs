use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::frontend::discord;
use crate::frontend::gateway::rpc::handle_rpc;
use crate::frontend::gateway::{Gateway, GatewayServices, ManagedService, ServiceControlResult};

pub(crate) fn start_websocket_service(
    services: &GatewayServices,
    gateway: &Arc<Gateway>,
) -> ServiceControlResult {
    let host = gateway.config().websocket_host().to_string();
    let port = gateway.config().websocket_port();
    let bind_host = host.clone();
    let service_gateway = Arc::clone(gateway);
    let started =
        services.spawn_frontend_task(ManagedService::Websocket, move |generation, shutdown| {
            tokio::spawn(async move {
                if let Err(error) =
                    start_ws(service_gateway.clone(), &bind_host, port, shutdown).await
                {
                    log::error!("WebSocket gateway error: {error}");
                    service_gateway.services().record_service_event(
                        ManagedService::Websocket,
                        format!("WebSocket gateway stopped after error: {error}"),
                    );
                }
                service_gateway
                    .services()
                    .finish_frontend_task(ManagedService::Websocket, generation);
            })
        });
    if !started {
        let result = ServiceControlResult::Unchanged("WebSocket gateway already running".into());
        services.record_service_outcome(ManagedService::Websocket, Some(true), &result);
        return result;
    }

    let result =
        ServiceControlResult::Changed(format!("WebSocket gateway started on ws://{host}:{port}"));
    services.record_service_outcome(ManagedService::Websocket, Some(true), &result);
    result
}

pub(crate) fn start_discord_service(
    services: &GatewayServices,
    gateway: &Arc<Gateway>,
) -> ServiceControlResult {
    let Some(token) = gateway.config().discord_token().map(ToOwned::to_owned) else {
        let result = ServiceControlResult::Unchanged("Discord bot token not configured".into());
        services.record_service_outcome(ManagedService::Discord, None, &result);
        return result;
    };

    let service_gateway = Arc::clone(gateway);
    let started =
        services.spawn_frontend_task(ManagedService::Discord, move |generation, shutdown| {
            tokio::spawn(async move {
                if let Err(error) =
                    discord::start_gateway(token, service_gateway.clone(), shutdown).await
                {
                    log::error!("Discord gateway error: {error}");
                    service_gateway.services().record_service_event(
                        ManagedService::Discord,
                        format!("Discord gateway stopped after error: {error}"),
                    );
                }
                service_gateway
                    .services()
                    .finish_frontend_task(ManagedService::Discord, generation);
            })
        });
    if !started {
        let result = ServiceControlResult::Unchanged("Discord gateway already running".into());
        services.record_service_outcome(ManagedService::Discord, Some(true), &result);
        return result;
    }

    let result = ServiceControlResult::Changed("Discord gateway started".into());
    services.record_service_outcome(ManagedService::Discord, Some(true), &result);
    result
}

/// Start the WebSocket frontend listener and drain connection tasks on shutdown.
///
/// # Errors
///
/// Returns an error if the TCP listener fails to bind or accept a socket.
pub(crate) async fn start_ws(
    gateway: Arc<Gateway>,
    host: &str,
    port: u16,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{host}:{port}")).await?;
    let mut connections = JoinSet::new();
    log::info!("Gateway started ws://{host}:{port}");

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            result = listener.accept() => {
                let (stream, addr) = result?;
                log::debug!("Gateway: new connection from {addr}");
                connections.spawn(handle_ws_connection(
                    stream,
                    addr,
                    gateway.clone(),
                    shutdown.clone(),
                ));
            }
            task = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = task {
                    log::debug!("Gateway: WebSocket connection task ended: {error}");
                }
            }
        }
    }

    connections.abort_all();
    while connections.join_next().await.is_some() {}
    log::info!("Gateway stopped ws://{host}:{port}");
    Ok(())
}

async fn handle_ws_connection(
    stream: TcpStream,
    addr: SocketAddr,
    gateway: Arc<Gateway>,
    mut shutdown: watch::Receiver<bool>,
) {
    let ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(error) => {
            log::error!("Gateway: WebSocket handshake failed: {error}");
            return;
        }
    };

    let (write, mut read) = ws.split();
    let write = Arc::new(Mutex::new(write));

    loop {
        let ws_msg = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            ws_msg = read.next() => ws_msg,
        };
        let Some(Ok(ws_msg)) = ws_msg else {
            break;
        };
        let text = match ws_msg {
            WsMessage::Text(text) => text,
            WsMessage::Close(_) => break,
            _ => continue,
        };

        let resp = handle_rpc(&gateway, &text).await;
        if let Some(resp) = resp {
            let msg = WsMessage::Text(resp.to_string().into());
            if write.lock().await.send(msg).await.is_err() {
                break;
            }
        }
    }

    log::debug!("Gateway: connection from {addr} closed");
}
