use std::collections::HashMap;

use chrono::Utc;

use crate::intelligence::PromptMode;
use crate::intelligence::memory::MemoryEntry;
use crate::intelligence::skills::Skill;

/// Boundary marker between static (cacheable) and dynamic (per-turn) layers.
///
/// Placing this in the prompt text helps LLM providers with KV cache prefix
/// matching: everything before this marker is byte-identical across turns.
const DYNAMIC_BOUNDARY: &str = "═══ DYNAMIC_BOUNDARY ═══";

/// Wrap content as a `# heading` section, returns `None` if content is empty
///
/// Strips a leading H1 line from content if it matches the heading to avoid
/// duplication (e.g. TOOLS.md starting with `# Tool Usage Guidelines`).
fn section(heading: &str, content: &str) -> Option<String> {
    let content = content.trim();
    if content.is_empty() {
        return None;
    }
    let body = match content.split_once('\n') {
        Some((first, rest)) if first.trim().trim_start_matches('#').trim() == heading => {
            rest.trim_start()
        }
        _ => content,
    };
    Some(format!("# {heading}\n\n{body}"))
}

/// Format memory TLDRs into prompt content (without heading)
///
/// Each entry shows name, TLDR, and file path so the LLM can
/// use `read_file` to progressively load full content when relevant.
pub fn format_memory_content(entries: &[MemoryEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    entries
        .iter()
        .map(|e| {
            format!(
                "- `{}`: {} (path: `{}`)",
                e.name,
                e.tldr,
                e.path.display(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format discovered skills as a summary list (name + description only)
///
/// Full skill body is loaded on activation, not at prompt assembly time.
pub fn format_skills_content(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    skills
        .iter()
        .map(|s| format!("- `{}`: {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build the static prefix (cacheable across turns within a session)
///
/// # Static Layers
///
/// 1. Identity (AGENT.md body)
/// 2. Tool usage guidelines (TOOLS.md)
/// 3. Skills (name + description summary)
/// 5. Bootstrap context (HEARTBEAT.md, BOOTSTRAP.md, AGENTS.md, USER.md)
/// 7. Channel hints
pub fn build_static_prefix(
    mode: PromptMode,
    identity: &str,
    bootstrap: &HashMap<String, String>,
    skills: &[Skill],
    channel: &str,
) -> String {
    let is_full = mode == PromptMode::Full;
    let mut sections: Vec<String> = Vec::new();

    // Layer 1: Identity
    if !identity.trim().is_empty() {
        sections.push(identity.to_string());
    }

    // Layer 2: Tool usage guidelines
    sections.extend(section(
        "Tool Usage Guidelines",
        bootstrap.get("TOOLS.md").map(|s| s.as_str()).unwrap_or(""),
    ));

    // Layer 3: Skills
    if is_full {
        let skills_block = format_skills_content(skills);
        sections.extend(section("Available Skills", &skills_block));
    }

    // Layer 5: Bootstrap context
    if is_full || mode == PromptMode::Minimal {
        for name in ["HEARTBEAT.md", "BOOTSTRAP.md", "AGENTS.md", "USER.md"] {
            let title = name.trim_end_matches(".md");
            sections.extend(section(
                title,
                bootstrap.get(name).map(|s| s.as_str()).unwrap_or(""),
            ));
        }
    }

    // Layer 7: Channel hints
    sections.extend(section("Channel", channel_hint(channel)));

    sections.join("\n\n")
}

/// Build the dynamic suffix (rebuilt each turn)
///
/// # Dynamic Layers
///
/// 4. Memory (TLDR index — hot-updated via /remember)
/// 6. Runtime context (timestamp changes each turn)
pub fn build_dynamic_suffix(
    mode: PromptMode,
    memories: &[MemoryEntry],
    agent_id: &str,
    model: &str,
    channel: &str,
) -> String {
    let is_full = mode == PromptMode::Full;
    let mut sections: Vec<String> = Vec::new();

    // Layer 4: Memory
    if is_full {
        let memory_block = format_memory_content(memories);
        sections.extend(section("Memory", &memory_block));
    }

    // Layer 6: Runtime context
    let now = Utc::now().format("%Y-%m-%d %H:%M UTC");
    sections.extend(section(
        "Runtime Context",
        &format!(
            "- Agent ID: {agent_id}\n\
             - Model: {model}\n\
             - Channel: {channel}\n\
             - Current time: {now}\n\
             - Prompt mode: {mode}"
        ),
    ));

    sections.join("\n\n")
}

/// Build the full system prompt by joining static prefix and dynamic suffix
///
/// Convenience function that assembles all 7 layers with a boundary marker.
#[cfg(test)]
fn build_system_prompt(
    mode: PromptMode,
    identity: &str,
    bootstrap: &HashMap<String, String>,
    skills: &[Skill],
    memories: &[MemoryEntry],
    agent_id: &str,
    model: &str,
    channel: &str,
) -> String {
    let static_part = build_static_prefix(mode, identity, bootstrap, skills, channel);
    let dynamic_part = build_dynamic_suffix(mode, memories, agent_id, model, channel);
    join_prompt(&static_part, &dynamic_part)
}

/// Join static prefix and dynamic suffix with boundary marker
pub fn join_prompt(static_prefix: &str, dynamic_suffix: &str) -> String {
    if dynamic_suffix.is_empty() {
        return static_prefix.to_string();
    }
    format!("{static_prefix}\n\n{DYNAMIC_BOUNDARY}\n\n{dynamic_suffix}")
}

fn channel_hint(channel: &str) -> &str {
    match channel {
        "cli" | "terminal" => {
            "You are responding via a terminal REPL. Markdown is supported."
        }
        "telegram" => "You are responding via Telegram. Keep messages concise.",
        "discord" => {
            "You are responding via Discord. Keep messages under 2000 characters."
        }
        "slack" => "You are responding via Slack. Use Slack mrkdwn formatting.",
        _ => "You are responding via an API channel.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_in_prompt() {
        let prompt = build_system_prompt(
            PromptMode::Full, "You are Luna.", &HashMap::new(),
            &[], &[], "luna", "gpt-4", "cli",
        );
        assert!(prompt.starts_with("You are Luna."));
        assert!(prompt.contains("Agent ID: luna"));
    }

    #[test]
    fn test_empty_identity() {
        let prompt = build_system_prompt(
            PromptMode::Full, "", &HashMap::new(),
            &[], &[], "main", "gpt-4", "cli",
        );
        // Should not start with an empty section
        assert!(!prompt.starts_with('\n'));
        assert!(prompt.contains("Agent ID: main"));
    }

    #[test]
    fn test_minimal_skips_skills_and_memory() {
        let skills = vec![Skill {
            name: "greet".into(),
            description: "Say hello".into(),
            path: "/skills/greet/SKILL.md".into(),
        }];
        let memories = vec![MemoryEntry {
            name: "fact".into(),
            tldr: "A fact".into(),
            path: "/memory/fact.md".into(),
        }];
        let prompt = build_system_prompt(
            PromptMode::Minimal, "Identity.", &HashMap::new(),
            &skills, &memories, "main", "gpt-4", "cli",
        );
        assert!(!prompt.contains("Available Skills"));
        assert!(!prompt.contains("Memory"));
    }

    #[test]
    fn test_memory_tldr_index() {
        let memories = vec![MemoryEntry {
            name: "dark_mode".into(),
            tldr: "User prefers dark mode".into(),
            path: "/workspace/memory/dark_mode.md".into(),
        }];
        let prompt = build_system_prompt(
            PromptMode::Full, "Identity.", &HashMap::new(),
            &[], &memories, "main", "gpt-4", "cli",
        );
        assert!(prompt.contains("# Memory"));
        assert!(prompt.contains("`dark_mode`"));
        assert!(prompt.contains("User prefers dark mode"));
    }

    #[test]
    fn test_skills_summary() {
        let skills = vec![Skill {
            name: "greet".into(),
            description: "Say hello".into(),
            path: "/skills/greet/SKILL.md".into(),
        }];
        let prompt = build_system_prompt(
            PromptMode::Full, "Identity.", &HashMap::new(),
            &skills, &[], "main", "gpt-4", "cli",
        );
        assert!(prompt.contains("# Available Skills"));
        assert!(prompt.contains("`greet`: Say hello"));
        // Body (from file) should NOT appear in prompt
        assert!(!prompt.contains("Full instructions here."));
    }

    #[test]
    fn test_section_helper() {
        assert!(section("Title", "").is_none());
        assert_eq!(
            section("Title", "content").unwrap(),
            "# Title\n\ncontent"
        );
    }

    #[test]
    fn test_section_strips_duplicate_heading() {
        let result = section("Tool Usage Guidelines", "# Tool Usage Guidelines\n\nBody text.");
        assert_eq!(
            result.unwrap(),
            "# Tool Usage Guidelines\n\nBody text."
        );
    }

    #[test]
    fn test_section_keeps_non_matching_heading() {
        let result = section("Title", "# Different Heading\n\nBody text.");
        assert_eq!(
            result.unwrap(),
            "# Title\n\n# Different Heading\n\nBody text."
        );
    }

    #[test]
    fn test_boundary_present_with_dynamic_content() {
        let prompt = build_system_prompt(
            PromptMode::Full, "Identity.", &HashMap::new(),
            &[], &[], "main", "gpt-4", "cli",
        );
        assert!(
            prompt.contains(DYNAMIC_BOUNDARY),
            "should contain boundary marker when dynamic content exists"
        );
    }

    #[test]
    fn test_static_prefix_stable_across_calls() {
        let a = build_static_prefix(
            PromptMode::Full, "Identity.", &HashMap::new(), &[], "cli",
        );
        let b = build_static_prefix(
            PromptMode::Full, "Identity.", &HashMap::new(), &[], "cli",
        );
        assert_eq!(a, b, "static prefix should be identical across calls");
    }

    #[test]
    fn test_static_excludes_memory_and_runtime() {
        let memories = vec![MemoryEntry {
            name: "fact".into(),
            tldr: "A fact".into(),
            path: "/memory/fact.md".into(),
        }];
        let static_part = build_static_prefix(
            PromptMode::Full, "Identity.", &HashMap::new(), &[], "cli",
        );
        let dynamic_part = build_dynamic_suffix(
            PromptMode::Full, &memories, "main", "gpt-4", "cli",
        );
        assert!(!static_part.contains("Memory"));
        assert!(!static_part.contains("Runtime Context"));
        assert!(dynamic_part.contains("Memory"));
        assert!(dynamic_part.contains("Runtime Context"));
    }
}
