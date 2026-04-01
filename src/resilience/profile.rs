use std::time::Instant;

use crate::resilience::classify::FailoverReason;

/// A single API key with cooldown tracking
pub struct AuthProfile {
    pub api_key: String,
    pub base_url: Option<String>,
    cooldown_until: Option<Instant>,
    failure_reason: Option<FailoverReason>,
    last_good_at: Option<Instant>,
}

impl AuthProfile {
    /// Create a profile from an API key
    pub fn new(api_key: String, base_url: Option<String>) -> Self {
        Self {
            api_key,
            base_url,
            cooldown_until: None,
            failure_reason: None,
            last_good_at: None,
        }
    }

    /// Whether this profile is available (not in cooldown)
    pub fn is_available(&self) -> bool {
        match self.cooldown_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }
}

/// Manages multiple auth profiles with cooldown-aware selection
pub struct ProfileManager {
    profiles: Vec<AuthProfile>,
}

impl ProfileManager {
    pub fn new(profiles: Vec<AuthProfile>) -> Self {
        Self { profiles }
    }

    /// Select the first non-cooldown profile
    pub fn select(&self) -> Option<usize> {
        self.profiles.iter().position(|p| p.is_available())
    }

    /// Get profile by index
    pub fn get(&self, idx: usize) -> Option<&AuthProfile> {
        self.profiles.get(idx)
    }

    /// Mark a profile as failed with cooldown
    pub fn mark_failure(&mut self, idx: usize, reason: FailoverReason, cooldown_secs: u64) {
        if let Some(p) = self.profiles.get_mut(idx) {
            p.cooldown_until = Some(Instant::now() + std::time::Duration::from_secs(cooldown_secs));
            p.failure_reason = Some(reason);
        }
    }

    /// Mark a profile as succeeded, clearing failure state
    pub fn mark_success(&mut self, idx: usize) {
        if let Some(p) = self.profiles.get_mut(idx) {
            p.cooldown_until = None;
            p.failure_reason = None;
            p.last_good_at = Some(Instant::now());
        }
    }

    /// Number of registered profiles
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Profile status summary for display
    pub fn status_lines(&self) -> Vec<String> {
        self.profiles
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let key_preview = if p.api_key.len() > 8 {
                    format!("{}...{}", &p.api_key[..4], &p.api_key[p.api_key.len() - 4..])
                } else {
                    "****".to_string()
                };
                let state = if p.is_available() {
                    "ready".to_string()
                } else if let Some(reason) = &p.failure_reason {
                    let remaining = p
                        .cooldown_until
                        .map(|u| u.saturating_duration_since(Instant::now()).as_secs())
                        .unwrap_or(0);
                    format!("cooldown ({reason}, {remaining}s)")
                } else {
                    "cooldown".to_string()
                };
                format!("  [{i}] {key_preview}  {state}")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_first_available() {
        let profiles = vec![
            AuthProfile::new("key-a".into(), None),
            AuthProfile::new("key-b".into(), None),
        ];
        let mgr = ProfileManager::new(profiles);
        assert_eq!(mgr.select(), Some(0));
    }

    #[test]
    fn test_skip_cooled_down_profile() {
        let mut profiles = vec![
            AuthProfile::new("key-a".into(), None),
            AuthProfile::new("key-b".into(), None),
        ];
        profiles[0].cooldown_until =
            Some(Instant::now() + std::time::Duration::from_secs(300));
        let mgr = ProfileManager::new(profiles);
        assert_eq!(mgr.select(), Some(1));
    }

    #[test]
    fn test_all_in_cooldown_returns_none() {
        let mut profiles = vec![
            AuthProfile::new("key-a".into(), None),
        ];
        profiles[0].cooldown_until =
            Some(Instant::now() + std::time::Duration::from_secs(300));
        let mgr = ProfileManager::new(profiles);
        assert_eq!(mgr.select(), None);
    }

    #[test]
    fn test_mark_success_clears_cooldown() {
        let mut mgr = ProfileManager::new(vec![
            AuthProfile::new("key-a".into(), None),
        ]);
        mgr.mark_failure(0, FailoverReason::RateLimit, 120);
        assert!(!mgr.profiles[0].is_available());

        mgr.mark_success(0);
        assert!(mgr.profiles[0].is_available());
        assert!(mgr.profiles[0].failure_reason.is_none());
    }
}
