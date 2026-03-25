use crate::intelligence::manager::{AgentManager, normalize_agent_id};
use crate::frontend::UserMessage;

/// Routing decision result
pub struct RouteResult {
    pub agent_id: String,
    pub session_key: String,
}

/// Route resolver
///
/// Given an inbound message, determines which agent handles it
/// and what session context to use.
pub trait Router: Send + Sync {
    /// Resolve routing for a message
    fn resolve(&self, msg: &UserMessage) -> RouteResult;

    /// Resolve routing with an explicit agent ID override (e.g. /switch)
    fn resolve_explicit(&self, agent_id: &str, msg: &UserMessage) -> RouteResult;
}

/// A routing rule that maps a match condition to an agent
///
/// Tiers (lower = more specific, matched first):
/// 1. peer_id — route a specific user
/// 2. guild_id — route by server/guild
/// 3. account_id — route by bot account
/// 4. channel — route by entire channel type
/// 5. default — catch-all fallback
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub agent_id: String,
    pub tier: u8,
    pub match_key: String,
    pub match_value: String,
    /// Tie-breaker within the same tier (higher wins)
    pub priority: i32,
}

/// Five-tier binding table, sorted by (tier ASC, priority DESC)
pub struct BindingTable {
    bindings: Vec<Binding>,
}

impl BindingTable {
    /// Create an empty binding table
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Load bindings from a JSON file
    ///
    /// Each entry is a `Binding` object. Existing bindings are preserved;
    /// loaded bindings are appended and the table is re-sorted.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to a JSON file containing an array of bindings
    pub fn load_file(&mut self, path: &std::path::Path) {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let bindings: Vec<Binding> = match serde_json::from_str(&content) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("Failed to parse {}: {e}", path.display());
                return;
            }
        };
        for b in bindings {
            self.add(b);
        }
    }

    /// List all bindings
    pub fn list(&self) -> &[Binding] {
        &self.bindings
    }

    /// Add a binding and re-sort the table
    pub fn add(&mut self, binding: Binding) {
        self.bindings.push(binding);
        self.bindings
            .sort_by(|a, b| a.tier.cmp(&b.tier).then(b.priority.cmp(&a.priority)));
    }

    /// Remove a binding by (agent_id, match_key, match_value)
    ///
    /// # Arguments
    ///
    /// * `agent_id` - Agent to match
    /// * `match_key` - Key to match (e.g. "peer_id", "channel")
    /// * `match_value` - Value to match
    ///
    /// # Returns
    ///
    /// `true` if a binding was removed.
    pub fn remove(
        &mut self,
        agent_id: &str,
        match_key: &str,
        match_value: &str,
    ) -> bool {
        let before = self.bindings.len();
        self.bindings.retain(|b| {
            !(b.agent_id == agent_id
                && b.match_key == match_key
                && b.match_value == match_value)
        });
        self.bindings.len() < before
    }

    /// Walk tiers 1-5, return first matching binding
    ///
    /// # Arguments
    ///
    /// * `channel` - Channel type (e.g. "cli", "discord")
    /// * `account_id` - Bot account identifier
    /// * `guild_id` - Server/guild identifier
    /// * `peer_id` - User identifier
    ///
    /// # Returns
    ///
    /// The first matched binding, or `None` if no binding matches.
    pub fn resolve_msg(
        &self,
        channel: &str,
        account_id: &str,
        guild_id: &str,
        peer_id: &str,
    ) -> Option<&Binding> {
        for b in &self.bindings {
            let matched = match (b.tier, b.match_key.as_str()) {
                (1, "peer_id") => {
                    if b.match_value.contains(':') {
                        b.match_value == format!("{channel}:{peer_id}")
                    } else {
                        b.match_value == peer_id
                    }
                }
                (2, "guild_id") => b.match_value == guild_id,
                (3, "account_id") => b.match_value == account_id,
                (4, "channel") => b.match_value == channel,
                (5, "default") => true,
                _ => false,
            };
            if matched {
                return Some(b);
            }
        }
        None
    }
}

/// Build a session key based on dm_scope
///
/// # Arguments
///
/// * `agent_id` - Agent identifier
/// * `channel` - Channel type (e.g. "cli", "discord")
/// * `account_id` - Bot account identifier
/// * `peer_id` - User identifier
/// * `dm_scope` - Isolation scope
///
/// # Returns
///
/// Session key string. Scopes:
/// - `main`                     → `agent:{id}:main`
/// - `per-peer`                 → `agent:{id}:direct:{peer}`
/// - `per-channel-peer`         → `agent:{id}:{ch}:direct:{peer}`
/// - `per-account-channel-peer` → `agent:{id}:{ch}:{acc}:direct:{peer}`
pub fn build_session_key(
    agent_id: &str,
    channel: &str,
    account_id: &str,
    peer_id: &str,
    dm_scope: &str,
) -> String {
    let aid = normalize_agent_id(agent_id);
    let ch = if channel.is_empty() {
        "unknown"
    } else {
        channel
    };
    let acc = if account_id.is_empty() {
        "default"
    } else {
        account_id
    };

    if !peer_id.is_empty() {
        match dm_scope {
            "per-account-channel-peer" => {
                return format!("agent:{aid}:{ch}:{acc}:direct:{peer_id}");
            }
            "per-channel-peer" => {
                return format!("agent:{aid}:{ch}:direct:{peer_id}");
            }
            "per-peer" => {
                return format!("agent:{aid}:direct:{peer_id}");
            }
            _ => {}
        }
    }
    format!("agent:{aid}:main")
}

/// Router backed by a BindingTable and AgentManager
///
/// Falls back to `default_agent_id` when no binding matches.
pub struct BindingRouter {
    table: BindingTable,
    mgr: AgentManager,
    default_agent_id: String,
}

impl BindingRouter {
    /// Create a new binding router
    ///
    /// # Arguments
    ///
    /// * `table` - Pre-configured binding table
    /// * `mgr` - Agent manager with registered agents
    /// * `default_agent_id` - Fallback agent when no binding matches
    pub fn new(
        table: BindingTable,
        mgr: AgentManager,
        default_agent_id: &str,
    ) -> Self {
        Self {
            table,
            mgr,
            default_agent_id: normalize_agent_id(default_agent_id),
        }
    }

    /// Mutable access to the binding table
    pub fn table_mut(&mut self) -> &mut BindingTable {
        &mut self.table
    }

    /// Read access to the binding table
    pub fn table(&self) -> &BindingTable {
        &self.table
    }

    /// Mutable access to the agent manager
    pub fn manager_mut(&mut self) -> &mut AgentManager {
        &mut self.mgr
    }

    /// Read access to the agent manager
    pub fn manager(&self) -> &AgentManager {
        &self.mgr
    }
}

impl BindingRouter {
    fn build_result(&self, agent_id: &str, msg: &UserMessage) -> RouteResult {
        let dm_scope = self
            .mgr
            .get(agent_id)
            .map(|a| a.dm_scope.as_str())
            .unwrap_or("per-peer");

        let session_key = build_session_key(
            agent_id,
            &msg.channel,
            &msg.account_id,
            &msg.sender_id,
            dm_scope,
        );

        RouteResult {
            agent_id: agent_id.to_string(),
            session_key,
        }
    }
}

impl Router for BindingRouter {
    fn resolve(&self, msg: &UserMessage) -> RouteResult {
        let agent_id = self
            .table
            .resolve_msg(
                &msg.channel,
                &msg.account_id,
                &msg.guild_id,
                &msg.sender_id,
            )
            .map(|b| b.agent_id.clone())
            .unwrap_or_else(|| self.default_agent_id.clone());

        self.build_result(&agent_id, msg)
    }

    fn resolve_explicit(
        &self,
        agent_id: &str,
        msg: &UserMessage,
    ) -> RouteResult {
        let aid = normalize_agent_id(agent_id);
        self.build_result(&aid, msg)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::manager::AgentConfig;

    fn msg(channel: &str, sender_id: &str) -> UserMessage {
        UserMessage {
            text: String::new(),
            sender_id: sender_id.into(),
            channel: channel.into(),
            account_id: String::new(),
            guild_id: String::new(),
        }
    }

    // BindingTable

    #[test]
    fn test_binding_table_tier_order() {
        let mut bt = BindingTable::new();
        bt.add(Binding {
            agent_id: "luna".into(),
            tier: 5,
            match_key: "default".into(),
            match_value: "*".into(),
            priority: 0,
        });
        bt.add(Binding {
            agent_id: "sage".into(),
            tier: 4,
            match_key: "channel".into(),
            match_value: "telegram".into(),
            priority: 0,
        });

        let b = bt.resolve_msg("telegram", "", "", "user1").unwrap();
        assert_eq!(b.agent_id, "sage");

        let b = bt.resolve_msg("cli", "", "", "user1").unwrap();
        assert_eq!(b.agent_id, "luna");
    }

    #[test]
    fn test_binding_table_peer_specific() {
        let mut bt = BindingTable::new();
        bt.add(Binding {
            agent_id: "luna".into(),
            tier: 5,
            match_key: "default".into(),
            match_value: "*".into(),
            priority: 0,
        });
        bt.add(Binding {
            agent_id: "sage".into(),
            tier: 1,
            match_key: "peer_id".into(),
            match_value: "discord:admin-001".into(),
            priority: 10,
        });

        let b = bt.resolve_msg("discord", "", "", "admin-001").unwrap();
        assert_eq!(b.agent_id, "sage");

        let b = bt.resolve_msg("discord", "", "", "user-999").unwrap();
        assert_eq!(b.agent_id, "luna");
    }

    #[test]
    fn test_binding_table_guild() {
        let mut bt = BindingTable::new();
        bt.add(Binding {
            agent_id: "luna".into(),
            tier: 5,
            match_key: "default".into(),
            match_value: "*".into(),
            priority: 0,
        });
        bt.add(Binding {
            agent_id: "sage".into(),
            tier: 2,
            match_key: "guild_id".into(),
            match_value: "guild-42".into(),
            priority: 0,
        });

        let b = bt.resolve_msg("discord", "", "guild-42", "user1").unwrap();
        assert_eq!(b.agent_id, "sage");

        let b = bt.resolve_msg("discord", "", "guild-99", "user1").unwrap();
        assert_eq!(b.agent_id, "luna");
    }

    #[test]
    fn test_binding_table_no_match() {
        let bt = BindingTable::new();
        assert!(bt.resolve_msg("cli", "", "", "user1").is_none());
    }

    #[test]
    fn test_binding_table_remove() {
        let mut bt = BindingTable::new();
        bt.add(Binding {
            agent_id: "luna".into(),
            tier: 5,
            match_key: "default".into(),
            match_value: "*".into(),
            priority: 0,
        });
        assert!(bt.remove("luna", "default", "*"));
        assert!(bt.list().is_empty());
        assert!(!bt.remove("luna", "default", "*"));
    }

    // Session key builder

    #[test]
    fn test_session_key_per_peer() {
        let sk = build_session_key("luna", "discord", "", "user1", "per-peer");
        assert_eq!(sk, "agent:luna:direct:user1");
    }

    #[test]
    fn test_session_key_per_channel_peer() {
        let sk = build_session_key("luna", "discord", "", "user1", "per-channel-peer");
        assert_eq!(sk, "agent:luna:discord:direct:user1");
    }

    #[test]
    fn test_session_key_main() {
        let sk = build_session_key("luna", "discord", "", "user1", "main");
        assert_eq!(sk, "agent:luna:main");
    }

    #[test]
    fn test_session_key_no_peer() {
        let sk = build_session_key("luna", "discord", "", "", "per-peer");
        assert_eq!(sk, "agent:luna:main");
    }

    // BindingRouter (integration)

    #[test]
    fn test_binding_router_full() {
        let mut mgr = AgentManager::new("default-model".into());
        mgr.register(AgentConfig {
            id: "luna".into(),
            name: "Luna".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-peer".into(),
            workspace_dir: String::new(),
        });
        mgr.register(AgentConfig {
            id: "sage".into(),
            name: "Sage".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-channel-peer".into(),
            workspace_dir: String::new(),
        });

        let mut bt = BindingTable::new();
        bt.add(Binding {
            agent_id: "luna".into(),
            tier: 5,
            match_key: "default".into(),
            match_value: "*".into(),
            priority: 0,
        });
        bt.add(Binding {
            agent_id: "sage".into(),
            tier: 4,
            match_key: "channel".into(),
            match_value: "telegram".into(),
            priority: 0,
        });

        let router = BindingRouter::new(bt, mgr, "luna");

        let r = router.resolve(&msg("cli", "user1"));
        assert_eq!(r.agent_id, "luna");
        assert_eq!(r.session_key, "agent:luna:direct:user1");

        let r = router.resolve(&msg("telegram", "user2"));
        assert_eq!(r.agent_id, "sage");
        assert_eq!(r.session_key, "agent:sage:telegram:direct:user2");
    }

    #[test]
    fn test_binding_router_fallback() {
        let mgr = AgentManager::new("m".into());
        let bt = BindingTable::new();
        let router = BindingRouter::new(bt, mgr, "mandeven");

        let r = router.resolve(&msg("cli", "user1"));
        assert_eq!(r.agent_id, "mandeven");
        assert_eq!(r.session_key, "agent:mandeven:direct:user1");
    }

    #[test]
    fn test_binding_router_explicit() {
        let mut mgr = AgentManager::new("m".into());
        mgr.register(AgentConfig {
            id: "sage".into(),
            name: "Sage".into(),
            system_prompt: String::new(),
            model: String::new(),
            dm_scope: "per-channel-peer".into(),
            workspace_dir: String::new(),
        });
        let bt = BindingTable::new();
        let router = BindingRouter::new(bt, mgr, "mandeven");

        let r = router.resolve_explicit("sage", &msg("discord", "user1"));
        assert_eq!(r.agent_id, "sage");
        assert_eq!(r.session_key, "agent:sage:discord:direct:user1");
    }
}
