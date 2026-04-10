use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::config::tuning;
use crate::engine::llm::{LLMCall, ResponseToolCall};
use crate::engine::node::Node;
use crate::engine::store::{Config, Context, LLMConfig, Message, Role, SharedStore, SystemState};
use crate::frontend::{Channel, SilentChannel};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::routing::protocol::ToolPolicy;
use crate::team::{BackgroundManager, TaskManager, TodoManager, append_reminder};
use crate::tool::dispatch_tool;

const BACKGROUND_RESULTS_ACK: &str = "Noted background results.";
const INBOX_RESULTS_ACK: &str = "Noted inbox messages.";
const TODO_REMINDER: &str = "\n\n<reminder>Update your todos.</reminder>";

/// Options for the chain-of-thought loop
pub struct CotOptions {
    /// Maximum turns before stopping. `None` = unbounded.
    pub max_turns: Option<usize>,
    /// Inject `<reminder>` when LLM ignores todos for too long
    pub nag_reminder: bool,
    /// Flush the channel when the LLM finishes (no more tool calls)
    pub flush_on_done: bool,
    /// Shared flag to interrupt the loop from outside (e.g. via control protocol)
    pub interrupted: Option<Arc<AtomicBool>>,
}

/// Run the chain-of-thought loop: call LLM → dispatch tools → repeat
///
/// # Returns
///
/// The `total_tokens` from the last LLM call, if available.
///
/// # Errors
///
/// Returns error if the LLM call fails or a tool dispatch errors.
pub async fn cot_loop(
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
    http: &reqwest::Client,
    opts: &CotOptions,
) -> Result<Option<usize>> {
    let mut last_total_tokens = None;
    let llm = LLMCall {
        channel: channel.clone(),
        http: http.clone(),
    };

    let mut turns: usize = 0;
    loop {
        if should_interrupt(opts, channel).await {
            break;
        }

        if reached_turn_limit(turns, opts) {
            break;
        }
        turns += 1;

        inject_runtime_updates(store);

        let response = llm.run(store).await?;

        if let Some(usage) = &response.usage {
            last_total_tokens = Some(usage.total_tokens);
        }

        let Some(tool_calls) = response.tool_calls else {
            if opts.flush_on_done {
                channel.flush().await;
            }
            break;
        };

        if handle_tool_calls(&tool_calls, store, channel, opts.nag_reminder).await? {
            break;
        }
    }

    Ok(last_total_tokens)
}

async fn should_interrupt(opts: &CotOptions, channel: &Arc<dyn Channel>) -> bool {
    if let Some(flag) = &opts.interrupted
        && flag.swap(false, Ordering::Relaxed)
    {
        channel.send("[interrupted]").await;
        return true;
    }
    false
}

fn reached_turn_limit(turns: usize, opts: &CotOptions) -> bool {
    opts.max_turns.is_some_and(|max| turns >= max)
}

fn inject_runtime_updates(store: &mut SharedStore) {
    inject_background_results(store);
    inject_team_inbox(store);
}

fn inject_background_results(store: &mut SharedStore) {
    let notifications = store.state.background.drain_notifications();
    if notifications.is_empty() {
        return;
    }

    let notif_text = notifications
        .iter()
        .map(|(id, result)| format!("[bg:{id}] {result}"))
        .collect::<Vec<_>>()
        .join("\n");
    push_runtime_exchange(
        store,
        format!(
            "<background-results>\n\
             {notif_text}\n\
             </background-results>"
        ),
        BACKGROUND_RESULTS_ACK,
    );
}

fn inject_team_inbox(store: &mut SharedStore) {
    let Some(team) = &store.state.team else {
        return;
    };

    let name = store.state.sender_name();
    let messages = team.bus().read_inbox(name);
    if messages.is_empty() {
        return;
    }

    let inbox_json = serde_json::to_string(&messages).unwrap_or_default();
    push_runtime_exchange(
        store,
        format!("<inbox>\n{inbox_json}\n</inbox>"),
        INBOX_RESULTS_ACK,
    );
}

fn push_runtime_exchange(store: &mut SharedStore, user_content: String, assistant_ack: &str) {
    store.context.history.push(Message {
        role: Role::User,
        content: Some(user_content),
        tool_calls: None,
        tool_call_id: None,
    });
    store.context.history.push(Message {
        role: Role::Assistant,
        content: Some(assistant_ack.into()),
        tool_calls: None,
        tool_call_id: None,
    });
}

async fn handle_tool_calls(
    tool_calls: &[ResponseToolCall],
    store: &mut SharedStore,
    channel: &Arc<dyn Channel>,
    nag_reminder: bool,
) -> Result<bool> {
    let mut had_todo = false;
    for tool_call in tool_calls {
        had_todo |= tool_call.name == "todo";
        let handled = dispatch_tool(
            &tool_call.name,
            tool_call.id.clone(),
            &tool_call.arguments,
            store,
            channel,
        )
        .await?;

        if !handled {
            push_unknown_tool_result(store, &tool_call.id, &tool_call.name);
        }
    }

    if store.state.idle_requested {
        store.state.idle_requested = false;
        return Ok(true);
    }

    if nag_reminder {
        update_todo_reminder(store, had_todo);
    }

    Ok(false)
}

fn push_unknown_tool_result(store: &mut SharedStore, call_id: &str, tool_name: &str) {
    store.context.history.push(Message {
        role: Role::Tool,
        content: Some(format!("Unknown tool: {tool_name}")),
        tool_calls: None,
        tool_call_id: Some(call_id.to_string()),
    });
}

fn update_todo_reminder(store: &mut SharedStore, had_todo_update: bool) {
    store.state.todo.record_round(had_todo_update);
    if store.state.todo.should_nag(tuning().agent.nag_threshold) {
        let _ = append_reminder(&mut store.context.history, TODO_REMINDER);
    }
}

/// Run an agent with isolated context
///
/// Creates a fresh message history, runs a `CoT` loop up to `max_subagent_turns`,
/// and returns only the final assistant text. The agent's full history
/// is discarded — the caller sees just the summary.
///
/// Returns a boxed future to break the async recursion cycle
/// (`dispatch_tool → run_subagent → dispatch_tool`).
///
/// # Arguments
///
/// * `spec` - Subagent execution configuration
///
/// # Returns
///
/// The last assistant message text, or `"(no summary)"` if none.
pub struct SubagentSpec {
    pub prompt: String,
    pub system_prompt: String,
    pub llm_config: LLMConfig,
    pub workspace_dir: Option<PathBuf>,
    pub agent_id: String,
    pub agents: SharedAgents,
    pub tasks: Option<TaskManager>,
    pub denied_tools: Vec<String>,
}

#[must_use]
pub fn run_subagent(spec: SubagentSpec) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        let SubagentSpec {
            prompt,
            system_prompt,
            llm_config,
            workspace_dir,
            agent_id,
            agents,
            tasks,
            denied_tools,
        } = spec;

        // Build Intelligence from workspace if available
        let intelligence = workspace_dir.as_ref().map(|ws| {
            Intelligence::new(
                ws,
                &system_prompt,
                agent_id.clone(),
                "task".into(),
                llm_config.model.clone(),
            )
        });
        let effective_prompt = intelligence
            .as_ref()
            .map(Intelligence::build_prompt)
            .unwrap_or(system_prompt);

        let mut store = SharedStore {
            context: Context {
                system_prompt: effective_prompt,
                history: vec![Message {
                    role: Role::User,
                    content: Some(prompt),
                    tool_calls: None,
                    tool_call_id: None,
                }],
            },
            state: SystemState {
                config: Config { llm: llm_config },
                intelligence,
                todo: TodoManager::new(),
                is_subagent: true,
                agents,
                tasks,
                background: BackgroundManager::new(),
                team: None,
                team_name: None,
                worktrees: None,
                tool_policy: ToolPolicy::from_denied(&denied_tools),
                idle_requested: false,
                plan_mode: false,
                cron: None,
                read_file_state: HashMap::new(),
            },
        };

        let channel: Arc<dyn Channel> = Arc::new(SilentChannel);
        let http = reqwest::Client::new();

        cot_loop(
            &mut store,
            &channel,
            &http,
            &CotOptions {
                max_turns: Some(tuning().agent.max_subagent_turns),
                nag_reminder: false,
                flush_on_done: false,
                interrupted: None,
            },
        )
        .await?;

        Ok(extract_last_assistant_summary(&store))
    })
}

fn extract_last_assistant_summary(store: &SharedStore) -> String {
    store
        .context
        .history
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant && message.content.is_some())
        .and_then(|message| message.content.clone())
        .unwrap_or_else(|| "(no summary)".into())
}

/// Chain-of-thought agent
///
/// Drives the LLM loop: call LLM → dispatch tool calls → repeat until done.
/// Does not own `SharedStore` or `Channel` — receives both per call.
pub struct Agent;

impl Agent {
    /// Run the `CoT` loop against the given store
    ///
    /// # Arguments
    ///
    /// * `store` - Shared store for context and config
    /// * `channel` - Channel to send responses and confirmations through
    ///
    /// # Returns
    ///
    /// The `total_tokens` from the last LLM call, if available.
    /// Session uses this to decide whether to compact.
    ///
    /// # Errors
    ///
    /// Returns error on LLM call failure or tool execution failure.
    pub async fn run(
        &self,
        store: &mut SharedStore,
        channel: &Arc<dyn Channel>,
        http: &reqwest::Client,
        interrupted: Option<Arc<AtomicBool>>,
    ) -> Result<Option<usize>> {
        cot_loop(
            store,
            channel,
            http,
            &CotOptions {
                max_turns: None,
                nag_reminder: true,
                flush_on_done: true,
                interrupted,
            },
        )
        .await
    }
}
