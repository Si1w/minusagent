pub mod bootstrap;
pub mod manager;
pub mod memory;
pub mod prompt;
pub mod skills;
pub mod utils;

use std::collections::HashMap;
use std::path::Path;

use crate::intelligence::bootstrap::BootstrapLoader;
use crate::intelligence::memory::MemoryStore;
use crate::intelligence::prompt::PromptFragments;
use crate::intelligence::skills::{Skill, SkillsManager};

/// Intelligence context for dynamic system prompt assembly
///
/// Loaded once at session creation. System prompt is rebuilt each turn.
pub struct Intelligence {
    pub memory: MemoryStore,
    fragments: PromptFragments,
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
    /// Loads prompt fragments, bootstrap files, and discovers skills once.
    ///
    /// # Arguments
    ///
    /// * `workspace_dir` - Path to the workspace containing bootstrap files
    /// * `prompts_dir` - Path to the `prompts/` directory for prompt fragments
    /// * `identity` - Identity text from AGENT.md body (Layer 1)
    /// * `agent_id` - Agent identifier for runtime context
    /// * `channel` - Channel type (cli, discord, etc.)
    /// * `model` - Model name for runtime context
    pub fn new(
        workspace_dir: &Path,
        prompts_dir: &Path,
        identity: String,
        agent_id: String,
        channel: String,
        model: String,
    ) -> Self {
        let fragments = PromptFragments::load(prompts_dir);

        let loader = BootstrapLoader::new(workspace_dir);
        let bootstrap_data = loader.load_all("full");

        let mut skills_mgr = SkillsManager::new(workspace_dir);
        skills_mgr.discover(&[]);

        let mut memory = MemoryStore::new(&workspace_dir.join("memory"));
        memory.discover();

        Self {
            memory,
            fragments,
            bootstrap_data,
            skills: skills_mgr.skills,
            identity,
            agent_id,
            channel,
            model,
        }
    }

    /// Find a skill by its invocation command (e.g. "/greet")
    pub fn find_skill(&self, invocation: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.invocation == invocation)
    }

    /// Build the system prompt from all loaded layers
    pub fn build_prompt(&self) -> String {
        prompt::build_system_prompt(
            "full",
            &self.fragments,
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
        let prompts = Path::new("prompts");
        if !workspace.exists() || !prompts.exists() {
            return; // skip if not running from project root
        }

        // Read AGENT.md body as identity (same as discover_workspace)
        let agent_md = std::fs::read_to_string(
            workspace.join("AGENT.md"),
        )
        .unwrap();
        let identity =
            crate::intelligence::utils::extract_body(&agent_md);

        let intel = Intelligence::new(
            workspace,
            prompts,
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

        // Layer 2: Soul from workspace/mandeven/SOUL.md
        assert!(
            prompt.contains("# Personality"),
            "should contain personality section"
        );
        assert!(
            prompt.contains("Direct and to the point"),
            "should contain soul content"
        );

        // Layer 7: Runtime context
        assert!(prompt.contains("Agent ID: mandeven"));
        assert!(prompt.contains("Channel: cli"));

        // Should NOT use fallback system.md identity
        assert!(
            !prompt.starts_with("You are a helpful"),
            "should use AGENT.md identity, not fallback"
        );

        println!("--- Mandeven System Prompt ---");
        println!("{prompt}");
        println!("--- End ({} chars) ---", prompt.len());
    }
}