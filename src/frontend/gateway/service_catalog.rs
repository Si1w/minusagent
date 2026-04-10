use super::{ManagedService, ServiceCommand, ServiceControlResult};

impl ManagedService {
    pub(in crate::frontend::gateway) const ALL: [Self; 4] =
        [Self::Cron, Self::Delivery, Self::Discord, Self::Websocket];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cron => "cron",
            Self::Delivery => "delivery",
            Self::Discord => "discord",
            Self::Websocket => "websocket",
        }
    }

    pub(in crate::frontend::gateway) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|service| service.label() == value)
    }
}

impl ServiceCommand {
    #[must_use]
    pub(in crate::frontend::gateway) const fn label(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Reload => "reload",
            Self::Restart => "restart",
        }
    }

    pub(in crate::frontend::gateway) fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "reload" => Some(Self::Reload),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
}

impl ServiceControlResult {
    pub(in crate::frontend::gateway) const fn changed(&self) -> bool {
        matches!(self, Self::Changed(_))
    }
}
