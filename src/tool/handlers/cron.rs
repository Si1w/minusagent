use std::fmt::Write;

use anyhow::Result;
use serde::Deserialize;

use crate::scheduler::cron::{CronJob, Payload, ScheduleConfig};

use super::super::ToolContext;
use super::common::{parse_request, require_cron};

#[derive(Default, Deserialize)]
struct CronCreateRequest {
    id: String,
    name: String,
    #[serde(default = "default_bg_channel")]
    channel: String,
    #[serde(default)]
    delete_after_run: bool,
    #[serde(flatten)]
    schedule: CronScheduleRequest,
    #[serde(flatten)]
    payload: CronPayloadRequest,
}

#[derive(Default, Deserialize)]
struct CronScheduleRequest {
    #[serde(default = "default_every_schedule")]
    schedule_kind: String,
    #[serde(default)]
    cron_expr: String,
    #[serde(default)]
    at: String,
    #[serde(default = "default_every_seconds")]
    every_seconds: u64,
}

#[derive(Default, Deserialize)]
struct CronPayloadRequest {
    #[serde(default)]
    payload_kind: String,
    message: String,
}

#[derive(Default, Deserialize)]
struct CronDeleteRequest {
    job_id: String,
}

pub(super) async fn handle(ctx: &mut ToolContext<'_>) -> Result<bool> {
    match ctx.name {
        "cron_list" => handle_cron_list(ctx).await,
        "cron_create" => {
            let Some(request) = parse_request::<CronCreateRequest>(ctx) else {
                return Ok(true);
            };
            handle_cron_create(ctx, request).await;
        }
        "cron_delete" => {
            let Some(request) = parse_request::<CronDeleteRequest>(ctx) else {
                return Ok(true);
            };
            handle_cron_delete(ctx, request).await;
        }
        _ => return Ok(false),
    }

    Ok(true)
}

async fn handle_cron_list(ctx: &mut ToolContext<'_>) {
    let Some(handle) = require_cron(ctx) else {
        return;
    };

    let jobs = handle.list_jobs().await;
    if jobs.is_empty() {
        ctx.push_result("No cron jobs.");
        return;
    }

    let mut output = String::from("Cron jobs:\n");
    for job in &jobs {
        let status = if job.enabled { "enabled" } else { "disabled" };
        let _ = writeln!(
            output,
            "  {} [{}] {} ({}) errors={} next={}",
            job.id, status, job.name, job.kind, job.errors, job.next_run,
        );
    }
    ctx.push_result(output);
}

async fn handle_cron_create(ctx: &mut ToolContext<'_>, request: CronCreateRequest) {
    let Some(handle) = require_cron(ctx) else {
        return;
    };

    let job = CronJob {
        id: request.id,
        name: request.name,
        enabled: true,
        schedule: build_schedule(request.schedule),
        payload: build_payload(request.payload),
        channel: request.channel,
        to: String::new(),
        delete_after_run: request.delete_after_run,
        consecutive_errors: 0,
        last_run_at: None,
        next_run_at: None,
    };
    ctx.push_result(handle.create_job(job).await);
}

async fn handle_cron_delete(ctx: &mut ToolContext<'_>, request: CronDeleteRequest) {
    let Some(handle) = require_cron(ctx) else {
        return;
    };
    ctx.push_result(handle.delete_job(&request.job_id).await);
}

fn build_payload(request: CronPayloadRequest) -> Payload {
    if request.payload_kind == "system_event" {
        Payload {
            kind: "system_event".into(),
            message: String::new(),
            text: request.message,
        }
    } else {
        Payload {
            kind: "agent_turn".into(),
            message: request.message,
            text: String::new(),
        }
    }
}

fn build_schedule(request: CronScheduleRequest) -> ScheduleConfig {
    ScheduleConfig {
        kind: request.schedule_kind,
        expr: request.cron_expr,
        at: request.at,
        every_seconds: request.every_seconds,
        anchor: String::new(),
    }
}

fn default_bg_channel() -> String {
    "bg".to_string()
}

fn default_every_schedule() -> String {
    "every".to_string()
}

fn default_every_seconds() -> u64 {
    3600
}
