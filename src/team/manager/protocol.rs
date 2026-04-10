use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{LEAD_NAME, TeammateManager, TeammateStatus};

/// Request-response FSM status
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Approved,
    Rejected,
}

impl fmt::Display for RequestStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(formatter, "pending"),
            Self::Approved => write!(formatter, "approved"),
            Self::Rejected => write!(formatter, "rejected"),
        }
    }
}

/// Tracked shutdown request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRequest {
    pub target: String,
    pub status: RequestStatus,
}

/// Tracked plan approval request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRequest {
    #[serde(rename = "from")]
    pub submitter: String,
    pub plan: String,
    pub status: RequestStatus,
}

impl TeammateManager {
    /// Send a shutdown request to a teammate
    ///
    /// # Errors
    ///
    /// Returns error if teammate is unknown.
    pub fn request_shutdown(&self, teammate: &str) -> Result<String> {
        if !self.has_member(teammate) {
            return Err(anyhow::anyhow!("Unknown teammate: {teammate}"));
        }

        let req_id = Self::next_request_id();

        {
            let mut state = self.lock_state();
            state.shutdown_requests.insert(
                req_id.clone(),
                ShutdownRequest {
                    target: teammate.to_string(),
                    status: RequestStatus::Pending,
                },
            );
        }

        self.bus.send(
            LEAD_NAME,
            teammate,
            "Please shut down gracefully.",
            "shutdown_request",
            Some(json!({ "request_id": req_id })),
        )?;

        self.wake_teammate(teammate);

        Ok(format!(
            "Shutdown request {req_id} sent to '{teammate}' (status: pending)"
        ))
    }

    /// Respond to a shutdown request
    ///
    /// If approved, the teammate's wake channel is dropped so it
    /// exits after the current work cycle.
    ///
    /// # Errors
    ///
    /// Returns error if request is unknown or already resolved.
    pub fn respond_shutdown(
        &self,
        req_id: &str,
        approve: bool,
        reason: &str,
        sender: &str,
    ) -> Result<String> {
        let target = {
            let mut state = self.lock_state();
            let request = state
                .shutdown_requests
                .get_mut(req_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown shutdown request: {req_id}"))?;
            if request.status != RequestStatus::Pending {
                return Err(anyhow::anyhow!(
                    "Request {req_id} already {:?}",
                    request.status
                ));
            }
            request.status = if approve {
                RequestStatus::Approved
            } else {
                RequestStatus::Rejected
            };
            request.target.clone()
        };

        let status = if approve { "approved" } else { "rejected" };
        let content = if reason.is_empty() {
            format!("Shutdown {status}.")
        } else {
            format!("Shutdown {status}. Reason: {reason}")
        };
        self.bus.send(
            sender,
            LEAD_NAME,
            &content,
            "shutdown_response",
            Some(json!({
                "request_id": req_id,
                "approve": approve,
            })),
        )?;

        if approve {
            self.set_status(&target, TeammateStatus::Shutdown);
        }

        Ok(format!("Shutdown {req_id}: {status}"))
    }

    /// Submit a plan for lead review
    ///
    /// # Returns
    ///
    /// Status message with the generated `request_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be written to the lead inbox.
    pub fn submit_plan(&self, from: &str, plan: &str) -> Result<String> {
        let req_id = Self::next_request_id();

        {
            let mut state = self.lock_state();
            state.plan_requests.insert(
                req_id.clone(),
                PlanRequest {
                    submitter: from.to_string(),
                    plan: plan.to_string(),
                    status: RequestStatus::Pending,
                },
            );
        }

        self.bus.send(
            from,
            LEAD_NAME,
            plan,
            "plan_request",
            Some(json!({ "request_id": req_id })),
        )?;

        Ok(format!(
            "Plan submitted (request_id: {req_id}, status: pending)"
        ))
    }

    /// Respond to a plan submission
    ///
    /// Sends the decision to the submitter's inbox and wakes
    /// them if idle.
    ///
    /// # Errors
    ///
    /// Returns error if request is unknown or already resolved.
    pub fn respond_plan(&self, req_id: &str, approve: bool, feedback: &str) -> Result<String> {
        let submitter = {
            let mut state = self.lock_state();
            let request = state
                .plan_requests
                .get_mut(req_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown plan request: {req_id}"))?;
            if request.status != RequestStatus::Pending {
                return Err(anyhow::anyhow!(
                    "Plan {req_id} already {:?}",
                    request.status
                ));
            }
            request.status = if approve {
                RequestStatus::Approved
            } else {
                RequestStatus::Rejected
            };
            request.submitter.clone()
        };

        let status = if approve { "approved" } else { "rejected" };
        let content = if feedback.is_empty() {
            format!("Plan {status}.")
        } else {
            format!("Plan {status}. Feedback: {feedback}")
        };

        self.bus.send(
            LEAD_NAME,
            &submitter,
            &content,
            "plan_response",
            Some(json!({
                "request_id": req_id,
                "approve": approve,
            })),
        )?;

        self.wake_teammate(&submitter);

        Ok(format!("Plan {req_id}: {status}"))
    }

    /// Format pending protocol requests for display
    #[must_use]
    pub fn list_requests(&self) -> String {
        let state = self.lock_state();
        let mut lines = Vec::new();
        for (id, request) in &state.shutdown_requests {
            lines.push(format!(
                "  shutdown {id} -> {} ({})",
                request.target, request.status
            ));
        }
        for (id, request) in &state.plan_requests {
            lines.push(format!(
                "  plan {id} from {} ({})",
                request.submitter, request.status
            ));
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!("\nRequests:\n{}", lines.join("\n"))
        }
    }
}
