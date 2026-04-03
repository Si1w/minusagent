---
description: Runtime-tunable parameters loaded from config.json
---

# Tuning

All parameters have compiled defaults. Override via the `"tuning"` key in `config.json`. Access at runtime via `tuning()` (global `OnceLock`).

## Agent

| Parameter | Default | Description |
|-----------|---------|-------------|
| `nag_threshold` | 3 | CoT turns without todo updates before nagging |
| `max_subagent_turns` | 30 | Max CoT turns for subagents |
| `max_teammate_turns` | 50 | Max CoT turns for teammates |
| `compact_threshold` | 0.87 | Context-window usage ratio triggering compaction |
| `compact_max_failures` | 3 | Consecutive L2 failures before circuit breaker |
| `compact_summary_ratio` | 0.10 | L2 summary budget as ratio of context window |
| `full_compact_summary_ratio` | 0.25 | L3 summary budget as ratio of context window |

## Timeouts (seconds)

| Parameter | Default | Description |
|-----------|---------|-------------|
| `bash_timeout_secs` | 120 | Bash command timeout |
| `bg_timeout_secs` | 300 | Background task timeout |
| `idle_timeout_secs` | 60 | Teammate idle timeout |
| `idle_poll_interval_secs` | 5 | Teammate idle poll interval |
| `reconnect_delay_secs` | 5 | Discord reconnection delay |
| `heartbeat_interval_secs` | 1800 | Default heartbeat interval |
| `heartbeat_active_hours` | (9, 22) | Active hours for heartbeat |

## Limits

| Parameter | Default | Description |
|-----------|---------|-------------|
| `notification_max_len` | 500 | Max notification message length |
| `output_max_len` | 50,000 | Max background output length |
| `cli_max_output_bytes` | 100,000 | Max CLI display output |
| `max_skills` | 150 | Max discovered skills |
| `bootstrap_max_file_chars` | 20,000 | Max chars per bootstrap file |
| `bootstrap_max_total_chars` | 150,000 | Max total bootstrap chars |
| `max_tracked_files` | 1,000 | Max read_file_state entries |
| `glob_max_results` | 500 | Max glob results |
| `grep_max_results` | 200 | Max grep matches |
| `web_fetch_max_body` | 50,000 | Max web_fetch response chars |
| `web_timeout_secs` | 30 | HTTP timeout for web tools |

## Resilience

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_overflow_compaction` | 2 | Max overflow compaction attempts |
| `auth_cooldown_secs` | 300 | Auth/billing failure cooldown |
| `rate_limit_cooldown_secs` | 120 | Rate-limit failure cooldown |
| `timeout_cooldown_secs` | 60 | Timeout failure cooldown |

## Delivery

| Parameter | Default | Description |
|-----------|---------|-------------|
| `delivery_max_retries` | 5 | Max delivery retry attempts |
| `delivery_backoff_ms` | [5000, 25000, 120000, 600000] | Exponential backoff schedule |
| `delivery_chunk_limit` | 4096 | Default message chunk size |

## Other

| Parameter | Default | Description |
|-----------|---------|-------------|
| `cron_auto_disable_threshold` | 5 | Consecutive cron errors before disable |
| `default_agent_id` | "mandeven" | Default agent for unbound sessions |
| `log_level` | "info" | Log level (overridden by RUST_LOG) |

## AppConfig

Top-level config loaded from `config.json`:

```json
{
  "llm": [{ "model": "...", "base_url": "...", "api_key": "$ENV_VAR", "context_window": 256000 }],
  "workspace_dir": "./workspace",
  "discord_token": "$DISCORD_TOKEN",
  "tuning": { ... }
}
```

- `llm` — Array of LLM configs. First = primary, rest = resilience fallback profiles.
- Values starting with `$` are resolved from environment variables.
