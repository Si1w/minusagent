//! Application configuration.
//!
//! `AppConfig` is the user-facing root of the configuration tree, loaded
//! from `config.toml`. It composes:
//!
//! - `LlmSettings` — LLM provider profiles (`[llm]` section).
//! - `FrontendConfig` — Frontend mode and managed-service startup policy.
//! - `Tuning` — Tunable runtime parameters (intervals, timeouts, limits)
//!   accessible globally via [`tuning`].
//! - `WorkspaceConfig` — Optional workspace directory pointing at the
//!   per-agent file tree (`.agents/`, `routes.json`, `HEARTBEAT.md`, …).

use std::path::Path;
use std::path::PathBuf;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

mod frontend;
mod llm;
mod tuning;

use self::frontend::{FrontendConfig, ServicesConfig};
pub use self::frontend::{FrontendMode, ServiceStartup, StartupPolicy};
pub use self::llm::{
    LLMConfig, LlmSettings, add_llm, list_llm_profiles, remove_llm, set_primary_llm,
};
use self::tuning::set_tuning;
pub use self::tuning::{
    AgentTuning, CompactionTuning, DeliveryTuning, FrontendTuning, LimitTuning, LoggingTuning,
    RatioBps, ResilienceTuning, RoutingTuning, SchedulerTuning, TimeoutTuning, Tuning, tuning,
};

const CONFIG_PATH: &str = "config.toml";

fn config_path() -> PathBuf {
    PathBuf::from(CONFIG_PATH)
}

fn read_config<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let raw = std::fs::read_to_string(path)?;
    Ok(toml_edit::de::from_str(&raw)?)
}

fn render_config<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    Ok(format!(
        "# MinusAgent config\n# User-facing runtime settings live here.\n\n{}",
        toml_edit::ser::to_string_pretty(value)?
    ))
}

fn write_config<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let rendered = render_config(value)?;
    std::fs::write(path, rendered)?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub dir: Option<PathBuf>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            dir: Some(PathBuf::from("./workspace")),
        }
    }
}

/// Build a config template from struct defaults
fn config_template() -> Result<String> {
    render_config(&AppConfig::default())
}

/// Resolve a string value: if it starts with `$`, treat as env var reference.
fn resolve(value: &mut String, config_path: &Path) {
    if let Some(var) = value.strip_prefix('$') {
        *value = std::env::var(var).unwrap_or_else(|_| {
            panic!(
                "env var ${var} not set (referenced in {})",
                config_path.display()
            )
        });
    }
}

/// Unified application configuration
///
/// Loaded from `config.toml` at startup.
/// String values starting with `$` are resolved as environment variables.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub llm: LlmSettings,
    pub workspace: WorkspaceConfig,
    frontend: FrontendConfig,
    services: ServicesConfig,
    /// Runtime-tunable parameters (all have sensible defaults)
    pub tuning: Tuning,
}

impl AppConfig {
    /// Load configuration from `config.toml`
    ///
    /// String values starting with `$` are resolved as environment variables,
    /// so secrets can live in the shell profile instead of the config file.
    ///
    /// # Panics
    ///
    /// Panics if the file is missing, malformed, `llm` array is empty,
    /// or a referenced env var is not set.
    #[must_use]
    pub fn load() -> Self {
        let config_path = config_path();
        let raw = match std::fs::read_to_string(&config_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let template = config_template()
                    .unwrap_or_else(|e| panic!("Failed to render {}: {e}", config_path.display()));
                std::fs::write(&config_path, template).unwrap_or_else(|err| {
                    panic!("Failed to create {}: {err}", config_path.display())
                });
                panic!(
                    "Created {} — please fill in your configuration and restart",
                    config_path.display()
                );
            }
            Err(e) => panic!("Failed to read {}: {e}", config_path.display()),
        };
        let mut config: Self = toml_edit::de::from_str(&raw)
            .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", config_path.display()));

        assert!(
            !config.llm.profiles.is_empty(),
            "llm array must have at least one entry"
        );

        // Resolve $ENV_VAR references
        for llm in &mut config.llm.profiles {
            resolve(&mut llm.api_key, &config_path);
            resolve(&mut llm.base_url, &config_path);
        }
        resolve(&mut config.frontend.websocket.host, &config_path);
        if let Some(ref mut token) = config.frontend.discord.token {
            resolve(token, &config_path);
        }

        config.workspace.dir = match config.workspace.dir {
            Some(ws) if ws.is_dir() => Some(ws),
            Some(ws) => {
                log::warn!(
                    "workspace_dir {} is not a directory, ignoring",
                    ws.display()
                );
                None
            }
            None => {
                let default = PathBuf::from("./workspace");
                default.is_dir().then_some(default)
            }
        };

        // Publish tuning as a global singleton
        set_tuning(config.tuning.clone());

        config
    }

    /// Primary LLM config (first entry)
    #[must_use]
    pub fn primary_llm(&self) -> &LLMConfig {
        &self.llm.profiles[0]
    }

    /// Extra profiles for auth rotation (entries after the first)
    #[must_use]
    pub fn extra_profiles(&self) -> &[LLMConfig] {
        &self.llm.profiles[1..]
    }

    /// Fallback model list for resilience failover.
    #[must_use]
    pub fn fallback_models(&self) -> &[String] {
        &self.llm.fallback_models
    }

    /// Workspace root directory, if configured and present.
    #[must_use]
    pub fn workspace_dir(&self) -> Option<&Path> {
        self.workspace.dir.as_deref()
    }

    /// Discord bot token, if configured.
    #[must_use]
    pub fn discord_token(&self) -> Option<&str> {
        self.frontend.discord.token.as_deref()
    }

    /// Default frontend entry mode when no CLI override is provided.
    #[must_use]
    pub const fn frontend_mode(&self) -> FrontendMode {
        self.frontend.startup.mode
    }

    /// Startup policy for the cron service.
    #[must_use]
    pub const fn cron_startup(&self) -> ServiceStartup {
        self.services.cron
    }

    /// Startup policy for the delivery service.
    #[must_use]
    pub const fn delivery_startup(&self) -> ServiceStartup {
        self.services.delivery
    }

    /// Startup policy for the Discord gateway.
    #[must_use]
    pub const fn discord_startup(&self) -> ServiceStartup {
        self.services.discord
    }

    /// Startup policy for the WebSocket gateway.
    #[must_use]
    pub const fn websocket_startup(&self) -> ServiceStartup {
        self.services.websocket
    }

    /// WebSocket host for the RPC gateway.
    #[must_use]
    pub fn websocket_host(&self) -> &str {
        &self.frontend.websocket.host
    }

    /// WebSocket port for the RPC gateway.
    #[must_use]
    pub const fn websocket_port(&self) -> u16 {
        self.frontend.websocket.port
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        AppConfig, FrontendMode, LLMConfig, LlmSettings, RatioBps, ServiceStartup, StartupPolicy,
        Tuning, WorkspaceConfig, read_config, write_config,
    };

    #[test]
    fn test_ratio_bps_accepts_decimal_and_basis_points() {
        let tuning: Tuning = serde_json::from_str(
            r#"{
                "compaction": {
                    "compact_threshold": 0.875,
                    "compact_summary_ratio": 1250,
                    "full_compact_summary_ratio": "0.25"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(tuning.compaction.compact_threshold.apply_to(10_000), 8_750);
        assert_eq!(
            tuning.compaction.compact_summary_ratio.apply_to(10_000),
            1_250
        );
        assert_eq!(
            tuning
                .compaction
                .full_compact_summary_ratio
                .apply_to(10_000),
            2_500
        );
    }

    #[test]
    fn test_ratio_bps_rejects_excess_precision() {
        let result = serde_json::from_str::<RatioBps>("0.12345");
        assert!(result.is_err());
    }

    #[test]
    fn test_toml_config_roundtrip_preserves_llm_and_frontend() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let config = AppConfig {
            llm: LlmSettings {
                profiles: vec![LLMConfig {
                    model: "gpt-x".into(),
                    base_url: "https://example.com/v1".into(),
                    api_key: "$TEST_KEY".into(),
                    context_window: 128_000,
                }],
                fallback_models: vec!["gpt-y".into()],
            },
            workspace: WorkspaceConfig {
                dir: Some("./workspace".into()),
            },
            ..AppConfig::default()
        };
        let mut config = config;
        config.frontend.startup.mode = FrontendMode::Stdio;
        config.services.discord = ServiceStartup::new(true, StartupPolicy::Runtime);
        config.services.websocket = ServiceStartup::new(true, StartupPolicy::Config);

        write_config(&config_path, &config).unwrap();
        let loaded: AppConfig = read_config(&config_path).unwrap();

        assert_eq!(loaded.primary_llm().model, "gpt-x");
        assert_eq!(loaded.fallback_models(), ["gpt-y"]);
        assert_eq!(loaded.frontend_mode(), FrontendMode::Stdio);
        assert_eq!(
            loaded.discord_startup(),
            ServiceStartup::new(true, StartupPolicy::Runtime)
        );
        assert_eq!(
            loaded.websocket_startup(),
            ServiceStartup::new(true, StartupPolicy::Config)
        );
        assert_eq!(loaded.websocket_host(), "localhost");
        assert_eq!(loaded.websocket_port(), 8765);
    }

    #[test]
    fn test_toml_config_includes_frontend_tables() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let config = AppConfig::default();

        write_config(&config_path, &config).unwrap();
        let raw = std::fs::read_to_string(&config_path).unwrap();

        assert!(raw.contains("dir = \"./workspace\""));
        assert!(raw.contains("[frontend.startup]"));
        assert!(raw.contains("[services.cron]"));
        assert!(raw.contains("[services.delivery]"));
        assert!(raw.contains("[services.discord]"));
        assert!(raw.contains("[services.websocket]"));
        assert!(raw.contains("[frontend.websocket]"));
        assert!(raw.contains("[frontend.discord]"));
        assert!(raw.contains("[tuning.agent]"));
        assert!(raw.contains("[tuning.compaction]"));
        assert!(raw.contains("[tuning.delivery]"));
    }

}
