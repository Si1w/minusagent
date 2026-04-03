use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{Local, Timelike, Utc};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::config::tuning;
use crate::engine::store::LLMConfig;
use crate::intelligence::memory::MemoryStore;
use crate::intelligence::prompt::format_memory_content;
use crate::intelligence::utils::{extract_body, parse_frontmatter};
use crate::routing::delivery::DeliveryHandle;
use crate::scheduler::{LANE_SESSION, LaneLock, now_secs, run_single_turn};

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Parse heartbeat-specific overrides from HEARTBEAT.md frontmatter
///
/// Supported keys:
/// - `interval`: seconds between runs (e.g. `600`)
/// - `active_hours`: comma-separated start,end (e.g. `8, 23`)
///
/// Returns `(interval, active_hours)` using tuning defaults for missing keys.
fn parse_heartbeat_config(meta: &HashMap<String, String>) -> (Duration, (u8, u8)) {
    let interval = meta
        .get("interval")
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(tuning().heartbeat_interval_secs));

    let active_hours = meta
        .get("active_hours")
        .and_then(|v| {
            let parts: Vec<&str> = v.split(',').collect();
            if parts.len() == 2 {
                let s = parts[0].trim().parse::<u8>().ok()?;
                let e = parts[1].trim().parse::<u8>().ok()?;
                Some((s, e))
            } else {
                None
            }
        })
        .unwrap_or(tuning().heartbeat_active_hours);

    (interval, active_hours)
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
    last_run_at: f64,
    running: bool,
    last_output: String,
    output_count: usize,
    /// Cached raw content from the last successful read of HEARTBEAT.md
    cached_content: Option<String>,
}

impl HeartbeatRunner {
    /// Read HEARTBEAT.md once, update config and cache content.
    /// Returns the body text, or None if unreadable/empty.
    fn refresh_file(&mut self) -> Option<String> {
        let raw = match std::fs::read_to_string(&self.heartbeat_path) {
            Ok(c) => c,
            Err(_) => return None,
        };
        let meta = parse_frontmatter(&raw);
        let (interval, active_hours) = parse_heartbeat_config(&meta);
        self.interval = interval;
        self.active_hours = active_hours;
        let body = extract_body(&raw);
        if body.trim().is_empty() {
            self.cached_content = None;
            return None;
        }
        self.cached_content = Some(raw);
        Some(body)
    }

    /// Check 4 preconditions for running. Lock check is separate in execute().
    fn should_run(&mut self) -> (bool, String) {
        if !self.heartbeat_path.exists() {
            return (false, "HEARTBEAT.md not found".into());
        }
        let body = match self.refresh_file() {
            Some(b) => b,
            None => return (false, "HEARTBEAT.md body is empty".into()),
        };
        let _ = body;

        let now = now_secs();
        let elapsed = now - self.last_run_at;
        if elapsed < self.interval.as_secs_f64() {
            let remaining = self.interval.as_secs_f64() - elapsed;
            return (
                false,
                format!("interval not elapsed ({remaining:.0}s remaining)"),
            );
        }

        let hour = Local::now().hour() as u8;
        let (s, e) = self.active_hours;
        let in_hours = if s <= e {
            s <= hour && hour < e
        } else {
            !(e <= hour && hour < s)
        };
        if !in_hours {
            return (
                false,
                format!("outside active hours ({s}:00-{e}:00)"),
            );
        }

        if self.running {
            return (false, "already running".into());
        }
        (true, "all checks passed".into())
    }

    /// Parse heartbeat response. HEARTBEAT_OK means nothing to report.
    fn parse_response(&self, response: &str) -> Option<String> {
        if response.contains("HEARTBEAT_OK") {
            let stripped =
                response.replace("HEARTBEAT_OK", "").trim().to_string();
            if stripped.is_empty() { None } else { Some(stripped) }
        } else {
            let trimmed = response.trim().to_string();
            if trimmed.is_empty() { None } else { Some(trimmed) }
        }
    }

    /// Build system prompt and instruction for heartbeat turn
    fn build_prompt(&mut self) -> Option<(String, String)> {
        let raw = self.cached_content.as_ref().cloned()
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
            prompt.push_str(&format!("\n\n# Known Context\n\n{mem_block}"));
        }

        let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
        prompt.push_str(&format!("\n\nCurrent time: {now}"));

        Some((instructions, prompt))
    }

    /// Execute one heartbeat. Skip if session lane is active.
    async fn execute(&mut self) {
        let stats = self.lane_lock.lane_stats(LANE_SESSION).await;
        if stats.map_or(false, |s| s.active > 0) {
            log::debug!("Heartbeat skipped: session lane occupied");
            return;
        }

        self.lane_lock.mark_active(LANE_SESSION).await;
        self.running = true;
        log::debug!("Heartbeat executing");

        let result = self.execute_inner().await;

        self.lane_lock.mark_done(LANE_SESSION).await;
        self.running = false;
        self.last_run_at = now_secs();

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
        let (instructions, sys_prompt) = match self.build_prompt() {
            Some(p) => p,
            None => return Ok(()),
        };

        let response =
            run_single_turn(&sys_prompt, &instructions, &self.llm_config)
                .await?;
        let meaningful = match self.parse_response(&response) {
            Some(m) => m,
            None => return Ok(()),
        };

        if meaningful.trim() == self.last_output {
            return Ok(());
        }
        self.last_output = meaningful.trim().to_string();
        self.output_count += 1;
        self.delivery.enqueue(
            &self.delivery_channel,
            &self.delivery_to,
            &format!("[heartbeat] {meaningful}"),
        );
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
                match run_single_turn(
                    &sys_prompt,
                    &instructions,
                    &self.llm_config,
                )
                .await
                {
                    Ok(response) => match self.parse_response(&response) {
                        None => {
                            "HEARTBEAT_OK (nothing to report)".to_string()
                        }
                        Some(m) if m.trim() == self.last_output => {
                            "duplicate content (skipped)".to_string()
                        }
                        Some(m) => {
                            self.last_output = m.trim().to_string();
                            self.output_count += 1;
                            let len = m.len();
                            self.delivery.enqueue(
                                &self.delivery_channel,
                                &self.delivery_to,
                                &format!("[heartbeat] {m}"),
                            );
                            format!("triggered, output queued ({len} chars)")
                        }
                    },
                    Err(e) => format!("trigger failed: {e}"),
                }
            }
            None => "HEARTBEAT.md is empty or unreadable".to_string(),
        };

        self.running = false;
        self.last_run_at = now_secs();
        result
    }

    fn status(&mut self) -> HeartbeatStatus {
        let now = now_secs();
        let elapsed = if self.last_run_at > 0.0 {
            Some(now - self.last_run_at)
        } else {
            None
        };
        let next_in = elapsed
            .map(|e| (self.interval.as_secs_f64() - e).max(0.0))
            .unwrap_or(self.interval.as_secs_f64());
        let (ok, reason) = self.should_run();

        HeartbeatStatus {
            enabled: self.heartbeat_path.exists(),
            running: self.running,
            should_run: ok,
            reason,
            last_run: if self.last_run_at > 0.0 {
                chrono::DateTime::from_timestamp(
                    self.last_run_at as i64,
                    0,
                )
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "unknown".into())
            } else {
                "never".into()
            },
            next_in: format!("{next_in:.0}s"),
            interval_secs: self.interval.as_secs_f64(),
            active_hours: self.active_hours,
            queue_size: self.output_count,
        }
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
/// * `workspace_dir` - Agent workspace directory (contains HEARTBEAT.md)
/// * `lane_lock` - Shared lane lock with the session
/// * `llm_config` - LLM provider configuration
/// * `identity` - Agent identity text for system prompt
/// * `delivery` - Delivery handle for background output
/// * `delivery_channel` - Outbound channel for delivery (e.g. "bg", "discord")
/// * `delivery_to` - Outbound target (e.g. Discord channel ID)
pub fn spawn(
    workspace_dir: PathBuf,
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

    let (interval, active_hours) = std::fs::read_to_string(&heartbeat_path)
        .ok()
        .map(|raw| parse_heartbeat_config(&parse_frontmatter(&raw)))
        .unwrap_or_else(|| {
            (
                Duration::from_secs(tuning().heartbeat_interval_secs),
                tuning().heartbeat_active_hours,
            )
        });

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
        last_run_at: now_secs(),
        running: false,
        last_output: String::new(),
        output_count: 0,
        cached_content: None,
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL_INTERVAL);
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
            interval: Duration::from_secs(tuning().heartbeat_interval_secs),
            active_hours: tuning().heartbeat_active_hours,
            last_run_at: 0.0,
            running: false,
            last_output: String::new(),
            output_count: 0,
            cached_content: None,
        }
    }

    #[test]
    fn test_parse_response_ok() {
        let runner = test_runner();
        assert!(runner.parse_response("HEARTBEAT_OK").is_none());
        assert!(runner.parse_response("HEARTBEAT_OK  ").is_none());
        assert_eq!(
            runner.parse_response("HEARTBEAT_OK ok"),
            Some("ok".to_string())
        );
        assert!(runner
            .parse_response("HEARTBEAT_OK something meaningful here")
            .is_some());
        assert!(runner.parse_response("").is_none());
        assert_eq!(
            runner.parse_response("important update"),
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
        assert_eq!(interval.as_secs(), tuning().heartbeat_interval_secs);
        assert_eq!(hours, tuning().heartbeat_active_hours);
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
        assert_eq!(hours, tuning().heartbeat_active_hours);
    }
}
