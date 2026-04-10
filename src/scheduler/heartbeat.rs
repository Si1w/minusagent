use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{Local, Timelike, Utc};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::config::tuning;
use crate::engine::store::LLMConfig;
use crate::intelligence::memory::MemoryStore;
use crate::intelligence::prompt::format_memory_content;
use crate::intelligence::utils::{extract_body, parse_frontmatter};
use crate::routing::delivery::DeliveryHandle;
use crate::scheduler::{LANE_SESSION, LaneLock, run_single_turn};

type ActiveHours = (u8, u8);

/// Parse heartbeat-specific overrides from HEARTBEAT.md frontmatter
///
/// Supported keys:
/// - `interval`: seconds between runs (e.g. `600`)
/// - `active_hours`: comma-separated start,end (e.g. `8, 23`)
///
/// Returns `(interval, active_hours)` using tuning defaults for missing keys.
fn parse_heartbeat_config(meta: &HashMap<String, String>) -> (Duration, ActiveHours) {
    let defaults = default_heartbeat_config();
    let interval = meta
        .get("interval")
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(defaults.0, Duration::from_secs);

    let active_hours = meta
        .get("active_hours")
        .and_then(|value| parse_active_hours(value))
        .unwrap_or(defaults.1);

    (interval, active_hours)
}

fn default_heartbeat_config() -> (Duration, ActiveHours) {
    (
        Duration::from_secs(tuning().scheduler.heartbeat_interval_secs),
        tuning().scheduler.heartbeat_active_hours,
    )
}

fn parse_active_hours(value: &str) -> Option<ActiveHours> {
    let (start, end) = value.split_once(',')?;
    Some((
        start.trim().parse::<u8>().ok()?,
        end.trim().parse::<u8>().ok()?,
    ))
}

fn is_within_active_hours(hour: u32, active_hours: ActiveHours) -> bool {
    let (start, end) = active_hours;
    let start = u32::from(start);
    let end = u32::from(end);
    if start <= end {
        start <= hour && hour < end
    } else {
        !(end <= hour && hour < start)
    }
}

fn format_last_run(last_run_at: Option<SystemTime>) -> String {
    last_run_at.map_or_else(
        || "never".to_string(),
        |last_run| {
            let timestamp: chrono::DateTime<Local> = chrono::DateTime::from(last_run);
            timestamp.format("%Y-%m-%d %H:%M:%S").to_string()
        },
    )
}

fn heartbeat_message(output: &str) -> String {
    let mut message = String::from("[heartbeat] ");
    message.push_str(output);
    message
}

fn load_heartbeat_config(path: &Path) -> (Duration, ActiveHours) {
    std::fs::read_to_string(path)
        .ok()
        .map_or_else(default_heartbeat_config, |raw| {
            parse_heartbeat_config(&parse_frontmatter(&raw))
        })
}

/// Heartbeat runner status snapshot
pub struct HeartbeatStatus {
    pub enabled: bool,
    pub running: bool,
    pub should_run: bool,
    pub reason: String,
    pub last_run: String,
    pub next_in: String,
    pub interval_secs: f64,
    pub active_hours: (u8, u8),
    pub queue_size: usize,
}

/// Commands sent to the heartbeat task
enum HeartbeatCmd {
    Trigger(oneshot::Sender<String>),
    Status(oneshot::Sender<HeartbeatStatus>),
    Stop,
}

/// Handle for interacting with a running heartbeat task
#[derive(Clone)]
pub struct HeartbeatHandle {
    cmd_tx: mpsc::Sender<HeartbeatCmd>,
}

impl HeartbeatHandle {
    /// Request a manual heartbeat trigger, bypassing interval check
    ///
    /// # Returns
    ///
    /// Status message describing the trigger result.
    pub async fn trigger(&self) -> String {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(HeartbeatCmd::Trigger(tx)).await.is_err() {
            return "heartbeat task not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// Get the current heartbeat status
    pub async fn status(&self) -> Option<HeartbeatStatus> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(HeartbeatCmd::Status(tx)).await.ok()?;
        rx.await.ok()
    }

    /// Stop the heartbeat task
    pub fn stop(&self) {
        let _ = self.cmd_tx.try_send(HeartbeatCmd::Stop);
    }
}

/// Internal heartbeat runner (owned by its tokio task, not shared)
struct HeartbeatRunner {
    heartbeat_path: PathBuf,
    lane_lock: LaneLock,
    llm_config: LLMConfig,
    identity: String,
    memory: MemoryStore,
    delivery: DeliveryHandle,
    delivery_channel: String,
    delivery_to: String,
    interval: Duration,
    active_hours: (u8, u8),
    last_run_at: Option<SystemTime>,
    running: bool,
    last_output: String,
    output_count: usize,
    /// Cached raw content from the last successful read of HEARTBEAT.md
    cached_content: Option<String>,
    /// Mtime of the file when `cached_content` was loaded; skip re-read if unchanged
    cached_mtime: Option<SystemTime>,
    /// Cached body extracted from `cached_content`
    cached_body: Option<String>,
}

impl HeartbeatRunner {
    /// Read HEARTBEAT.md if it changed on disk, update config and cache content.
    /// Returns the body text, or `None` if unreadable/empty.
    fn refresh_file(&mut self) -> Option<String> {
        let mtime = std::fs::metadata(&self.heartbeat_path)
            .ok()
            .and_then(|meta| meta.modified().ok());
        if mtime.is_some() && mtime == self.cached_mtime {
            return self.cached_body.clone();
        }

        let Ok(raw) = std::fs::read_to_string(&self.heartbeat_path) else {
            self.cached_content = None;
            self.cached_body = None;
            self.cached_mtime = None;
            return None;
        };
        let meta = parse_frontmatter(&raw);
        let (interval, active_hours) = parse_heartbeat_config(&meta);
        self.interval = interval;
        self.active_hours = active_hours;
        let body = extract_body(&raw);
        if body.trim().is_empty() {
            self.cached_content = None;
            self.cached_body = None;
            self.cached_mtime = mtime;
            return None;
        }
        self.cached_content = Some(raw);
        self.cached_body = Some(body.clone());
        self.cached_mtime = mtime;
        Some(body)
    }

    /// Check 4 preconditions for running. Lock check is separate in `execute()`.
    fn should_run(&mut self) -> (bool, String) {
        if !self.heartbeat_path.exists() {
            return (false, "HEARTBEAT.md not found".into());
        }
        let Some(_body) = self.refresh_file() else {
            return (false, "HEARTBEAT.md body is empty".into());
        };

        if let Some(elapsed) = self.elapsed_since_last_run()
            && elapsed < self.interval
        {
            let remaining = self.interval.saturating_sub(elapsed).as_secs_f64();
            return (
                false,
                format!("interval not elapsed ({remaining:.0}s remaining)"),
            );
        }

        if !is_within_active_hours(Local::now().hour(), self.active_hours) {
            let (start, end) = self.active_hours;
            return (false, format!("outside active hours ({start}:00-{end}:00)"));
        }

        if self.running {
            return (false, "already running".into());
        }
        (true, "all checks passed".into())
    }

    /// Parse heartbeat response. `HEARTBEAT_OK` means nothing to report.
    fn parse_response(response: &str) -> Option<String> {
        if response.contains("HEARTBEAT_OK") {
            let stripped = response.replace("HEARTBEAT_OK", "").trim().to_string();
            if stripped.is_empty() {
                None
            } else {
                Some(stripped)
            }
        } else {
            let trimmed = response.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
    }

    /// Build system prompt and instruction for heartbeat turn
    fn build_prompt(&mut self) -> Option<(String, String)> {
        let raw = self
            .cached_content
            .clone()
            .or_else(|| std::fs::read_to_string(&self.heartbeat_path).ok())?;
        let instructions = extract_body(&raw);
        if instructions.is_empty() {
            return None;
        }

        // Refresh memory to pick up entries added via /remember
        self.memory.discover();

        let mut prompt = self.identity.clone();

        if !self.memory.entries.is_empty() {
            let mem_block = format_memory_content(&self.memory.entries);
            prompt.push_str("\n\n# Known Context\n\n");
            prompt.push_str(&mem_block);
        }

        let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
        prompt.push_str("\n\nCurrent time: ");
        prompt.push_str(&now.to_string());

        Some((instructions, prompt))
    }

    /// Execute one heartbeat. Skip if session lane is active.
    async fn execute(&mut self) {
        let stats = self.lane_lock.lane_stats(LANE_SESSION).await;
        if stats.is_some_and(|stats| stats.active > 0) {
            log::debug!("Heartbeat skipped: session lane occupied");
            return;
        }

        self.lane_lock.mark_active(LANE_SESSION).await;
        self.running = true;
        log::debug!("Heartbeat executing");

        let result = self.execute_inner().await;

        self.lane_lock.mark_done(LANE_SESSION).await;
        self.running = false;
        self.record_run();

        if let Err(e) = result {
            log::error!("Heartbeat error: {e}");
            self.delivery.enqueue(
                &self.delivery_channel,
                &self.delivery_to,
                &format!("[heartbeat error] {e}"),
            );
        }
    }

    async fn execute_inner(&mut self) -> anyhow::Result<()> {
        let Some((instructions, sys_prompt)) = self.build_prompt() else {
            return Ok(());
        };

        let response = run_single_turn(&sys_prompt, &instructions, &self.llm_config).await?;
        let Some(meaningful) = Self::parse_response(&response) else {
            return Ok(());
        };

        if meaningful.trim() == self.last_output {
            return Ok(());
        }
        self.record_output(&meaningful);
        Ok(())
    }

    /// Manual trigger (bypasses interval check and lane lock)
    ///
    /// Lane lock is only for automatic execution yielding to user turns.
    /// Manual trigger is an explicit user action — no lock needed.
    async fn trigger(&mut self) -> String {
        self.running = true;

        let result = match self.build_prompt() {
            Some((instructions, sys_prompt)) => {
                match run_single_turn(&sys_prompt, &instructions, &self.llm_config).await {
                    Ok(response) => match Self::parse_response(&response) {
                        None => "HEARTBEAT_OK (nothing to report)".to_string(),
                        Some(m) if m.trim() == self.last_output => {
                            "duplicate content (skipped)".to_string()
                        }
                        Some(m) => {
                            self.record_output(&m);
                            let len = m.len();
                            format!("triggered, output queued ({len} chars)")
                        }
                    },
                    Err(e) => format!("trigger failed: {e}"),
                }
            }
            None => "HEARTBEAT.md is empty or unreadable".to_string(),
        };

        self.running = false;
        self.record_run();
        result
    }

    fn status(&mut self) -> HeartbeatStatus {
        let next_in = self
            .elapsed_since_last_run()
            .map_or(self.interval.as_secs_f64(), |elapsed| {
                self.interval.saturating_sub(elapsed).as_secs_f64()
            });
        let (ok, reason) = self.should_run();

        HeartbeatStatus {
            enabled: self.heartbeat_path.exists(),
            running: self.running,
            should_run: ok,
            reason,
            last_run: format_last_run(self.last_run_at),
            next_in: format!("{next_in:.0}s"),
            interval_secs: self.interval.as_secs_f64(),
            active_hours: self.active_hours,
            queue_size: self.output_count,
        }
    }

    fn elapsed_since_last_run(&self) -> Option<Duration> {
        self.last_run_at
            .and_then(|last_run| SystemTime::now().duration_since(last_run).ok())
    }

    fn record_output(&mut self, output: &str) {
        self.last_output.clear();
        self.last_output.push_str(output.trim());
        self.output_count += 1;
        self.delivery.enqueue(
            &self.delivery_channel,
            &self.delivery_to,
            &heartbeat_message(output),
        );
    }

    fn record_run(&mut self) {
        self.last_run_at = Some(SystemTime::now());
    }
}

/// Spawn a heartbeat task and return its handle
///
/// Interval and active hours are read from HEARTBEAT.md frontmatter,
/// falling back to tuning defaults. The file is re-read on each poll
/// so changes take effect without restart.
///
/// # Arguments
///
/// * `workspace_dir` - Agent workspace directory (contains `HEARTBEAT.md`)
/// * `lane_lock` - Shared lane lock with the session
/// * `llm_config` - LLM provider configuration
/// * `identity` - Agent identity text for system prompt
/// * `delivery` - Delivery handle for background output
/// * `delivery_channel` - Outbound channel for delivery (e.g. `bg`, `discord`)
/// * `delivery_to` - Outbound target (e.g. Discord channel ID)
pub fn spawn(
    workspace_dir: &Path,
    lane_lock: LaneLock,
    llm_config: LLMConfig,
    identity: String,
    delivery: DeliveryHandle,
    delivery_channel: String,
    delivery_to: String,
) -> HeartbeatHandle {
    let heartbeat_path = workspace_dir.join("HEARTBEAT.md");
    let mut memory = MemoryStore::new(&workspace_dir.join("memory"));
    memory.discover();

    let (interval, active_hours) = load_heartbeat_config(&heartbeat_path);

    log::info!(
        "Heartbeat started for {} (interval={}s, hours={}:00-{}:00)",
        heartbeat_path.display(),
        interval.as_secs(),
        active_hours.0,
        active_hours.1,
    );

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<HeartbeatCmd>(8);

    let mut runner = HeartbeatRunner {
        heartbeat_path,
        lane_lock,
        llm_config,
        identity,
        memory,
        delivery,
        delivery_channel,
        delivery_to,
        interval,
        active_hours,
        last_run_at: Some(SystemTime::now()),
        running: false,
        last_output: String::new(),
        output_count: 0,
        cached_content: None,
        cached_mtime: None,
        cached_body: None,
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(
            tuning().scheduler.heartbeat_poll_interval_ms,
        ));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let (ok, _) = runner.should_run();
                    if ok {
                        runner.execute().await;
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(HeartbeatCmd::Trigger(reply)) => {
                            let result = runner.trigger().await;
                            let _ = reply.send(result);
                        }
                        Some(HeartbeatCmd::Status(reply)) => {
                            let _ = reply.send(runner.status());
                        }
                        Some(HeartbeatCmd::Stop) | None => break,
                    }
                }
            }
        }
    });

    HeartbeatHandle { cmd_tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runner() -> HeartbeatRunner {
        HeartbeatRunner {
            heartbeat_path: PathBuf::from("/tmp/nonexistent_heartbeat.md"),
            lane_lock: std::sync::Arc::new(crate::scheduler::lane::CommandQueue::new()),
            llm_config: LLMConfig {
                model: String::new(),
                base_url: String::new(),
                api_key: String::new(),
                context_window: 0,
            },
            identity: String::new(),
            memory: MemoryStore::new(&PathBuf::from("/tmp/nonexistent")),
            delivery: DeliveryHandle::noop(),
            delivery_channel: "bg".into(),
            delivery_to: String::new(),
            interval: Duration::from_secs(tuning().scheduler.heartbeat_interval_secs),
            active_hours: tuning().scheduler.heartbeat_active_hours,
            last_run_at: None,
            running: false,
            last_output: String::new(),
            output_count: 0,
            cached_content: None,
            cached_mtime: None,
            cached_body: None,
        }
    }

    #[test]
    fn test_parse_response_ok() {
        assert!(HeartbeatRunner::parse_response("HEARTBEAT_OK").is_none());
        assert!(HeartbeatRunner::parse_response("HEARTBEAT_OK  ").is_none());
        assert_eq!(
            HeartbeatRunner::parse_response("HEARTBEAT_OK ok"),
            Some("ok".to_string())
        );
        assert!(
            HeartbeatRunner::parse_response("HEARTBEAT_OK something meaningful here").is_some()
        );
        assert!(HeartbeatRunner::parse_response("").is_none());
        assert_eq!(
            HeartbeatRunner::parse_response("important update"),
            Some("important update".to_string())
        );
    }

    #[test]
    fn test_should_run_no_file() {
        let mut runner = test_runner();
        let (ok, reason) = runner.should_run();
        assert!(!ok);
        assert!(reason.contains("not found"));
    }

    #[test]
    fn test_parse_heartbeat_config_defaults() {
        let meta = HashMap::new();
        let (interval, hours) = parse_heartbeat_config(&meta);
        assert_eq!(
            interval.as_secs(),
            tuning().scheduler.heartbeat_interval_secs
        );
        assert_eq!(hours, tuning().scheduler.heartbeat_active_hours);
    }

    #[test]
    fn test_parse_heartbeat_config_overrides() {
        let mut meta = HashMap::new();
        meta.insert("interval".into(), "600".into());
        meta.insert("active_hours".into(), "8, 23".into());
        let (interval, hours) = parse_heartbeat_config(&meta);
        assert_eq!(interval.as_secs(), 600);
        assert_eq!(hours, (8, 23));
    }

    #[test]
    fn test_parse_heartbeat_config_partial() {
        let mut meta = HashMap::new();
        meta.insert("interval".into(), "300".into());
        let (interval, hours) = parse_heartbeat_config(&meta);
        assert_eq!(interval.as_secs(), 300);
        assert_eq!(hours, tuning().scheduler.heartbeat_active_hours);
    }
}
