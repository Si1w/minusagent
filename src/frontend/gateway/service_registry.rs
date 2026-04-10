use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::watch;

use super::service_controller::{
    CronController, DeliveryController, FrontendController, FrontendStatusView,
};
use super::{
    FrontendServices, Gateway, GatewayServices, ManagedService, ServiceCommand,
    ServiceControlResult, ServiceRuntimeState, ServiceStatus,
};
use crate::config::StartupPolicy;
use crate::routing::delivery::DeliveryHandle;
use crate::runtime::service_state::{PersistedServiceState, ServiceStateStore};
use crate::scheduler::cron::{CronHandle, CronJobStatus};

impl GatewayServices {
    #[must_use]
    pub fn new(
        delivery: DeliveryHandle,
        cron_handle: Option<CronHandle>,
        store: Option<ServiceStateStore>,
    ) -> Self {
        let runtime_state = ServiceRuntimeState::new(
            delivery.is_running(),
            cron_handle.as_ref().is_some_and(CronHandle::is_running),
            store,
        );
        Self {
            delivery,
            cron_handle,
            frontend_services: FrontendServices::default(),
            runtime_state: StdMutex::new(runtime_state),
        }
    }

    pub(crate) fn update_service_state(
        &self,
        service: ManagedService,
        desired_running: Option<bool>,
        last_event: Option<String>,
    ) {
        self.lock_runtime_state()
            .update(service, desired_running, last_event);
    }

    pub(crate) fn record_service_event(&self, service: ManagedService, message: impl Into<String>) {
        self.update_service_state(service, None, Some(message.into()));
    }

    pub(crate) fn record_service_outcome(
        &self,
        service: ManagedService,
        desired_running: Option<bool>,
        outcome: &ServiceControlResult,
    ) {
        self.update_service_state(service, desired_running, Some(outcome.to_string()));
    }

    pub(crate) fn spawn_frontend_task<F>(&self, service: ManagedService, spawn: F) -> bool
    where
        F: FnOnce(u64, watch::Receiver<bool>) -> tokio::task::JoinHandle<()>,
    {
        self.frontend_services.start_task(service, spawn)
    }

    pub(crate) fn finish_frontend_task(&self, service: ManagedService, generation: u64) {
        self.frontend_services.finish(service, generation);
    }

    fn service_state_store(&self) -> Option<ServiceStateStore> {
        self.lock_runtime_state().store.clone()
    }

    fn persisted_state(&self) -> Option<PersistedServiceState> {
        let store = self.service_state_store()?;
        match store.load_existing() {
            Ok(state) => state,
            Err(error) => {
                log::error!(
                    "Failed to load runtime service state from {}: {error}",
                    store.path().display()
                );
                None
            }
        }
    }

    pub async fn apply_startup_policy(&self, gateway: &Arc<Gateway>) {
        let persisted = self.persisted_state();
        self.apply_boot_intents(gateway, persisted.as_ref());

        for service in ManagedService::ALL {
            self.reconcile_service(gateway, service).await;
        }
    }

    fn apply_boot_intents(
        &self,
        gateway: &Arc<Gateway>,
        persisted: Option<&PersistedServiceState>,
    ) {
        for service in ManagedService::ALL {
            let desired_running = Self::boot_desired(gateway, service, persisted);
            self.update_service_state(service, Some(desired_running), None);
        }
    }

    fn boot_desired(
        gateway: &Gateway,
        service: ManagedService,
        persisted: Option<&PersistedServiceState>,
    ) -> bool {
        let startup = match service {
            ManagedService::Cron => gateway.config().cron_startup(),
            ManagedService::Delivery => gateway.config().delivery_startup(),
            ManagedService::Discord => gateway.config().discord_startup(),
            ManagedService::Websocket => gateway.config().websocket_startup(),
        };
        let persisted_intent = persisted.and_then(|state| match service {
            ManagedService::Cron => state.cron,
            ManagedService::Delivery => state.delivery,
            ManagedService::Discord => state.discord,
            ManagedService::Websocket => state.websocket,
        });
        match startup.policy {
            StartupPolicy::Config => startup.enabled,
            StartupPolicy::Runtime => {
                persisted_intent.map_or(startup.enabled, |intent| intent.desired_running)
            }
        }
    }

    async fn reconcile_service(&self, gateway: &Arc<Gateway>, service: ManagedService) {
        let desired_running = self.service_snapshot(service).desired_running;
        let currently_running = self.is_service_running(service);
        if desired_running == currently_running {
            return;
        }

        let result = if desired_running {
            self.control(gateway, service, ServiceCommand::Start).await
        } else {
            self.control(gateway, service, ServiceCommand::Stop).await
        };
        log::info!("{} startup policy: {}", service.label(), result);
    }

    fn is_service_running(&self, service: ManagedService) -> bool {
        match service {
            ManagedService::Cron => self.cron_handle().is_some_and(|handle| handle.is_running()),
            ManagedService::Delivery => self.delivery.is_running(),
            ManagedService::Discord | ManagedService::Websocket => {
                self.frontend_services.is_running(service)
            }
        }
    }

    #[must_use]
    pub fn delivery(&self) -> &DeliveryHandle {
        &self.delivery
    }

    pub async fn status_snapshot(&self, gateway: &Gateway) -> Vec<ServiceStatus> {
        let mut statuses = Vec::with_capacity(ManagedService::ALL.len());
        for service in ManagedService::ALL {
            statuses.push(self.status(gateway, service).await);
        }
        statuses
    }

    pub async fn control(
        &self,
        gateway: &Arc<Gateway>,
        service: ManagedService,
        command: ServiceCommand,
    ) -> ServiceControlResult {
        match service {
            ManagedService::Cron => self.control_cron(command).await,
            ManagedService::Delivery => self.control_delivery(command).await,
            ManagedService::Discord | ManagedService::Websocket => {
                self.control_frontend(gateway, service, command).await
            }
        }
    }

    async fn control_cron(&self, command: ServiceCommand) -> ServiceControlResult {
        CronController::new(self).control(command).await
    }

    async fn control_delivery(&self, command: ServiceCommand) -> ServiceControlResult {
        DeliveryController::new(self).control(command).await
    }

    async fn control_frontend(
        &self,
        gateway: &Arc<Gateway>,
        service: ManagedService,
        command: ServiceCommand,
    ) -> ServiceControlResult {
        FrontendController::new(self, gateway, service)
            .control(command)
            .await
    }

    #[must_use]
    pub fn cron_handle(&self) -> Option<CronHandle> {
        self.cron_handle.clone()
    }

    pub async fn list_cron_jobs(&self) -> Vec<CronJobStatus> {
        match self.cron_handle() {
            Some(handle) => handle.list_jobs().await,
            None => Vec::new(),
        }
    }

    pub(in crate::frontend::gateway) fn build_service_status(
        &self,
        service: ManagedService,
        running: bool,
        summary: String,
    ) -> ServiceStatus {
        let runtime = self.service_snapshot(service);
        let mut details = summary;
        if runtime.desired_running != running {
            details.push_str("; desired=");
            details.push_str(if runtime.desired_running { "on" } else { "off" });
        }
        if (!running || runtime.desired_running != running)
            && let Some(last_event) = &runtime.last_event
        {
            details.push_str("; last=");
            details.push_str(last_event);
        }
        ServiceStatus {
            service,
            running,
            desired_running: runtime.desired_running,
            last_event: runtime.last_event,
            summary: details,
        }
    }

    async fn status(&self, gateway: &Gateway, service: ManagedService) -> ServiceStatus {
        match service {
            ManagedService::Cron => self.cron_status().await,
            ManagedService::Delivery => self.delivery_status().await,
            ManagedService::Discord | ManagedService::Websocket => {
                self.frontend_status(gateway, service)
            }
        }
    }

    pub(in crate::frontend::gateway) async fn cron_status(&self) -> ServiceStatus {
        CronController::new(self).status().await
    }

    pub(in crate::frontend::gateway) async fn delivery_status(&self) -> ServiceStatus {
        DeliveryController::new(self).status().await
    }

    pub(in crate::frontend::gateway) fn frontend_status(
        &self,
        gateway: &Gateway,
        service: ManagedService,
    ) -> ServiceStatus {
        FrontendStatusView::new(self, gateway, service).status()
    }
}
