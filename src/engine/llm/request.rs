use serde::Serialize;

use super::LLMRequest;
use crate::engine::store::{Message, Role, SharedStore, SystemState};
use crate::tool::{ToolAvailability, ToolCapability, ToolDefinition, all_tools_filtered};

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
pub(super) struct LLMRequestBody {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    stream: bool,
}

pub(super) fn build_request(store: &SharedStore) -> LLMRequest {
    let config = &store.state.config.llm;

    LLMRequest {
        url: format!("{}/chat/completions", config.base_url.trim_end_matches('/')),
        api_key: config.api_key.clone(),
        body: LLMRequestBody {
            model: config.model.clone(),
            messages: build_messages(store),
            tools: Some(build_tools(&store.state)),
            stream: true,
        },
    }
}

fn build_messages(store: &SharedStore) -> Vec<ChatMessage> {
    let mut messages = vec![ChatMessage {
        role: ChatRole::System,
        content: Some(store.context.system_prompt.clone()),
        tool_calls: None,
        tool_call_id: None,
    }];

    messages.extend(store.context.history.iter().map(message_from_store));
    messages
}

fn message_from_store(message: &Message) -> ChatMessage {
    ChatMessage {
        role: role_from_store(&message.role),
        content: message.content.clone(),
        tool_calls: message
            .tool_calls
            .as_ref()
            .map(|tool_calls| chat_tool_calls(tool_calls)),
        tool_call_id: message.tool_call_id.clone(),
    }
}

fn role_from_store(role: &Role) -> ChatRole {
    match role {
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
        Role::Tool => ChatRole::Tool,
    }
}

fn chat_tool_calls(tool_calls: &[crate::engine::store::ToolCall]) -> Vec<ChatToolCall> {
    tool_calls
        .iter()
        .map(|tool_call| ChatToolCall {
            id: tool_call.id.clone(),
            r#type: "function".into(),
            function: ChatFunction {
                name: tool_call.name.clone(),
                arguments: tool_call.arguments.clone(),
            },
        })
        .collect()
}

fn build_tools(state: &SystemState) -> Vec<ToolDefinition> {
    let denied = state.tool_policy.denied_names();
    all_tools_filtered(tool_availability(state), &denied)
}

fn tool_availability(state: &SystemState) -> ToolAvailability {
    let mut availability = if state.is_subagent {
        ToolAvailability::subagent()
    } else {
        ToolAvailability::primary()
    };

    for (enabled, capability) in [
        (state.tasks.is_some(), ToolCapability::Tasks),
        (state.team.is_some(), ToolCapability::Team),
        (state.worktrees.is_some(), ToolCapability::Worktrees),
        (state.cron.is_some(), ToolCapability::Cron),
    ] {
        if enabled {
            availability = availability.with(capability);
        }
    }

    availability
}
