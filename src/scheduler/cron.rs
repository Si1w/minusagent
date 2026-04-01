use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::core::store::LLMConfig;
use crate::routing::delivery::DeliveryHandle;
use crate::config::tuning;
use crate::scheduler::{now_secs, run_single_turn};

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
    pub last_run_at: f64,
    #[serde(skip)]
    pub next_run_at: f64,
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
#[derive(Debug, Deserialize)]
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
    Reload(oneshot::Sender<String>),
    Stop,
}

/// Handle for interacting with a running cron service
#[derive(Clone)]
pub struct CronHandle {
    cmd_tx: mpsc::Sender<CronCmd>,
}

impl CronHandle {
    /// Manually trigger a cron job by ID
    pub async fn trigger_job(&self, job_id: &str) -> String {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_tx
            .send(CronCmd::TriggerJob(job_id.to_string(), tx))
            .await
            .is_err()
        {
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// List all cron jobs with status
    pub async fn list_jobs(&self) -> Vec<CronJobStatus> {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(CronCmd::ListJobs(tx)).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Reload CRON.json
    pub async fn reload(&self) -> String {
        let (tx, rx) = oneshot::channel();
        if self.cmd_tx.send(CronCmd::Reload(tx)).await.is_err() {
            return "cron service not running".to_string();
        }
        rx.await.unwrap_or_else(|_| "channel closed".to_string())
    }

    /// Stop the cron service
    pub fn stop(&self) {
        let _ = self.cmd_tx.try_send(CronCmd::Stop);
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
    fn new(
        cron_file: PathBuf,
        llm_config: LLMConfig,
        delivery: DeliveryHandle,
    ) -> Self {
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

        let now = now_secs();
        for mut job in file.jobs {
            if !matches!(
                job.schedule.kind.as_str(),
                "at" | "every" | "cron"
            ) {
                log::warn!("Skipping job '{}': unknown kind '{}'", job.id, job.schedule.kind);
                continue;
            }
            job.next_run_at = compute_next(&job, now);
            log::info!("Loaded cron job '{}' ({})", job.name, job.schedule.kind);
            self.jobs.push(job);
        }
    }

    /// Check and execute due jobs (called every second)
    async fn tick(&mut self) {
        let now = now_secs();
        let mut remove_ids: Vec<String> = Vec::new();

        for i in 0..self.jobs.len() {
            let job = &self.jobs[i];
            if !job.enabled || job.next_run_at <= 0.0 || now < job.next_run_at
            {
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

    async fn run_job(&mut self, idx: usize, now: f64) {
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
                    let now_str =
                        Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let sys_prompt = format!(
                        "You are performing a scheduled background task. \
                         Be concise. Current time: {now_str}"
                    );
                    match run_single_turn(
                        &sys_prompt,
                        &payload_message,
                        &self.llm_config,
                    )
                    .await
                    {
                        Ok(resp) => (resp, "ok", String::new()),
                        Err(e) => {
                            log::error!("Cron job '{}' failed: {e}", job_name);
                            (
                                format!("[cron error: {e}]"),
                                "error",
                                e.to_string(),
                            )
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
        job.last_run_at = now;

        if status == "error" {
            job.consecutive_errors += 1;
            if job.consecutive_errors >= tuning().cron_auto_disable_threshold {
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
                &format!("[{job_name}] {output}"),
            );
        }
    }

    fn trigger_job(&mut self, job_id: &str) -> Option<usize> {
        self.jobs.iter().position(|j| j.id == job_id)
    }

    fn list_jobs(&self) -> Vec<CronJobStatus> {
        self.jobs
            .iter()
            .map(|j| {
                CronJobStatus {
                    id: j.id.clone(),
                    name: j.name.clone(),
                    enabled: j.enabled,
                    kind: j.schedule.kind.clone(),
                    errors: j.consecutive_errors,
                    last_run: if j.last_run_at > 0.0 {
                        format_timestamp(j.last_run_at)
                    } else {
                        "never".into()
                    },
                    next_run: if j.next_run_at > 0.0 {
                        format_timestamp(j.next_run_at)
                    } else {
                        "n/a".into()
                    },
                }
            })
            .collect()
    }
}

/// Compute the next run timestamp for a job
fn compute_next(job: &CronJob, now: f64) -> f64 {
    let cfg = &job.schedule;
    match cfg.kind.as_str() {
        "at" => {
            let ts = DateTime::parse_from_rfc3339(&cfg.at)
                .map(|dt| dt.timestamp() as f64)
                .unwrap_or(0.0);
            if ts > now { ts } else { 0.0 }
        }
        "every" => {
            let every = cfg.every_seconds as f64;
            let anchor = DateTime::parse_from_rfc3339(&cfg.anchor)
                .map(|dt| dt.timestamp() as f64)
                .unwrap_or(now);
            if now < anchor {
                return anchor;
            }
            let steps = ((now - anchor) / every) as u64 + 1;
            anchor + steps as f64 * every
        }
        "cron" => {
            if cfg.expr.is_empty() {
                return 0.0;
            }
            Schedule::from_str(&cfg.expr)
                .ok()
                .and_then(|s| {
                    let dt = DateTime::from_timestamp(now as i64, 0)?;
                    s.after(&dt).next().map(|t| t.timestamp() as f64)
                })
                .unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// Spawn a cron service task and return its handle
///
/// # Arguments
///
/// * `cron_file` - Path to CRON.json
/// * `llm_config` - LLM provider configuration for agent_turn jobs
/// * `delivery` - Delivery handle for background output
pub fn spawn(
    cron_file: PathBuf,
    llm_config: LLMConfig,
    delivery: DeliveryHandle,
) -> CronHandle {
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<CronCmd>(8);

    let mut svc = CronService::new(cron_file, llm_config, delivery);
    log::info!("Cron service started with {} jobs", svc.jobs.len());

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
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
                                    let now = now_secs();
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
                        Some(CronCmd::Reload(reply)) => {
                            svc.load_jobs();
                            let msg = format!(
                                "Reloaded {} jobs",
                                svc.jobs.len()
                            );
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
    });

    CronHandle { cmd_tx }
}

fn format_timestamp(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .map(|dt: DateTime<Utc>| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| "unknown".into())
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
            last_run_at: 0.0,
            next_run_at: 0.0,
        }
    }

    #[test]
    fn test_compute_next_at_future() {
        let mut job = test_job("at");
        job.schedule.at = "2099-01-01T00:00:00Z".into();
        let now = now_secs();
        assert!(compute_next(&job, now) > now);
    }

    #[test]
    fn test_compute_next_at_past() {
        let mut job = test_job("at");
        job.schedule.at = "2000-01-01T00:00:00Z".into();
        assert_eq!(compute_next(&job, now_secs()), 0.0);
    }

    #[test]
    fn test_compute_next_every() {
        let now = now_secs();
        let anchor = now - 100.0;
        let anchor_str = DateTime::from_timestamp(anchor as i64, 0)
            .unwrap()
            .to_rfc3339();
        let mut job = test_job("every");
        job.schedule.every_seconds = 60;
        job.schedule.anchor = anchor_str;

        let next = compute_next(&job, now);
        assert!(next > now);
        assert!(next <= now + 60.0);
    }

    #[test]
    fn test_compute_next_cron() {
        let mut job = test_job("cron");
        job.schedule.expr = "0 0 9 * * * *".into();
        assert!(compute_next(&job, now_secs()) > 0.0);
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
}
