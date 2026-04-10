use std::sync::Arc;

use anyhow::Result;
use tokio::sync::oneshot;

use super::Gateway;
#[path = "session_runtime/pool.rs"]
mod pool;
#[path = "session_runtime/resolve.rs"]
mod resolve;
#[path = "session_runtime/store.rs"]
mod store;
#[path = "session_runtime/worker.rs"]
mod worker;

use self::pool::{get_or_spawn_session_sender, record_active_session};
use self::resolve::resolve_dispatch;
pub(super) use self::worker::SessionHandle;
use self::worker::{send_control, send_turn};
use crate::frontend::{Channel, UserMessage};
use crate::routing::protocol::{ControlEvent, SessionControl};

pub struct DispatchResult {
    pub agent_id: String,
    pub session_key: String,
    pub done: oneshot::Receiver<()>,
}

impl Gateway {
    async fn session_handle(&self, session_key: &str) -> Result<SessionHandle> {
        let txs = self.session_txs.lock().await;
        txs.get(session_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))
    }

    /// Interrupts an active session turn.
    ///
    /// # Errors
    ///
    /// Returns an error if the session key is unknown.
    pub async fn interrupt(&self, session_key: &str) -> Result<()> {
        let handle = self.session_handle(session_key).await?;
        handle.interrupt();
        Ok(())
    }

    /// Sends a control event to an active session.
    ///
    /// # Errors
    ///
    /// Returns an error if the session key is unknown, the session task has
    /// already closed, or the session does not reply.
    pub async fn send_control(
        &self,
        session_key: &str,
        ctrl: SessionControl,
    ) -> Result<ControlEvent> {
        let handle = self.session_handle(session_key).await?;
        send_control(&handle, ctrl).await
    }

    /// Routes a user message to the appropriate session and queues a turn.
    ///
    /// # Errors
    ///
    /// Returns an error if the session task cannot be created or the turn
    /// cannot be delivered to the session runtime.
    pub async fn dispatch(
        &self,
        msg: UserMessage,
        frontend: Arc<dyn Channel>,
        agent_override: Option<&str>,
    ) -> Result<DispatchResult> {
        let resolved = resolve_dispatch(self, &msg, agent_override).await;
        let (done_tx, done_rx) = oneshot::channel();
        let text = msg.text;
        let session_tx = get_or_spawn_session_sender(self, &resolved).await;
        record_active_session(self, &resolved.session_key).await;
        send_turn(&session_tx, text, frontend, done_tx).await?;

        Ok(DispatchResult {
            agent_id: resolved.agent_id,
            session_key: resolved.session_key,
            done: done_rx,
        })
    }
}
