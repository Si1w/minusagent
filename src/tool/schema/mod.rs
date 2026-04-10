mod background;
mod builder;
mod cron;
mod execution;
mod task;
mod team;
mod web;
mod worktree;

use crate::tool::ToolDefinition;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    Tasks = 1 << 0,
    Team = 1 << 1,
    Worktrees = 1 << 2,
    Cron = 1 << 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolScope {
    Primary,
    Subagent,
}

impl ToolScope {
    const fn is_subagent(self) -> bool {
        matches!(self, Self::Subagent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ToolCapabilities(u8);

impl ToolCapabilities {
    const fn with(self, capability: ToolCapability) -> Self {
        Self(self.0 | capability as u8)
    }

    const fn contains(self, capability: ToolCapability) -> bool {
        (self.0 & capability as u8) != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolAvailability {
    scope: ToolScope,
    capabilities: ToolCapabilities,
}

impl Default for ToolAvailability {
    fn default() -> Self {
        Self::primary()
    }
}

impl ToolAvailability {
    #[must_use]
    pub const fn primary() -> Self {
        Self {
            scope: ToolScope::Primary,
            capabilities: ToolCapabilities(0),
        }
    }

    #[must_use]
    pub const fn subagent() -> Self {
        Self {
            scope: ToolScope::Subagent,
            capabilities: ToolCapabilities(0),
        }
    }

    #[must_use]
    pub const fn with(self, capability: ToolCapability) -> Self {
        Self {
            scope: self.scope,
            capabilities: self.capabilities.with(capability),
        }
    }

    const fn is_subagent(self) -> bool {
        self.scope.is_subagent()
    }

    const fn is_primary(self) -> bool {
        !self.is_subagent()
    }

    const fn has(self, capability: ToolCapability) -> bool {
        self.capabilities.contains(capability)
    }
}

/// All built-in tool definitions for LLM registration (no filtering)
#[cfg(test)]
pub fn all_tools(availability: ToolAvailability) -> Vec<ToolDefinition> {
    all_tools_filtered(availability, &[])
}

/// All built-in tool definitions, excluding denied tools
#[must_use]
pub fn all_tools_filtered(
    availability: ToolAvailability,
    denied: &[String],
) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();
    tools.extend(execution::tools());
    tools.extend(background::tools());
    tools.extend(web::tools());

    if availability.is_primary() {
        tools.extend(task::primary_tools());
    }
    if availability.has(ToolCapability::Tasks) {
        tools.extend(task::graph_tools());
    }
    if availability.has(ToolCapability::Team) {
        if availability.is_primary() {
            tools.extend(team::primary_tools());
        }
        tools.extend(team::shared_tools());
        if availability.is_subagent() {
            tools.extend(team::subagent_tools());
        }
    }
    if availability.has(ToolCapability::Worktrees) {
        tools.extend(worktree::tools());
    }
    if availability.has(ToolCapability::Cron) {
        tools.extend(cron::tools());
    }
    if !denied.is_empty() {
        tools.retain(|tool| !denied.contains(&tool.function.name));
    }
    tools
}
