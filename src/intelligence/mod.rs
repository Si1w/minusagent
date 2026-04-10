//! Dynamic system-prompt assembly and per-agent intelligence.
//!
//! This module assembles the seven layers of an agent's system prompt at
//! every turn:
//!
//! 1. Identity (from `AGENTS.md` frontmatter)
//! 2. Tools (built dynamically from the registered tool set)
//! 3. Skills (loaded on demand via `/<skill>` commands)
//! 4. Memory (persisted across conversations)
//! 5. Bootstrap (markdown files like `AGENTS.md`, `TOOLS.md`)
//! 6. Runtime (current time, working directory, …)
//! 7. Channel (frontend-specific affordances)
//!
//! ## Submodules
//!
//! - [`bootstrap`] — Loads markdown bootstrap files (`AGENTS.md`, `TOOLS.md`, …).
//! - [`manager`] — `AgentManager`, `AgentConfig`, workspace discovery.
//! - [`memory`] — Persistent per-agent memory store.
//! - [`prompt`] — Prompt assembly itself (`Intelligence::build_prompt`).
//! - [`skills`] — On-demand `SKILL.md` loading.
//! - [`utils`] — Frontmatter parsing and small file helpers.

pub mod bootstrap;
pub mod manager;
pub mod memory;
pub mod prompt;
pub mod skills;
pub mod utils;

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

struct IntelligenceResources {
    bootstrap_data: std::collections::HashMap<String, String>,
    skills: Vec<Skill>,
    memory: MemoryStore,
}

/// Intelligence context for dynamic system prompt assembly
///
/// Static layers (identity, tools, skills, bootstrap, channel) are cached at
/// session creation. Only dynamic layers (memory, runtime) are rebuilt each turn.
pub struct Intelligence {
    pub memory: MemoryStore,
    skills: Vec<Skill>,
    agent_id: String,
    channel: String,
    model: String,
    mode: PromptMode,
    /// Cached static prefix (built once in `new()`)
    static_prefix: String,
}

impl Intelligence {
    /// Initialize intelligence from a workspace directory
    ///
    /// Loads bootstrap files, discovers skills and memory, then caches the
    /// static prompt prefix for reuse across turns.
    ///
    /// # Arguments
    ///
    /// * `workspace_dir` - Path to the workspace containing bootstrap files
    /// * `identity` - Identity text from AGENT.md body (Layer 1)
    /// * `agent_id` - Agent identifier for runtime context
    /// * `channel` - Channel type (cli, discord, etc.)
    /// * `model` - Model name for runtime context
    #[must_use]
    pub fn new(
        workspace_dir: &Path,
        identity: &str,
        agent_id: String,
        channel: String,
        model: String,
    ) -> Self {
        let mode = PromptMode::Full;
        let IntelligenceResources {
            bootstrap_data,
            skills,
            memory,
        } = load_intelligence_resources(workspace_dir, mode);
        let static_prefix =
            prompt::build_static_prefix(mode, identity, &bootstrap_data, &skills, &channel);

        Self {
            memory,
            skills,
            agent_id,
            channel,
            model,
            mode,
            static_prefix,
        }
    }

    /// Find a skill by name (e.g. "greet")
    #[must_use]
    pub fn find_skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Build the system prompt: cached static prefix + fresh dynamic suffix
    #[must_use]
    pub fn build_prompt(&self) -> String {
        let dynamic = prompt::build_dynamic_suffix(
            self.mode,
            &self.memory.entries,
            &self.agent_id,
            &self.model,
            &self.channel,
        );
        prompt::join_prompt(&self.static_prefix, &dynamic)
    }
}

fn load_intelligence_resources(workspace_dir: &Path, mode: PromptMode) -> IntelligenceResources {
    let loader = BootstrapLoader::new(workspace_dir);
    let bootstrap_data = loader.load_all(mode);

    let mut skills_mgr = SkillsManager::new(workspace_dir);
    skills_mgr.discover(&[]);

    let mut memory = MemoryStore::new(&workspace_dir.join("memory"));
    memory.discover();

    IntelligenceResources {
        bootstrap_data,
        skills: skills_mgr.skills,
        memory,
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
        let identity = std::fs::read_to_string(workspace.join("AGENT.md"))
            .unwrap()
            .trim()
            .to_string();

        let intel = Intelligence::new(
            workspace,
            &identity,
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
