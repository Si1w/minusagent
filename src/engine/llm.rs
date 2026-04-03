use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::engine::node::Node;
use crate::engine::store::{Message, Role, SharedStore, ToolCall};
use crate::tool::{ToolDefinition, all_tools_filtered};
use crate::frontend::Channel;

// Request types

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize)]
struct ChatMessage {
    role: ChatRole,
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ChatToolCall {
    id: String,
    r#type: String,
    function: ChatFunction,
}

#[derive(Debug, Clone, Serialize)]
struct ChatFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct LLMRequestBody {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    stream: bool,
}

#[derive(Debug, Clone)]
pub struct LLMRequest {
    url: String,
    api_key: String,
    body: LLMRequestBody,
}

/// Token usage from the LLM API response
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}

/// Aggregated LLM response after streaming
#[derive(Debug, Clone)]
pub struct LLMResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ResponseToolCall>>,
    /// Token usage from the final streaming chunk, `None` if unavailable
    pub usage: Option<Usage>,
}

/// A tool call parsed from the LLM response
#[derive(Debug, Clone)]
pub struct ResponseToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

// Streaming delta types

#[derive(Debug, Deserialize)]
struct StreamUsage {
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
}

#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    delta: StreamDelta,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    content: Option<String>,
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct StreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<StreamFunction>,
}

#[derive(Debug, Deserialize)]
struct StreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

/// Node that calls an OpenAI-compatible LLM API with streaming
pub struct LLMCall {
    pub channel: Arc<dyn Channel>,
    pub http: reqwest::Client,
}

impl Node for LLMCall {
    type PrepRes = LLMRequest;
    type ExecRes = LLMResponse;

    async fn prep(&self, store: &SharedStore) -> Result<LLMRequest> {
        let config = &store.state.config.llm;
        let ctx = &store.context;

        let mut messages = vec![ChatMessage {
            role: ChatRole::System,
            content: Some(ctx.system_prompt.clone()),
            tool_calls: None,
            tool_call_id: None,
        }];

        for msg in &ctx.history {
            let role = match msg.role {
                Role::User => ChatRole::User,
                Role::Assistant => ChatRole::Assistant,
                Role::Tool => ChatRole::Tool,
            };
            messages.push(ChatMessage {
                role,
                content: msg.content.clone(),
                tool_calls: msg.tool_calls.as_ref().map(|tcs| {
                    tcs.iter()
                        .map(|tc| ChatToolCall {
                            id: tc.id.clone(),
                            r#type: "function".into(),
                            function: ChatFunction {
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: msg.tool_call_id.clone(),
            });
        }

        let denied = store.state.tool_policy.denied_names();
        let tools = all_tools_filtered(
            store.state.is_subagent,
            store.state.tasks.is_some(),
            store.state.team.is_some(),
            store.state.worktrees.is_some(),
            store.state.cron.is_some(),
            &denied,
        );

        Ok(LLMRequest {
            url: format!(
                "{}/chat/completions",
                config.base_url.trim_end_matches('/')
            ),
            api_key: config.api_key.clone(),
            body: LLMRequestBody {
                model: config.model.clone(),
                messages,
                tools: Some(tools),
                stream: true,
            },
        })
    }

    async fn exec(&self, prep_res: LLMRequest) -> Result<LLMResponse> {
        let resp = self.http
            .post(&prep_res.url)
            .bearer_auth(&prep_res.api_key)
            .json(&prep_res.body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("LLM request to {} failed: {e}", prep_res.url))?
            .error_for_status()?;

        let mut content = String::new();
        let mut tool_calls: Vec<ResponseToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buf.find('\n') {
                let line = buf[..line_end].trim().to_string();
                buf = buf[line_end + 1..].to_string();

                if line.is_empty() || line == "data: [DONE]" {
                    continue;
                }

                let data = match line.strip_prefix("data: ") {
                    Some(d) => d,
                    None => continue,
                };

                let chunk: StreamChunk = match serde_json::from_str(data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };

                if let Some(u) = chunk.usage {
                    usage = Some(Usage {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                        total_tokens: u.total_tokens,
                    });
                }

                let Some(choice) = chunk.choices.into_iter().next() else {
                    continue;
                };

                if let Some(text) = choice.delta.content {
                    if !text.is_empty() {
                        self.channel.on_stream_chunk(&text).await;
                    }
                    content.push_str(&text);
                }

                if let Some(tcs) = choice.delta.tool_calls {
                    for tc in tcs {
                        // Fill gaps for non-contiguous indices
                        while tc.index >= tool_calls.len() {
                            tool_calls.push(ResponseToolCall {
                                id: tc.id.clone().unwrap_or_default(),
                                name: tc.function
                                    .as_ref()
                                    .and_then(|f| f.name.clone())
                                    .unwrap_or_default(),
                                arguments: String::new(),
                            });
                        }
                        // Update id/name if provided on this chunk
                        if let Some(id) = &tc.id {
                            tool_calls[tc.index].id = id.clone();
                        }
                        if let Some(f) = &tc.function {
                            if let Some(name) = &f.name {
                                tool_calls[tc.index].name = name.clone();
                            }
                            if let Some(args) = &f.arguments {
                                tool_calls[tc.index].arguments.push_str(args);
                            }
                        }
                    }
                }
            }
        }

        Ok(LLMResponse {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            usage,
        })
    }

    async fn post(
        &self,
        store: &mut SharedStore,
        _prep_res: LLMRequest,
        exec_res: LLMResponse,
    ) -> Result<()> {
        store.context.history.push(Message {
            role: Role::Assistant,
            content: exec_res.content,
            tool_calls: exec_res.tool_calls.map(|tcs| {
                tcs.into_iter()
                    .map(|tc| ToolCall {
                        id: tc.id,
                        name: tc.name,
                        arguments: tc.arguments,
                    })
                    .collect()
            }),
            tool_call_id: None,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::engine::store::{Config, Context, LLMConfig, SystemState};
    use crate::team::{BackgroundManager, TodoManager};
    use crate::frontend::SilentChannel;
    use crate::intelligence::manager::SharedAgents;

    #[tokio::test]
    #[ignore] // requires LLM_API_KEY
    async fn test_llm_call() {
        dotenvy::dotenv().ok();

        let mut store = SharedStore {
            context: Context {
                system_prompt: "You are a helpful assistant.".into(),
                history: vec![Message {
                    role: Role::User,
                    content: Some("What is 1 + 1?".into()),
                    tool_calls: None,
                    tool_call_id: None,
                }],
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model: std::env::var("LLM_MODEL").expect("LLM_MODEL not set"),
                        base_url: std::env::var("LLM_BASE_URL").expect("LLM_BASE_URL not set"),
                        api_key: std::env::var("LLM_API_KEY").expect("LLM_API_KEY not set"),
                        context_window: 256_000,
                    },
                },
                intelligence: None,
                todo: TodoManager::new(),
                is_subagent: false,
                agents: SharedAgents::empty(),
                tasks: None,
                background: BackgroundManager::new(),
                team: None,
                team_name: None,
                worktrees: None,
                tool_policy: crate::routing::protocol::ToolPolicy::default(),
                idle_requested: false,
                plan_mode: false,
                cron: None,
                read_file_state: HashMap::new(),
            },
        };

        let node = LLMCall {
            channel: Arc::new(SilentChannel) as Arc<dyn Channel>,
            http: reqwest::Client::new(),
        };

        let prep_res = node.prep(&store).await.expect("prep failed");
        let prep_res_clone = prep_res.clone();
        let exec_res = node.exec(prep_res).await.expect("exec failed");

        println!();
        println!("Content: {:?}", exec_res.content);
        println!("Tool calls: {:?}", exec_res.tool_calls);

        node.post(&mut store, prep_res_clone, exec_res)
            .await
            .expect("post failed");

        let last = store.context.history.last().expect("history empty");
        assert!(matches!(last.role, Role::Assistant));
    }
}
