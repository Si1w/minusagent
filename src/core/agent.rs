use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{
    Config, Context, LLMConfig, Message, Role, SharedStore, SystemState,
};
use crate::config::tuning;
use crate::frontend::{Channel, SilentChannel};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::routing::protocol::ToolPolicy;
use crate::team::{BackgroundManager, TaskManager, TodoManager};
use crate::tool::dispatch_tool;

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
        if let Some(flag) = &opts.interrupted {
            if flag.swap(false, Ordering::Relaxed) {
                channel.send("[interrupted]").await;
                break;
            }
        }

        if let Some(max) = opts.max_turns {
            if turns >= max {
                break;
            }
        }
        turns += 1;

        // Drain background task notifications before LLM call
        let notifs = store.state.background.drain_notifications();
        if !notifs.is_empty() {
            let notif_text: String = notifs
                .iter()
                .map(|(id, result)| format!("[bg:{id}] {result}"))
                .collect::<Vec<_>>()
                .join("\n");
            store.context.history.push(Message {
                role: Role::User,
                content: Some(format!(
                    "<background-results>\n\
                     {notif_text}\n\
                     </background-results>"
                )),
                tool_calls: None,
                tool_call_id: None,
            });
            store.context.history.push(Message {
                role: Role::Assistant,
                content: Some(
                    "Noted background results.".into(),
                ),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Drain team inbox before LLM call
        if let Some(team) = &store.state.team {
            let name = store.state.sender_name();
            let msgs = team.bus().read_inbox(name);
            if !msgs.is_empty() {
                let inbox_json =
                    serde_json::to_string(&msgs).unwrap_or_default();
                store.context.history.push(Message {
                    role: Role::User,
                    content: Some(format!(
                        "<inbox>\n{inbox_json}\n</inbox>"
                    )),
                    tool_calls: None,
                    tool_call_id: None,
                });
                store.context.history.push(Message {
                    role: Role::Assistant,
                    content: Some(
                        "Noted inbox messages.".into(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        let response = llm.run(store).await?;

        if let Some(usage) = &response.usage {
            last_total_tokens = Some(usage.total_tokens);
        }

        match response.tool_calls {
            Some(tool_calls) => {
                let mut had_todo = false;
                for tc in &tool_calls {
                    if tc.name == "todo" {
                        had_todo = true;
                    }
                    let handled = dispatch_tool(
                        &tc.name,
                        tc.id.clone(),
                        &tc.arguments,
                        store,
                        channel,
                    )
                    .await?;

                    if !handled {
                        store.context.history.push(Message {
                            role: Role::Tool,
                            content: Some(format!(
                                "Unknown tool: {}",
                                tc.name
                            )),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                        });
                    }
                }

                // Check if idle was requested by a tool
                if store.state.idle_requested {
                    store.state.idle_requested = false;
                    break;
                }

                if opts.nag_reminder {
                    if had_todo {
                        store.state.todo.rounds_since_update = 0;
                    } else {
                        store.state.todo.rounds_since_update += 1;
                    }
                    if store.state.todo.rounds_since_update >= tuning().nag_threshold
                        && !store.state.todo.items.is_empty()
                    {
                        if let Some(last) =
                            store.context.history.last_mut()
                        {
                            if last.role == Role::Tool {
                                if let Some(content) = &mut last.content {
                                    content.push_str(
                                        "\n\n<reminder>Update your \
                                         todos.</reminder>",
                                    );
                                }
                            }
                        }
                    }
                }
            }
            None => {
                if opts.flush_on_done {
                    channel.flush().await;
                }
                break;
            }
        }
    }

    Ok(last_total_tokens)
}

/// Run an agent with isolated context
///
/// Creates a fresh message history, runs a CoT loop up to `max_subagent_turns`,
/// and returns only the final assistant text. The agent's full history
/// is discarded — the caller sees just the summary.
///
/// Returns a boxed future to break the async recursion cycle
/// (`dispatch_tool → run_subagent → dispatch_tool`).
///
/// # Arguments
///
/// * `prompt` - The task description
/// * `system_prompt` - Agent identity (from AGENT.md)
/// * `llm_config` - LLM configuration cloned from the parent
/// * `workspace_dir` - Agent workspace for Intelligence loading
/// * `agent_id` - Agent identifier for runtime context
/// * `agents` - Shared agent registry (passed through for nested dispatch)
/// * `tasks` - Shared task graph (passed through for task operations)
///
/// # Returns
///
/// The last assistant message text, or `"(no summary)"` if none.
pub fn run_subagent(
    prompt: String,
    system_prompt: String,
    llm_config: LLMConfig,
    workspace_dir: Option<PathBuf>,
    agent_id: String,
    agents: SharedAgents,
    tasks: Option<TaskManager>,
) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> {
    Box::pin(async move {
        // Build Intelligence from workspace if available
        let intelligence = workspace_dir.as_ref().map(|ws| {
            Intelligence::new(
                ws,
                system_prompt.clone(),
                agent_id.clone(),
                "task".into(),
                llm_config.model.clone(),
            )
        });
        let effective_prompt = intelligence
            .as_ref()
            .map(|i| i.build_prompt())
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
                tool_policy: ToolPolicy::default(),
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
                max_turns: Some(tuning().max_subagent_turns),
                nag_reminder: false,
                flush_on_done: false,
                interrupted: None,
            },
        )
        .await?;

        // Extract last assistant text as summary
        let summary = store
            .context
            .history
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && m.content.is_some())
            .and_then(|m| m.content.clone())
            .unwrap_or_else(|| "(no summary)".into());

        Ok(summary)
    })
}

/// Chain-of-thought agent
///
/// Drives the LLM loop: call LLM → dispatch tool calls → repeat until done.
/// Does not own SharedStore or Channel — receives both per call.
pub struct Agent;

impl Agent {
    /// Run the CoT loop against the given store
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
