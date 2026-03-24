---
description: Intelligence module — dynamic prompt assembly, skills, memory
---

# Intelligence

Dynamic 8-layer system prompt assembly. Located in `src/intelligence/`.

## Modules

- **mod.rs** — `Intelligence` struct: orchestrates all layers, rebuilt each turn
- **manager.rs** — `AgentConfig`, `AgentManager`, workspace discovery
- **bootstrap.rs** — `BootstrapLoader`: loads workspace `.md` files with truncation
- **skills.rs** — `SkillsManager`: discovers `SKILL.md` files from workspace
- **memory.rs** — `MemoryStore` (TLDR index) + `MemoryWrite` Node (LLM-generated TLDR)
- **prompt.rs** — `build_system_prompt()`: 8-layer assembly + formatting helpers
- **utils.rs** — Shared `parse_frontmatter()` and `extract_body()`

## 8-Layer Prompt Assembly

1. **Identity** — `AGENT.md` body, fallback to `prompts/system.md`
2. **Personality** — workspace `SOUL.md`
3. **Tool guidelines** — workspace `TOOLS.md`
4. **Skills** — `prompts/skills.md` template + discovered skill list
5. **Memory** — `prompts/memory.md` template + TLDR index (name, tldr, path)
6. **Bootstrap** — remaining workspace files (HEARTBEAT, BOOTSTRAP, AGENTS, USER)
7. **Runtime** — agent_id, model, channel, current time, prompt mode
8. **Channel** — format hints (cli: markdown, discord: 2000 char limit, etc.)

## Progressive Loading

- Skills: frontmatter only at startup, body available for invocation
- Memory: frontmatter TLDR only at startup, LLM uses `read_file` for full content
- Bootstrap: truncated to `MAX_FILE_CHARS` per file, `MAX_TOTAL_CHARS` total

## Workspace Structure

```
workspace/<agent>/
├── AGENT.md        config (frontmatter) + identity (body)
├── SOUL.md         personality
├── TOOLS.md        tool usage guidelines
├── MEMORY.md       long-term memory index
├── memory/         progressive memory files (*.md with TLDR frontmatter)
├── HEARTBEAT.md    project status
├── BOOTSTRAP.md    startup context
├── AGENTS.md       multi-agent descriptions
└── USER.md         user information
```