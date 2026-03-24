use std::collections::HashMap;

use chrono::Utc;

use crate::intelligence::memory::MemoryEntry;
use crate::intelligence::skills::Skill;

/// Wrap content as a `# heading` section, returns `None` if content is empty
fn section(heading: &str, content: &str) -> Option<String> {
    let content = content.trim();
    if content.is_empty() {
        return None;
    }
    Some(format!("# {heading}\n\n{content}"))
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

/// Build the system prompt by assembling 7 layers
///
/// # Layers
///
/// 1. Identity (AGENT.md body)
/// 2. Tool usage guidelines (TOOLS.md)
/// 3. Skills (name + description summary)
/// 4. Memory (TLDR index)
/// 5. Bootstrap context (HEARTBEAT.md, BOOTSTRAP.md, AGENTS.md, USER.md)
/// 6. Runtime context (agent_id, model, time, mode)
/// 7. Channel hints
pub fn build_system_prompt(
    mode: &str,
    identity: &str,
    bootstrap: &HashMap<String, String>,
    skills: &[Skill],
    memories: &[MemoryEntry],
    agent_id: &str,
    model: &str,
    channel: &str,
) -> String {
    let is_full = mode == "full";
    let mut sections: Vec<String> = Vec::new();

    // Layer 1: Identity (AGENT.md body)
    if !identity.trim().is_empty() {
        sections.push(identity.to_string());
    }

    // Layer 2: Tool usage guidelines
    sections.extend(section(
        "Tool Usage Guidelines",
        bootstrap.get("TOOLS.md").map(|s| s.as_str()).unwrap_or(""),
    ));

    // Layer 3: Skills (name + description only; body loaded on activation)
    if is_full {
        let skills_block = format_skills_content(skills);
        sections.extend(section("Available Skills", &skills_block));
    }

    // Layer 4: Memory (TLDR index; full content loaded via read_file)
    if is_full {
        let memory_block = format_memory_content(memories);
        sections.extend(section("Memory", &memory_block));
    }

    // Layer 5: Bootstrap context
    if is_full || mode == "minimal" {
        for name in ["HEARTBEAT.md", "BOOTSTRAP.md", "AGENTS.md", "USER.md"] {
            let title = name.trim_end_matches(".md");
            sections.extend(section(
                title,
                bootstrap.get(name).map(|s| s.as_str()).unwrap_or(""),
            ));
        }
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

    // Layer 7: Channel hints
    sections.extend(section("Channel", channel_hint(channel)));

    sections.join("\n\n")
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
            "full", "You are Luna.", &HashMap::new(),
            &[], &[], "luna", "gpt-4", "cli",
        );
        assert!(prompt.starts_with("You are Luna."));
        assert!(prompt.contains("Agent ID: luna"));
    }

    #[test]
    fn test_empty_identity() {
        let prompt = build_system_prompt(
            "full", "", &HashMap::new(),
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
            "minimal", "Identity.", &HashMap::new(),
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
            "full", "Identity.", &HashMap::new(),
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
            "full", "Identity.", &HashMap::new(),
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
}
