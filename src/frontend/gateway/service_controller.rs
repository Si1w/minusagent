use std::sync::Arc;

use super::{
    Gateway, GatewayServices, ManagedService, ServiceCommand, ServiceControlResult, ServiceStatus,
};
use crate::frontend::launch::{start_discord_service, start_websocket_service};
use crate::scheduler::cron::CronHandle;

fn frontend_summary(gateway: &Gateway, service: ManagedService, running: bool) -> String {
    match service {
        ManagedService::Discord => {
            if running {
                "running".into()
            } else if gateway.config().discord_token().is_some() {
                "stopped".into()
            } else {
                "not configured".into()
            }
        }
        ManagedService::Websocket => {
            let host = gateway.config().websocket_host();
            let port = gateway.config().websocket_port();
            let state = if running { "running" } else { "stopped" };
            format!("{state}; bind=ws://{host}:{port}")
        }
        ManagedService::Cron | ManagedService::Delivery => {
            unreachable!("frontend status requested for non-frontend service: {service:?}")
        }
    }
}

pub(super) struct CronController<'a> {
    services: &'a GatewayServices,
}

impl<'a> CronController<'a> {
    const SERVICE: ManagedService = ManagedService::Cron;

    pub(super) const fn new(services: &'a GatewayServices) -> Self {
        Self { services }
    }

    fn handle(&self) -> Option<CronHandle> {
        self.services.cron_handle()
    }

    async fn stop(&self) -> bool {
        match self.handle() {
            Some(handle) => handle.stop().await,
            None => false,
        }
    }

    pub(super) async fn control(&self, command: ServiceCommand) -> ServiceControlResult {
        match command {
            ServiceCommand::Start => {
                if let Some(handle) = self.handle() {
                    let result = if handle.start() {
                        ServiceControlResult::Changed("Cron service started.".into())
                    } else {
                        ServiceControlResult::Unchanged("Cron service already running.".into())
                    };
                    self.services
                        .record_service_outcome(Self::SERVICE, Some(true), &result);
                    result
                } else {
                    let result =
                        ServiceControlResult::Unsupported("Cron service is not configured.".into());
                    self.services
                        .record_service_outcome(Self::SERVICE, None, &result);
                    result
                }
            }
            ServiceCommand::Stop => {
                let result = if self.stop().await {
                    ServiceControlResult::Changed("Cron service stopped.".into())
                } else {
                    ServiceControlResult::Unchanged("Cron service not running.".into())
                };
                self.services
                    .record_service_outcome(Self::SERVICE, Some(false), &result);
                result
            }
            ServiceCommand::Reload => {
                if let Some(handle) = self.handle() {
                    let result = ServiceControlResult::Changed(handle.reload().await);
                    self.services
                        .record_service_outcome(Self::SERVICE, None, &result);
                    result
                } else {
                    let result =
                        ServiceControlResult::Unchanged("Cron service not running.".into());
                    self.services
                        .record_service_outcome(Self::SERVICE, None, &result);
                    result
                }
            }
            ServiceCommand::Restart => {
                if let Some(handle) = self.handle() {
                    let result = if handle.restart().await {
                        ServiceControlResult::Changed("Cron service restarted.".into())
                    } else {
                        ServiceControlResult::Unchanged("Cron service restart skipped.".into())
                    };
                    self.services
                        .record_service_outcome(Self::SERVICE, Some(true), &result);
                    result
                } else {
                    let result =
                        ServiceControlResult::Unsupported("Cron service is not configured.".into());
                    self.services
                        .record_service_outcome(Self::SERVICE, None, &result);
                    result
                }
            }
        }
    }

    pub(super) async fn status(&self) -> ServiceStatus {
        match self.handle() {
            Some(handle) => {
                let running = handle.is_running();
                let jobs = handle.list_jobs().await;
                self.services.build_service_status(
                    Self::SERVICE,
                    running,
                    if running {
                        format!("running; jobs={}", jobs.len())
                    } else {
                        "stopped".into()
                    },
                )
            }
            None => {
                self.services
                    .build_service_status(Self::SERVICE, false, "not configured".into())
            }
        }
    }
}

pub(super) struct DeliveryController<'a> {
    services: &'a GatewayServices,
}

impl<'a> DeliveryController<'a> {
    const SERVICE: ManagedService = ManagedService::Delivery;

    pub(super) const fn new(services: &'a GatewayServices) -> Self {
        Self { services }
    }

    async fn stop(&self) -> bool {
        self.services.delivery.stop().await
    }

    pub(super) async fn control(&self, command: ServiceCommand) -> ServiceControlResult {
        match command {
            ServiceCommand::Start => {
                let result = if self.services.delivery.start() {
                    ServiceControlResult::Changed("Delivery runner started.".into())
                } else {
                    ServiceControlResult::Unchanged("Delivery runner already running.".into())
                };
                self.services
                    .record_service_outcome(Self::SERVICE, Some(true), &result);
                result
            }
            ServiceCommand::Stop => {
                let result = if self.stop().await {
                    ServiceControlResult::Changed("Delivery runner stopped.".into())
                } else {
                    ServiceControlResult::Unchanged("Delivery runner not running.".into())
                };
                self.services
                    .record_service_outcome(Self::SERVICE, Some(false), &result);
                result
            }
            ServiceCommand::Restart => {
                let result = if self.services.delivery.restart().await {
                    ServiceControlResult::Changed("Delivery runner restarted.".into())
                } else {
                    ServiceControlResult::Unchanged("Delivery runner restart skipped.".into())
                };
                self.services
                    .record_service_outcome(Self::SERVICE, Some(true), &result);
                result
            }
            ServiceCommand::Reload => ServiceControlResult::Unsupported(format!(
                "{} does not support {}",
                Self::SERVICE.label(),
                ServiceCommand::Reload.label()
            )),
        }
    }

    pub(super) async fn status(&self) -> ServiceStatus {
        let running = self.services.delivery.is_running();
        match self.services.delivery.stats().await {
            Some(stats) => self.services.build_service_status(
                Self::SERVICE,
                running,
                format!(
                    "running; pending={} attempted={} succeeded={} failed={}",
                    stats.pending, stats.total_attempted, stats.total_succeeded, stats.total_failed
                ),
            ),
            None => self
                .services
                .build_service_status(Self::SERVICE, false, "stopped".into()),
        }
    }
}

pub(super) struct FrontendController<'a> {
    services: &'a GatewayServices,
    gateway: &'a Arc<Gateway>,
    service: ManagedService,
}

impl<'a> FrontendController<'a> {
    pub(super) fn new(
        services: &'a GatewayServices,
        gateway: &'a Arc<Gateway>,
        service: ManagedService,
    ) -> Self {
        debug_assert!(matches!(
            service,
            ManagedService::Discord | ManagedService::Websocket
        ));
        Self {
            services,
            gateway,
            service,
        }
    }

    async fn stop(&self) -> bool {
        self.services
            .frontend_services
            .stop_task(self.service)
            .await
    }

    fn start(&self) -> ServiceControlResult {
        match self.service {
            ManagedService::Discord => start_discord_service(self.services, self.gateway),
            ManagedService::Websocket => start_websocket_service(self.services, self.gateway),
            ManagedService::Cron | ManagedService::Delivery => unreachable!(
                "frontend start requested for non-frontend service: {:?}",
                self.service
            ),
        }
    }

    pub(super) async fn control(&self, command: ServiceCommand) -> ServiceControlResult {
        match command {
            ServiceCommand::Start => self.start(),
            ServiceCommand::Stop => {
                let result = if self.stop().await {
                    ServiceControlResult::Changed(format!("{} stopped.", self.service.label()))
                } else {
                    ServiceControlResult::Unchanged(format!(
                        "{} not running.",
                        self.service.label()
                    ))
                };
                self.services
                    .record_service_outcome(self.service, Some(false), &result);
                result
            }
            ServiceCommand::Restart => {
                let _ = self.stop().await;
                let result = self.start();
                self.services
                    .record_service_outcome(self.service, Some(true), &result);
                result
            }
            ServiceCommand::Reload => ServiceControlResult::Unsupported(format!(
                "{} does not support {}",
                self.service.label(),
                ServiceCommand::Reload.label()
            )),
        }
    }
}

pub(super) struct FrontendStatusView<'a> {
    services: &'a GatewayServices,
    gateway: &'a Gateway,
    service: ManagedService,
}

impl<'a> FrontendStatusView<'a> {
    pub(super) fn new(
        services: &'a GatewayServices,
        gateway: &'a Gateway,
        service: ManagedService,
    ) -> Self {
        debug_assert!(matches!(
            service,
            ManagedService::Discord | ManagedService::Websocket
        ));
        Self {
            services,
            gateway,
            service,
        }
    }

    pub(super) fn status(&self) -> ServiceStatus {
        let running = self.services.frontend_services.is_running(self.service);
        let summary = frontend_summary(self.gateway, self.service, running);
        self.services
            .build_service_status(self.service, running, summary)
    }
}
