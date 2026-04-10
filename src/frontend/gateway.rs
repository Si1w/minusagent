use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use tokio::sync::{Mutex, RwLock, watch};

use crate::config::AppConfig;
use crate::routing::delivery::DeliveryHandle;
use crate::routing::router::BindingRouter;
use crate::runtime::service_state::ServiceStateStore;
use crate::scheduler::cron::{CronHandle, CronJobStatus};

#[path = "gateway/rpc.rs"]
pub(crate) mod rpc;
#[path = "gateway/service_catalog.rs"]
mod service_catalog;
#[path = "gateway/service_controller.rs"]
mod service_controller;
#[path = "gateway/service_registry.rs"]
mod service_registry;
#[path = "gateway/service_runtime.rs"]
mod service_runtime;
#[path = "gateway/session_runtime.rs"]
mod session_runtime;

pub use self::session_runtime::DispatchResult;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedService {
    Cron,
    Delivery,
    Discord,
    Websocket,
}

#[derive(Clone, Copy)]
pub enum ServiceCommand {
    Start,
    Stop,
    Reload,
    Restart,
}

pub enum ServiceControlResult {
    Changed(String),
    Unchanged(String),
    Unsupported(String),
}

#[derive(serde::Serialize)]
pub struct ServiceStatus {
    pub service: ManagedService,
    pub running: bool,
    pub desired_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
    pub summary: String,
}

impl fmt::Display for ServiceControlResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed(message) | Self::Unchanged(message) | Self::Unsupported(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone, Default)]
pub(in crate::frontend::gateway) struct ServiceRuntimeEntry {
    pub(in crate::frontend::gateway) desired_running: bool,
    pub(in crate::frontend::gateway) last_event: Option<String>,
}

#[derive(Clone)]
pub(in crate::frontend::gateway) struct ServiceRuntimeSnapshot {
    pub(in crate::frontend::gateway) desired_running: bool,
    pub(in crate::frontend::gateway) last_event: Option<String>,
}

/// Desired service state plus the last recorded transition event.
pub(in crate::frontend::gateway) struct ServiceRuntimeState {
    pub(in crate::frontend::gateway) store: Option<ServiceStateStore>,
    pub(in crate::frontend::gateway) cron: ServiceRuntimeEntry,
    pub(in crate::frontend::gateway) delivery: ServiceRuntimeEntry,
    pub(in crate::frontend::gateway) discord: ServiceRuntimeEntry,
    pub(in crate::frontend::gateway) websocket: ServiceRuntimeEntry,
}

#[derive(Default)]
pub(in crate::frontend::gateway) struct FrontendServices {
    pub(in crate::frontend::gateway) websocket: StdMutex<FrontendTaskSlot>,
    pub(in crate::frontend::gateway) discord: StdMutex<FrontendTaskSlot>,
}

pub(in crate::frontend::gateway) struct RunningFrontendTask {
    pub(in crate::frontend::gateway) generation: u64,
    pub(in crate::frontend::gateway) shutdown_tx: watch::Sender<bool>,
    pub(in crate::frontend::gateway) join_handle: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub(in crate::frontend::gateway) struct FrontendTaskSlot {
    pub(in crate::frontend::gateway) next_generation: u64,
    pub(in crate::frontend::gateway) running: Option<RunningFrontendTask>,
}

/// Shared application state between gateway and frontends
pub struct AppState {
    pub router: BindingRouter,
    pub sessions: HashSet<String>,
    pub start_time: Instant,
}

/// Thread-safe shared state handle
pub type SharedState = Arc<RwLock<AppState>>;

/// Central dispatcher: routes messages and manages session lifecycle
///
/// Owns the shared state (router + agent manager), app config,
/// and the per-session task pool.
pub struct Gateway {
    state: SharedState,
    config: AppConfig,
    session_txs: Mutex<HashMap<String, session_runtime::SessionHandle>>,
    services: GatewayServices,
}

pub struct GatewayServices {
    delivery: DeliveryHandle,
    cron_handle: Option<CronHandle>,
    frontend_services: FrontendServices,
    runtime_state: StdMutex<ServiceRuntimeState>,
}

impl Gateway {
    /// Create a new gateway
    #[must_use]
    pub fn new(state: SharedState, config: AppConfig, services: GatewayServices) -> Self {
        Self {
            state,
            config,
            session_txs: Mutex::new(HashMap::new()),
            services,
        }
    }

    /// Read access to the shared state
    pub fn state(&self) -> &SharedState {
        &self.state
    }

    /// Read access to the app config
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// Shared runtime services registry.
    pub fn services(&self) -> &GatewayServices {
        &self.services
    }

    /// Get the delivery handle
    pub fn delivery(&self) -> &DeliveryHandle {
        self.services.delivery()
    }

    /// Get the cron handle
    pub fn cron_handle(&self) -> Option<CronHandle> {
        self.services.cron_handle()
    }

    /// List cron jobs
    pub async fn cron_list_jobs(&self) -> Vec<CronJobStatus> {
        self.services.list_cron_jobs().await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Instant;

    use tempfile::tempdir;
    use tokio::sync::RwLock;

    use super::{AppState, Gateway, GatewayServices, SharedState};
    use crate::config::AppConfig;
    use crate::frontend::gateway::ManagedService;
    use crate::intelligence::manager::AgentManager;
    use crate::routing::delivery::{BgOutputSink, OutboundSinks};
    use crate::routing::router::{BindingRouter, BindingTable};
    use crate::runtime::service_state::{PersistedServiceState, ServiceIntent, ServiceStateStore};

    fn test_gateway(services: GatewayServices, config: AppConfig) -> Arc<Gateway> {
        let router = BindingRouter::new(
            BindingTable::new(),
            Arc::new(std::sync::RwLock::new(AgentManager::new(
                "test-model".into(),
            ))),
            "mandeven",
            Arc::new(OutboundSinks::new(Arc::new(BgOutputSink))),
        );
        let state: SharedState = Arc::new(RwLock::new(AppState {
            router,
            sessions: HashSet::new(),
            start_time: Instant::now(),
        }));
        Arc::new(Gateway::new(state, config, services))
    }

    #[tokio::test]
    async fn test_gateway_services_startup_policy_stops_delivery_from_persisted_state() {
        let dir = tempdir().unwrap();
        let store = ServiceStateStore::new(dir.path().join(".runtime").join("services.json"));
        store
            .save(&PersistedServiceState {
                cron: Some(ServiceIntent {
                    desired_running: false,
                }),
                delivery: Some(ServiceIntent {
                    desired_running: false,
                }),
                discord: None,
                websocket: None,
            })
            .unwrap();

        let delivery = crate::routing::delivery::spawn(
            &dir.path().join(".delivery"),
            Arc::new(OutboundSinks::new(Arc::new(BgOutputSink))),
        )
        .unwrap();
        assert!(delivery.is_running());

        let services = GatewayServices::new(delivery.clone(), None, Some(store));
        let gateway = test_gateway(services, AppConfig::default());
        gateway.services().apply_startup_policy(&gateway).await;

        assert!(!delivery.is_running());

        let status = gateway.services().delivery_status().await;
        assert_eq!(status.service, ManagedService::Delivery);
        assert!(!status.running);
        assert!(!status.desired_running);
        assert_eq!(
            status.last_event.as_deref(),
            Some("Delivery runner stopped.")
        );
    }

    #[tokio::test]
    async fn test_gateway_services_frontend_config_policy_ignores_persisted_frontend_state() {
        let dir = tempdir().unwrap();
        let store = ServiceStateStore::new(dir.path().join(".runtime").join("services.json"));
        store
            .save(&PersistedServiceState {
                cron: None,
                delivery: None,
                discord: Some(ServiceIntent {
                    desired_running: false,
                }),
                websocket: Some(ServiceIntent {
                    desired_running: false,
                }),
            })
            .unwrap();

        let delivery = crate::routing::delivery::spawn(
            &dir.path().join(".delivery"),
            Arc::new(OutboundSinks::new(Arc::new(BgOutputSink))),
        )
        .unwrap();
        let services = GatewayServices::new(delivery, None, Some(store));
        let config: AppConfig = toml_edit::de::from_str(
            r#"
                [frontend.startup]
                mode = "repl"

                [services.discord]
                enabled = true
                policy = "config"

                [services.websocket]
                enabled = false
                policy = "config"
            "#,
        )
        .unwrap();
        let gateway = test_gateway(services, config);

        gateway.services().apply_startup_policy(&gateway).await;

        let discord = gateway
            .services()
            .frontend_status(&gateway, ManagedService::Discord);
        let websocket = gateway
            .services()
            .frontend_status(&gateway, ManagedService::Websocket);

        assert!(discord.desired_running);
        assert!(!websocket.desired_running);
    }

    #[tokio::test]
    async fn test_gateway_services_config_policy_ignores_persisted_delivery_state() {
        let dir = tempdir().unwrap();
        let store = ServiceStateStore::new(dir.path().join(".runtime").join("services.json"));
        store
            .save(&PersistedServiceState {
                cron: None,
                delivery: Some(ServiceIntent {
                    desired_running: false,
                }),
                discord: None,
                websocket: None,
            })
            .unwrap();

        let delivery = crate::routing::delivery::spawn(
            &dir.path().join(".delivery"),
            Arc::new(OutboundSinks::new(Arc::new(BgOutputSink))),
        )
        .unwrap();
        let services = GatewayServices::new(delivery.clone(), None, Some(store));
        let config: AppConfig = toml_edit::de::from_str(
            r#"
                [services.delivery]
                enabled = true
                policy = "config"
            "#,
        )
        .unwrap();
        let gateway = test_gateway(services, config);

        gateway.services().apply_startup_policy(&gateway).await;

        let status = gateway.services().delivery_status().await;
        assert!(delivery.is_running());
        assert!(status.running);
        assert!(status.desired_running);
    }
}
