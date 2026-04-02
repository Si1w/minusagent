use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::resilience::profile::AuthProfile;

const CONFIG_PATH: &str = "config.json";

static TUNING: OnceLock<Tuning> = OnceLock::new();

/// Access the global tuning parameters
///
/// Returns the config-loaded values if `AppConfig::load()` has run,
/// otherwise falls back to compiled defaults (useful in tests).
pub fn tuning() -> &'static Tuning {
    TUNING.get_or_init(Tuning::default)
}

/// Runtime-tunable parameters
///
/// All fields have sensible defaults. Override via the `"tuning"` key in `config.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    // ── Agent ──
    /// CoT turns without todo updates before nagging LLM
    pub nag_threshold: usize,
    /// Max CoT turns for subagents
    pub max_subagent_turns: usize,
    /// Max CoT turns for teammates
    pub max_teammate_turns: usize,
    /// Context-window usage ratio triggering compaction
    pub compact_threshold: f64,

    // ── Timeouts (seconds) ──
    /// Bash command execution timeout
    pub bash_timeout_secs: u64,
    /// Background task execution timeout
    pub bg_timeout_secs: u64,
    /// Teammate idle timeout before shutdown
    pub idle_timeout_secs: u64,
    /// Teammate idle polling interval
    pub idle_poll_interval_secs: u64,
    /// Discord reconnection delay
    pub reconnect_delay_secs: u64,
    /// Default heartbeat interval (overridden by HEARTBEAT.md frontmatter)
    pub heartbeat_interval_secs: u64,
    /// Default heartbeat active hours (overridden by HEARTBEAT.md frontmatter)
    pub heartbeat_active_hours: (u8, u8),

    // ── Limits ──
    /// Max notification message length before truncation
    pub notification_max_len: usize,
    /// Max background task output length before truncation
    pub output_max_len: usize,
    /// Max CLI display output bytes
    pub cli_max_output_bytes: usize,
    /// Max skills to discover
    pub max_skills: usize,
    /// Max chars per bootstrap file
    pub bootstrap_max_file_chars: usize,
    /// Max total chars for bootstrap context
    pub bootstrap_max_total_chars: usize,
    /// Max results returned by glob tool
    pub glob_max_results: usize,
    /// Max matches returned by grep tool
    pub grep_max_results: usize,
    /// Max response body length (chars) for web_fetch before truncation
    pub web_fetch_max_body: usize,
    /// HTTP timeout for web_fetch / web_search (seconds)
    pub web_timeout_secs: u64,

    // ── Resilience ──
    /// Max overflow compaction attempts
    pub max_overflow_compaction: usize,
    /// Cooldown for auth/billing failures (seconds)
    pub auth_cooldown_secs: u64,
    /// Cooldown for rate-limit failures (seconds)
    pub rate_limit_cooldown_secs: u64,
    /// Cooldown for timeout failures (seconds)
    pub timeout_cooldown_secs: u64,

    // ── Delivery ──
    /// Max delivery retry attempts
    pub delivery_max_retries: u32,
    /// Exponential backoff schedule (milliseconds)
    pub delivery_backoff_ms: Vec<u64>,
    /// Default message chunk size
    pub delivery_chunk_limit: usize,

    // ── Cron ──
    /// Consecutive errors before auto-disabling a cron job
    pub cron_auto_disable_threshold: u32,

    // ── Routing ──
    /// Default agent ID for unbound sessions
    pub default_agent_id: String,

    // ── Logging ──
    /// Log level: error, warn, info, debug, trace (overridden by `RUST_LOG` env var)
    pub log_level: String,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            nag_threshold: 3,
            max_subagent_turns: 30,
            max_teammate_turns: 50,
            compact_threshold: 0.8,

            bash_timeout_secs: 120,
            bg_timeout_secs: 300,
            idle_timeout_secs: 60,
            idle_poll_interval_secs: 5,
            reconnect_delay_secs: 5,
            heartbeat_interval_secs: 1800,
            heartbeat_active_hours: (9, 22),

            notification_max_len: 500,
            output_max_len: 50_000,
            cli_max_output_bytes: 100_000,
            max_skills: 150,
            bootstrap_max_file_chars: 20_000,
            bootstrap_max_total_chars: 150_000,
            glob_max_results: 500,
            grep_max_results: 200,
            web_fetch_max_body: 50_000,
            web_timeout_secs: 30,

            max_overflow_compaction: 2,
            auth_cooldown_secs: 300,
            rate_limit_cooldown_secs: 120,
            timeout_cooldown_secs: 60,

            delivery_max_retries: 5,
            delivery_backoff_ms: vec![5_000, 25_000, 120_000, 600_000],
            delivery_chunk_limit: 4096,

            cron_auto_disable_threshold: 5,

            default_agent_id: "mandeven".into(),

            log_level: "info".into(),
        }
    }
}

/// Build a config template from struct defaults
fn config_template() -> String {
    let template = json!({
        "llm": [LLMConfig::default()],
        "workspace_dir": "./workspace",
        "tuning": Tuning::default(),
    });
    serde_json::to_string_pretty(&template).unwrap()
}

/// Resolve a string value: if it starts with `$`, treat as env var reference.
fn resolve(value: &mut String) {
    if let Some(var) = value.strip_prefix('$') {
        *value = std::env::var(var)
            .unwrap_or_else(|_| panic!("env var ${var} not set (referenced in {CONFIG_PATH})"));
    }
}

/// LLM provider configuration
///
/// First entry in the `llm` array is the primary profile.
/// Additional entries provide auth rotation (api_key + base_url).
#[derive(Clone, Serialize, Deserialize)]
pub struct LLMConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub context_window: usize,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            model: "labs-leanstral-2603".into(),
            base_url: "https://api.mistral.ai/v1/".into(),
            api_key: "$MISTRAL_API_KEY".into(),
            context_window: 256_000,
        }
    }
}

/// Unified application configuration
///
/// Loaded from `config.json` at startup.
/// String values starting with `$` are resolved as environment variables.
#[derive(Deserialize)]
pub struct AppConfig {
    /// LLM profiles. First = primary, rest = auth rotation.
    pub llm: Vec<LLMConfig>,
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub workspace_dir: Option<PathBuf>,
    #[serde(default)]
    pub discord_token: Option<String>,
    /// Runtime-tunable parameters (all have sensible defaults)
    #[serde(default)]
    pub tuning: Tuning,
}

impl AppConfig {
    /// Load configuration from `config.json`
    ///
    /// String values starting with `$` are resolved as environment variables,
    /// so secrets can live in the shell profile instead of the config file.
    ///
    /// # Panics
    ///
    /// Panics if the file is missing, malformed, `llm` array is empty,
    /// or a referenced env var is not set.
    pub fn load() -> Self {
        let path = std::path::Path::new(CONFIG_PATH);
        let raw = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::write(path, config_template())
                    .unwrap_or_else(|e| panic!("Failed to create {CONFIG_PATH}: {e}"));
                panic!("Created {CONFIG_PATH} — please fill in your configuration and restart");
            }
            Err(e) => panic!("Failed to read {CONFIG_PATH}: {e}"),
        };
        let mut config: Self = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("Failed to parse {CONFIG_PATH}: {e}"));

        assert!(!config.llm.is_empty(), "llm array must have at least one entry");

        // Resolve $ENV_VAR references
        for llm in &mut config.llm {
            resolve(&mut llm.api_key);
            resolve(&mut llm.base_url);
        }
        if let Some(ref mut token) = config.discord_token {
            resolve(token);
        }

        config.workspace_dir = match config.workspace_dir {
            Some(ws) if ws.is_dir() => Some(ws),
            Some(ws) => {
                log::warn!("workspace_dir {ws:?} is not a directory, ignoring");
                None
            }
            None => {
                let default = PathBuf::from("./workspace");
                default.is_dir().then_some(default)
            }
        };

        // Publish tuning as a global singleton
        let _ = TUNING.set(config.tuning.clone());

        config
    }

    /// Primary LLM config (first entry)
    pub fn primary_llm(&self) -> &LLMConfig {
        &self.llm[0]
    }

    /// Extra profiles for auth rotation (entries after the first)
    pub fn extra_profiles(&self) -> &[LLMConfig] {
        &self.llm[1..]
    }
}

impl LLMConfig {
    /// Convert to an AuthProfile for the resilience layer
    pub fn to_auth_profile(&self) -> AuthProfile {
        AuthProfile::new(self.api_key.clone(), Some(self.base_url.clone()))
    }
}

// ── LLM profile management (persists to config.json) ──────

/// Read config.json as raw JSON value
fn read_raw() -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(CONFIG_PATH)?;
    Ok(serde_json::from_str(&raw)?)
}

/// Write raw JSON value back to config.json
fn write_raw(value: &serde_json::Value) -> Result<()> {
    let pretty = serde_json::to_string_pretty(value)?;
    std::fs::write(CONFIG_PATH, pretty)?;
    Ok(())
}

/// Read-modify-write the `llm` array in config.json
fn update_llm_array(
    f: impl FnOnce(&mut Vec<serde_json::Value>) -> Result<()>,
) -> Result<()> {
    let mut root = read_raw()?;
    let arr = root
        .get_mut("llm")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("missing or invalid `llm` array in {CONFIG_PATH}"))?;
    f(arr)?;
    write_raw(&root)
}

/// Find a model's index in the llm array by name
fn find_model(arr: &[serde_json::Value], model: &str) -> Result<usize> {
    arr.iter()
        .position(|v| v.get("model").and_then(|m| m.as_str()) == Some(model))
        .ok_or_else(|| anyhow!("model {model:?} not found"))
}

/// Add an LLM profile to config.json
///
/// # Errors
///
/// Returns error if config.json is unreadable or the `llm` array is missing.
pub fn add_llm(config: &LLMConfig) -> Result<()> {
    update_llm_array(|arr| {
        arr.push(serde_json::to_value(config)?);
        Ok(())
    })
}

/// Remove an LLM profile by model name
///
/// # Errors
///
/// Returns error if the model is not found or it is the only entry.
pub fn remove_llm(model: &str) -> Result<()> {
    update_llm_array(|arr| {
        let idx = find_model(arr, model)?;
        ensure!(arr.len() > 1, "cannot remove the only LLM profile");
        arr.remove(idx);
        Ok(())
    })
}

/// Set an LLM profile as primary (move to index 0) by model name
///
/// # Errors
///
/// Returns error if the model is not found.
pub fn set_primary_llm(model: &str) -> Result<()> {
    update_llm_array(|arr| {
        let idx = find_model(arr, model)?;
        if idx != 0 {
            let entry = arr.remove(idx);
            arr.insert(0, entry);
        }
        Ok(())
    })
}

/// List all LLM profiles, primary first
pub fn list_llm_profiles() -> Result<Vec<LLMConfig>> {
    let root = read_raw()?;
    let arr = root
        .get("llm")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("missing or invalid `llm` array in {CONFIG_PATH}"))?;
    Ok(arr
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect())
}
