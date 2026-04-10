use std::collections::HashMap;
use std::path::Path;

use crate::config::{AppConfig, LLMConfig};
use crate::engine::store::{Config, Context, SharedStore, SystemState};
use crate::intelligence::Intelligence;
use crate::intelligence::manager::SharedAgents;
use crate::routing::protocol::ToolPolicy;
use crate::scheduler::cron::CronHandle;
use crate::team::{BackgroundManager, TaskManager, TeammateManager, TodoManager, WorktreeManager};

pub(super) struct SessionStoreConfig<'a> {
    pub(super) system_prompt: String,
    pub(super) model: String,
    pub(super) intelligence: Option<Intelligence>,
    pub(super) agents: SharedAgents,
    pub(super) workspace_dir: Option<&'a Path>,
    pub(super) cron: Option<CronHandle>,
    pub(super) denied_tools: &'a [String],
}

pub(super) fn build_session_store(config: &AppConfig, spec: SessionStoreConfig<'_>) -> SharedStore {
    let primary = config.primary_llm();
    let tasks = spec
        .workspace_dir
        .and_then(|workspace_dir| TaskManager::new(workspace_dir.join(".tasks")).ok());
    let team = spec
        .workspace_dir
        .and_then(|workspace_dir| TeammateManager::new(&workspace_dir.join(".team")).ok());
    let worktrees = spec.workspace_dir.and_then(|workspace_dir| {
        WorktreeManager::new(
            workspace_dir.join(".worktrees"),
            workspace_dir.to_path_buf(),
        )
        .ok()
    });

    SharedStore {
        context: Context {
            system_prompt: spec.system_prompt,
            history: Vec::new(),
        },
        state: SystemState {
            config: Config {
                llm: LLMConfig {
                    model: spec.model,
                    base_url: primary.base_url.clone(),
                    api_key: primary.api_key.clone(),
                    context_window: primary.context_window,
                },
            },
            intelligence: spec.intelligence,
            todo: TodoManager::new(),
            is_subagent: false,
            agents: spec.agents,
            tasks,
            background: BackgroundManager::new(),
            team,
            team_name: None,
            worktrees,
            tool_policy: ToolPolicy::from_denied(spec.denied_tools),
            idle_requested: false,
            plan_mode: false,
            cron: spec.cron,
            read_file_state: HashMap::new(),
        },
    }
}
