use serde::{Deserialize, Serialize};

pub use crate::config::LLMConfig;
use crate::core::task::{BackgroundManager, TaskManager};
use crate::core::team::TeammateManager;
use crate::core::todo::TodoManager;
use crate::core::worktree::WorktreeManager;
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
    /// Background task runner with notification queue
    pub background: BackgroundManager,
    /// Team manager for multi-agent collaboration
    pub team: Option<TeammateManager>,
    /// This agent's team name (`None` for lead, `Some` for teammates)
    pub team_name: Option<String>,
    /// Worktree manager for task isolation
    pub worktrees: Option<WorktreeManager>,
    /// Set by the `idle` tool to break out of cot_loop
    pub idle_requested: bool,
}

impl SystemState {
    /// This agent's team identity ("lead" if unnamed)
    pub fn sender_name(&self) -> &str {
        self.team_name.as_deref().unwrap_or("lead")
    }
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
                background: BackgroundManager::new(),
                team: None,
                team_name: None,
                worktrees: None,
                idle_requested: false,
            },
        }
    }
}
