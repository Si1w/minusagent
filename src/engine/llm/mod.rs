mod request;
mod stream;

use std::sync::Arc;

use anyhow::Result;

use crate::engine::node::Node;
use crate::engine::store::{Message, Role, SharedStore, ToolCall};
use crate::frontend::Channel;

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

#[derive(Debug, Clone)]
pub struct LLMRequest {
    url: String,
    api_key: String,
    body: request::LLMRequestBody,
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
        Ok(request::build_request(store))
    }

    async fn exec(&self, prep_res: LLMRequest) -> Result<LLMResponse> {
        let response = self
            .http
            .post(&prep_res.url)
            .bearer_auth(&prep_res.api_key)
            .json(&prep_res.body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("LLM request to {} failed: {e}", prep_res.url))?
            .error_for_status()?;

        stream::collect_response(response, &self.channel).await
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
            tool_calls: exec_res.tool_calls.map(response_tool_calls),
            tool_call_id: None,
        });
        Ok(())
    }
}

fn response_tool_calls(tool_calls: Vec<ResponseToolCall>) -> Vec<ToolCall> {
    tool_calls
        .into_iter()
        .map(|tool_call| ToolCall {
            id: tool_call.id,
            name: tool_call.name,
            arguments: tool_call.arguments,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::engine::store::{Config, Context, LLMConfig, SystemState};
    use crate::frontend::SilentChannel;
    use crate::intelligence::manager::SharedAgents;
    use crate::team::{BackgroundManager, TodoManager};

    #[tokio::test]
    #[ignore = "requires LLM_API_KEY"]
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
