use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};

use crate::engine::session::Session;
use crate::engine::store::SharedStore;
use crate::frontend::Channel;
use crate::routing::protocol::{ControlEvent, SessionControl};
use crate::scheduler::LaneLock;
use crate::scheduler::heartbeat::HeartbeatHandle;

pub(super) enum SessionMessage {
    Turn {
        text: String,
        frontend: Arc<dyn Channel>,
        done: Option<oneshot::Sender<()>>,
    },
    Control {
        ctrl: SessionControl,
        reply: oneshot::Sender<ControlEvent>,
    },
}

#[derive(Clone)]
pub(in crate::frontend::gateway) struct SessionHandle {
    tx: mpsc::Sender<SessionMessage>,
    interrupted: Arc<AtomicBool>,
}

impl SessionHandle {
    pub(super) fn sender(&self) -> mpsc::Sender<SessionMessage> {
        self.tx.clone()
    }

    pub(super) fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Relaxed);
    }

    pub(super) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

pub(super) async fn send_turn(
    session_tx: &mpsc::Sender<SessionMessage>,
    text: String,
    frontend: Arc<dyn Channel>,
    done: oneshot::Sender<()>,
) -> Result<()> {
    session_tx
        .send(SessionMessage::Turn {
            text,
            frontend,
            done: Some(done),
        })
        .await
        .map_err(|_| anyhow::anyhow!("Session task closed"))
}

pub(super) async fn send_control(
    handle: &SessionHandle,
    ctrl: SessionControl,
) -> Result<ControlEvent> {
    let (reply_tx, reply_rx) = oneshot::channel();
    handle
        .sender()
        .send(SessionMessage::Control {
            ctrl,
            reply: reply_tx,
        })
        .await
        .map_err(|_| anyhow::anyhow!("Session task closed"))?;

    reply_rx
        .await
        .map_err(|_| anyhow::anyhow!("Session did not respond"))
}

pub(super) fn spawn_session_task(
    store: SharedStore,
    lane_lock: LaneLock,
    heartbeat: Option<HeartbeatHandle>,
    extra_profiles: Vec<crate::resilience::profile::AuthProfile>,
    fallback_models: Vec<String>,
) -> SessionHandle {
    let interrupted = Arc::new(AtomicBool::new(false));
    let interrupted_clone = interrupted.clone();
    let (tx, mut rx) = mpsc::channel::<SessionMessage>(8);

    tokio::spawn(async move {
        let mut session = match Session::new(
            store,
            lane_lock,
            heartbeat,
            extra_profiles,
            fallback_models,
            interrupted_clone,
        ) {
            Ok(session) => session,
            Err(error) => {
                log::error!("Failed to create session: {error}");
                report_session_creation_error(&mut rx, &error.to_string()).await;
                return;
            }
        };

        while let Some(message) = rx.recv().await {
            handle_session_message(&mut session, message).await;
        }
    });

    SessionHandle { tx, interrupted }
}

async fn report_session_creation_error(rx: &mut mpsc::Receiver<SessionMessage>, error: &str) {
    if let Some(SessionMessage::Turn { frontend, done, .. }) = rx.recv().await {
        frontend.send(&format!("Error: {error}")).await;
        if let Some(done) = done {
            let _ = done.send(());
        }
    }
}

async fn handle_session_message(session: &mut Session, message: SessionMessage) {
    match message {
        SessionMessage::Turn {
            text,
            frontend,
            done,
        } => {
            if let Err(error) = session.turn(&text, &frontend).await {
                frontend.send(&format!("Error: {error}")).await;
            }
            if let Some(done) = done {
                let _ = done.send(());
            }
        }
        SessionMessage::Control { ctrl, reply } => {
            let event = session.handle_control(ctrl);
            let _ = reply.send(event);
        }
    }
}
