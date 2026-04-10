use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, LazyLock, RwLock};

use regex::Regex;

use crate::config::tuning;
use crate::intelligence::utils::{DiscoveredFile, discover_subdirs, extract_body};

static VALID_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_-]{0,63}$").unwrap());
static INVALID_CHARS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9_-]+").unwrap());

/// Normalize a raw string into a valid agent ID
///
/// Lowercases, replaces invalid characters with hyphens,
/// and truncates to 64 chars. Falls back to default agent ID if empty.
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
        return tuning().routing.default_agent_id.clone();
    }
    if VALID_ID_RE.is_match(trimmed) {
        return trimmed.to_lowercase();
    }
    let lower = trimmed.to_lowercase();
    let cleaned = INVALID_CHARS_RE.replace_all(&lower, "-");
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        return tuning().routing.default_agent_id.clone();
    }
    cleaned[..cleaned.len().min(64)].to_string()
}

/// Per-agent configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DmScope {
    Main,
    #[default]
    PerPeer,
    PerChannelPeer,
    PerAccountChannelPeer,
}

impl DmScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::PerPeer => "per-peer",
            Self::PerChannelPeer => "per-channel-peer",
            Self::PerAccountChannelPeer => "per-account-channel-peer",
        }
    }
}

impl FromStr for DmScope {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "main" => Ok(Self::Main),
            "per-peer" => Ok(Self::PerPeer),
            "per-channel-peer" => Ok(Self::PerChannelPeer),
            "per-account-channel-peer" => Ok(Self::PerAccountChannelPeer),
            _ => Err(format!(
                "invalid dm_scope '{value}', expected one of: main, per-peer, per-channel-peer, per-account-channel-peer"
            )),
        }
    }
}

/// Per-agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    /// System prompt (identity); if empty, generated from name
    pub system_prompt: String,
    /// Model override; empty means use global default
    pub model: String,
    /// Session isolation scope.
    pub dm_scope: DmScope,
    /// Per-agent workspace directory; overrides global `WORKSPACE_DIR`
    pub workspace_dir: String,
    /// Tools this agent is not allowed to use (parsed from AGENT.md frontmatter)
    pub denied_tools: Vec<String>,
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
    #[must_use]
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
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<&AgentConfig> {
        self.agents.get(&normalize_agent_id(agent_id))
    }

    /// List all registered agents
    #[must_use]
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
    #[must_use]
    pub fn effective_model(&self, agent_id: &str) -> String {
        self.get(agent_id)
            .filter(|a| !a.model.is_empty())
            .map_or_else(|| self.default_model.clone(), |agent| agent.model.clone())
    }

    /// Discover agents from workspace subdirectories
    ///
    /// Each subdirectory containing `AGENT.md` is registered as an agent.
    /// Directory name becomes both the agent ID and name.
    /// The entire file content is used as the system prompt (identity).
    pub fn discover_workspace(&mut self, base_dir: &Path) {
        for file in discover_subdirs(base_dir, "AGENT.md") {
            self.register(Self::build_discovered_agent(file));
        }
    }

    fn build_discovered_agent(file: DiscoveredFile) -> AgentConfig {
        let system_prompt = extract_identity(&file.content);
        let workspace_dir = file
            .path
            .parent()
            .map_or_else(String::new, |path| path.to_string_lossy().to_string());
        let denied_tools = denied_tools_from_meta(&file.meta);
        let dm_scope = discovered_dm_scope(&file.meta);

        AgentConfig {
            id: file.name.clone(),
            name: file.name,
            system_prompt,
            model: String::new(),
            dm_scope,
            workspace_dir,
            denied_tools,
        }
    }
}

/// Read-only handle to the agent registry
///
/// Wraps `Arc<RwLock<AgentManager>>` and exposes only read operations.
/// Shared between router (owns the manager) and sessions (read-only).
#[derive(Clone)]
pub struct SharedAgents(Arc<RwLock<AgentManager>>);

impl SharedAgents {
    /// Create from an `Arc<RwLock<AgentManager>>`
    pub fn new(mgr: Arc<RwLock<AgentManager>>) -> Self {
        Self(mgr)
    }

    /// Create an empty registry (for tests and standalone contexts)
    #[must_use]
    pub fn empty() -> Self {
        Self(Arc::new(RwLock::new(AgentManager::new(String::new()))))
    }

    /// Look up an agent by ID
    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<AgentConfig> {
        self.read().get(agent_id).cloned()
    }

    /// List all registered agents
    #[must_use]
    pub fn list(&self) -> Vec<AgentConfig> {
        self.read().list().into_iter().cloned().collect()
    }

    /// Resolve the effective model for an agent
    #[must_use]
    pub fn effective_model(&self, agent_id: &str) -> String {
        self.read().effective_model(agent_id)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, AgentManager> {
        self.0.read().unwrap_or_else(|e| {
            log::error!("AgentManager lock poisoned, recovering: {e}");
            e.into_inner()
        })
    }
}

fn extract_identity(content: &str) -> String {
    let identity = extract_body(content);
    if identity.is_empty() {
        content.trim().to_string()
    } else {
        identity
    }
}

fn denied_tools_from_meta(meta: &HashMap<String, String>) -> Vec<String> {
    meta.get("denied_tools").map_or_else(Vec::new, |value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    })
}

fn discovered_dm_scope(meta: &HashMap<String, String>) -> DmScope {
    let Some(value) = meta.get("dm_scope") else {
        return DmScope::default();
    };

    match value.parse::<DmScope>() {
        Ok(scope) => scope,
        Err(error) => {
            log::warn!("Invalid AGENT.md dm_scope '{value}': {error}");
            DmScope::default()
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
        assert_eq!(normalize_agent_id(""), "mandeven");
        assert_eq!(normalize_agent_id("   "), "mandeven");
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
            system_prompt: "You are Luna, warm and curious.".into(),
            model: String::new(),
            dm_scope: DmScope::PerPeer,
            workspace_dir: String::new(),
            denied_tools: Vec::new(),
        };
        assert_eq!(config.system_prompt, "You are Luna, warm and curious.");
    }

    #[test]
    fn test_manager_register_and_get() {
        let mut mgr = AgentManager::new("default-model".into());
        mgr.register(AgentConfig {
            id: "Luna".into(),
            name: "Luna".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: DmScope::PerPeer,
            workspace_dir: String::new(),
            denied_tools: Vec::new(),
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
            system_prompt: String::new(),
            model: "custom-model".into(),
            dm_scope: DmScope::PerPeer,
            workspace_dir: String::new(),
            denied_tools: Vec::new(),
        });
        mgr.register(AgentConfig {
            id: "b".into(),
            name: "B".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: DmScope::PerPeer,
            workspace_dir: String::new(),
            denied_tools: Vec::new(),
        });
        assert_eq!(mgr.effective_model("a"), "custom-model");
        assert_eq!(mgr.effective_model("b"), "global-model");
    }

    #[test]
    fn test_dm_scope_roundtrip() {
        assert_eq!("per-peer".parse::<DmScope>().unwrap(), DmScope::PerPeer);
        assert_eq!(
            DmScope::PerAccountChannelPeer.as_str(),
            "per-account-channel-peer"
        );
        assert!("invalid".parse::<DmScope>().is_err());
    }
}
