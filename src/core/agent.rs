use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::core::tool::dispatch_tool;
use crate::frontend::Channel;

fn truncate_result(text: &str) -> String {
    let lines: Vec<&str> = text.lines().take(3).collect();
    let short = lines.join("\n");
    if text.lines().count() > 3 {
        format!("{short}\n...")
    } else {
        short
    }
}

/// Chain-of-thought agent
///
/// Drives the LLM loop: call LLM → dispatch tool calls → repeat until done.
/// Does not own SharedStore or Channel — receives both per call.
pub struct Agent;

impl Agent {
    pub fn new() -> Self {
        Self
    }

    /// Run the CoT loop against the given store
    ///
    /// # Arguments
    ///
    /// * `store` - Shared store for context and config
    /// * `channel` - Channel to send responses and confirmations through
    ///
    /// # Returns
    ///
    /// The `prompt_tokens` from the last LLM call, if available.
    /// Session uses this to decide whether to compact.
    ///
    /// # Errors
    ///
    /// Returns error on LLM call failure or tool execution failure.
    pub async fn run(
        &self,
        store: &mut SharedStore,
        channel: &Arc<dyn Channel>,
    ) -> Result<Option<usize>> {
        let mut last_prompt_tokens = None;

        loop {
            let llm = LLMCall {
                channel: channel.clone(),
            };
            let response = llm.run(store).await?;

            if let Some(usage) = &response.usage {
                last_prompt_tokens = Some(usage.prompt_tokens);
            }

            match response.tool_calls {
                Some(tool_calls) => {
                    for tc in tool_calls {
                        let tool_name = tc.name.clone();
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
                                tool_call_id: Some(tc.id),
                            });
                        }

                        // Display tool result summary
                        if let Some(last) = store.context.history.last()
                        {
                            if last.role == Role::Tool {
                                let result = last
                                    .content
                                    .as_deref()
                                    .unwrap_or("");
                                channel
                                    .send(&format!(
                                        "[{tool_name}] {}",
                                        truncate_result(result)
                                    ))
                                    .await;
                            }
                        }
                    }
                }
                None => {
                    channel.send("").await;
                    break;
                }
            }
        }

        Ok(last_prompt_tokens)
    }
}
