use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::core::tool::dispatch_tool;
use crate::frontend::Channel;

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
        http: &reqwest::Client,
    ) -> Result<Option<usize>> {
        let mut last_total_tokens = None;
        let llm = LLMCall {
            channel: channel.clone(),
            http: http.clone(),
        };

        loop {
            let response = llm.run(store).await?;

            if let Some(usage) = &response.usage {
                last_total_tokens = Some(usage.total_tokens);
            }

            match response.tool_calls {
                Some(tool_calls) => {
                    for tc in tool_calls {
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
                    }
                }
                None => {
                    channel.flush().await;
                    break;
                }
            }
        }

        Ok(last_total_tokens)
    }
}
