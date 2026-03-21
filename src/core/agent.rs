use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::core::tool::{BashExec, EditFile, ReadFile, WriteFile};
use crate::frontend::Channel;

/// Chain-of-thought agent
///
/// Drives the LLM loop: call LLM → dispatch tool calls → repeat until done.
/// Does not own SharedStore — receives it by reference from Session.
pub struct Agent {
    channel: Arc<dyn Channel>,
}

impl Agent {
    pub fn new(channel: Arc<dyn Channel>) -> Self {
        Self { channel }
    }

    /// Run the CoT loop against the given store
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
    ) -> Result<Option<usize>> {
        let mut last_prompt_tokens = None;

        loop {
            let llm = LLMCall {
                channel: self.channel.clone(),
            };
            let response = llm.run(store).await?;

            if let Some(usage) = &response.usage {
                last_prompt_tokens = Some(usage.prompt_tokens);
            }

            match response.tool_calls {
                Some(tool_calls) => {
                    for tc in tool_calls {
                        match tc.name.as_str() {
                            "bash" => {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tc.arguments)?;
                                let command = args["command"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();

                                if !self.channel.confirm(&command).await {
                                    store.context.history.push(Message {
                                        role: Role::Tool,
                                        content: Some(
                                            "User denied execution.".into(),
                                        ),
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id.clone()),
                                    });
                                    continue;
                                }

                                let bash = BashExec {
                                    call_id: tc.id,
                                    command,
                                };
                                bash.run(store).await?;
                            }
                            "read_file" => {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tc.arguments)?;
                                let path = args["path"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                let node = ReadFile {
                                    call_id: tc.id,
                                    path,
                                };
                                node.run(store).await?;
                            }
                            "write_file" => {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tc.arguments)?;
                                let path = args["path"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                let content = args["content"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string();
                                let node = WriteFile {
                                    call_id: tc.id,
                                    path,
                                    content,
                                };
                                node.run(store).await?;
                            }
                            "edit_file" => {
                                let args: serde_json::Value =
                                    serde_json::from_str(&tc.arguments)?;
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
                                let node = EditFile {
                                    call_id: tc.id,
                                    path,
                                    old_string,
                                    new_string,
                                };
                                node.run(store).await?;
                            }
                            _ => {
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
                    }
                }
                None => {
                    self.channel.send("").await;
                    break;
                }
            }
        }

        Ok(last_prompt_tokens)
    }
}
