use serde::{Deserialize, Serialize};

use crate::core::task::TaskManager;
use crate::core::todo::TodoManager;
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;

/// LLM-visible conversation state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    pub system_prompt: String,
    pub history: Vec<Message>,
}

/// A single message in the conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

/// Message author role
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// A tool invocation requested by the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// LLM provider configuration
#[derive(Clone)]
pub struct LLMConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub context_window: usize,
}

/// Top-level configuration
pub struct Config {
    pub llm: LLMConfig,
}

/// LLM-invisible system state
pub struct SystemState {
    pub config: Config,
    pub intelligence: Option<Intelligence>,
    pub todo: TodoManager,
    pub is_subagent: bool,
    /// Read-only handle to the shared agent registry
    pub agents: SharedAgents,
    /// Persistent task graph (workspace-level)
    pub tasks: Option<TaskManager>,
}

/// Two-layer state container shared across all nodes
///
/// - `context`: LLM-visible (system prompt, conversation history)
/// - `state`: LLM-invisible (config, runtime state)
pub struct SharedStore {
    pub context: Context,
    pub state: SystemState,
}

#[cfg(test)]
impl SharedStore {
    /// Empty store with default config for unit tests
    pub fn test_default() -> Self {
        Self {
            context: Context {
                system_prompt: String::new(),
                history: Vec::new(),
            },
            state: SystemState {
                config: Config {
                    llm: LLMConfig {
                        model: String::new(),
                        base_url: String::new(),
                        api_key: String::new(),
                        context_window: 256_000,
                    },
                },
                intelligence: None,
                todo: TodoManager::new(),
                is_subagent: false,
                agents: SharedAgents::empty(),
                tasks: None,
            },
        }
    }
}
