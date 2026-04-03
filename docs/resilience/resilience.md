---
description: Three-layer resilience runner with auth rotation and overflow recovery
---

# Resilience

Three-layer resilient wrapper around `Agent::run()`. Located in `src/resilience/`.

## ResilienceRunner

Wraps agent execution with:

- **Layer 1: Auth Rotation** — Cycles through `ProfileManager` profiles on auth/rate-limit failures
- **Layer 2: Overflow Recovery** — Compacts history on context overflow errors
- **Layer 3: Agent::run()** — The actual tool-use loop

Falls back to `fallback_models` if all profiles are exhausted.

## FailoverReason

Classification of LLM API errors by pattern matching:

| Reason | Trigger | Cooldown |
|--------|---------|----------|
| `RateLimit` | 429, "rate", "too many" | `rate_limit_cooldown_secs` (120s) |
| `Auth` | 401, "auth", "invalid key" | `auth_cooldown_secs` (300s) |
| `Timeout` | "timeout", "timed out" | `timeout_cooldown_secs` (60s) |
| `Billing` | 402, "billing", "quota" | `auth_cooldown_secs` (300s) |
| `Overflow` | Context exceeded | 0 (compact, don't rotate) |
| `Unknown` | Unrecognized | 0 |

## ProfileManager

Manages multiple `AuthProfile` instances with cooldown-aware selection:

- `select()` — First non-cooldown profile
- `mark_failure(idx, reason, cooldown)` — Put profile in cooldown
- `mark_success(idx)` — Clear cooldown
- `status_lines()` — Human-readable status for display

## AuthProfile

Single API key with tracking:
- `api_key`, `base_url` (optional)
- `cooldown_until` — When the profile becomes available again
- `failure_reason` — Last failure category
- `last_good_at` — Last successful use timestamp
