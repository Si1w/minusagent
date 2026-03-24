use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;

use crate::intelligence::memory::MemoryEntry;
use crate::intelligence::skills::Skill;

const MAX_SKILLS_PROMPT: usize = 30_000;

/// Loaded prompt fragments from `prompts/` directory
pub struct PromptFragments {
    /// Default system prompt (fallback when workspace has no IDENTITY.md)
    pub system: String,
    /// Skills section template (# Available Skills heading + ## Instructions)
    pub skills: String,
    /// Memory section template (# Memory heading + ## Instructions)
    pub memory: String,
}

impl PromptFragments {
    /// Load prompt fragments from the given directory
    ///
    /// Falls back to empty strings for missing files.
    pub fn load(prompts_dir: &Path) -> Self {
        Self {
            system: load_trimmed(&prompts_dir.join("system.md")),
            skills: load_trimmed(&prompts_dir.join("skills.md")),
            memory: load_trimmed(&prompts_dir.join("memory.md")),
        }
    }
}

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
/// Each entry shows id, TLDR, and file path so the LLM can
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

/// Format discovered skills into prompt content (without heading)
pub fn format_skills_content(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }

    let mut parts = Vec::new();
    let mut total = 0;

    for skill in skills {
        let mut block = format!(
            "## {}\n- Description: {}\n- Invocation: {}\n",
            skill.name, skill.description, skill.invocation,
        );
        if !skill.body.is_empty() {
            block.push_str(&format!("\n{}\n", skill.body));
        }

        if total + block.len() > MAX_SKILLS_PROMPT {
            parts.push("(... more skills truncated)".to_string());
            break;
        }
        total += block.len();
        parts.push(block);
    }

    parts.join("\n")
}

/// Build the system prompt by assembling 8 layers
///
/// # Layers
///
/// 1. Identity (AGENT.md body, or prompts/system.md fallback)
/// 2. Soul (SOUL.md personality — placed early for stronger influence)
/// 3. Tool usage guidelines (TOOLS.md)
/// 4. Skills
/// 5. Memory (prompts/memory.md template + TLDR index)
/// 6. Bootstrap context (HEARTBEAT.md, BOOTSTRAP.md, AGENTS.md, USER.md)
/// 7. Runtime context (agent_id, model, time, mode)
/// 8. Channel hints
pub fn build_system_prompt(
    mode: &str,
    fragments: &PromptFragments,
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

    // Layer 1: Identity (AGENT.md body > prompts/system.md fallback)
    let identity = if identity.trim().is_empty() {
        fragments.system.as_str()
    } else {
        identity
    };
    sections.push(identity.to_string());

    // Layer 2: Soul
    if is_full {
        sections.extend(section(
            "Personality",
            bootstrap.get("SOUL.md").map(|s| s.as_str()).unwrap_or(""),
        ));
    }

    // Layer 3: Tool usage guidelines
    sections.extend(section(
        "Tool Usage Guidelines",
        bootstrap.get("TOOLS.md").map(|s| s.as_str()).unwrap_or(""),
    ));

    // Layer 4: Skills — template + discovered skills
    if is_full {
        let mut skills_section = fragments.skills.clone();
        let skills_block = format_skills_content(skills);
        if !skills_block.is_empty() {
            skills_section
                .push_str(&format!("\n\n## Discovered Skills\n\n{skills_block}"));
        }
        if !skills_section.is_empty() {
            sections.push(skills_section);
        }
    }

    // Layer 5: Memory — template + TLDR index
    if is_full {
        let mut memory_section = fragments.memory.clone();
        let tldr_block = format_memory_content(memories);
        if !tldr_block.is_empty() {
            memory_section
                .push_str(&format!("\n\n## Known Memories\n\n{tldr_block}"));
        }
        if !memory_section.is_empty() {
            sections.push(memory_section);
        }
    }

    // Layer 6: Bootstrap context (remaining files)
    if is_full || mode == "minimal" {
        for name in ["HEARTBEAT.md", "BOOTSTRAP.md", "AGENTS.md", "USER.md"] {
            let title = name.trim_end_matches(".md");
            sections.extend(section(
                title,
                bootstrap.get(name).map(|s| s.as_str()).unwrap_or(""),
            ));
        }
    }

    // Layer 7: Runtime context
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

    // Layer 8: Channel hints
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

fn load_trimmed(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_fragments() -> PromptFragments {
        PromptFragments {
            system: "You are a helpful assistant.".into(),
            skills: "# Available Skills\n\n## Instructions\n\nUse skills.".into(),
            memory: "# Memory\n\n## Instructions\n\nUse memory tools.".into(),
        }
    }

    #[test]
    fn test_default_identity() {
        let f = test_fragments();
        let prompt = build_system_prompt(
            "full", &f, "", &HashMap::new(), &[], &[], "main", "gpt-4", "cli",
        );
        assert!(prompt.starts_with("You are a helpful assistant."));
        assert!(prompt.contains("# Memory"));
        assert!(prompt.contains("Agent ID: main"));
    }

    #[test]
    fn test_agent_identity_overrides_fragment() {
        let f = test_fragments();
        let prompt = build_system_prompt(
            "full", &f, "You are Luna.", &HashMap::new(),
            &[], &[], "luna", "gpt-4", "cli",
        );
        assert!(prompt.starts_with("You are Luna."));
    }

    #[test]
    fn test_minimal_skips_soul_and_memory() {
        let f = test_fragments();
        let mut bs = HashMap::new();
        bs.insert("SOUL.md".into(), "Be kind.".into());
        let prompt = build_system_prompt(
            "minimal", &f, "", &bs, &[], &[], "main", "gpt-4", "cli",
        );
        assert!(!prompt.contains("Personality"));
        assert!(!prompt.contains("# Memory"));
    }

    #[test]
    fn test_memory_tldr_index() {
        let f = test_fragments();
        let memories = vec![MemoryEntry {
            name: "dark_mode".into(),
            tldr: "User prefers dark mode".into(),
            path: "/workspace/memory/dark_mode.md".into(),
        }];
        let prompt = build_system_prompt(
            "full", &f, "", &HashMap::new(), &[], &memories,
            "main", "gpt-4", "cli",
        );
        assert!(prompt.contains("## Instructions"));
        assert!(prompt.contains("## Known Memories"));
        assert!(prompt.contains("`dark_mode`"));
        assert!(prompt.contains("User prefers dark mode"));
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
    fn test_format_skills_content() {
        let skills = vec![Skill {
            name: "greet".into(),
            description: "Say hello".into(),
            invocation: "/greet".into(),
            body: String::new(),
        }];
        let content = format_skills_content(&skills);
        assert!(content.contains("## greet"));
        assert!(content.contains("/greet"));
    }
}