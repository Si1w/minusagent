use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;

use crate::core::llm::LLMCall;
use crate::core::node::Node;
use crate::core::store::{
    Config, Context, LLMConfig, Message, Role, SharedStore, SystemState,
};
use crate::core::todo::TodoManager;
use crate::core::tool::dispatch_tool;
use crate::frontend::{Channel, SilentChannel};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;

const MAX_TURNS: usize = 30;

/// Run an agent with isolated context
///
/// Creates a fresh message history, runs a CoT loop up to `MAX_TURNS`,
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
            },
        };

        let channel: Arc<dyn Channel> = Arc::new(SilentChannel);
        let http = reqwest::Client::new();
        let llm = LLMCall {
            channel: channel.clone(),
            http: http.clone(),
        };

        for _ in 0..MAX_TURNS {
            let response = llm.run(&mut store).await?;

            match response.tool_calls {
                Some(tool_calls) => {
                    for tc in &tool_calls {
                        let handled = dispatch_tool(
                            &tc.name,
                            tc.id.clone(),
                            &tc.arguments,
                            &mut store,
                            &channel,
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
                }
                None => break,
            }
        }

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
