use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendMode {
    #[default]
    Repl,
    Stdio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPolicy {
    #[default]
    Config,
    Runtime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceStartup {
    pub enabled: bool,
    pub policy: StartupPolicy,
}

impl ServiceStartup {
    #[must_use]
    pub const fn new(enabled: bool, policy: StartupPolicy) -> Self {
        Self { enabled, policy }
    }
}

impl Default for ServiceStartup {
    fn default() -> Self {
        Self::new(false, StartupPolicy::Config)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct FrontendConfig {
    pub(super) startup: FrontendStartupConfig,
    pub(super) websocket: WebSocketConfig,
    pub(super) discord: DiscordConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct FrontendStartupConfig {
    pub(super) mode: FrontendMode,
}

impl Default for FrontendStartupConfig {
    fn default() -> Self {
        Self {
            mode: FrontendMode::Repl,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ServicesConfig {
    pub(super) cron: ServiceStartup,
    pub(super) delivery: ServiceStartup,
    pub(super) discord: ServiceStartup,
    pub(super) websocket: ServiceStartup,
}

impl Default for ServicesConfig {
    fn default() -> Self {
        Self {
            cron: ServiceStartup::new(true, StartupPolicy::Runtime),
            delivery: ServiceStartup::new(true, StartupPolicy::Runtime),
            discord: ServiceStartup::default(),
            websocket: ServiceStartup::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct WebSocketConfig {
    pub(super) host: String,
    pub(super) port: u16,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 8765,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct DiscordConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) token: Option<String>,
}

