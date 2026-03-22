use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{Duration, interval};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::frontend::utils::chunk_text;
use crate::frontend::{Channel, UserMessage};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const API_BASE: &str = "https://discord.com/api/v10";
const MAX_MSG_LEN: usize = 2000;

/// Shared map for pending bash confirmations, keyed by Discord channel ID
pub type PendingConfirms =
    Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>;

// Gateway payload

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    d: Option<serde_json::Value>,
    s: Option<u64>,
    t: Option<String>,
}

// Discord message event

#[derive(Debug, Deserialize)]
struct MessageCreate {
    content: String,
    channel_id: String,
    author: Author,
}

#[derive(Debug, Deserialize)]
struct Author {
    id: String,
    bot: Option<bool>,
}

/// Discord reply context
///
/// Implements Channel for sending responses back to a specific
/// Discord channel. Buffers streaming chunks and flushes on `send`.
pub struct DiscordReply {
    channel_id: String,
    token: String,
    http: reqwest::Client,
    buffer: Mutex<String>,
    pending_confirms: PendingConfirms,
}

impl DiscordReply {
    fn new(
        channel_id: String,
        token: String,
        http: reqwest::Client,
        pending_confirms: PendingConfirms,
    ) -> Self {
        Self {
            channel_id,
            token,
            http,
            buffer: Mutex::new(String::new()),
            pending_confirms,
        }
    }

    async fn send_message(&self, text: &str) -> Result<()> {
        for chunk in chunk_text(text, MAX_MSG_LEN) {
            self.http
                .post(format!(
                    "{API_BASE}/channels/{}/messages",
                    self.channel_id
                ))
                .header("Authorization", format!("Bot {}", self.token))
                .json(&json!({ "content": chunk }))
                .send()
                .await?
                .error_for_status()?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Channel for DiscordReply {
    async fn receive(&self) -> Option<UserMessage> {
        None
    }

    async fn send(&self, text: &str) {
        let mut buf = self.buffer.lock().await;
        let full = if buf.is_empty() {
            text.to_string()
        } else {
            let combined = std::mem::take(&mut *buf);
            if text.is_empty() {
                combined
            } else {
                format!("{combined}\n\n{text}")
            }
        };
        drop(buf);

        if !full.is_empty() {
            if let Err(e) = self.send_message(&full).await {
                log::error!("Discord: failed to send message: {e}");
            }
        }
    }

    async fn confirm(&self, command: &str) -> bool {
        let _ = self
            .send_message(&format!("Execute: `{command}` ? (y/n)"))
            .await;

        let (tx, rx) = oneshot::channel();
        self.pending_confirms
            .lock()
            .await
            .insert(self.channel_id.clone(), tx);

        match rx.await {
            Ok(reply) => reply.trim().eq_ignore_ascii_case("y"),
            Err(_) => false,
        }
    }

    async fn on_stream_chunk(&self, chunk: &str) {
        self.buffer.lock().await.push_str(chunk);
    }
}

/// Start the Discord gateway connection and route messages to the main loop
///
/// # Arguments
///
/// * `token` - Discord bot token
/// * `tx` - Sender to route messages to the main loop
pub async fn start_gateway(
    token: String,
    tx: mpsc::Sender<crate::RoutedMessage>,
) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(GATEWAY_URL).await?;
    let (write, mut read) = ws.split();
    let write = Arc::new(Mutex::new(write));

    let http = reqwest::Client::new();
    let pending_confirms: PendingConfirms =
        Arc::new(Mutex::new(HashMap::new()));
    let mut seq: Option<u64> = None;
    let mut heartbeat_interval_ms: u64 = 41250;

    // Read Hello to get heartbeat interval
    if let Some(Ok(WsMessage::Text(text))) = read.next().await {
        if let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) {
            if payload.op == 10 {
                if let Some(d) = &payload.d {
                    heartbeat_interval_ms =
                        d["heartbeat_interval"].as_u64().unwrap_or(41250);
                }
            }
        }
    }

    // Send Identify
    let identify = json!({
        "op": 2,
        "d": {
            "token": token,
            "intents": 512 | 32768,
            "properties": {
                "os": "linux",
                "browser": "minusagent",
                "device": "minusagent",
            }
        }
    });
    write
        .lock()
        .await
        .send(WsMessage::Text(identify.to_string().into()))
        .await?;

    // Wait for READY event
    log::info!("Discord: connecting...");
    while let Some(Ok(WsMessage::Text(text))) = read.next().await {
        if let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) {
            if let Some(s) = payload.s {
                seq = Some(s);
            }
            if payload.op == 0 && payload.t.as_deref() == Some("READY") {
                log::info!("Discord: connected and ready");
                break;
            }
        }
    }

    // Heartbeat task
    let hb_write = write.clone();
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(heartbeat_interval_ms));
        loop {
            ticker.tick().await;
            let payload = json!({ "op": 1, "d": null });
            let msg = WsMessage::Text(payload.to_string().into());
            if hb_write.lock().await.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Message loop
    let token_clone = token.clone();
    while let Some(Ok(ws_msg)) = read.next().await {
        let text = match ws_msg {
            WsMessage::Text(t) => t,
            _ => continue,
        };

        let payload: GatewayPayload = match serde_json::from_str(&text) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(s) = payload.s {
            seq = Some(s);
        }

        // Respond to heartbeat request
        if payload.op == 1 {
            let hb = json!({ "op": 1, "d": seq });
            let msg = WsMessage::Text(hb.to_string().into());
            let _ = write.lock().await.send(msg).await;
            continue;
        }

        // Dispatch events
        if payload.op != 0 {
            continue;
        }

        if payload.t.as_deref() != Some("MESSAGE_CREATE") {
            continue;
        }

        let d = match payload.d {
            Some(d) => d,
            None => continue,
        };

        let msg: MessageCreate = match serde_json::from_value(d) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if msg.author.bot.unwrap_or(false) || msg.content.is_empty() {
            continue;
        }

        log::debug!(
            "Discord: message from {}: {}",
            msg.author.id,
            &msg.content[..msg.content.len().min(80)]
        );

        // Check if this is a reply to a pending confirmation
        if let Some(confirm_tx) =
            pending_confirms.lock().await.remove(&msg.channel_id)
        {
            let _ = confirm_tx.send(msg.content);
            continue;
        }

        let reply = Arc::new(DiscordReply::new(
            msg.channel_id,
            token_clone.clone(),
            http.clone(),
            pending_confirms.clone(),
        ));

        let _ = tx
            .send(crate::RoutedMessage {
                text: msg.content,
                session_key: format!("discord:{}", msg.author.id),
                channel: reply,
                done: None,
            })
            .await;
    }

    Ok(())
}
