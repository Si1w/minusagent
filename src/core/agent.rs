use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::config::tuning;
use crate::core::tool::dispatch_tool;
use crate::frontend::Channel;

/// Options for the chain-of-thought loop
pub struct CotOptions {
    /// Maximum turns before stopping. `None` = unbounded.
    pub max_turns: Option<usize>,
    /// Inject `<reminder>` when LLM ignores todos for too long
    pub nag_reminder: bool,
    /// Flush the channel when the LLM finishes (no more tool calls)
    pub flush_on_done: bool,
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
    ) -> Result<Option<usize>> {
        cot_loop(
            store,
            channel,
            http,
            &CotOptions {
                max_turns: None,
                nag_reminder: true,
                flush_on_done: true,
            },
        )
        .await
    }
}
