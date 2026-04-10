use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};

use crate::config::tuning;

type EpochMillis = u64;
const LEGACY_SECONDS_THRESHOLD_MS: EpochMillis = 100_000_000_000;
const LEGACY_SECONDS_THRESHOLD_MS_F64: f64 = 100_000_000_000.0;

/// Async delivery target for outbound messages
#[async_trait::async_trait]
pub trait DeliverySink: Send + Sync {
    async fn deliver(&self, to: &str, text: &str) -> std::result::Result<(), String>;
}

/// Routes outbound messages to registered per-channel sinks
///
/// Falls back to the default sink when no sink is registered for a channel.
pub struct OutboundSinks {
    sinks: RwLock<HashMap<String, Arc<dyn DeliverySink>>>,
    fallback: Arc<dyn DeliverySink>,
}

impl OutboundSinks {
    /// Create with a fallback sink for unregistered channels
    #[must_use]
    pub fn new(fallback: Arc<dyn DeliverySink>) -> Self {
        Self {
            sinks: RwLock::new(HashMap::new()),
            fallback,
        }
    }

    /// Register a sink for a channel type (e.g. "discord", "websocket")
    pub fn register(&self, channel: &str, sink: Arc<dyn DeliverySink>) {
        self.write_sinks().insert(channel.to_string(), sink);
    }

    /// Route a message to the appropriate sink
    ///
    /// # Errors
    ///
    /// Returns the error produced by the selected delivery sink.
    pub async fn deliver(
        &self,
        channel: &str,
        to: &str,
        text: &str,
    ) -> std::result::Result<(), String> {
        let sink = {
            let sinks = self.read_sinks();
            sinks.get(channel).cloned()
        };
        match sink {
            Some(s) => s.deliver(to, text).await,
            None => self.fallback.deliver(to, text).await,
        }
    }

    fn read_sinks(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Arc<dyn DeliverySink>>> {
        self.sinks.read().unwrap_or_else(|error| {
            log::error!("Sink registry read lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }

    fn write_sinks(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Arc<dyn DeliverySink>>> {
        self.sinks.write().unwrap_or_else(|error| {
            log::error!("Sink registry write lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }
}

/// Delivers messages to the TUI background output buffer
pub struct BgOutputSink;

#[async_trait::async_trait]
impl DeliverySink for BgOutputSink {
    async fn deliver(&self, _to: &str, text: &str) -> std::result::Result<(), String> {
        crate::scheduler::push_bg_output(text.to_string());
        Ok(())
    }
}

/// A queued delivery entry persisted to disk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedDelivery {
    pub id: String,
    pub channel: String,
    pub to: String,
    pub text: String,
    #[serde(deserialize_with = "deserialize_epoch_millis")]
    pub enqueued_at: EpochMillis,
    #[serde(default, deserialize_with = "deserialize_optional_epoch_millis")]
    pub next_retry_at: Option<EpochMillis>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Disk-persisted write-ahead delivery queue
///
/// Enqueue writes to disk atomically (tmp + fsync + rename) before
/// attempting delivery. Crash-safe: incomplete writes leave only
/// harmless orphaned tmp files.
pub struct DeliveryQueue {
    queue_dir: PathBuf,
}

impl DeliveryQueue {
    /// Create a new delivery queue, ensuring directories exist
    ///
    /// # Errors
    ///
    /// Returns an error if the queue directory structure cannot be created.
    pub fn new(queue_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(queue_dir)?;
        std::fs::create_dir_all(queue_dir.join("failed"))?;
        Ok(Self {
            queue_dir: queue_dir.to_path_buf(),
        })
    }

    /// Write-ahead enqueue: persist to disk, return delivery ID
    ///
    /// # Errors
    ///
    /// Returns an error if queue entry serialization or atomic persistence fails.
    pub fn enqueue(&self, channel: &str, to: &str, text: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().to_string()[..12].to_string();
        let entry = QueuedDelivery {
            id: id.clone(),
            channel: channel.to_string(),
            to: to.to_string(),
            text: text.to_string(),
            enqueued_at: now_millis(),
            next_retry_at: None,
            retry_count: 0,
            last_error: None,
        };
        self.write_entry(&entry)?;
        Ok(id)
    }

    /// Acknowledge successful delivery (delete queue file)
    ///
    /// # Errors
    ///
    /// Returns an error if deleting an existing queue file fails.
    pub fn ack(&self, id: &str) -> Result<()> {
        let path = self.queue_dir.join(format!("{id}.json"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Record delivery failure: increment retry, schedule backoff, or
    /// move to `failed/` after max retries
    ///
    /// # Errors
    ///
    /// Returns an error if the queue entry cannot be loaded or updated on disk.
    pub fn fail(&self, id: &str, error: &str) -> Result<()> {
        let mut entry = self.read_entry(id)?;
        entry.retry_count += 1;
        entry.last_error = Some(error.to_string());
        if entry.retry_count >= tuning().delivery.delivery_max_retries {
            return self.move_to_failed(id);
        }
        let backoff = compute_backoff_ms(entry.retry_count);
        entry.next_retry_at = Some(now_millis().saturating_add(backoff));
        self.write_entry(&entry)
    }

    /// Scan queue directory for pending entries (excludes tmp and failed)
    ///
    /// # Errors
    ///
    /// Returns an error if the queue directory cannot be scanned.
    pub fn load_pending(&self) -> Result<Vec<QueuedDelivery>> {
        let mut entries = Vec::new();
        for dir_entry in std::fs::read_dir(&self.queue_dir)? {
            let dir_entry = dir_entry?;
            let name = dir_entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".tmp.") || !name.ends_with(".json") {
                continue;
            }
            if let Ok(data) = std::fs::read_to_string(dir_entry.path())
                && let Ok(delivery) = serde_json::from_str::<QueuedDelivery>(&data)
            {
                entries.push(delivery);
            }
        }
        Ok(entries)
    }

    /// Atomic write: tmp file → fsync → rename
    fn write_entry(&self, entry: &QueuedDelivery) -> Result<()> {
        let final_path = self.queue_dir.join(format!("{}.json", entry.id));
        let tmp_path =
            self.queue_dir
                .join(format!(".tmp.{}.{}.json", std::process::id(), entry.id));

        let data = serde_json::to_string_pretty(entry)?;
        {
            let mut f = std::fs::File::create(&tmp_path)?;
            f.write_all(data.as_bytes())?;
            f.flush()?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }

    fn read_entry(&self, id: &str) -> Result<QueuedDelivery> {
        let path = self.queue_dir.join(format!("{id}.json"));
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    fn move_to_failed(&self, id: &str) -> Result<()> {
        let src = self.queue_dir.join(format!("{id}.json"));
        let dst = self.queue_dir.join("failed").join(format!("{id}.json"));
        if src.exists() {
            std::fs::rename(src, dst)?;
        }
        Ok(())
    }

    /// Remove orphaned tmp files from a previous crash
    fn cleanup_tmp(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.queue_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(".tmp.") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// Delivery statistics snapshot
pub struct DeliveryStats {
    pub total_attempted: u64,
    pub total_succeeded: u64,
    pub total_failed: u64,
    pub pending: usize,
}

/// Commands sent to the delivery runner task
enum DeliveryCmd {
    Wake,
    Stats(oneshot::Sender<DeliveryStats>),
    Stop,
}

/// Clonable handle for enqueuing messages and querying delivery status
///
/// Enqueue writes to disk synchronously (write-ahead) then signals the
/// background runner. If the process crashes after enqueue but before
/// delivery, the entry survives on disk for recovery.
#[derive(Clone)]
pub struct DeliveryHandle {
    queue: Arc<DeliveryQueue>,
    runner: Arc<DeliveryRunner>,
}

struct DeliveryRunner {
    cmd_tx: Mutex<Option<mpsc::Sender<DeliveryCmd>>>,
    generation: AtomicU64,
    sinks: Arc<OutboundSinks>,
}

impl DeliveryHandle {
    /// Enqueue a message for delivery
    ///
    /// Chunks text by platform limit, writes each chunk to disk, then
    /// wakes the runner. All-or-nothing: if any chunk fails to enqueue,
    /// previously written chunks are removed.
    pub fn enqueue(&self, channel: &str, to: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        let limit = platform_limit(channel);
        let chunks = chunk_message(text, limit);
        let mut written_ids: Vec<String> = Vec::new();
        for chunk in chunks {
            match self.queue.enqueue(channel, to, chunk) {
                Ok(id) => written_ids.push(id),
                Err(e) => {
                    log::error!("Delivery enqueue failed: {e}");
                    for id in &written_ids {
                        let _ = self.queue.ack(id);
                    }
                    return;
                }
            }
        }
        if let Some(sender) = self.runner.sender() {
            let _ = sender.try_send(DeliveryCmd::Wake);
        }
    }

    /// Get delivery statistics
    pub async fn stats(&self) -> Option<DeliveryStats> {
        let sender = self.runner.sender()?;
        let (tx, rx) = oneshot::channel();
        sender.send(DeliveryCmd::Stats(tx)).await.ok()?;
        rx.await.ok()
    }

    /// Stop the delivery runner
    pub async fn stop(&self) -> bool {
        let Some(sender) = self.runner.sender() else {
            return false;
        };
        if sender.send(DeliveryCmd::Stop).await.is_err() {
            self.runner.clear_closed_sender();
            return false;
        }
        self.wait_for_stopped().await;
        !self.is_running()
    }

    /// Start the delivery runner if it is currently stopped.
    #[must_use]
    pub fn start(&self) -> bool {
        if self.is_running() {
            return false;
        }
        self.spawn_runner();
        true
    }

    /// Restart the delivery runner and keep the shared handle alive.
    pub async fn restart(&self) -> bool {
        let _ = self.stop().await;
        self.start()
    }

    /// Whether the delivery runner is currently active.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.runner.sender().is_some()
    }

    fn spawn_runner(&self) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<DeliveryCmd>(64);
        let generation = self.runner.install_sender(cmd_tx);

        let runner_queue = Arc::clone(&self.queue);
        let runner = Arc::clone(&self.runner);
        let sinks = Arc::clone(&self.runner.sinks);

        runner_queue.cleanup_tmp();
        match runner_queue.load_pending() {
            Ok(pending) if !pending.is_empty() => {
                log::info!("Delivery: recovering {} pending entries", pending.len());
            }
            _ => {}
        }

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(
                tuning().delivery.delivery_poll_interval_ms,
            ));
            let mut total_attempted: u64 = 0;
            let mut total_succeeded: u64 = 0;
            let mut total_failed: u64 = 0;

            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        process_pending(
                            &runner_queue, &sinks,
                            &mut total_attempted,
                            &mut total_succeeded,
                            &mut total_failed,
                        ).await;
                    }
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(DeliveryCmd::Wake) => {
                                process_pending(
                                    &runner_queue, &sinks,
                                    &mut total_attempted,
                                    &mut total_succeeded,
                                    &mut total_failed,
                                ).await;
                            }
                            Some(DeliveryCmd::Stats(reply)) => {
                                let pending = runner_queue
                                    .load_pending()
                                    .map(|entries| entries.len())
                                    .unwrap_or(0);
                                let _ = reply.send(DeliveryStats {
                                    total_attempted,
                                    total_succeeded,
                                    total_failed,
                                    pending,
                                });
                            }
                            Some(DeliveryCmd::Stop) | None => break,
                        }
                    }
                }
            }

            runner.clear_sender_if_current(generation);
        });
    }

    async fn wait_for_stopped(&self) {
        for _ in 0..50 {
            if !self.is_running() {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
}

impl DeliveryRunner {
    fn new(sinks: Arc<OutboundSinks>) -> Self {
        Self {
            cmd_tx: Mutex::new(None),
            generation: AtomicU64::new(0),
            sinks,
        }
    }

    fn sender(&self) -> Option<mpsc::Sender<DeliveryCmd>> {
        let mut guard = self.lock_cmd_tx();
        if guard.as_ref().is_some_and(mpsc::Sender::is_closed) {
            *guard = None;
        }
        guard.clone()
    }

    fn install_sender(&self, sender: mpsc::Sender<DeliveryCmd>) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        *self.lock_cmd_tx() = Some(sender);
        generation
    }

    fn clear_sender_if_current(&self, generation: u64) {
        if self.generation.load(Ordering::Relaxed) == generation {
            *self.lock_cmd_tx() = None;
        }
    }

    fn clear_closed_sender(&self) {
        let mut guard = self.lock_cmd_tx();
        if guard.as_ref().is_some_and(mpsc::Sender::is_closed) {
            *guard = None;
        }
    }

    fn lock_cmd_tx(&self) -> std::sync::MutexGuard<'_, Option<mpsc::Sender<DeliveryCmd>>> {
        self.cmd_tx.lock().unwrap_or_else(|error| {
            log::error!("Delivery runner lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }
}

/// Spawn the delivery runner and return a clonable handle
///
/// The runner scans the queue directory every second and delivers
/// due entries via `sinks`. On startup it cleans orphaned tmp
/// files and retries entries left from a previous crash.
///
/// # Arguments
///
/// * `queue_dir` - Directory for queue files
/// * `sinks` - Outbound sink registry for routing deliveries
///
/// # Errors
///
/// Returns an error if the on-disk delivery queue cannot be initialized.
pub fn spawn(queue_dir: &Path, sinks: Arc<OutboundSinks>) -> Result<DeliveryHandle> {
    let queue = Arc::new(DeliveryQueue::new(queue_dir)?);
    let handle = DeliveryHandle {
        queue,
        runner: Arc::new(DeliveryRunner::new(sinks)),
    };
    let _ = handle.start();
    Ok(handle)
}

/// Process all due pending entries
async fn process_pending(
    queue: &DeliveryQueue,
    sinks: &OutboundSinks,
    attempted: &mut u64,
    succeeded: &mut u64,
    failed: &mut u64,
) {
    let pending = match queue.load_pending() {
        Ok(p) => p,
        Err(e) => {
            log::error!("Delivery scan failed: {e}");
            return;
        }
    };
    let now = now_millis();
    for entry in pending {
        if entry
            .next_retry_at
            .is_some_and(|next_retry_at| next_retry_at > now)
        {
            continue;
        }
        *attempted += 1;
        match sinks.deliver(&entry.channel, &entry.to, &entry.text).await {
            Ok(()) => {
                if let Err(e) = queue.ack(&entry.id) {
                    log::error!("Delivery ack failed: {e}");
                }
                *succeeded += 1;
            }
            Err(e) => {
                if let Err(e2) = queue.fail(&entry.id, &e) {
                    log::error!("Delivery fail record error: {e2}");
                }
                *failed += 1;
            }
        }
    }
}

/// Exponential backoff with ±20% jitter
///
/// Schedule: [5s, 25s, 2min, 10min], capped at index 3.
fn compute_backoff_ms(retry_count: u32) -> u64 {
    if retry_count == 0 {
        return 0;
    }
    let backoff = &tuning().delivery.delivery_backoff_ms;
    let idx = std::cmp::min(retry_count as usize - 1, backoff.len() - 1);
    let base = backoff[idx];
    let range = base / 5;
    if range == 0 {
        return base;
    }
    let nanos = u64::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    );
    let span = range.saturating_mul(2).saturating_add(1);
    let offset = nanos % span;
    if offset >= range {
        base.saturating_add(offset - range)
    } else {
        base.saturating_sub(range - offset)
    }
}

/// Split text at paragraph boundaries (\n\n), falling back to line
/// boundaries (\n), respecting platform size limits.
fn chunk_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = std::cmp::min(start + max_len, text.len());
        if end == text.len() {
            chunks.push(&text[start..]);
            break;
        }
        let cut = text[start..end]
            .rfind("\n\n")
            .map(|i| start + i + 2)
            .or_else(|| text[start..end].rfind('\n').map(|i| start + i + 1))
            .unwrap_or(end);
        chunks.push(&text[start..cut]);
        start = cut;
    }
    chunks
}

/// Platform-specific message size limits
fn platform_limit(channel: &str) -> usize {
    match channel {
        "discord" => 2000,
        "telegram" => 4096,
        _ => tuning().delivery.delivery_chunk_limit,
    }
}

fn now_millis() -> EpochMillis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(EpochMillis::MAX)
}

fn deserialize_epoch_millis<'de, D>(deserializer: D) -> Result<EpochMillis, D::Error>
where
    D: serde::Deserializer<'de>,
{
    EpochValue::deserialize(deserializer)?
        .into_millis()
        .map_err(|error| serde::de::Error::custom(error.to_string()))
}

fn deserialize_optional_epoch_millis<'de, D>(
    deserializer: D,
) -> Result<Option<EpochMillis>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<EpochValue>::deserialize(deserializer)?
        .map(EpochValue::into_millis)
        .transpose()
        .map_err(|error| serde::de::Error::custom(error.to_string()))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EpochValue {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
}

impl EpochValue {
    fn into_millis(self) -> std::result::Result<EpochMillis, &'static str> {
        match self {
            Self::Unsigned(value) => Ok(normalize_epoch_millis(value)),
            Self::Signed(value) => u64::try_from(value)
                .map(normalize_epoch_millis)
                .map_err(|_| "negative epoch timestamp"),
            Self::Float(value) => normalize_float_epoch_millis(value).ok_or("invalid epoch float"),
        }
    }
}

fn normalize_epoch_millis(value: EpochMillis) -> EpochMillis {
    if value < LEGACY_SECONDS_THRESHOLD_MS {
        value.saturating_mul(1000)
    } else {
        value
    }
}

fn normalize_float_epoch_millis(value: f64) -> Option<EpochMillis> {
    if !value.is_finite() || value.is_sign_negative() {
        return None;
    }
    let duration = if value < LEGACY_SECONDS_THRESHOLD_MS_F64 {
        StdDuration::from_secs_f64(value)
    } else {
        StdDuration::from_secs_f64(value / 1000.0)
    };
    duration.as_millis().try_into().ok()
}

impl DeliveryHandle {
    /// Create a no-op delivery handle for testing
    #[cfg(test)]
    #[must_use]
    pub fn noop() -> Self {
        Self {
            queue: Arc::new(DeliveryQueue {
                queue_dir: PathBuf::new(),
            }),
            runner: Arc::new(DeliveryRunner::new(Arc::new(OutboundSinks::new(Arc::new(
                BgOutputSink,
            ))))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSink;

    #[async_trait::async_trait]
    impl DeliverySink for TestSink {
        async fn deliver(&self, _to: &str, _text: &str) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn test_enqueue_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        let id = q.enqueue("discord", "user123", "hello").unwrap();
        let pending = q.load_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].text, "hello");
        assert_eq!(pending[0].channel, "discord");
    }

    #[test]
    fn test_ack_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        let id = q.enqueue("bg", "", "test").unwrap();
        assert_eq!(q.load_pending().unwrap().len(), 1);
        q.ack(&id).unwrap();
        assert_eq!(q.load_pending().unwrap().len(), 0);
    }

    #[test]
    fn test_fail_increments_retry() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        let id = q.enqueue("bg", "", "test").unwrap();
        q.fail(&id, "timeout").unwrap();
        let pending = q.load_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].retry_count, 1);
        assert!(pending[0].next_retry_at.is_some());
        assert_eq!(pending[0].last_error.as_deref(), Some("timeout"));
    }

    #[test]
    fn test_move_to_failed_after_max_retries() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        let id = q.enqueue("bg", "", "test").unwrap();
        for _ in 0..tuning().delivery.delivery_max_retries {
            q.fail(&id, "err").unwrap();
        }
        assert_eq!(q.load_pending().unwrap().len(), 0);
        assert!(
            dir.path()
                .join("failed")
                .join(format!("{id}.json"))
                .exists()
        );
    }

    #[test]
    fn test_atomic_write_no_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        q.enqueue("bg", "", "test").unwrap();
        // No .tmp. files should remain after successful enqueue
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().to_string();
            assert!(!name.starts_with(".tmp."), "orphaned tmp file: {name}");
        }
    }

    #[test]
    fn test_cleanup_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let q = DeliveryQueue::new(dir.path()).unwrap();
        // Create a fake orphaned tmp file
        std::fs::write(dir.path().join(".tmp.999.orphan.json"), "{}").unwrap();
        q.cleanup_tmp();
        assert!(!dir.path().join(".tmp.999.orphan.json").exists());
    }

    #[test]
    fn test_compute_backoff_bounds() {
        for retry in 1..=5 {
            let backoff = compute_backoff_ms(retry);
            let backoff_schedule = &tuning().delivery.delivery_backoff_ms;
            let idx = std::cmp::min(retry as usize - 1, backoff_schedule.len() - 1);
            let base = backoff_schedule[idx];
            let min = base - base / 5;
            let max = base + base / 5;
            assert!(
                backoff >= min && backoff <= max,
                "retry={retry} backoff={backoff} expected [{min}, {max}]"
            );
        }
    }

    #[test]
    fn test_compute_backoff_zero() {
        assert_eq!(compute_backoff_ms(0), 0);
    }

    #[test]
    fn test_chunk_message_short() {
        let chunks = chunk_message("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_chunk_message_paragraph_boundary() {
        let text = "para one\n\npara two\n\npara three";
        // max_len=18 should split at the first \n\n
        let chunks = chunk_message(text, 18);
        assert_eq!(chunks[0], "para one\n\n");
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn test_chunk_message_line_fallback() {
        let text = "line1\nline2\nline3";
        let chunks = chunk_message(text, 10);
        assert_eq!(chunks[0], "line1\n");
    }

    #[test]
    fn test_platform_limit() {
        assert_eq!(platform_limit("discord"), 2000);
        assert_eq!(platform_limit("telegram"), 4096);
        assert_eq!(
            platform_limit("cli"),
            tuning().delivery.delivery_chunk_limit
        );
    }

    #[tokio::test]
    async fn test_delivery_handle_restart_preserves_clones() {
        let dir = tempfile::tempdir().unwrap();
        let sinks = Arc::new(OutboundSinks::new(Arc::new(TestSink)));
        let handle = spawn(dir.path(), sinks).unwrap();
        let clone = handle.clone();

        assert!(handle.is_running());
        assert!(handle.stop().await);
        assert!(!clone.is_running());

        assert!(clone.start());
        assert!(handle.is_running());
        assert!(handle.stats().await.is_some());

        assert!(handle.restart().await);
        assert!(clone.is_running());
        assert!(clone.stats().await.is_some());
    }
}
