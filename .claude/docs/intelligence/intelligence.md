---
description: Intelligence module — dynamic prompt assembly, skills, memory
---

# Intelligence

Dynamic 7-layer system prompt assembly. Located in `src/intelligence/`.

## Modules

- **mod.rs** — `Intelligence` struct: orchestrates all layers, rebuilt each turn
- **manager.rs** — `AgentConfig`, `AgentManager`, workspace discovery
- **bootstrap.rs** — `BootstrapLoader`: loads workspace `.md` files with truncation
- **skills.rs** — `SkillsManager`: discovers `SKILL.md` files from workspace
- **memory.rs** — `MemoryStore` (TLDR index) + `MemoryWrite` Node (LLM-generated TLDR)
- **prompt.rs** — `build_system_prompt()`: 7-layer assembly + formatting helpers
- **utils.rs** — Shared `parse_frontmatter()` and `extract_body()`

## 7-Layer Prompt Assembly

1. **Identity** — `AGENT.md` body (plain markdown, no frontmatter)
2. **Tool guidelines** — workspace `TOOLS.md`
3. **Skills** — name + description summary (full mode only)
4. **Memory** — TLDR index with file paths (full mode only)
5. **Bootstrap** — workspace files (HEARTBEAT.md, BOOTSTRAP.md, AGENTS.md, USER.md)
6. **Runtime** — agent_id, model, channel, current time, prompt mode
7. **Channel** — format hints (cli: markdown, discord: 2000 char limit, etc.)

## Progressive Loading

- Skills: frontmatter only at startup, body loaded on `/<skill>` invocation
- Memory: frontmatter TLDR only at startup, LLM uses `read_file` for full content
- Bootstrap: truncated to `MAX_FILE_CHARS` (20k) per file, `MAX_TOTAL_CHARS` (150k) total

## Workspace Structure

```
WORKSPACE_DIR/
└── .agents/
    └── <agent_id>/
        ├── AGENT.md        identity (body text, no frontmatter)
        ├── TOOLS.md        tool usage guidelines (optional)
        ├── memory/         progressive memory files (*.md with TLDR frontmatter)
        ├── skills/         discovered SKILL.md directories
        ├── HEARTBEAT.md    project status (optional)
        ├── BOOTSTRAP.md    startup context (optional)
        ├── AGENTS.md       multi-agent descriptions (optional)
        └── USER.md         user information (optional)
```
