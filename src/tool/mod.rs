mod exec;
mod search;
mod schema;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::engine::agent::run_subagent;
use crate::engine::node::Node;
use crate::engine::store::{Message, Role, SharedStore};
use crate::frontend::Channel;
use crate::intelligence::manager::normalize_agent_id;
use crate::scheduler::cron::{CronJob, Payload, ScheduleConfig};
use crate::team::{BackgroundStatus, TaskStatus, TodoItem, TodoWrite, WorktreeStatus};

pub use schema::all_tools_filtered;

/// Tool definition for LLM function calling registration
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: ToolFunction,
}

/// Function schema within a tool definition
#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Dispatch a tool call by name
///
/// # Arguments
///
/// * `name` - Tool name from LLM response
/// * `call_id` - Unique tool call ID
/// * `arguments` - JSON string of tool arguments
/// * `store` - Shared store for reading/writing context
/// * `channel` - Channel for user confirmation (bash only)
///
/// # Returns
///
/// `true` if the tool was found and executed, `false` if unknown.
///
/// # Errors
///
/// Returns error on argument parsing or tool execution failure.
pub async fn dispatch_tool(
    name: &str,
    call_id: String,
    arguments: &str,
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
) -> Result<bool> {
    let args: serde_json::Value = serde_json::from_str(arguments)?;

    // Enforce tool policy: deny tools that are explicitly blocked
    if store.state.tool_policy.is_denied(name) {
        store.context.history.push(Message {
            role: Role::Tool,
            content: Some(format!("Error: tool '{name}' is not allowed for this agent.")),
            tool_calls: None,
            tool_call_id: Some(call_id),
        });
        return Ok(true);
    }

    match name {
        "bash" => {
            let command = args["command"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            if !channel.confirm(&command).await {
                store.context.history.push(Message {
                    role: Role::Tool,
                    content: Some("User denied execution.".into()),
                    tool_calls: None,
                    tool_call_id: Some(call_id),
                });
                return Ok(true);
            }

            let disable_sandbox = args
                .get("dangerously_disable_sandbox")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let timeout_secs = args
                .get("timeout")
                .and_then(|v| v.as_u64());
            let node = exec::BashExec {
                call_id: call_id.clone(),
                command,
                sandbox: !disable_sandbox,
                timeout_secs,
                current_dir: None,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "read_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = exec::ReadFile {
                call_id: call_id.clone(),
                path,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "write_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let content = args["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = exec::WriteFile {
                call_id: call_id.clone(),
                path,
                content,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "edit_file" => {
            let path = args["path"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let old_string = args["old_string"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let new_string = args["new_string"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = exec::EditFile {
                call_id: call_id.clone(),
                path,
                old_string,
                new_string,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "glob" => {
            let pattern = args["pattern"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let directory = args.get("directory")
                .and_then(|v| v.as_str())
                .map(String::from);
            let node = search::GlobFile {
                call_id: call_id.clone(),
                pattern,
                directory,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "grep" => {
            let pattern = args["pattern"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let path = args.get("path")
                .and_then(|v| v.as_str())
                .map(String::from);
            let include = args.get("include")
                .and_then(|v| v.as_str())
                .map(String::from);
            let node = search::GrepFile {
                call_id: call_id.clone(),
                pattern,
                path,
                include,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "todo" => {
            let raw = args.get("items").cloned()
                .unwrap_or(serde_json::Value::Array(Vec::new()));
            let items: Vec<TodoItem> = match serde_json::from_value(
                if raw.is_null() {
                    serde_json::Value::Array(Vec::new())
                } else {
                    raw
                },
            ) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("todo: failed to parse items: {e}");
                    push_tool_result(
                        store,
                        &call_id,
                        format!("Error: invalid items JSON: {e}"),
                    );
                    return Ok(true);
                }
            };
            let node = TodoWrite {
                call_id: call_id.clone(),
                items,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(store, &call_id, format!("Error: {e}"));
            }
        }
        "background_run" => {
            let command = args["command"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            if exec::is_dangerous_command(&command) {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: command blocked (dangerous pattern)".into(),
                );
            } else if !channel.confirm(&command).await {
                push_tool_result(
                    store,
                    &call_id,
                    "User denied execution.".into(),
                );
            } else {
                let task_id = store.state.background.run(&command);
                push_tool_result(
                    store,
                    &call_id,
                    format!(
                        "Background task {task_id} started: {command}"
                    ),
                );
            }
        }
        "background_check" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str());
            match task_id {
                Some(id) => match store.state.background.get(id) {
                    Some(task) => {
                        let status = match task.status {
                            BackgroundStatus::Running => "running",
                            BackgroundStatus::Completed => "completed",
                            BackgroundStatus::Failed => "failed",
                        };
                        let output = task
                            .output
                            .as_deref()
                            .unwrap_or("(still running)");
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Task {id}: {status}\n\
                                 Command: {}\n\
                                 Output:\n{output}",
                                task.command,
                            ),
                        );
                    }
                    None => {
                        push_tool_result(
                            store,
                            &call_id,
                            format!("Error: background task '{id}' not found"),
                        );
                    }
                },
                None => {
                    let tasks = store.state.background.list();
                    if tasks.is_empty() {
                        push_tool_result(
                            store,
                            &call_id,
                            "No background tasks.".into(),
                        );
                    } else {
                        let lines: Vec<String> = tasks
                            .iter()
                            .map(|t| {
                                let status = match t.status {
                                    BackgroundStatus::Running => {
                                        "running"
                                    }
                                    BackgroundStatus::Completed => {
                                        "completed"
                                    }
                                    BackgroundStatus::Failed => "failed",
                                };
                                format!(
                                    "  {} [{}] {}",
                                    t.id, status, t.command,
                                )
                            })
                            .collect();
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Background tasks:\n{}",
                                lines.join("\n")
                            ),
                        );
                    }
                }
            }
        }
        "task_create" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let subject = args["subject"]
                        .as_str()
                        .unwrap_or_default();
                    let description = args["description"]
                        .as_str()
                        .unwrap_or_default();
                    match mgr.create(subject, description) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_update" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id = args["task_id"].as_u64().unwrap_or(0) as usize;
                    let status = args["status"].as_str().and_then(|s| {
                        serde_json::from_value::<TaskStatus>(
                            serde_json::Value::String(s.to_string()),
                        )
                        .ok()
                    });
                    let blocked_by: Option<Vec<usize>> = args
                        .get("blocked_by")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_u64().map(|n| n as usize))
                                .collect()
                        });
                    let blocks: Option<Vec<usize>> = args
                        .get("blocks")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_u64().map(|n| n as usize))
                                .collect()
                        });
                    match mgr.update(task_id, status, blocked_by, blocks) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_list" => {
            match &store.state.tasks {
                Some(mgr) => match mgr.list_all() {
                    Ok(json) => push_tool_result(store, &call_id, json),
                    Err(e) => push_tool_result(
                        store,
                        &call_id,
                        format!("Error: {e}"),
                    ),
                },
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task_get" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id = args["task_id"].as_u64().unwrap_or(0) as usize;
                    match mgr.get(task_id) {
                        Ok(json) => push_tool_result(store, &call_id, json),
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "task" => {
            if store.state.is_subagent {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: task tool is not available here".into(),
                );
            } else {
                let prompt = args["prompt"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let agent_id = normalize_agent_id(
                    args["agent"].as_str().unwrap_or_default(),
                );
                let agent_config = store.state.agents.get(&agent_id);
                match agent_config {
                    Some(config) => {
                        let ws_dir = if config.workspace_dir.is_empty() {
                            None
                        } else {
                            Some(PathBuf::from(&config.workspace_dir))
                        };
                        let llm_config = store.state.config.llm.clone();
                        let denied = config.denied_tools.clone();
                        let summary = run_subagent(
                            prompt,
                            config.system_prompt.clone(),
                            llm_config,
                            ws_dir,
                            agent_id,
                            store.state.agents.clone(),
                            store.state.tasks.clone(),
                            denied,
                        )
                        .await?;
                        push_tool_result(store, &call_id, summary);
                    }
                    None => {
                        let available: Vec<String> = store
                            .state
                            .agents
                            .list()
                            .iter()
                            .map(|a| a.id.clone())
                            .collect();
                        push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: unknown agent '{}'. Available: {}",
                                agent_id,
                                available.join(", ")
                            ),
                        );
                    }
                }
            }
        }
        "claim_task" => {
            match &store.state.tasks {
                Some(mgr) => {
                    let task_id =
                        args["task_id"].as_u64().unwrap_or(0)
                            as usize;
                    let owner = store
                        .state
                        .team_name
                        .as_deref()
                        .unwrap_or("lead")
                        .to_string();
                    match mgr.claim(task_id, &owner) {
                        Ok(json) => {
                            push_tool_result(store, &call_id, json)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: task system not available".into(),
                    );
                }
            }
        }
        "idle" => {
            store.state.idle_requested = true;
            push_tool_result(
                store,
                &call_id,
                "Entering idle state. Will resume when new \
                 work arrives."
                    .into(),
            );
        }
        "team_spawn" => {
            if store.state.is_subagent {
                push_tool_result(
                    store,
                    &call_id,
                    "Error: team_spawn not available for teammates"
                        .into(),
                );
            } else {
                let team = store.state.team.clone();
                match team {
                    Some(team) => {
                        let name = args["name"]
                            .as_str()
                            .unwrap_or_default();
                        let role = args["role"]
                            .as_str()
                            .unwrap_or_default();
                        let prompt = args["prompt"]
                            .as_str()
                            .unwrap_or_default();
                        let agent_id = normalize_agent_id(
                            args.get("agent")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                        );
                        let llm_config =
                            store.state.config.llm.clone();
                        let agents = store.state.agents.clone();
                        let tasks =
                            store.state.tasks.clone();
                        match team.spawn(
                            name,
                            role,
                            prompt,
                            &agent_id,
                            llm_config,
                            agents,
                            tasks,
                        ) {
                            Ok(msg) => push_tool_result(
                                store, &call_id, msg,
                            ),
                            Err(e) => push_tool_result(
                                store,
                                &call_id,
                                format!("Error: {e}"),
                            ),
                        }
                    }
                    None => {
                        push_tool_result(
                            store,
                            &call_id,
                            "Error: team not available".into(),
                        );
                    }
                }
            }
        }
        "team_send" => {
            let sender = store.state.sender_name().to_string();
            let to = args["to"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let content = args["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    match team.send_message(&sender, &to, &content)
                    {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "shutdown_request" => {
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let teammate = args["teammate"]
                        .as_str()
                        .unwrap_or_default();
                    match team.request_shutdown(teammate) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "shutdown_response" => {
            let sender = store.state.sender_name().to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let req_id = args["request_id"]
                        .as_str()
                        .unwrap_or_default();
                    let approve = args["approve"]
                        .as_bool()
                        .unwrap_or(false);
                    let reason = args
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match team.respond_shutdown(
                        req_id, approve, reason, &sender,
                    ) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "plan_submit" => {
            let sender = store.state.sender_name().to_string();
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let plan = args["plan"]
                        .as_str()
                        .unwrap_or_default();
                    match team.submit_plan(&sender, plan) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "plan_response" => {
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let req_id = args["request_id"]
                        .as_str()
                        .unwrap_or_default();
                    let approve = args["approve"]
                        .as_bool()
                        .unwrap_or(false);
                    let feedback = args
                        .get("feedback")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match team.respond_plan(
                        req_id, approve, feedback,
                    ) {
                        Ok(msg) => {
                            push_tool_result(store, &call_id, msg)
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "team_read_inbox" => {
            let default_name = store.state.sender_name().to_string();
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&default_name);
            let team = store.state.team.clone();
            match team {
                Some(team) => {
                    let result = team.read_inbox(name);
                    push_tool_result(store, &call_id, result);
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: team not available".into(),
                    );
                }
            }
        }
        "worktree_create" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let task_id = args
                        .get("task_id")
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize);
                    match wt.create(name, task_id) {
                        Ok(json) => {
                            if let (Some(tid), Some(tasks)) =
                                (task_id, &store.state.tasks)
                            {
                                let _ = tasks
                                    .bind_worktree(tid, name);
                            }
                            push_tool_result(
                                store, &call_id, json,
                            );
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_remove" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let force = args
                        .get("force")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let complete_task = args
                        .get("complete_task")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    match wt.remove(name, force) {
                        Ok(entry) => {
                            if complete_task {
                                if let (Some(tid), Some(tasks)) =
                                    (
                                        entry.task_id,
                                        &store.state.tasks,
                                    )
                                {
                                    let _ = tasks.update(
                                        tid,
                                        Some(
                                            TaskStatus::Completed,
                                        ),
                                        None,
                                        None,
                                    );
                                    let _ = tasks
                                        .unbind_worktree(tid);
                                }
                            }
                            push_tool_result(
                                store,
                                &call_id,
                                format!(
                                    "Worktree '{name}' removed"
                                ),
                            );
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_keep" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    match wt.keep(name) {
                        Ok(msg) => {
                            push_tool_result(
                                store, &call_id, msg,
                            )
                        }
                        Err(e) => push_tool_result(
                            store,
                            &call_id,
                            format!("Error: {e}"),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_list" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    push_tool_result(
                        store,
                        &call_id,
                        wt.list_formatted(),
                    );
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "worktree_exec" => {
            let wt = store.state.worktrees.clone();
            match wt {
                Some(wt) => {
                    let name = args["name"]
                        .as_str()
                        .unwrap_or_default();
                    let command = args["command"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    match wt.get(name) {
                        Some(entry)
                            if entry.status
                                == WorktreeStatus::Active
                                || entry.status
                                    == WorktreeStatus::Kept =>
                        {
                            if !channel.confirm(&command).await {
                                push_tool_result(
                                    store,
                                    &call_id,
                                    "User denied execution."
                                        .into(),
                                );
                            } else {
                                let node = exec::BashExec {
                                    call_id: call_id.clone(),
                                    command,
                                    sandbox: true,
                                    timeout_secs: None,
                                    current_dir: Some(
                                        entry.path.clone().into(),
                                    ),
                                };
                                if let Err(e) = node.run(store).await {
                                    push_tool_result(
                                        store,
                                        &call_id,
                                        format!("Error: {e}"),
                                    );
                                }
                            }
                        }
                        Some(_) => push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: worktree '{name}' \
                                 is not active"
                            ),
                        ),
                        None => push_tool_result(
                            store,
                            &call_id,
                            format!(
                                "Error: worktree '{name}' \
                                 not found"
                            ),
                        ),
                    }
                }
                None => {
                    push_tool_result(
                        store,
                        &call_id,
                        "Error: worktree system not available"
                            .into(),
                    );
                }
            }
        }
        "web_fetch" => {
            let url = args["url"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let max_length = args
                .get("max_length")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let node = web::WebFetch {
                call_id: call_id.clone(),
                url,
                max_length,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "web_search" => {
            let query = args["query"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let node = web::WebSearch {
                call_id: call_id.clone(),
                query,
            };
            if let Err(e) = node.run(store).await {
                push_tool_result(
                    store,
                    &call_id,
                    format!("Error: {e}"),
                );
            }
        }
        "plan_mode" => {
            let active = args["active"].as_bool().unwrap_or(false);
            store.state.plan_mode = active;
            let status = if active {
                "Plan mode ON — research and plan only, no execution."
            } else {
                "Plan mode OFF — resuming normal execution."
            };
            push_tool_result(store, &call_id, status.into());
        }
        "cron_list" => {
            let Some(handle) = &store.state.cron else {
                push_tool_result(store, &call_id, "Error: cron service not available".into());
                return Ok(true);
            };
            let jobs = handle.list_jobs().await;
            if jobs.is_empty() {
                push_tool_result(store, &call_id, "No cron jobs.".into());
            } else {
                let mut output = String::from("Cron jobs:\n");
                for j in &jobs {
                    let status = if j.enabled { "enabled" } else { "disabled" };
                    output.push_str(&format!(
                        "  {} [{}] {} ({}) errors={} next={}\n",
                        j.id, status, j.name, j.kind, j.errors, j.next_run,
                    ));
                }
                push_tool_result(store, &call_id, output);
            }
        }
        "cron_create" => {
            let Some(handle) = &store.state.cron else {
                push_tool_result(store, &call_id, "Error: cron service not available".into());
                return Ok(true);
            };
            let id = args["id"].as_str().unwrap_or_default().to_string();
            let name = args["name"].as_str().unwrap_or_default().to_string();
            let schedule_kind = args["schedule_kind"]
                .as_str()
                .unwrap_or("every")
                .to_string();
            let message = args["message"].as_str().unwrap_or_default().to_string();
            let payload_kind = args
                .get("payload_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("agent_turn");
            let channel_name = args
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or("bg")
                .to_string();
            let delete_after_run = args
                .get("delete_after_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let payload = if payload_kind == "system_event" {
                Payload {
                    kind: "system_event".into(),
                    message: String::new(),
                    text: message,
                }
            } else {
                Payload {
                    kind: "agent_turn".into(),
                    message,
                    text: String::new(),
                }
            };

            let schedule = ScheduleConfig {
                kind: schedule_kind,
                expr: args
                    .get("cron_expr")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                at: args
                    .get("at")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                every_seconds: args
                    .get("every_seconds")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3600),
                anchor: String::new(),
            };

            let job = CronJob {
                id,
                name,
                enabled: true,
                schedule,
                payload,
                channel: channel_name,
                to: String::new(),
                delete_after_run,
                consecutive_errors: 0,
                last_run_at: 0.0,
                next_run_at: 0.0,
            };

            let result = handle.create_job(job).await;
            push_tool_result(store, &call_id, result);
        }
        "cron_delete" => {
            let Some(handle) = &store.state.cron else {
                push_tool_result(store, &call_id, "Error: cron service not available".into());
                return Ok(true);
            };
            let job_id = args["job_id"].as_str().unwrap_or_default();
            let result = handle.delete_job(job_id).await;
            push_tool_result(store, &call_id, result);
        }
        _ => return Ok(false),
    }

    Ok(true)
}

/// Push a tool result message into conversation history
pub fn push_tool_result(store: &mut SharedStore, call_id: &str, content: String) {
    store.context.history.push(Message {
        role: Role::Tool,
        content: Some(content),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::schema::all_tools;

    #[tokio::test]
    async fn test_bash_exec() {
        let mut store = SharedStore::test_default();
        let node = exec::BashExec {
            call_id: "call_123".into(),
            command: "echo hello".into(),
            sandbox: false,
            timeout_secs: None,
            current_dir: None,
        };

        let prep_res = node.prep(&store).await.expect("prep failed");
        let exec_res = node.exec(prep_res.clone()).await.expect("exec failed");
        assert_eq!(exec_res.trim(), "hello");

        node.post(&mut store, prep_res, exec_res)
            .await
            .expect("post failed");

        let last = store.context.history.last().expect("history empty");
        assert!(matches!(last.role, Role::Tool));
        assert_eq!(last.tool_call_id.as_deref(), Some("call_123"));
        assert!(last.content.as_ref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_exec_dangerous() {
        let store = SharedStore::test_default();
        let node = exec::BashExec {
            call_id: "call_456".into(),
            command: "sudo rm -rf /".into(),
            sandbox: false,
            timeout_secs: None,
            current_dir: None,
        };

        let result = node.prep(&store).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let mut store = SharedStore::test_default();
        let test_path = "test_rw_output.txt";

        let write_node = exec::WriteFile {
            call_id: "w1".into(),
            path: test_path.into(),
            content: "hello world".into(),
        };
        write_node.run(&mut store).await.expect("write failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("Written"));

        let read_node = exec::ReadFile {
            call_id: "r1".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.expect("read failed");
        let last = store.context.history.last().unwrap();
        assert!(last.content.as_ref().unwrap().contains("hello world"));

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file() {
        let mut store = SharedStore::test_default();
        let test_path = "test_edit_output.txt";
        std::fs::write(test_path, "foo bar baz").unwrap();

        // Must read before edit
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.expect("read failed");

        let node = exec::EditFile {
            call_id: "e1".into(),
            path: test_path.into(),
            old_string: "bar".into(),
            new_string: "qux".into(),
        };
        node.run(&mut store).await.expect("edit failed");

        let content = std::fs::read_to_string(test_path).unwrap();
        assert_eq!(content, "foo qux baz");

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_string_not_found() {
        let test_path = "test_edit_notfound.txt";
        std::fs::write(test_path, "some content").unwrap();

        let mut store = SharedStore::test_default();
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.unwrap();

        let node = exec::EditFile {
            call_id: "e2".into(),
            path: test_path.into(),
            old_string: "nonexistent".into(),
            new_string: "replacement".into(),
        };
        let result = node.run(&mut store).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[tokio::test]
    async fn test_edit_file_not_unique() {
        let test_path = "test_edit_dup.txt";
        std::fs::write(test_path, "aaa aaa").unwrap();

        let mut store = SharedStore::test_default();
        let read_node = exec::ReadFile {
            call_id: "r_pre".into(),
            path: test_path.into(),
        };
        read_node.run(&mut store).await.unwrap();

        let node = exec::EditFile {
            call_id: "e3".into(),
            path: test_path.into(),
            old_string: "aaa".into(),
            new_string: "bbb".into(),
        };
        let result = node.run(&mut store).await;
        assert!(result.is_err());

        std::fs::remove_file(test_path).ok();
    }

    #[test]
    fn test_safe_path_blocks_traversal() {
        let result = exec::safe_path("../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_safe_path_allows_relative() {
        std::fs::write("test_safe_path.txt", "").unwrap();
        let result = exec::safe_path("test_safe_path.txt");
        assert!(result.is_ok());
        std::fs::remove_file("test_safe_path.txt").ok();
    }

    #[tokio::test]
    async fn test_task_rejects_unknown_agent() {
        let mut store = SharedStore::test_default();
        let args = r#"{"prompt":"do something","agent":"nonexistent"}"#;
        let result =
            dispatch_tool("task", "t1".into(), args, &mut store, &silent())
                .await
                .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        let content = last.content.as_ref().unwrap();
        assert!(content.contains("Error: unknown agent"));
        assert!(content.contains("Available:"));
    }

    #[tokio::test]
    async fn test_task_blocked_in_subagent() {
        let mut store = SharedStore::test_default();
        store.state.is_subagent = true;

        let args = r#"{"prompt":"do something","agent":"any"}"#;
        let result =
            dispatch_tool("task", "t1".into(), args, &mut store, &silent())
                .await
                .unwrap();
        assert!(result);

        let last = store.context.history.last().unwrap();
        assert!(last
            .content
            .as_ref()
            .unwrap()
            .contains("not available here"));
    }

    #[test]
    fn test_all_tools_excludes_task_for_subagent() {
        let parent_tools = all_tools(false, false, false, false, false);
        let sub_tools = all_tools(true, false, false, false, false);

        assert!(parent_tools.iter().any(|t| t.function.name == "task"));
        assert!(!sub_tools.iter().any(|t| t.function.name == "task"));
        assert!(parent_tools.iter().any(|t| t.function.name == "bash"));
        assert!(sub_tools.iter().any(|t| t.function.name == "bash"));
    }

    #[test]
    fn test_all_tools_includes_task_graph_tools() {
        let without = all_tools(false, false, false, false, false);
        let with = all_tools(false, true, false, false, false);

        assert!(!without.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_create"));
        assert!(with.iter().any(|t| t.function.name == "task_update"));
        assert!(with.iter().any(|t| t.function.name == "task_list"));
        assert!(with.iter().any(|t| t.function.name == "task_get"));
    }

    #[test]
    fn test_all_tools_includes_team_tools() {
        let without = all_tools(false, false, false, false, false);
        let with = all_tools(false, false, true, false, false);
        let sub_with = all_tools(true, false, true, false, false);

        assert!(
            !without.iter().any(|t| t.function.name == "team_spawn")
        );
        assert!(
            with.iter().any(|t| t.function.name == "team_spawn")
        );
        assert!(
            with.iter().any(|t| t.function.name == "team_send")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "team_read_inbox")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "shutdown_request")
        );
        assert!(
            with.iter()
                .any(|t| t.function.name == "plan_response")
        );
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "team_spawn")
        );
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_request")
        );
        assert!(
            !sub_with
                .iter()
                .any(|t| t.function.name == "plan_response")
        );
        assert!(
            sub_with.iter().any(|t| t.function.name == "team_send")
        );
        assert!(
            sub_with
                .iter()
                .any(|t| t.function.name == "shutdown_response")
        );
        assert!(
            sub_with
                .iter()
                .any(|t| t.function.name == "plan_submit")
        );
    }

    fn silent() -> Arc<dyn Channel> {
        Arc::new(crate::frontend::SilentChannel)
    }
}
