use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct ServiceStateStore {
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceIntent {
    pub desired_running: bool,
}

impl Default for ServiceIntent {
    fn default() -> Self {
        Self {
            desired_running: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedServiceState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<ServiceIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ServiceIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord: Option<ServiceIntent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket: Option<ServiceIntent>,
}

impl ServiceStateStore {
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the persisted runtime service state.
    ///
    /// # Errors
    ///
    /// Returns an error if the state file exists but cannot be read or parsed.
    pub fn load(&self) -> Result<PersistedServiceState> {
        if !self.path.exists() {
            return Ok(PersistedServiceState::default());
        }
        let raw = fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Load the persisted runtime service state only if the state file exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the state file exists but cannot be read or parsed.
    pub fn load_existing(&self) -> Result<Option<PersistedServiceState>> {
        if !self.path.exists() {
            return Ok(None);
        }
        self.load().map(Some)
    }

    #[must_use]
    pub fn load_or_default(&self) -> PersistedServiceState {
        match self.load() {
            Ok(state) => state,
            Err(error) => {
                log::error!(
                    "Failed to load runtime service state from {}: {error}",
                    self.path.display()
                );
                PersistedServiceState::default()
            }
        }
    }

    /// Persist the runtime service state to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory cannot be created or the file cannot be written.
    pub fn save(&self, state: &PersistedServiceState) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }

    pub fn persist(&self, state: &PersistedServiceState) {
        if let Err(error) = self.save(state) {
            log::error!(
                "Failed to persist runtime service state to {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{PersistedServiceState, ServiceIntent, ServiceStateStore};

    #[test]
    fn test_service_state_store_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".runtime").join("services.json");
        let store = ServiceStateStore::new(path.clone());
        let state = PersistedServiceState {
            cron: Some(ServiceIntent {
                desired_running: false,
            }),
            delivery: Some(ServiceIntent {
                desired_running: true,
            }),
            discord: None,
            websocket: None,
        };

        store.save(&state).unwrap();
        let loaded = store.load().unwrap();

        assert_eq!(loaded, state);
        assert_eq!(store.path(), path.as_path());
    }

    #[test]
    fn test_service_state_store_load_existing_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let store = ServiceStateStore::new(dir.path().join(".runtime").join("services.json"));

        let loaded = store.load_existing().unwrap();

        assert!(loaded.is_none());
    }
}
