use serde_json::Value;

use crate::frontend::gateway::{ManagedService, ServiceCommand};
use crate::intelligence::manager::DmScope;
use crate::routing::protocol::PermissionMode;

pub(super) struct SendRequest {
    pub(super) text: String,
    pub(super) channel: String,
    pub(super) peer_id: String,
    pub(super) account_id: String,
    pub(super) guild_id: String,
    pub(super) agent_override: Option<String>,
}

impl SendRequest {
    pub(super) fn from_params(params: &Value) -> std::result::Result<Self, String> {
        Ok(Self {
            text: required_string(params, "text")?,
            channel: optional_string(params, "channel", "websocket"),
            peer_id: optional_string(params, "peer_id", "ws-client"),
            account_id: optional_string(params, "account_id", ""),
            guild_id: optional_string(params, "guild_id", ""),
            agent_override: params["agent_id"].as_str().map(str::to_owned),
        })
    }
}

pub(super) struct SessionKeyRequest {
    pub(super) session_key: String,
}

impl SessionKeyRequest {
    pub(super) fn from_params(params: &Value) -> std::result::Result<Self, String> {
        Ok(Self {
            session_key: required_string(params, "session_key")?,
        })
    }
}

pub(super) struct BindingSetRequest {
    pub(super) agent_id: String,
    pub(super) tier: u8,
    pub(super) match_key: String,
    pub(super) match_value: String,
    pub(super) priority: i32,
}

impl BindingSetRequest {
    pub(super) fn from_params(params: &Value) -> std::result::Result<Self, String> {
        Ok(Self {
            agent_id: optional_string(params, "agent_id", "mandeven"),
            tier: optional_u8(params, "tier", 5)?,
            match_key: optional_string(params, "match_key", "default"),
            match_value: optional_string(params, "match_value", "*"),
            priority: optional_i32(params, "priority", 0)?,
        })
    }
}

pub(super) struct BindingRemoveRequest {
    pub(super) agent_id: String,
    pub(super) match_key: String,
    pub(super) match_value: String,
}

impl BindingRemoveRequest {
    pub(super) fn from_params(params: &Value) -> Self {
        Self {
            agent_id: optional_string(params, "agent_id", ""),
            match_key: optional_string(params, "match_key", ""),
            match_value: optional_string(params, "match_value", ""),
        }
    }
}

pub(super) struct AgentRegisterRequest {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) system_prompt: String,
    pub(super) model: String,
    pub(super) dm_scope: DmScope,
    pub(super) workspace_dir: String,
    pub(super) denied_tools: Vec<String>,
}

impl AgentRegisterRequest {
    pub(super) fn from_params(params: &Value) -> std::result::Result<Self, String> {
        let dm_scope = params["dm_scope"]
            .as_str()
            .map_or_else(|| Ok(DmScope::default()), str::parse::<DmScope>)?;
        Ok(Self {
            id: required_string(params, "id")?,
            name: required_string(params, "name")?,
            system_prompt: optional_string(params, "system_prompt", ""),
            model: optional_string(params, "model", ""),
            dm_scope,
            workspace_dir: optional_string(params, "workspace_dir", ""),
            denied_tools: params["denied_tools"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

pub(super) struct ServiceControlRequest {
    pub(super) service: ManagedService,
    pub(super) command: ServiceCommand,
}

impl ServiceControlRequest {
    pub(super) fn from_params(params: &Value) -> std::result::Result<Self, String> {
        Ok(Self {
            service: params["service"]
                .as_str()
                .and_then(ManagedService::parse)
                .ok_or("service is required")?,
            command: params["command"]
                .as_str()
                .and_then(ServiceCommand::parse)
                .ok_or("command is required")?,
        })
    }
}

pub(super) fn rewind_count(params: &Value) -> std::result::Result<usize, String> {
    optional_usize(params, "count", 1)
}

pub(super) fn model_name(params: &Value) -> String {
    optional_string(params, "model", "")
}

pub(super) fn permission_mode(params: &Value) -> PermissionMode {
    params
        .get("mode")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

fn required_string(params: &Value, key: &str) -> std::result::Result<String, String> {
    params[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{key} is required"))
}

fn optional_string(params: &Value, key: &str, default: &str) -> String {
    params[key].as_str().unwrap_or(default).to_string()
}

fn optional_usize(params: &Value, key: &str, default: usize) -> std::result::Result<usize, String> {
    params[key].as_u64().map_or(Ok(default), |value| {
        usize::try_from(value).map_err(|_| format!("{key} is too large"))
    })
}

fn optional_u8(params: &Value, key: &str, default: u8) -> std::result::Result<u8, String> {
    params[key].as_u64().map_or(Ok(default), |value| {
        u8::try_from(value).map_err(|_| format!("{key} is out of range"))
    })
}

fn optional_i32(params: &Value, key: &str, default: i32) -> std::result::Result<i32, String> {
    params[key].as_i64().map_or(Ok(default), |value| {
        i32::try_from(value).map_err(|_| format!("{key} is out of range"))
    })
}
