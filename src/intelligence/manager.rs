use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::intelligence::utils::{extract_body, parse_frontmatter};

const DEFAULT_AGENT_ID: &str = "main";

static VALID_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,63}$").unwrap());
static INVALID_CHARS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").unwrap());

/// Normalize a raw string into a valid agent ID
///
/// Lowercases, replaces invalid characters with hyphens,
/// and truncates to 64 chars. Falls back to "main" if empty.
///
/// # Arguments
///
/// * `value` - Raw agent ID string
///
/// # Returns
///
/// A valid agent ID matching `[a-z0-9][a-z0-9_-]{0,63}`.
pub fn normalize_agent_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_AGENT_ID.to_string();
    }
    if VALID_ID_RE.is_match(trimmed) {
        return trimmed.to_lowercase();
    }
    let lower = trimmed.to_lowercase();
    let cleaned = INVALID_CHARS_RE.replace_all(&lower, "-");
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        return DEFAULT_AGENT_ID.to_string();
    }
    cleaned[..cleaned.len().min(64)].to_string()
}

/// Per-agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    pub personality: String,
    /// Explicit system prompt; if empty, generated from name + personality
    pub system_prompt: String,
    /// Model override; empty means use global default
    pub model: String,
    /// Session isolation scope: "main", "per-peer", "per-channel-peer",
    /// "per-account-channel-peer"
    pub dm_scope: String,
    /// Per-agent workspace directory; overrides global `WORKSPACE_DIR`
    pub workspace_dir: String,
}

impl AgentConfig {
    /// Resolve the effective system prompt
    ///
    /// # Returns
    ///
    /// Explicit `system_prompt` if set, otherwise generated from name + personality.
    pub fn effective_system_prompt(&self) -> String {
        if !self.system_prompt.is_empty() {
            return self.system_prompt.clone();
        }
        let mut parts = vec![format!("You are {}.", self.name)];
        if !self.personality.is_empty() {
            parts.push(format!("Your personality: {}", self.personality));
        }
        parts.push("Answer questions helpfully and stay in character.".into());
        parts.join(" ")
    }
}

/// Registry of agent configurations
pub struct AgentManager {
    agents: HashMap<String, AgentConfig>,
    /// Global default model from env
    default_model: String,
}

impl AgentManager {
    /// Create a new agent manager
    ///
    /// # Arguments
    ///
    /// * `default_model` - Global default model used when an agent has no override
    pub fn new(default_model: String) -> Self {
        Self {
            agents: HashMap::new(),
            default_model,
        }
    }

    /// Register an agent, normalizing its ID
    ///
    /// # Arguments
    ///
    /// * `config` - Agent configuration; `id` will be normalized
    pub fn register(&mut self, mut config: AgentConfig) {
        config.id = normalize_agent_id(&config.id);
        self.agents.insert(config.id.clone(), config);
    }

    /// Look up an agent by ID
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent identifier (normalized before lookup)
    ///
    /// # Returns
    ///
    /// `None` if no agent is registered with the given ID.
    pub fn get(&self, agent_id: &str) -> Option<&AgentConfig> {
        self.agents.get(&normalize_agent_id(agent_id))
    }

    /// List all registered agents
    pub fn list(&self) -> Vec<&AgentConfig> {
        self.agents.values().collect()
    }

    /// Resolve the effective model for an agent
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent identifier
    ///
    /// # Returns
    ///
    /// Per-agent model if set, otherwise the global default.
    pub fn effective_model(&self, agent_id: &str) -> String {
        self.get(agent_id)
            .filter(|a| !a.model.is_empty())
            .map(|a| a.model.clone())
            .unwrap_or_else(|| self.default_model.clone())
    }

    /// Discover agents from workspace subdirectories
    ///
    /// Each subdirectory containing `AGENT.md` is registered as an agent.
    /// Frontmatter: `name`, `personality`, `model`, `dm_scope`.
    /// Body: used as the agent's system prompt (identity).
    /// Directory name becomes the agent ID.
    pub fn discover_workspace(&mut self, base_dir: &Path) {
        let entries = match std::fs::read_dir(base_dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let agent_md = entry.path().join("AGENT.md");
            let content = match std::fs::read_to_string(&agent_md) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let meta = parse_frontmatter(&content);
            let dir_name =
                entry.file_name().to_string_lossy().to_string();
            let identity = extract_body(&content);

            self.register(AgentConfig {
                id: dir_name.clone(),
                name: meta
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| dir_name.clone()),
                personality: meta
                    .get("personality")
                    .cloned()
                    .unwrap_or_default(),
                system_prompt: identity,
                model: meta
                    .get("model")
                    .cloned()
                    .unwrap_or_default(),
                dm_scope: meta
                    .get("dm_scope")
                    .cloned()
                    .unwrap_or_else(|| "per-peer".into()),
                workspace_dir: entry
                    .path()
                    .to_string_lossy()
                    .to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_valid() {
        assert_eq!(normalize_agent_id("luna"), "luna");
        assert_eq!(normalize_agent_id("sage-2"), "sage-2");
        assert_eq!(normalize_agent_id("bot_v3"), "bot_v3");
    }

    #[test]
    fn test_normalize_empty() {
        assert_eq!(normalize_agent_id(""), "main");
        assert_eq!(normalize_agent_id("   "), "main");
    }

    #[test]
    fn test_normalize_invalid_chars() {
        assert_eq!(normalize_agent_id("Hello World!"), "hello-world");
        assert_eq!(normalize_agent_id("agent@v2.0"), "agent-v2-0");
    }

    #[test]
    fn test_normalize_uppercase() {
        assert_eq!(normalize_agent_id("LUNA"), "luna");
    }

    #[test]
    fn test_agent_config_system_prompt() {
        let config = AgentConfig {
            id: "luna".into(),
            name: "Luna".into(),
            personality: "warm and curious".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        };
        let prompt = config.effective_system_prompt();
        assert!(prompt.contains("You are Luna."));
        assert!(prompt.contains("warm and curious"));
    }

    #[test]
    fn test_agent_config_system_prompt_no_personality() {
        let config = AgentConfig {
            id: "bot".into(),
            name: "Bot".into(),
            personality: String::new(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        };
        let prompt = config.effective_system_prompt();
        assert!(prompt.contains("You are Bot."));
        assert!(!prompt.contains("personality"));
    }

    #[test]
    fn test_manager_register_and_get() {
        let mut mgr = AgentManager::new("default-model".into());
        mgr.register(AgentConfig {
            id: "Luna".into(),
            name: "Luna".into(),
            personality: String::new(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        });
        assert!(mgr.get("luna").is_some());
        assert!(mgr.get("LUNA").is_some());
        assert!(mgr.get("nonexistent").is_none());
    }

    #[test]
    fn test_manager_effective_model() {
        let mut mgr = AgentManager::new("global-model".into());
        mgr.register(AgentConfig {
            id: "a".into(),
            name: "A".into(),
            personality: String::new(),
            system_prompt: String::new(),
            model: "custom-model".into(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        });
        mgr.register(AgentConfig {
            id: "b".into(),
            name: "B".into(),
            personality: String::new(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        });
        assert_eq!(mgr.effective_model("a"), "custom-model");
        assert_eq!(mgr.effective_model("b"), "global-model");
    }
}
