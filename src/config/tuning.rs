use std::fmt;
use std::sync::OnceLock;

use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};

static TUNING: OnceLock<Tuning> = OnceLock::new();

/// Access the global tuning parameters
///
/// Returns the config-loaded values if `AppConfig::load()` has run,
/// otherwise falls back to compiled defaults (useful in tests).
pub fn tuning() -> &'static Tuning {
    TUNING.get_or_init(Tuning::default)
}

pub(crate) fn set_tuning(tuning: Tuning) {
    let _ = TUNING.set(tuning);
}

/// Runtime-tunable parameters
///
/// All fields have sensible defaults. Override via the `"tuning"` key in the app config file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RatioBps(u16);

impl RatioBps {
    #[must_use]
    pub const fn from_bps(bps: u16) -> Self {
        Self(bps)
    }

    #[must_use]
    pub fn apply_to(self, value: usize) -> usize {
        value.saturating_mul(usize::from(self.0)) / 10_000
    }
}

impl Serialize for RatioBps {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f64(f64::from(self.0) / 10_000.0)
    }
}

impl<'de> Deserialize<'de> for RatioBps {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RatioBpsVisitor)
    }
}

struct RatioBpsVisitor;

impl Visitor<'_> for RatioBpsVisitor {
    type Value = RatioBps;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a ratio between 0 and 1, or an integer basis-point value")
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        let bps = u16::try_from(value)
            .map_err(|_| E::custom("basis-point value must be between 0 and 10000"))?;
        if bps > 10_000 {
            return Err(E::custom("basis-point value must be between 0 and 10000"));
        }
        Ok(RatioBps::from_bps(bps))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value < 0 {
            return Err(E::custom("ratio must be non-negative"));
        }
        self.visit_u64(u64::try_from(value).map_err(|_| E::custom("ratio must be non-negative"))?)
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_ratio_bps(&value.to_string())
            .map(RatioBps::from_bps)
            .map_err(E::custom)
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        parse_ratio_bps(value)
            .map(RatioBps::from_bps)
            .map_err(E::custom)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_str(&value)
    }
}

fn parse_ratio_bps(input: &str) -> std::result::Result<u16, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("ratio cannot be empty".into());
    }
    if trimmed.starts_with('-') {
        return Err("ratio must be non-negative".into());
    }

    if let Some((whole, fractional)) = trimmed.split_once('.') {
        let whole: u16 = whole
            .parse()
            .map_err(|_| format!("invalid ratio value: {trimmed}"))?;
        if whole > 1 {
            return Err("ratio must be between 0 and 1".into());
        }
        if !fractional.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(format!("invalid ratio value: {trimmed}"));
        }

        let significant = fractional.chars().skip(4).any(|ch| ch != '0');
        if significant {
            return Err("ratio supports up to 4 decimal places".into());
        }

        let mut padded = fractional.chars().take(4).collect::<String>();
        while padded.len() < 4 {
            padded.push('0');
        }
        let fractional_bps: u16 = padded
            .parse()
            .map_err(|_| format!("invalid ratio value: {trimmed}"))?;

        if whole == 1 {
            if fractional_bps == 0 {
                return Ok(10_000);
            }
            return Err("ratio must be between 0 and 1".into());
        }

        return Ok(fractional_bps);
    }

    let bps: u16 = trimmed
        .parse()
        .map_err(|_| format!("invalid ratio value: {trimmed}"))?;
    if bps > 10_000 {
        return Err("basis-point value must be between 0 and 10000".into());
    }
    Ok(bps)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    pub agent: AgentTuning,
    pub compaction: CompactionTuning,
    pub timeouts: TimeoutTuning,
    pub limits: LimitTuning,
    pub resilience: ResilienceTuning,
    pub frontend: FrontendTuning,
    pub scheduler: SchedulerTuning,
    pub delivery: DeliveryTuning,
    pub routing: RoutingTuning,
    pub logging: LoggingTuning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentTuning {
    /// `CoT` turns without todo updates before nagging LLM
    pub nag_threshold: usize,
    /// Max `CoT` turns for subagents
    pub max_subagent_turns: usize,
    /// Max `CoT` turns for teammates
    pub max_teammate_turns: usize,
}

impl Default for AgentTuning {
    fn default() -> Self {
        Self {
            nag_threshold: 3,
            max_subagent_turns: 30,
            max_teammate_turns: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionTuning {
    /// Context-window usage threshold triggering auto-compact (L2).
    /// Accepts `0.87` or `8700` basis points in the app config.
    pub compact_threshold: RatioBps,
    /// Max consecutive auto-compact failures before circuit breaker trips
    pub compact_max_failures: usize,
    /// Auto-compact summary budget as ratio of context window (L2).
    /// Accepts `0.10` or `1000` basis points in the app config.
    pub compact_summary_ratio: RatioBps,
    /// Full-compact summary budget as ratio of context window (L3).
    /// Accepts `0.25` or `2500` basis points in the app config.
    pub full_compact_summary_ratio: RatioBps,
}

impl Default for CompactionTuning {
    fn default() -> Self {
        Self {
            compact_threshold: RatioBps::from_bps(8_700),
            compact_max_failures: 3,
            compact_summary_ratio: RatioBps::from_bps(1_000),
            full_compact_summary_ratio: RatioBps::from_bps(2_500),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimeoutTuning {
    /// Bash command execution timeout
    pub bash_timeout_secs: u64,
    /// Background task execution timeout
    pub bg_timeout_secs: u64,
    /// Regex/ripgrep search timeout
    pub search_timeout_secs: u64,
    /// Teammate idle timeout before shutdown
    pub idle_timeout_secs: u64,
    /// Teammate idle polling interval
    pub idle_poll_interval_secs: u64,
    /// Discord reconnection delay
    pub reconnect_delay_secs: u64,
    /// HTTP timeout for `web_fetch` / `web_search` (seconds)
    pub web_timeout_secs: u64,
}

impl Default for TimeoutTuning {
    fn default() -> Self {
        Self {
            bash_timeout_secs: 120,
            bg_timeout_secs: 300,
            search_timeout_secs: 30,
            idle_timeout_secs: 60,
            idle_poll_interval_secs: 5,
            reconnect_delay_secs: 5,
            web_timeout_secs: 30,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitTuning {
    /// Max notification message length before truncation
    pub notification_max_len: usize,
    /// Max background task output length before truncation
    pub output_max_len: usize,
    /// Max skills to discover
    pub max_skills: usize,
    /// Max chars per bootstrap file
    pub bootstrap_max_file_chars: usize,
    /// Max total chars for bootstrap context
    pub bootstrap_max_total_chars: usize,
    /// Max tracked files in `read_file_state` before clearing
    pub max_tracked_files: usize,
    /// Max results returned by glob tool
    pub glob_max_results: usize,
    /// Max matches returned by grep tool
    pub grep_max_results: usize,
    /// Max response body length (chars) for `web_fetch` before truncation
    pub web_fetch_max_body: usize,
}

impl Default for LimitTuning {
    fn default() -> Self {
        Self {
            notification_max_len: 500,
            output_max_len: 50_000,
            max_skills: 150,
            bootstrap_max_file_chars: 20_000,
            bootstrap_max_total_chars: 150_000,
            max_tracked_files: 1000,
            glob_max_results: 500,
            grep_max_results: 200,
            web_fetch_max_body: 50_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ResilienceTuning {
    /// Max overflow compaction attempts
    pub max_overflow_compaction: usize,
    /// Max chars kept per tool message during emergency compaction
    pub emergency_tool_truncate_chars: usize,
    /// Cooldown for auth/billing failures (seconds)
    pub auth_cooldown_secs: u64,
    /// Cooldown for rate-limit failures (seconds)
    pub rate_limit_cooldown_secs: u64,
    /// Cooldown for timeout failures (seconds)
    pub timeout_cooldown_secs: u64,
}

impl Default for ResilienceTuning {
    fn default() -> Self {
        Self {
            max_overflow_compaction: 2,
            emergency_tool_truncate_chars: 2_000,
            auth_cooldown_secs: 300,
            rate_limit_cooldown_secs: 120,
            timeout_cooldown_secs: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrontendTuning {
    /// Max CLI display output bytes
    pub cli_max_output_bytes: usize,
    /// UI/event loop refresh interval for the TUI (milliseconds)
    pub cli_refresh_interval_ms: u64,
}

impl Default for FrontendTuning {
    fn default() -> Self {
        Self {
            cli_max_output_bytes: 100_000,
            cli_refresh_interval_ms: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerTuning {
    /// Default heartbeat interval (overridden by HEARTBEAT.md frontmatter)
    pub heartbeat_interval_secs: u64,
    /// Default heartbeat active hours (overridden by HEARTBEAT.md frontmatter)
    pub heartbeat_active_hours: (u8, u8),
    /// Heartbeat scheduler polling interval (milliseconds)
    pub heartbeat_poll_interval_ms: u64,
    /// Consecutive errors before auto-disabling a cron job
    pub cron_auto_disable_threshold: u32,
    /// Cron scheduler polling interval (milliseconds)
    pub cron_poll_interval_ms: u64,
}

impl Default for SchedulerTuning {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 1800,
            heartbeat_active_hours: (9, 22),
            heartbeat_poll_interval_ms: 1_000,
            cron_auto_disable_threshold: 5,
            cron_poll_interval_ms: 1_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeliveryTuning {
    /// Max delivery retry attempts
    pub delivery_max_retries: u32,
    /// Exponential backoff schedule (milliseconds)
    pub delivery_backoff_ms: Vec<u64>,
    /// Default message chunk size
    pub delivery_chunk_limit: usize,
    /// Delivery queue polling interval (milliseconds)
    pub delivery_poll_interval_ms: u64,
}

impl Default for DeliveryTuning {
    fn default() -> Self {
        Self {
            delivery_max_retries: 5,
            delivery_backoff_ms: vec![5_000, 25_000, 120_000, 600_000],
            delivery_chunk_limit: 4096,
            delivery_poll_interval_ms: 1_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingTuning {
    /// Default agent ID for unbound sessions
    pub default_agent_id: String,
}

impl Default for RoutingTuning {
    fn default() -> Self {
        Self {
            default_agent_id: "mandeven".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoggingTuning {
    /// Log level: error, warn, info, debug, trace (overridden by `RUST_LOG` env var)
    pub log_level: String,
}

impl Default for LoggingTuning {
    fn default() -> Self {
        Self {
            log_level: "info".into(),
        }
    }
}

