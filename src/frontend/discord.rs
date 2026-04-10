use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use anyhow::Result;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::time::{Duration, Instant, interval_at};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::config::tuning;
use crate::frontend::gateway::Gateway;
use crate::frontend::utils::chunk_text;
use crate::frontend::{Channel, UserMessage};
use crate::routing::delivery::DeliverySink;
use crate::routing::protocol::{ControlEvent, SessionControl};
use crate::routing::router::Router;

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const API_BASE: &str = "https://discord.com/api/v10";
const MAX_MSG_LEN: usize = 2000;
const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 41_250;

/// Shared map for pending bash confirmations, keyed by Discord channel ID
pub type PendingConfirms = Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>;
type DiscordSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type DiscordWrite = SplitSink<DiscordSocket, WsMessage>;
type DiscordRead = SplitStream<DiscordSocket>;

struct HeartbeatLoop {
    ack: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

struct GatewaySessionContext<'a> {
    token: &'a str,
    http: &'a reqwest::Client,
    gateway: &'a Arc<Gateway>,
    ack: &'a Arc<AtomicBool>,
    shutdown: &'a mut watch::Receiver<bool>,
}

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

#[derive(Default)]
struct ResumeState {
    session_id: Option<String>,
    url: Option<String>,
}

struct GatewayRuntime {
    bot_user_id: String,
    pending_confirms: PendingConfirms,
    resume: ResumeState,
    seq: Arc<AtomicI64>,
}

impl GatewayRuntime {
    fn new(pending_confirms: PendingConfirms) -> Self {
        Self {
            bot_user_id: String::new(),
            pending_confirms,
            resume: ResumeState::default(),
            seq: Arc::new(AtomicI64::new(-1)),
        }
    }

    fn gateway_url(&self) -> &str {
        self.resume.url.as_deref().unwrap_or(GATEWAY_URL)
    }

    fn clear_resume(&mut self) {
        self.resume = ResumeState::default();
        self.seq.store(-1, Ordering::Relaxed);
    }

    fn update_ready(&mut self, data: &Value) {
        self.bot_user_id = data["user"]["id"].as_str().unwrap_or("").to_string();
        self.resume.session_id = data["session_id"].as_str().map(str::to_owned);
        self.resume.url = data["resume_gateway_url"]
            .as_str()
            .filter(|url| !url.is_empty())
            .map(|url| format!("{url}?v=10&encoding=json"));
    }
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
        if !text.is_empty()
            && let Err(e) = self.send_message(text).await
        {
            log::error!("Discord: failed to send message: {e}");
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

    async fn flush(&self) {
        let buf = std::mem::take(&mut *self.buffer.lock().await);
        if !buf.is_empty()
            && let Err(e) = self.send_message(&buf).await
        {
            log::error!("Discord: failed to flush buffer: {e}");
        }
    }
}

/// Delivery sink for sending messages to Discord channels via REST API
pub struct DiscordSink {
    token: String,
    http: reqwest::Client,
}

impl DiscordSink {
    #[must_use]
    pub fn new(token: String, http: reqwest::Client) -> Self {
        Self { token, http }
    }
}

#[async_trait::async_trait]
impl DeliverySink for DiscordSink {
    /// Deliver a message to a Discord channel
    ///
    /// `to` is a Discord channel ID.
    async fn deliver(&self, to: &str, text: &str) -> std::result::Result<(), String> {
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
fn resume_payload(token: &str, session_id: &str, seq: i64) -> Value {
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
    if interval_ms == 0 {
        return 0;
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter = u128::from(interval_ms) * u128::from(nanos) / 1_000_000_000;
    u64::try_from(jitter).unwrap_or(interval_ms.saturating_sub(1))
}

// --- Gateway connection ---

async fn register_discord_sink(gateway: &Arc<Gateway>, token: &str, http: &reqwest::Client) {
    let s = gateway.state().read().await;
    s.router.outbound().register(
        "discord",
        Arc::new(DiscordSink::new(token.to_string(), http.clone())),
    );
}

async fn send_gateway_payload(write: &Arc<Mutex<DiscordWrite>>, payload: Value) -> bool {
    let msg = WsMessage::Text(payload.to_string().into());
    write.lock().await.send(msg).await.is_ok()
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

async fn read_hello_interval(
    read: &mut DiscordRead,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<u64> {
    let text = tokio::select! {
        changed = shutdown.changed() => {
            if changed.is_err() || shutdown_requested(shutdown) {
                return None;
            }
            return None;
        }
        ws_msg = read.next() => {
            match ws_msg {
                Some(Ok(WsMessage::Text(text))) => text,
                _ => return Some(DEFAULT_HEARTBEAT_INTERVAL_MS),
            }
        }
    };
    let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) else {
        return Some(DEFAULT_HEARTBEAT_INTERVAL_MS);
    };
    if payload.op != 10 {
        return Some(DEFAULT_HEARTBEAT_INTERVAL_MS);
    }

    Some(
        payload
            .d
            .as_ref()
            .and_then(|data| data["heartbeat_interval"].as_u64())
            .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS),
    )
}

fn store_sequence(seq: &AtomicI64, value: u64) {
    if let Ok(next) = i64::try_from(value) {
        seq.store(next, Ordering::Relaxed);
    } else {
        seq.store(-1, Ordering::Relaxed);
        log::warn!("Discord: sequence {value} overflowed i64, resetting seq");
    }
}

async fn initialize_session(
    read: &mut DiscordRead,
    write: &Arc<Mutex<DiscordWrite>>,
    token: &str,
    runtime: &mut GatewayRuntime,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    if let Some(session_id) = runtime.resume.session_id.as_deref() {
        let seq = runtime.seq.load(Ordering::Relaxed);
        let payload = resume_payload(token, session_id, seq);
        if !send_gateway_payload(write, payload).await {
            return false;
        }
        log::info!("Discord: resuming (session={session_id}, seq={seq})");
        return true;
    }

    if !send_gateway_payload(write, identify_payload(token)).await {
        return false;
    }

    log::info!("Discord: identifying...");
    wait_for_ready(read, runtime, shutdown).await
}

async fn wait_for_ready(
    read: &mut DiscordRead,
    runtime: &mut GatewayRuntime,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    loop {
        let next = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || shutdown_requested(shutdown) {
                    return false;
                }
                continue;
            }
            next = read.next() => next,
        };
        let Some(Ok(WsMessage::Text(text))) = next else {
            break;
        };
        let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) else {
            continue;
        };
        if let Some(seq) = payload.s {
            store_sequence(runtime.seq.as_ref(), seq);
        }
        if payload.op == 9 {
            log::warn!("Discord: invalid session during identify");
            runtime.clear_resume();
            return false;
        }
        if payload.op == 0 && payload.t.as_deref() == Some("READY") {
            if let Some(data) = payload.d.as_ref() {
                runtime.update_ready(data);
            }
            log::info!("Discord: ready (bot_id={})", runtime.bot_user_id);
            return true;
        }
    }

    runtime.clear_resume();
    false
}

fn spawn_heartbeat(
    write: Arc<Mutex<DiscordWrite>>,
    seq: Arc<AtomicI64>,
    heartbeat_interval_ms: u64,
    mut shutdown: watch::Receiver<bool>,
) -> HeartbeatLoop {
    let ack = Arc::new(AtomicBool::new(true));
    let hb_ack = ack.clone();
    let jitter = jitter_millis(heartbeat_interval_ms);

    let task = tokio::spawn(async move {
        let start = Instant::now() + Duration::from_millis(jitter);
        let mut ticker = interval_at(start, Duration::from_millis(heartbeat_interval_ms));
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || shutdown_requested(&shutdown) {
                        break;
                    }
                    continue;
                }
                _ = ticker.tick() => {}
            }
            if !hb_ack.swap(false, Ordering::Relaxed) {
                log::warn!("Discord: no heartbeat ACK, closing connection");
                let _ = write.lock().await.close().await;
                break;
            }

            let payload = heartbeat_payload(seq.load(Ordering::Relaxed));
            if !send_gateway_payload(&write, payload).await {
                break;
            }
        }
    });

    HeartbeatLoop { ack, task }
}

async fn run_gateway_session(
    ws: DiscordSocket,
    token: &str,
    http: &reqwest::Client,
    gateway: &Arc<Gateway>,
    runtime: &mut GatewayRuntime,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<()> {
    let (write, mut read) = ws.split();
    let write = Arc::new(Mutex::new(write));
    let Some(heartbeat_interval_ms) = read_hello_interval(&mut read, shutdown).await else {
        return Ok(());
    };

    if !initialize_session(&mut read, &write, token, runtime, shutdown).await {
        if shutdown_requested(shutdown) {
            return Ok(());
        }
        runtime.clear_resume();
        return Ok(());
    }

    let heartbeat = spawn_heartbeat(
        write.clone(),
        runtime.seq.clone(),
        heartbeat_interval_ms,
        shutdown.clone(),
    );
    let ctx = GatewaySessionContext {
        token,
        http,
        gateway,
        ack: &heartbeat.ack,
        shutdown,
    };
    let result = handle_gateway_messages(&mut read, &write, runtime, ctx).await;
    heartbeat.task.abort();
    let _ = heartbeat.task.await;
    result
}

async fn handle_gateway_messages(
    read: &mut DiscordRead,
    write: &Arc<Mutex<DiscordWrite>>,
    runtime: &mut GatewayRuntime,
    ctx: GatewaySessionContext<'_>,
) -> Result<()> {
    loop {
        let next = tokio::select! {
            changed = ctx.shutdown.changed() => {
                if changed.is_err() || shutdown_requested(ctx.shutdown) {
                    return Ok(());
                }
                continue;
            }
            next = read.next() => next,
        };
        let Some(Ok(ws_msg)) = next else {
            return Ok(());
        };
        let text = match ws_msg {
            WsMessage::Text(text) => text,
            WsMessage::Close(frame) => {
                if let Some(close_frame) = frame {
                    let code: u16 = close_frame.code.into();
                    log::warn!("Discord: close {code}: {}", close_frame.reason);
                    if matches!(code, 4004 | 4010 | 4011 | 4013 | 4014) {
                        return Err(anyhow::anyhow!("Discord: fatal close code {code}"));
                    }
                }
                return Ok(());
            }
            _ => continue,
        };

        let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) else {
            continue;
        };
        if let Some(seq) = payload.s {
            store_sequence(runtime.seq.as_ref(), seq);
        }

        if handle_gateway_payload(
            payload,
            write,
            ctx.token,
            ctx.http,
            ctx.gateway,
            runtime,
            ctx.ack,
        )
        .await?
        {
            return Ok(());
        }
    }
}

async fn handle_gateway_payload(
    payload: GatewayPayload,
    write: &Arc<Mutex<DiscordWrite>>,
    token: &str,
    http: &reqwest::Client,
    gateway: &Arc<Gateway>,
    runtime: &mut GatewayRuntime,
    ack: &Arc<AtomicBool>,
) -> Result<bool> {
    match payload.op {
        1 => {
            let payload = heartbeat_payload(runtime.seq.load(Ordering::Relaxed));
            let _ = send_gateway_payload(write, payload).await;
            Ok(false)
        }
        7 => {
            log::info!("Discord: reconnect requested");
            Ok(true)
        }
        9 => {
            let resumable = payload.d.as_ref().and_then(Value::as_bool).unwrap_or(false);
            if resumable {
                log::info!("Discord: invalid session (resumable)");
            } else {
                log::info!("Discord: invalid session (not resumable)");
                runtime.clear_resume();
            }
            Ok(true)
        }
        11 => {
            ack.store(true, Ordering::Relaxed);
            Ok(false)
        }
        0 => {
            handle_dispatch_event(payload, token, http, gateway, runtime).await;
            Ok(false)
        }
        _ => Ok(false),
    }
}

async fn handle_dispatch_event(
    payload: GatewayPayload,
    token: &str,
    http: &reqwest::Client,
    gateway: &Arc<Gateway>,
    runtime: &GatewayRuntime,
) {
    if payload.t.as_deref() == Some("RESUMED") {
        log::info!("Discord: resumed successfully");
        return;
    }

    if payload.t.as_deref() != Some("MESSAGE_CREATE") {
        return;
    }

    let Some(data) = payload.d else {
        return;
    };
    let Ok(msg) = serde_json::from_value::<MessageCreate>(data) else {
        return;
    };

    handle_message_create(msg, token, http, gateway, runtime).await;
}

async fn handle_message_create(
    msg: MessageCreate,
    token: &str,
    http: &reqwest::Client,
    gateway: &Arc<Gateway>,
    runtime: &GatewayRuntime,
) {
    if msg.author.bot.unwrap_or(false) || msg.content.is_empty() {
        return;
    }

    let preview: String = msg.content.chars().take(80).collect();
    log::debug!("Discord: message from {}: {}", msg.author.id, preview);

    if let Some(confirm_tx) = runtime
        .pending_confirms
        .lock()
        .await
        .remove(&msg.channel_id)
    {
        let _ = confirm_tx.send(msg.content);
        return;
    }

    if let Some(ctrl) = parse_control_command(&msg.content) {
        let reply_text =
            handle_control_command(gateway, ctrl, &msg.author.id, &runtime.bot_user_id).await;
        let _ = post_discord_message(http, token, &msg.channel_id, &reply_text).await;
        return;
    }

    let reply = Arc::new(DiscordReply::new(
        msg.channel_id.clone(),
        token.to_string(),
        http.clone(),
        runtime.pending_confirms.clone(),
    ));

    let user_msg = UserMessage {
        text: msg.content,
        sender_id: msg.author.id,
        channel: "discord".into(),
        account_id: runtime.bot_user_id.clone(),
        guild_id: msg.guild_id.unwrap_or_default(),
    };

    if let Err(e) = gateway.dispatch(user_msg, reply, None).await {
        log::error!("Discord dispatch error: {e}");
    }
}

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
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let http = reqwest::Client::new();
    register_discord_sink(&gateway, &token, &http).await;
    log::info!("Discord: registered outbound sink");

    let pending_confirms: PendingConfirms = Arc::new(Mutex::new(HashMap::new()));
    let mut runtime = GatewayRuntime::new(pending_confirms);

    loop {
        if shutdown_requested(&shutdown) {
            return Ok(());
        }
        let url = runtime.gateway_url().to_string();
        log::info!("Discord: connecting to {url}");

        let ws = match tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || shutdown_requested(&shutdown) {
                    return Ok(());
                }
                continue;
            }
            connect = tokio_tungstenite::connect_async(url) => connect,
        } {
            Ok((ws, _)) => ws,
            Err(error) => {
                log::error!("Discord: connection failed: {error}");
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || shutdown_requested(&shutdown) {
                            return Ok(());
                        }
                    }
                    () = tokio::time::sleep(Duration::from_secs(tuning().timeouts.reconnect_delay_secs)) => {}
                }
                continue;
            }
        };

        run_gateway_session(ws, &token, &http, &gateway, &mut runtime, &mut shutdown).await?;
        if shutdown_requested(&shutdown) {
            return Ok(());
        }

        log::info!("Discord: disconnected, reconnecting...");
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || shutdown_requested(&shutdown) {
                    return Ok(());
                }
            }
            () = tokio::time::sleep(Duration::from_secs(tuning().timeouts.reconnect_delay_secs)) => {}
        }
    }
}

// ── Control commands ─────────────────────────────────────────

/// Parsed Discord control command
enum DiscordControl {
    Interrupt,
    Session(SessionControl),
}

/// Parse a `!`-prefixed control command from Discord message
fn parse_control_command(text: &str) -> Option<DiscordControl> {
    let text = text.trim();
    if !text.starts_with('!') {
        return None;
    }
    let parts: Vec<&str> = text[1..].splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).copied().unwrap_or("").trim();

    match cmd {
        "interrupt" | "stop" => Some(DiscordControl::Interrupt),
        "rewind" => {
            let count = arg.parse().unwrap_or(1);
            Some(DiscordControl::Session(SessionControl::Rewind { count }))
        }
        "model" if !arg.is_empty() => Some(DiscordControl::Session(SessionControl::ModelSwitch {
            model: arg.to_string(),
        })),
        "context" => Some(DiscordControl::Session(SessionControl::ContextUsage)),
        _ => None,
    }
}

/// Execute a control command and return a human-readable response
async fn handle_control_command(
    gateway: &Arc<Gateway>,
    ctrl: DiscordControl,
    peer_id: &str,
    account_id: &str,
) -> String {
    let session_key = {
        let s = gateway.state().read().await;
        let test_msg = UserMessage {
            text: String::new(),
            sender_id: peer_id.to_string(),
            channel: "discord".into(),
            account_id: account_id.to_string(),
            guild_id: String::new(),
        };
        let result = s.router.resolve(&test_msg);
        result.session_key
    };

    match ctrl {
        DiscordControl::Interrupt => match gateway.interrupt(&session_key).await {
            Ok(()) => "interrupted".into(),
            Err(e) => format!("Error: {e}"),
        },
        DiscordControl::Session(sc) => match gateway.send_control(&session_key, sc).await {
            Ok(event) => format_control_event(&event),
            Err(e) => format!("Error: {e}"),
        },
    }
}

/// Format a `ControlEvent` as a Discord-friendly message
fn format_control_event(event: &ControlEvent) -> String {
    match event {
        ControlEvent::ContextInfo {
            used_tokens,
            total_tokens,
            history_messages,
        } => {
            let pct = if *total_tokens > 0 {
                *used_tokens * 100 / *total_tokens
            } else {
                0
            };
            format!(
                "Context: ~{used_tokens}/{total_tokens} tokens ({pct}%), {history_messages} messages"
            )
        }
        ControlEvent::Rewound { removed, remaining } => {
            format!("Rewound {removed} messages ({remaining} remaining)")
        }
        ControlEvent::SessionReady { model, .. } => {
            format!("OK (model: {model})")
        }
        ControlEvent::TurnComplete { text } => text.clone().unwrap_or_else(|| "OK".into()),
        ControlEvent::Error { message, .. } => {
            format!("Error: {message}")
        }
        _ => "OK".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jitter_within_range() {
        for interval in [1000, 41250, 60000] {
            let j = jitter_millis(interval);
            assert!(j < interval, "jitter {j} >= interval {interval}");
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
        assert_eq!(p["d"]["properties"]["browser"], "minusagent");
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
