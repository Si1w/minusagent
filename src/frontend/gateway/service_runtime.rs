use std::sync::{Mutex as StdMutex, MutexGuard};

use tokio::sync::watch;

use super::{
    FrontendServices, FrontendTaskSlot, GatewayServices, ManagedService, RunningFrontendTask,
    ServiceRuntimeEntry, ServiceRuntimeSnapshot, ServiceRuntimeState,
};
use crate::runtime::service_state::{PersistedServiceState, ServiceIntent, ServiceStateStore};

/// Map a [`ManagedService`] variant to its dedicated runtime entry.
fn entry_for(state: &ServiceRuntimeState, service: ManagedService) -> &ServiceRuntimeEntry {
    match service {
        ManagedService::Cron => &state.cron,
        ManagedService::Delivery => &state.delivery,
        ManagedService::Discord => &state.discord,
        ManagedService::Websocket => &state.websocket,
    }
}

fn entry_for_mut(
    state: &mut ServiceRuntimeState,
    service: ManagedService,
) -> &mut ServiceRuntimeEntry {
    match service {
        ManagedService::Cron => &mut state.cron,
        ManagedService::Delivery => &mut state.delivery,
        ManagedService::Discord => &mut state.discord,
        ManagedService::Websocket => &mut state.websocket,
    }
}

impl ServiceRuntimeState {
    pub(super) fn new(
        delivery_running: bool,
        cron_running: bool,
        store: Option<ServiceStateStore>,
    ) -> Self {
        Self {
            store,
            cron: ServiceRuntimeEntry {
                desired_running: cron_running,
                last_event: None,
            },
            delivery: ServiceRuntimeEntry {
                desired_running: delivery_running,
                last_event: None,
            },
            discord: ServiceRuntimeEntry::default(),
            websocket: ServiceRuntimeEntry::default(),
        }
    }

    pub(super) fn snapshot(&self, service: ManagedService) -> ServiceRuntimeSnapshot {
        let entry = self.entry(service);
        ServiceRuntimeSnapshot {
            desired_running: entry.desired_running,
            last_event: entry.last_event.clone(),
        }
    }

    pub(super) fn update(
        &mut self,
        service: ManagedService,
        desired_running: Option<bool>,
        last_event: Option<String>,
    ) {
        let entry = self.entry_mut(service);
        if let Some(desired_running) = desired_running {
            entry.desired_running = desired_running;
        }
        if let Some(last_event) = last_event {
            entry.last_event = Some(last_event);
        }
        if desired_running.is_some() {
            self.persist();
        }
    }

    fn persist(&self) {
        if let Some(store) = &self.store {
            store.persist(&self.persisted_state());
        }
    }

    fn persisted_state(&self) -> PersistedServiceState {
        PersistedServiceState {
            cron: Some(ServiceIntent {
                desired_running: self.cron.desired_running,
            }),
            delivery: Some(ServiceIntent {
                desired_running: self.delivery.desired_running,
            }),
            discord: Some(ServiceIntent {
                desired_running: self.discord.desired_running,
            }),
            websocket: Some(ServiceIntent {
                desired_running: self.websocket.desired_running,
            }),
        }
    }

    fn entry(&self, service: ManagedService) -> &ServiceRuntimeEntry {
        entry_for(self, service)
    }

    fn entry_mut(&mut self, service: ManagedService) -> &mut ServiceRuntimeEntry {
        entry_for_mut(self, service)
    }
}

impl FrontendServices {
    pub(super) fn start_task<F>(&self, service: ManagedService, spawn: F) -> bool
    where
        F: FnOnce(u64, watch::Receiver<bool>) -> tokio::task::JoinHandle<()>,
    {
        let slot = self.slot(service);
        let mut state = Self::lock_slot(slot);
        if state
            .running
            .as_ref()
            .is_some_and(|task| !task.join_handle.is_finished())
        {
            return false;
        }

        state.next_generation = state.next_generation.saturating_add(1);
        let generation = state.next_generation;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join_handle = spawn(generation, shutdown_rx);
        state.running = Some(RunningFrontendTask {
            generation,
            shutdown_tx,
            join_handle,
        });
        true
    }

    pub(super) async fn stop_task(&self, service: ManagedService) -> bool {
        let task = self.take_task(service);
        let Some(task) = task else {
            return false;
        };
        let _ = task.shutdown_tx.send(true);
        match task.join_handle.await {
            Ok(()) => true,
            Err(error) if error.is_cancelled() => true,
            Err(error) => {
                log::error!("{} task failed while stopping: {error}", service.label());
                false
            }
        }
    }

    pub(super) fn finish(&self, service: ManagedService, generation: u64) {
        let slot = self.slot(service);
        let mut state = Self::lock_slot(slot);
        if state
            .running
            .as_ref()
            .is_some_and(|task| task.generation == generation)
        {
            state.running = None;
        }
    }

    pub(super) fn is_running(&self, service: ManagedService) -> bool {
        let slot = self.slot(service);
        Self::lock_slot(slot)
            .running
            .as_ref()
            .is_some_and(|task| !task.join_handle.is_finished())
    }

    fn take_task(&self, service: ManagedService) -> Option<RunningFrontendTask> {
        let slot = self.slot(service);
        Self::lock_slot(slot).running.take()
    }

    fn slot(&self, service: ManagedService) -> &StdMutex<FrontendTaskSlot> {
        match service {
            ManagedService::Discord => &self.discord,
            ManagedService::Websocket => &self.websocket,
            ManagedService::Cron | ManagedService::Delivery => {
                unreachable!("frontend slot requested for non-frontend service: {service:?}")
            }
        }
    }

    fn lock_slot(slot: &StdMutex<FrontendTaskSlot>) -> MutexGuard<'_, FrontendTaskSlot> {
        slot.lock().unwrap_or_else(|error| {
            log::error!("Frontend service slot lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }
}

impl GatewayServices {
    pub(super) fn lock_runtime_state(&self) -> MutexGuard<'_, ServiceRuntimeState> {
        self.runtime_state.lock().unwrap_or_else(|error| {
            log::error!("Service runtime state lock poisoned, recovering: {error}");
            error.into_inner()
        })
    }

    pub(super) fn service_snapshot(&self, service: ManagedService) -> ServiceRuntimeSnapshot {
        self.lock_runtime_state().snapshot(service)
    }
}
