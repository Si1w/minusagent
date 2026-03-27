pub mod bootstrap;
pub mod manager;
pub mod memory;
pub mod prompt;
pub mod skills;
pub mod utils;

use std::collections::HashMap;
use std::path::Path;

/// Prompt assembly mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum PromptMode {
    /// All layers: identity, tools, skills, memory, bootstrap, runtime, channel
    Full,
    /// Subset: identity, tools, bootstrap (AGENTS.md + TOOLS.md only), runtime
    Minimal,
    /// Identity + runtime only, no bootstrap files loaded
    None,
}

impl std::fmt::Display for PromptMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "full"),
            Self::Minimal => write!(f, "minimal"),
            Self::None => write!(f, "none"),
        }
    }
}

use crate::intelligence::bootstrap::BootstrapLoader;
use crate::intelligence::memory::MemoryStore;
use crate::intelligence::skills::{Skill, SkillsManager};

/// Intelligence context for dynamic system prompt assembly
///
/// Loaded once at session creation. System prompt is rebuilt each turn.
pub struct Intelligence {
    pub memory: MemoryStore,
    bootstrap_data: HashMap<String, String>,
    skills: Vec<Skill>,
    /// Identity text from AGENT.md body (Layer 1)
    identity: String,
    agent_id: String,
    channel: String,
    model: String,
}

impl Intelligence {
    /// Initialize intelligence from a workspace directory
    ///
    /// Loads bootstrap files and discovers skills once.
    ///
    /// # Arguments
    ///
    /// * `workspace_dir` - Path to the workspace containing bootstrap files
    /// * `identity` - Identity text from AGENT.md body (Layer 1)
    /// * `agent_id` - Agent identifier for runtime context
    /// * `channel` - Channel type (cli, discord, etc.)
    /// * `model` - Model name for runtime context
    pub fn new(
        workspace_dir: &Path,
        identity: String,
        agent_id: String,
        channel: String,
        model: String,
    ) -> Self {
        let loader = BootstrapLoader::new(workspace_dir);
        let bootstrap_data = loader.load_all(PromptMode::Full);

        let mut skills_mgr = SkillsManager::new(workspace_dir);
        skills_mgr.discover(&[]);

        let mut memory = MemoryStore::new(&workspace_dir.join("memory"));
        memory.discover();

        Self {
            memory,
            bootstrap_data,
            skills: skills_mgr.skills,
            identity,
            agent_id,
            channel,
            model,
        }
    }

    /// Find a skill by name (e.g. "greet")
    pub fn find_skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Build the system prompt from all loaded layers
    pub fn build_prompt(&self) -> String {
        prompt::build_system_prompt(
            PromptMode::Full,
            &self.identity,
            &self.bootstrap_data,
            &self.skills,
            &self.memory.entries,
            &self.agent_id,
            &self.model,
            &self.channel,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_mandeven_workspace() {
        let workspace = Path::new("workspace/.agents/mandeven");
        if !workspace.exists() {
            return; // skip if not running from project root
        }

        // Read AGENT.md as identity (plain markdown, no frontmatter)
        let identity = std::fs::read_to_string(
            workspace.join("AGENT.md"),
        )
        .unwrap()
        .trim()
        .to_string();

        let intel = Intelligence::new(
            workspace,
            identity,
            "mandeven".into(),
            "cli".into(),
            "test-model".into(),
        );
        let prompt = intel.build_prompt();

        // Layer 1: Identity from AGENT.md body
        assert!(
            prompt.contains("Mandeven"),
            "should contain identity from AGENT.md body"
        );

        // Layer 6: Runtime context
        assert!(prompt.contains("Agent ID: mandeven"));
        assert!(prompt.contains("Channel: cli"));

        println!("--- Mandeven System Prompt ---");
        println!("{prompt}");
        println!("--- End ({} chars) ---", prompt.len());
    }
}
