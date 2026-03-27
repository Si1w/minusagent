use std::fmt;

/// Failure category that determines which retry layer handles recovery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverReason {
    /// 429 — wait for rate-limit window to reset
    RateLimit,
    /// 401 — bad key, won't self-heal quickly
    Auth,
    /// Request timed out — transient, short cooldown
    Timeout,
    /// 402 / quota — billing issue, long cooldown
    Billing,
    /// Context window exceeded — compact messages, don't rotate profile
    Overflow,
    /// Unrecognized error
    Unknown,
}

impl FailoverReason {
    /// Default cooldown in seconds for this failure category
    pub fn default_cooldown_secs(&self) -> u64 {
        match self {
            Self::Auth | Self::Billing => 300,
            Self::RateLimit => 120,
            Self::Timeout => 60,
            Self::Overflow | Self::Unknown => 0,
        }
    }
}

impl fmt::Display for FailoverReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RateLimit => write!(f, "rate_limit"),
            Self::Auth => write!(f, "auth"),
            Self::Timeout => write!(f, "timeout"),
            Self::Billing => write!(f, "billing"),
            Self::Overflow => write!(f, "overflow"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Classify an error by matching known patterns in its message
pub fn classify_failure(err: &anyhow::Error) -> FailoverReason {
    let msg = err.to_string().to_lowercase();

    if msg.contains("429") || msg.contains("rate") || msg.contains("too many") {
        return FailoverReason::RateLimit;
    }
    if msg.contains("401") || msg.contains("auth") || msg.contains("invalid key") || msg.contains("invalid api key") {
        return FailoverReason::Auth;
    }
    if msg.contains("timeout") || msg.contains("timed out") || msg.contains("deadline") {
        return FailoverReason::Timeout;
    }
    if msg.contains("402") || msg.contains("billing") || msg.contains("quota") {
        return FailoverReason::Billing;
    }
    if msg.contains("context") || msg.contains("context_length") || msg.contains("overflow")
        || msg.contains("too many tokens") || msg.contains("maximum context")
    {
        return FailoverReason::Overflow;
    }

    FailoverReason::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_rate_limit() {
        let err = anyhow::anyhow!("status 429: Too Many Requests");
        assert_eq!(classify_failure(&err), FailoverReason::RateLimit);
    }

    #[test]
    fn test_classify_auth() {
        let err = anyhow::anyhow!("status 401: Unauthorized");
        assert_eq!(classify_failure(&err), FailoverReason::Auth);
    }

    #[test]
    fn test_classify_timeout() {
        let err = anyhow::anyhow!("request timed out");
        assert_eq!(classify_failure(&err), FailoverReason::Timeout);
    }

    #[test]
    fn test_classify_billing() {
        let err = anyhow::anyhow!("402: billing quota exceeded");
        assert_eq!(classify_failure(&err), FailoverReason::Billing);
    }

    #[test]
    fn test_classify_overflow() {
        let err = anyhow::anyhow!("context length exceeded: 128000 tokens");
        assert_eq!(classify_failure(&err), FailoverReason::Overflow);
    }

    #[test]
    fn test_classify_unknown() {
        let err = anyhow::anyhow!("something weird happened");
        assert_eq!(classify_failure(&err), FailoverReason::Unknown);
    }

    #[test]
    fn test_cooldown_values() {
        assert_eq!(FailoverReason::Auth.default_cooldown_secs(), 300);
        assert_eq!(FailoverReason::RateLimit.default_cooldown_secs(), 120);
        assert_eq!(FailoverReason::Timeout.default_cooldown_secs(), 60);
        assert_eq!(FailoverReason::Overflow.default_cooldown_secs(), 0);
    }
}
