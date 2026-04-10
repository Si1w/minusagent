use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};

use crate::config::tuning;
use crate::engine::store::LLMConfig;
use crate::routing::delivery::DeliveryHandle;
use crate::scheduler::run_single_turn;

type UnixTime = i64;

/// A scheduled job definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub schedule: ScheduleConfig,
    pub payload: Payload,
    /// Delivery channel (e.g. "discord", "bg"). Defaults to "bg".
    #[serde(default = "default_channel")]
    pub channel: String,
    /// Delivery target (e.g. Discord channel ID). Defaults to empty.
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub delete_after_run: bool,
    #[serde(skip)]
    pub consecutive_errors: u32,
    #[serde(skip)]
    pub last_run_at: Option<UnixTime>,
    #[serde(skip)]
    pub next_run_at: Option<UnixTime>,
}

fn default_channel() -> String {
    "bg".to_string()
}

fn default_true() -> bool {
    true
}

/// Schedule configuration for a cron job
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub kind: String,
    #[serde(default)]
    pub expr: String,
    #[serde(default)]
    pub at: String,
    #[serde(default = "default_every")]
    pub every_seconds: u64,
    #[serde(default)]
    pub anchor: String,
}

fn default_every() -> u64 {
    3600
}

/// What to execute when a cron job fires
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payload {
    pub kind: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub text: String,
}

/// Job status snapshot for display
pub struct CronJobStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: String,
    pub errors: u32,
    pub last_run: String,
    pub next_run: String,
}

/// CRON.json file structure
#[derive(Debug, Serialize, Deserialize)]
struct CronFile {
    #[serde(default)]
    jobs: Vec<CronJob>,
}

/// Run log entry (appended to cron-runs.jsonl)
#[derive(Serialize)]
struct RunLogEntry {
    job_id: String,
    run_at: String,
    status: String,
    output_preview: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    error: String,
}

/// Commands sent to the cron task
enum CronCmd {
    TriggerJob(String, oneshot::Sender<String>),
    ListJobs(oneshot::Sender<Vec<CronJobStatus>>),
    CreateJob(Box<CronJob>, oneshot::Sender<String>),
    DeleteJob(String, oneshot::Sender<String>),
    Reload(oneshot::Sender<String>),
    Stop,
}

/// Handle for interacting with a running cron service
#[derive(Clone)]
pub struct CronHandle {
    runner: Arc<CronRunner>,
}

struct CronRunner {
    cmd_tx: Mutex<Option<mpsc::Sender<CronCmd>>>,
    generation: AtomicU64,
    cron_file: PathBuf,
    llm_config: LLMConfig,
    delivery: DeliveryHandle,
}

impl CronHandle {
    /// Manually trigger a cron job by ID
    pub async fn trigger_job(&self, job_id: &str) -> String {
        let Some(sender) = self.runner.sender() else {
            return "cron service not running".to_string();
        };
        let (tx, rx) = oneshot::channel();
        if sender
            .send(CronCmd::TriggerJob(job_id.to_string(), tx))
            .await
            .is_err()
        {
            self.runner.clear_closed_sender();
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// List all cron jobs with status
    pub async fn list_jobs(&self) -> Vec<CronJobStatus> {
        let Some(sender) = self.runner.sender() else {
            return Vec::new();
        };
        let (tx, rx) = oneshot::channel();
        if sender.send(CronCmd::ListJobs(tx)).await.is_err() {
            self.runner.clear_closed_sender();
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Create a new cron job (persists to CRON.json)
    pub async fn create_job(&self, job: CronJob) -> String {
        let Some(sender) = self.runner.sender() else {
            return "cron service not running".to_string();
        };
        let (tx, rx) = oneshot::channel();
        if sender
            .send(CronCmd::CreateJob(Box::new(job), tx))
            .await
            .is_err()
        {
            self.runner.clear_closed_sender();
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// Delete a cron job by ID (persists to CRON.json)
    pub async fn delete_job(&self, job_id: &str) -> String {
        let Some(sender) = self.runner.sender() else {
            return "cron service not running".to_string();
        };
        let (tx, rx) = oneshot::channel();
        if sender
            .send(CronCmd::DeleteJob(job_id.to_string(), tx))
            .await
            .is_err()
        {
            self.runner.clear_closed_sender();
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// Reload CRON.json
    pub async fn reload(&self) -> String {
        let Some(sender) = self.runner.sender() else {
            return "cron service not running".to_string();
        };
        let (tx, rx) = oneshot::channel();
        if sender.send(CronCmd::Reload(tx)).await.is_err() {
            self.runner.clear_closed_sender();
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// Stop the cron service
    pub async fn stop(&self) -> bool {
        let Some(sender) = self.runner.sender() else {
            return false;
        };
        if sender.send(CronCmd::Stop).await.is_err() {
            self.runner.clear_closed_sender();
            return false;
        }
        self.wait_for_stopped().await;
        !self.is_running()
    }

    /// Start the cron service if it is currently stopped.
    #[must_use]
    pub fn start(&self) -> bool {
        if self.is_running() {
            return false;
        }
        self.spawn_runner();
        true
    }

    /// Restart the cron service while keeping the shared handle stable.
    pub async fn restart(&self) -> bool {
        let _ = self.stop().await;
        self.start()
    }

    /// Whether the cron service is currently active.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.runner.sender().is_some()
    }

    fn spawn_runner(&self) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<CronCmd>(8);
        let generation = self.runner.install_sender(cmd_tx);

        let runner = Arc::clone(&self.runner);
        let cron_file = self.runner.cron_file.clone();
        let llm_config = self.runner.llm_config.clone();
        let delivery = self.runner.delivery.clone();
        let mut svc = CronService::new(cron_file, llm_config, delivery);
        log::info!("Cron service started with {} jobs", svc.jobs.len());

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(
                crate::config::tuning().scheduler.cron_poll_interval_ms,
            ));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        svc.tick().await;
                    }
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            Some(CronCmd::TriggerJob(id, reply)) => {
                                let result = match svc.trigger_job(&id) {
                                    Some(idx) => {
                                        let now = now_timestamp();
                                        svc.run_job(idx, now).await;
                                        let job = &svc.jobs[idx];
                                        format!(
                                            "'{}' triggered (errors={})",
                                            job.name, job.consecutive_errors
                                        )
                                    }
                                    None => format!("Job '{id}' not found"),
                                };
                                let _ = reply.send(result);
                            }
                            Some(CronCmd::ListJobs(reply)) => {
                                let _ = reply.send(svc.list_jobs());
                            }
                            Some(CronCmd::CreateJob(job, reply)) => {
                                let mut job = *job;
                                if svc.jobs.iter().any(|existing| existing.id == job.id) {
                                    let _ = reply.send(format!("Job '{}' already exists", job.id));
                                } else {
                                    let now = now_timestamp();
                                    job.next_run_at = compute_next(&job, now);
                                    let msg = format!("Created job '{}' ({})", job.name, job.schedule.kind);
                                    svc.jobs.push(job);
                                    svc.save_jobs();
                                    log::info!("{msg}");
                                    let _ = reply.send(msg);
                                }
                            }
                            Some(CronCmd::DeleteJob(id, reply)) => {
                                let before = svc.jobs.len();
                                svc.jobs.retain(|job| job.id != id);
                                if svc.jobs.len() < before {
                                    svc.save_jobs();
                                    let msg = format!("Deleted job '{id}'");
                                    log::info!("{msg}");
                                    let _ = reply.send(msg);
                                } else {
                                    let _ = reply.send(format!("Job '{id}' not found"));
                                }
                            }
                            Some(CronCmd::Reload(reply)) => {
                                svc.load_jobs();
                                let msg = format!("Reloaded {} jobs", svc.jobs.len());
                                log::info!("{msg}");
                                let _ = reply.send(msg);
                            }
                            Some(CronCmd::Stop) | None => {
                                log::info!("Cron service stopped");
                                break;
                            }
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

impl CronRunner {
    fn new(cron_file: PathBuf, llm_config: LLMConfig, delivery: DeliveryHandle) -> Self {
        Self {
            cmd_tx: Mutex::new(None),
            generation: AtomicU64::new(0),
            cron_file,
            llm_config,
            delivery,
        }
    }

    fn sender(&self) -> Option<mpsc::Sender<CronCmd>> {
        let mut guard = self.lock_cmd_tx();
        if guard.as_ref().is_some_and(mpsc::Sender::is_closed) {
            *guard = None;
        }
        guard.clone()
    }

    fn install_sender(&self, sender: mpsc::Sender<CronCmd>) -> u64 {
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

    fn lock_cmd_tx(&self) -> std::sync::MutexGuard<'_, Option<mpsc::Sender<CronCmd>>> {
        self.cmd_tx.lock().unwrap_or_else(|error| {
            log::error!("Cron runner lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }
}

/// Internal cron service (owned by its tokio task)
struct CronService {
    cron_file: PathBuf,
    run_log: PathBuf,
    jobs: Vec<CronJob>,
    llm_config: LLMConfig,
    delivery: DeliveryHandle,
}

impl CronService {
    fn new(cron_file: PathBuf, llm_config: LLMConfig, delivery: DeliveryHandle) -> Self {
        let run_log = cron_file
            .parent()
            .unwrap_or(Path::new("."))
            .join("cron-runs.jsonl");

        let mut svc = Self {
            cron_file,
            run_log,
            jobs: Vec::new(),
            llm_config,
            delivery,
        };
        svc.load_jobs();
        svc
    }

    fn load_jobs(&mut self) {
        self.jobs.clear();
        if !self.cron_file.exists() {
            return;
        }
        let content = match std::fs::read_to_string(&self.cron_file) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("CRON.json load error: {e}");
                return;
            }
        };
        let file: CronFile = match serde_json::from_str(&content) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("CRON.json parse error: {e}");
                return;
            }
        };

        let now = now_timestamp();
        for mut job in file.jobs {
            if !matches!(job.schedule.kind.as_str(), "at" | "every" | "cron") {
                log::warn!(
                    "Skipping job '{}': unknown kind '{}'",
                    job.id,
                    job.schedule.kind
                );
                continue;
            }
            job.next_run_at = compute_next(&job, now);
            log::info!("Loaded cron job '{}' ({})", job.name, job.schedule.kind);
            self.jobs.push(job);
        }
    }

    /// Persist current jobs back to CRON.json
    fn save_jobs(&self) {
        let file = CronFile {
            jobs: self.jobs.clone(),
        };
        match serde_json::to_string_pretty(&file) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.cron_file, json) {
                    log::warn!("Failed to save CRON.json: {e}");
                }
            }
            Err(e) => log::warn!("Failed to serialize cron jobs: {e}"),
        }
    }

    /// Check and execute due jobs (called every second)
    async fn tick(&mut self) {
        let now = now_timestamp();
        let mut remove_ids: Vec<String> = Vec::new();

        for i in 0..self.jobs.len() {
            let job = &self.jobs[i];
            if !job.enabled || job.next_run_at.is_none_or(|next_run_at| now < next_run_at) {
                continue;
            }
            log::info!("Running cron job '{}'", job.name);
            self.run_job(i, now).await;
            let job = &self.jobs[i];
            if job.delete_after_run && job.schedule.kind == "at" {
                remove_ids.push(job.id.clone());
            }
        }

        if !remove_ids.is_empty() {
            self.jobs.retain(|j| !remove_ids.contains(&j.id));
        }
    }

    async fn run_job(&mut self, idx: usize, now: UnixTime) {
        let job = &self.jobs[idx];
        let payload_kind = job.payload.kind.clone();
        let payload_message = job.payload.message.clone();
        let payload_text = job.payload.text.clone();
        let job_name = job.name.clone();
        let job_id = job.id.clone();
        let delivery_channel = job.channel.clone();
        let delivery_to = job.to.clone();

        let (output, status, error) = match payload_kind.as_str() {
            "agent_turn" => {
                if payload_message.is_empty() {
                    ("[empty message]".to_string(), "skipped", String::new())
                } else {
                    let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let sys_prompt = format!(
                        "You are performing a scheduled background task. \
                         Be concise. Current time: {now_str}"
                    );
                    match run_single_turn(&sys_prompt, &payload_message, &self.llm_config).await {
                        Ok(resp) => (resp, "ok", String::new()),
                        Err(e) => {
                            log::error!("Cron job '{job_name}' failed: {e}");
                            (format!("[cron error: {e}]"), "error", e.to_string())
                        }
                    }
                }
            }
            "system_event" => {
                if payload_text.is_empty() {
                    (String::new(), "skipped", String::new())
                } else {
                    (payload_text, "ok", String::new())
                }
            }
            _ => (
                format!("[unknown kind: {payload_kind}]"),
                "error",
                format!("unknown kind: {payload_kind}"),
            ),
        };

        let job = &mut self.jobs[idx];
        job.last_run_at = Some(now);

        if status == "error" {
            job.consecutive_errors += 1;
            if job.consecutive_errors >= tuning().scheduler.cron_auto_disable_threshold {
                job.enabled = false;
                let msg = format!(
                    "Job '{}' auto-disabled after {} consecutive errors: {}",
                    job_name, job.consecutive_errors, error
                );
                log::warn!("{msg}");
                self.delivery.enqueue("bg", "", &msg);
            }
        } else {
            job.consecutive_errors = 0;
        }

        job.next_run_at = compute_next(job, now);

        let entry = RunLogEntry {
            job_id,
            run_at: Utc::now().to_rfc3339(),
            status: status.to_string(),
            output_preview: output.chars().take(200).collect(),
            error,
        };
        if let Ok(line) = serde_json::to_string(&entry) {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.run_log)
                .and_then(|mut f| {
                    use std::io::Write;
                    writeln!(f, "{line}")
                });
        }

        if !output.is_empty() && status != "skipped" {
            self.delivery.enqueue(
                &delivery_channel,
                &delivery_to,
                &cron_delivery_message(&job_name, &output),
            );
        }
    }

    fn trigger_job(&mut self, job_id: &str) -> Option<usize> {
        self.jobs.iter().position(|j| j.id == job_id)
    }

    fn list_jobs(&self) -> Vec<CronJobStatus> {
        self.jobs
            .iter()
            .map(|j| CronJobStatus {
                id: j.id.clone(),
                name: j.name.clone(),
                enabled: j.enabled,
                kind: j.schedule.kind.clone(),
                errors: j.consecutive_errors,
                last_run: format_optional_timestamp(j.last_run_at, "never"),
                next_run: format_optional_timestamp(j.next_run_at, "n/a"),
            })
            .collect()
    }
}

/// Compute the next run timestamp for a job
fn compute_next(job: &CronJob, now: UnixTime) -> Option<UnixTime> {
    let cfg = &job.schedule;
    match cfg.kind.as_str() {
        "at" => parse_rfc3339_timestamp(&cfg.at).filter(|timestamp| *timestamp > now),
        "every" => compute_every_next(cfg, now),
        "cron" => {
            if cfg.expr.is_empty() {
                return None;
            }
            Schedule::from_str(&cfg.expr).ok().and_then(|s| {
                let dt = DateTime::from_timestamp(now, 0)?;
                s.after(&dt).next().map(|t| t.timestamp())
            })
        }
        _ => None,
    }
}

/// Spawn a cron service task and return its handle
///
/// # Arguments
///
/// * `cron_file` - Path to CRON.json
/// * `llm_config` - LLM provider configuration for `agent_turn` jobs
/// * `delivery` - Delivery handle for background output
#[must_use]
pub fn spawn(cron_file: PathBuf, llm_config: LLMConfig, delivery: DeliveryHandle) -> CronHandle {
    let handle = CronHandle {
        runner: Arc::new(CronRunner::new(cron_file, llm_config, delivery)),
    };
    let _ = handle.start();
    handle
}

fn parse_rfc3339_timestamp(value: &str) -> Option<UnixTime> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

fn compute_every_next(cfg: &ScheduleConfig, now: UnixTime) -> Option<UnixTime> {
    let every = i64::try_from(cfg.every_seconds).ok()?;
    if every <= 0 {
        return None;
    }

    let anchor = parse_rfc3339_timestamp(&cfg.anchor).unwrap_or(now);
    if now < anchor {
        return Some(anchor);
    }

    let steps = now.saturating_sub(anchor) / every + 1;
    anchor.checked_add(steps.checked_mul(every)?)
}

fn format_timestamp(ts: UnixTime) -> String {
    DateTime::from_timestamp(ts, 0).map_or_else(
        || "unknown".into(),
        |dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S").to_string(),
    )
}

fn format_optional_timestamp(timestamp: Option<UnixTime>, fallback: &str) -> String {
    timestamp.map_or_else(|| fallback.to_string(), format_timestamp)
}

fn now_timestamp() -> UnixTime {
    Utc::now().timestamp()
}

fn cron_delivery_message(job_name: &str, output: &str) -> String {
    format!("[{job_name}] {output}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_job(kind: &str) -> CronJob {
        CronJob {
            id: "test".into(),
            name: "Test".into(),
            enabled: true,
            schedule: ScheduleConfig {
                kind: kind.into(),
                expr: String::new(),
                at: String::new(),
                every_seconds: 3600,
                anchor: String::new(),
            },
            payload: Payload {
                kind: "agent_turn".into(),
                message: "hello".into(),
                text: String::new(),
            },
            channel: "bg".into(),
            to: String::new(),
            delete_after_run: false,
            consecutive_errors: 0,
            last_run_at: None,
            next_run_at: None,
        }
    }

    #[test]
    fn test_compute_next_at_future() {
        let mut job = test_job("at");
        job.schedule.at = "2099-01-01T00:00:00Z".into();
        let now = now_timestamp();
        assert!(compute_next(&job, now).is_some_and(|next| next > now));
    }

    #[test]
    fn test_compute_next_at_past() {
        let mut job = test_job("at");
        job.schedule.at = "2000-01-01T00:00:00Z".into();
        assert_eq!(compute_next(&job, now_timestamp()), None);
    }

    #[test]
    fn test_compute_next_every() {
        let now = now_timestamp();
        let anchor = now - 100;
        let anchor_str = DateTime::from_timestamp(anchor, 0).unwrap().to_rfc3339();
        let mut job = test_job("every");
        job.schedule.every_seconds = 60;
        job.schedule.anchor = anchor_str;

        let next = compute_next(&job, now).unwrap();
        assert!(next > now);
        assert!(next <= now + 60);
    }

    #[test]
    fn test_compute_next_cron() {
        let mut job = test_job("cron");
        job.schedule.expr = "0 0 9 * * * *".into();
        assert!(compute_next(&job, now_timestamp()).is_some());
    }

    #[test]
    fn test_load_nonexistent() {
        let svc = CronService::new(
            PathBuf::from("/tmp/nonexistent_cron.json"),
            LLMConfig {
                model: String::new(),
                base_url: String::new(),
                api_key: String::new(),
                context_window: 0,
            },
            DeliveryHandle::noop(),
        );
        assert!(svc.jobs.is_empty());
    }

    #[tokio::test]
    async fn test_cron_handle_restart_preserves_clones() {
        let dir = tempfile::tempdir().unwrap();
        let handle = spawn(
            dir.path().join("CRON.json"),
            LLMConfig {
                model: String::new(),
                base_url: String::new(),
                api_key: String::new(),
                context_window: 0,
            },
            DeliveryHandle::noop(),
        );
        let clone = handle.clone();

        assert!(handle.is_running());
        assert!(handle.stop().await);
        assert!(!clone.is_running());

        assert!(clone.start());
        assert!(handle.is_running());
        assert!(handle.list_jobs().await.is_empty());

        assert!(handle.restart().await);
        assert!(clone.is_running());
        assert!(clone.list_jobs().await.is_empty());
    }
}
