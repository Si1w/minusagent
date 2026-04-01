use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Duration, Instant, interval_at};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config::tuning;
use crate::frontend::gateway::Gateway;
use crate::frontend::utils::chunk_text;
use crate::frontend::{Channel, UserMessage};
use crate::routing::delivery::DeliverySink;

const GATEWAY_URL: &str =
    "wss://gateway.discord.gg/?v=10&encoding=json";
const API_BASE: &str = "https://discord.com/api/v10";
const MAX_MSG_LEN: usize = 2000;

/// Shared map for pending bash confirmations, keyed by Discord channel ID
pub type PendingConfirms =
    Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>;

#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u8,
    d: Option<Value>,
    s: Option<u64>,
    t: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MessageCreate {
    content: String,
    channel_id: String,
    guild_id: Option<String>,
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
        post_discord_message(&self.http, &self.token, &self.channel_id, text).await
    }
}

#[async_trait::async_trait]
impl Channel for DiscordReply {
    async fn receive(&self) -> Option<UserMessage> {
        None
    }

    async fn send(&self, text: &str) {
        if !text.is_empty() {
            if let Err(e) = self.send_message(text).await {
                log::error!(
                    "Discord: failed to send message: {e}"
                );
            }
        }
    }

    async fn confirm(&self, command: &str) -> bool {
        let _ = self
            .send_message(&format!(
                "Execute: `{command}` ? (y/n)"
            ))
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

    async fn flush(&self) {
        let buf = std::mem::take(&mut *self.buffer.lock().await);
        if !buf.is_empty() {
            if let Err(e) = self.send_message(&buf).await {
                log::error!(
                    "Discord: failed to flush buffer: {e}"
                );
            }
        }
    }
}

/// Delivery sink for sending messages to Discord channels via REST API
pub struct DiscordSink {
    token: String,
    http: reqwest::Client,
}

impl DiscordSink {
    pub fn new(token: String, http: reqwest::Client) -> Self {
        Self { token, http }
    }
}

#[async_trait::async_trait]
impl DeliverySink for DiscordSink {
    /// Deliver a message to a Discord channel
    ///
    /// `to` is a Discord channel ID.
    async fn deliver(
        &self,
        to: &str,
        text: &str,
    ) -> std::result::Result<(), String> {
        if to.is_empty() {
            return Err("Discord sink: empty channel_id".into());
        }
        post_discord_message(&self.http, &self.token, to, text)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Send chunked messages to a Discord channel via REST API
async fn post_discord_message(
    http: &reqwest::Client,
    token: &str,
    channel_id: &str,
    text: &str,
) -> Result<()> {
    for chunk in chunk_text(text, MAX_MSG_LEN) {
        http.post(format!("{API_BASE}/channels/{channel_id}/messages"))
            .header("Authorization", format!("Bot {token}"))
            .json(&json!({ "content": chunk }))
            .send()
            .await?
            .error_for_status()?;
    }
    Ok(())
}

// --- Gateway protocol helpers ---

/// Build a heartbeat payload from the current sequence number
///
/// Sends `null` as `d` if no sequence has been received yet (seq < 0).
fn heartbeat_payload(seq: i64) -> Value {
    if seq < 0 {
        json!({ "op": 1, "d": null })
    } else {
        json!({ "op": 1, "d": seq })
    }
}

/// Build an Identify payload
fn identify_payload(token: &str) -> Value {
    json!({
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
    })
}

/// Build a Resume payload
fn resume_payload(
    token: &str,
    session_id: &str,
    seq: i64,
) -> Value {
    let seq_val = if seq < 0 { Value::Null } else { json!(seq) };
    json!({
        "op": 6,
        "d": {
            "token": token,
            "session_id": session_id,
            "seq": seq_val
        }
    })
}

/// Compute jitter delay for the first heartbeat
///
/// Returns a value in `[0, interval_ms)` derived from the current
/// system time. Not cryptographically random, but sufficient for
/// Discord's jitter requirement.
fn jitter_millis(interval_ms: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (interval_ms as u128 * nanos as u128 / 1_000_000_000) as u64
}

// --- Gateway connection ---

/// Start the Discord gateway with automatic reconnection
///
/// Maintains a persistent connection to Discord's Gateway WebSocket.
/// Automatically resumes the session on disconnect when possible,
/// falling back to a fresh Identify when the session expires.
///
/// # Arguments
///
/// * `token` - Discord bot token
/// * `gateway` - Shared gateway for dispatch
///
/// # Errors
///
/// Returns error only on fatal Discord close codes (4004, 4010, 4011,
/// 4013, 4014). All other disconnects trigger automatic reconnection.
pub async fn start_gateway(
    token: String,
    gateway: Arc<Gateway>,
) -> Result<()> {
    let http = reqwest::Client::new();

    // Register Discord sink for outbound delivery
    {
        let s = gateway.state().read().await;
        s.router.outbound().register(
            "discord",
            Arc::new(DiscordSink::new(token.clone(), http.clone())),
        );
    }
    log::info!("Discord: registered outbound sink");

    let pending_confirms: PendingConfirms =
        Arc::new(Mutex::new(HashMap::new()));
    let seq = Arc::new(AtomicI64::new(-1));
    let mut bot_user_id = String::new();

    // Resume state persists across reconnections
    let mut resume_session_id: Option<String> = None;
    let mut resume_url: Option<String> = None;

    loop {
        let url =
            resume_url.as_deref().unwrap_or(GATEWAY_URL);
        log::info!("Discord: connecting to {url}");

        let ws =
            match tokio_tungstenite::connect_async(url).await {
                Ok((ws, _)) => ws,
                Err(e) => {
                    log::error!(
                        "Discord: connection failed: {e}"
                    );
                    tokio::time::sleep(Duration::from_secs(tuning().reconnect_delay_secs)).await;
                    continue;
                }
            };

        let (write, mut read) = ws.split();
        let write = Arc::new(Mutex::new(write));

        // Read Hello (op 10) to get heartbeat interval
        let mut heartbeat_interval_ms: u64 = 41250;
        if let Some(Ok(WsMessage::Text(text))) =
            read.next().await
        {
            if let Ok(p) =
                serde_json::from_str::<GatewayPayload>(&text)
            {
                if p.op == 10 {
                    if let Some(d) = &p.d {
                        heartbeat_interval_ms =
                            d["heartbeat_interval"]
                                .as_u64()
                                .unwrap_or(41250);
                    }
                }
            }
        }

        // Identify or Resume
        if let Some(sid) = &resume_session_id {
            let s = seq.load(Ordering::Relaxed);
            let payload = resume_payload(&token, sid, s);
            let msg =
                WsMessage::Text(payload.to_string().into());
            if write.lock().await.send(msg).await.is_err() {
                tokio::time::sleep(Duration::from_secs(tuning().reconnect_delay_secs)).await;
                continue;
            }
            log::info!(
                "Discord: resuming (session={sid}, seq={s})"
            );
        } else {
            let payload = identify_payload(&token);
            let msg =
                WsMessage::Text(payload.to_string().into());
            if write.lock().await.send(msg).await.is_err() {
                tokio::time::sleep(Duration::from_secs(tuning().reconnect_delay_secs)).await;
                continue;
            }

            // Wait for READY event
            log::info!("Discord: identifying...");
            let mut ready = false;
            while let Some(Ok(WsMessage::Text(text))) =
                read.next().await
            {
                let Ok(p) =
                    serde_json::from_str::<GatewayPayload>(
                        &text,
                    )
                else {
                    continue;
                };
                if let Some(s) = p.s {
                    seq.store(s as i64, Ordering::Relaxed);
                }
                if p.op == 9 {
                    log::warn!(
                        "Discord: invalid session \
                         during identify"
                    );
                    break;
                }
                if p.op == 0
                    && p.t.as_deref() == Some("READY")
                {
                    if let Some(d) = &p.d {
                        bot_user_id = d["user"]["id"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        resume_session_id = d["session_id"]
                            .as_str()
                            .map(String::from);
                        let rurl = d["resume_gateway_url"]
                            .as_str()
                            .unwrap_or("");
                        if !rurl.is_empty() {
                            resume_url = Some(format!(
                                "{rurl}?v=10&encoding=json"
                            ));
                        }
                    }
                    log::info!(
                        "Discord: ready \
                         (bot_id={bot_user_id})"
                    );
                    ready = true;
                    break;
                }
            }
            if !ready {
                resume_session_id = None;
                resume_url = None;
                seq.store(-1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(tuning().reconnect_delay_secs)).await;
                continue;
            }
        }

        // Heartbeat task with jitter and ACK tracking
        let ack = Arc::new(AtomicBool::new(true));
        let hb_write = write.clone();
        let hb_seq = seq.clone();
        let hb_ack = ack.clone();
        let jitter = jitter_millis(heartbeat_interval_ms);
        tokio::spawn(async move {
            let start =
                Instant::now() + Duration::from_millis(jitter);
            let mut ticker = interval_at(
                start,
                Duration::from_millis(heartbeat_interval_ms),
            );
            loop {
                ticker.tick().await;
                // Check ACK from previous heartbeat.
                // swap(false) returns the old value:
                //   true  → ACK received, send next heartbeat
                //   false → no ACK, connection is zombied
                if !hb_ack.swap(false, Ordering::Relaxed) {
                    log::warn!(
                        "Discord: no heartbeat ACK, \
                         closing connection"
                    );
                    let _ =
                        hb_write.lock().await.close().await;
                    break;
                }
                let s = hb_seq.load(Ordering::Relaxed);
                let payload = heartbeat_payload(s);
                let msg = WsMessage::Text(
                    payload.to_string().into(),
                );
                if hb_write
                    .lock()
                    .await
                    .send(msg)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        // Message loop
        while let Some(Ok(ws_msg)) = read.next().await {
            let text = match ws_msg {
                WsMessage::Text(t) => t,
                WsMessage::Close(frame) => {
                    if let Some(f) = frame {
                        let code: u16 = f.code.into();
                        log::warn!(
                            "Discord: close {code}: {}",
                            f.reason
                        );
                        if matches!(
                            code,
                            4004 | 4010 | 4011 | 4013 | 4014
                        ) {
                            return Err(anyhow::anyhow!(
                                "Discord: fatal close \
                                 code {code}"
                            ));
                        }
                    }
                    break;
                }
                _ => continue,
            };

            let payload: GatewayPayload =
                match serde_json::from_str(&text) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

            if let Some(s) = payload.s {
                seq.store(s as i64, Ordering::Relaxed);
            }

            match payload.op {
                // Heartbeat request from Discord
                1 => {
                    let s = seq.load(Ordering::Relaxed);
                    let hb = heartbeat_payload(s);
                    let msg = WsMessage::Text(
                        hb.to_string().into(),
                    );
                    let _ =
                        write.lock().await.send(msg).await;
                }
                // Reconnect requested
                7 => {
                    log::info!(
                        "Discord: reconnect requested"
                    );
                    break;
                }
                // Invalid Session
                9 => {
                    let resumable = payload
                        .d
                        .as_ref()
                        .and_then(|d| d.as_bool())
                        .unwrap_or(false);
                    if resumable {
                        log::info!(
                            "Discord: invalid session \
                             (resumable)"
                        );
                    } else {
                        log::info!(
                            "Discord: invalid session \
                             (not resumable)"
                        );
                        resume_session_id = None;
                        resume_url = None;
                        seq.store(-1, Ordering::Relaxed);
                    }
                    break;
                }
                // Heartbeat ACK
                11 => {
                    ack.store(true, Ordering::Relaxed);
                }
                // Dispatch events
                0 => {
                    if payload.t.as_deref()
                        == Some("RESUMED")
                    {
                        log::info!(
                            "Discord: resumed successfully"
                        );
                        continue;
                    }

                    if payload.t.as_deref()
                        != Some("MESSAGE_CREATE")
                    {
                        continue;
                    }

                    let d = match payload.d {
                        Some(d) => d,
                        None => continue,
                    };

                    let msg: MessageCreate =
                        match serde_json::from_value(d) {
                            Ok(m) => m,
                            Err(_) => continue,
                        };

                    if msg.author.bot.unwrap_or(false)
                        || msg.content.is_empty()
                    {
                        continue;
                    }

                    let preview: String = msg.content.chars().take(80).collect();
                    log::debug!(
                        "Discord: message from {}: {}",
                        msg.author.id,
                        preview
                    );

                    // Check pending confirmation
                    if let Some(confirm_tx) = pending_confirms
                        .lock()
                        .await
                        .remove(&msg.channel_id)
                    {
                        let _ =
                            confirm_tx.send(msg.content);
                        continue;
                    }

                    let reply = Arc::new(DiscordReply::new(
                        msg.channel_id,
                        token.clone(),
                        http.clone(),
                        pending_confirms.clone(),
                    ));

                    let user_msg = UserMessage {
                        text: msg.content,
                        sender_id: msg.author.id,
                        channel: "discord".into(),
                        account_id: bot_user_id.clone(),
                        guild_id: msg
                            .guild_id
                            .unwrap_or_default(),
                    };

                    if let Err(e) = gateway
                        .dispatch(user_msg, reply, None)
                        .await
                    {
                        log::error!(
                            "Discord dispatch error: {e}"
                        );
                    }
                }
                _ => {}
            }
        }

        log::info!(
            "Discord: disconnected, reconnecting..."
        );
        tokio::time::sleep(Duration::from_secs(tuning().reconnect_delay_secs)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jitter_within_range() {
        for interval in [1000, 41250, 60000] {
            let j = jitter_millis(interval);
            assert!(
                j < interval,
                "jitter {j} >= interval {interval}"
            );
        }
    }

    #[test]
    fn test_jitter_zero_interval() {
        assert_eq!(jitter_millis(0), 0);
    }

    #[test]
    fn test_heartbeat_no_seq() {
        let p = heartbeat_payload(-1);
        assert_eq!(p["op"], 1);
        assert!(p["d"].is_null());
    }

    #[test]
    fn test_heartbeat_with_seq() {
        let p = heartbeat_payload(42);
        assert_eq!(p["op"], 1);
        assert_eq!(p["d"], 42);
    }

    #[test]
    fn test_identify_structure() {
        let p = identify_payload("test-token");
        assert_eq!(p["op"], 2);
        assert_eq!(p["d"]["token"], "test-token");
        assert!(p["d"]["intents"].as_u64().unwrap() > 0);
        assert_eq!(
            p["d"]["properties"]["browser"],
            "minusagent"
        );
    }

    #[test]
    fn test_resume_structure() {
        let p = resume_payload("tok", "sess-1", 42);
        assert_eq!(p["op"], 6);
        assert_eq!(p["d"]["token"], "tok");
        assert_eq!(p["d"]["session_id"], "sess-1");
        assert_eq!(p["d"]["seq"], 42);
    }

    #[test]
    fn test_resume_no_seq() {
        let p = resume_payload("tok", "sess-1", -1);
        assert_eq!(p["op"], 6);
        assert!(p["d"]["seq"].is_null());
    }
}
