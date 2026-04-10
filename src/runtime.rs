//! Top-level application runtime.
//!
//! [`AppRuntime`] glues together the major subsystems: configuration,
//! routing, gateway services (delivery + cron), the agent manager, and the
//! foreground frontend (REPL or stdio). It is the single entry point used
//! by `main.rs`.
//!
//! The internal `service_state` submodule persists which managed services
//! should be auto-started across restarts.

pub(crate) mod service_state;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::RwLock;

use crate::config::{AppConfig, FrontendMode};
use crate::frontend::Channel;
use crate::frontend::cli::Cli;
use crate::frontend::gateway::{AppState, Gateway, GatewayServices, SharedState};
use crate::frontend::{repl, stdio};
use crate::intelligence::manager::AgentManager;
use crate::logger::TuiLogger;
use crate::routing::delivery::{BgOutputSink, DeliveryHandle, OutboundSinks};
use crate::routing::router::{BindingRouter, BindingTable};
use crate::runtime::service_state::ServiceStateStore;
use crate::scheduler;
use crate::scheduler::cron::CronHandle;

pub struct AppRuntime {
    gateway: Arc<Gateway>,
    frontend_mode: FrontendMode,
}

impl AppRuntime {
    /// Build the full runtime from config and CLI environment.
    ///
    /// # Errors
    ///
    /// Returns an error if runtime services such as delivery cannot be started.
    pub async fn from_env(config: AppConfig) -> Result<Self> {
        scheduler::init_bg_output();

        let frontend_mode = frontend_mode_override(std::env::args().skip(1))
            .unwrap_or_else(|| config.frontend_mode());
        let state = build_shared_state(&config);
        let services = build_gateway_services(&state, &config).await?;
        let gateway = Arc::new(Gateway::new(state, config, services));
        gateway.services().apply_startup_policy(&gateway).await;

        Ok(Self {
            gateway,
            frontend_mode,
        })
    }

    /// Run the configured foreground frontend.
    ///
    /// # Errors
    ///
    /// Returns an error if the selected foreground transport fails.
    pub async fn run(self) -> Result<()> {
        if matches!(self.frontend_mode, FrontendMode::Repl) {
            TuiLogger::init();
        }

        if matches!(self.frontend_mode, FrontendMode::Stdio) {
            stdio::run(self.gateway).await?;
        } else {
            let cli: Arc<dyn Channel> = Arc::new(Cli::new());
            repl::run(self.gateway, cli).await;
        }

        Ok(())
    }
}

fn build_shared_state(config: &AppConfig) -> SharedState {
    let router = build_router(config);

    Arc::new(RwLock::new(AppState {
        router,
        sessions: HashSet::new(),
        start_time: Instant::now(),
    }))
}

fn build_router(config: &AppConfig) -> BindingRouter {
    let manager = build_agent_manager(config);
    let table = build_binding_table(config);
    let outbound = Arc::new(OutboundSinks::new(Arc::new(BgOutputSink)));

    BindingRouter::new(
        table,
        manager,
        &config.tuning.routing.default_agent_id,
        outbound,
    )
}

fn build_agent_manager(config: &AppConfig) -> Arc<std::sync::RwLock<AgentManager>> {
    let mut manager = AgentManager::new(config.primary_llm().model.clone());
    if let Some(workspace_dir) = config.workspace_dir() {
        manager.discover_workspace(&workspace_dir.join(".agents"));
    }
    Arc::new(std::sync::RwLock::new(manager))
}

fn build_binding_table(config: &AppConfig) -> BindingTable {
    let mut table = BindingTable::new();
    if let Some(workspace_dir) = config.workspace_dir() {
        table.load_file(&workspace_dir.join("routes.json"));
    }
    table
}

async fn build_gateway_services(
    state: &SharedState,
    config: &AppConfig,
) -> Result<GatewayServices> {
    let outbound = {
        let state = state.read().await;
        Arc::clone(state.router.outbound())
    };
    let delivery = build_delivery_handle(config, outbound)?;
    let cron_handle = build_cron_handle(config, &delivery);
    Ok(GatewayServices::new(
        delivery,
        cron_handle,
        Some(service_state_store(config)),
    ))
}

fn build_delivery_handle(
    config: &AppConfig,
    outbound: Arc<OutboundSinks>,
) -> Result<DeliveryHandle> {
    crate::routing::delivery::spawn(&delivery_dir(config), outbound)
}

fn build_cron_handle(config: &AppConfig, delivery: &DeliveryHandle) -> Option<CronHandle> {
    config.workspace_dir().and_then(|workspace_dir| {
        let cron_file = workspace_dir.join("CRON.json");
        cron_file.exists().then(|| {
            crate::scheduler::cron::spawn(cron_file, config.primary_llm().clone(), delivery.clone())
        })
    })
}

fn delivery_dir(config: &AppConfig) -> PathBuf {
    config.workspace_dir().map_or_else(
        || PathBuf::from(".delivery"),
        |workspace_dir| workspace_dir.join(".delivery"),
    )
}

fn service_state_store(config: &AppConfig) -> ServiceStateStore {
    let path = config.workspace_dir().map_or_else(
        || PathBuf::from(".runtime/services.json"),
        |workspace_dir| workspace_dir.join(".runtime").join("services.json"),
    );
    ServiceStateStore::new(path)
}

fn frontend_mode_override<I, S>(args: I) -> Option<FrontendMode>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut mode = None;
    for arg in args {
        mode = match arg.as_ref() {
            "--stdio" => Some(FrontendMode::Stdio),
            "--repl" => Some(FrontendMode::Repl),
            _ => mode,
        };
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::{frontend_mode_override, service_state_store};
    use crate::config::FrontendMode;
    use crate::config::{AppConfig, WorkspaceConfig};
    use std::path::Path;

    #[test]
    fn test_frontend_mode_override_prefers_last_cli_flag() {
        let mode = frontend_mode_override(["--stdio", "--repl"]);
        assert_eq!(mode, Some(FrontendMode::Repl));
    }

    #[test]
    fn test_frontend_mode_override_ignores_unknown_args() {
        let mode = frontend_mode_override(["--foo", "--stdio"]);
        assert_eq!(mode, Some(FrontendMode::Stdio));
    }

    #[test]
    fn test_frontend_mode_override_none_when_absent() {
        let mode = frontend_mode_override(["--foo", "bar"]);
        assert_eq!(mode, None);
    }

    #[test]
    fn test_service_state_store_uses_workspace_runtime_dir() {
        let mut config = AppConfig::default();
        config.workspace = WorkspaceConfig {
            dir: Some(Path::new("./workspace").to_path_buf()),
        };

        let store = service_state_store(&config);

        assert_eq!(
            store.path(),
            Path::new("./workspace/.runtime/services.json")
        );
    }
}
