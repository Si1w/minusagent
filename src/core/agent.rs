use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{Message, Role, SharedStore};
use crate::core::tool::BashExec;
use crate::frontend::Channel;

pub struct Agent {
    store: SharedStore,
    channel: Arc<dyn Channel>,
}

impl Agent {
    pub fn new(store: SharedStore, channel: Arc<dyn Channel>) -> Self {
        Self { store, channel }
    }

    pub async fn turn(&mut self, input: &str) -> Result<()> {
        self.store.context.history.push(Message {
            role: Role::User,
            content: Some(input.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        loop {
            let llm = LLMCall {
                channel: self.channel.clone(),
            };
            let response = llm.run(&mut self.store).await?;

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
                                    self.store.context.history.push(Message {
                                        role: Role::Tool,
                                        content: Some("User denied execution.".into()),
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id.clone()),
                                    });
                                    continue;
                                }

                                let bash = BashExec {
                                    call_id: tc.id,
                                    command,
                                };
                                bash.run(&mut self.store).await?;
                            }
                            _ => {
                                // TODO: skill execution
                                self.store.context.history.push(Message {
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

        Ok(())
    }
}
