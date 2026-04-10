use std::path::Path;

use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::resilience::profile::AuthProfile;

use super::{AppConfig, config_path, read_config, write_config};

/// LLM provider configuration
///
/// First entry in the `llm` array is the primary profile.
/// Additional entries provide auth rotation (`api_key` + `base_url`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LLMConfig {
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub context_window: usize,
}

impl Default for LLMConfig {
    fn default() -> Self {
        Self {
            model: "labs-leanstral-2603".into(),
            base_url: "https://api.mistral.ai/v1/".into(),
            api_key: "$MISTRAL_API_KEY".into(),
            context_window: 256_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmSettings {
    pub profiles: Vec<LLMConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            profiles: vec![LLMConfig::default()],
            fallback_models: Vec::new(),
        }
    }
}

impl LLMConfig {
    /// Convert to an `AuthProfile` for the resilience layer.
    #[must_use]
    pub fn to_auth_profile(&self) -> AuthProfile {
        AuthProfile::new(self.api_key.clone(), Some(self.base_url.clone()))
    }
}

fn editable_config() -> Result<(std::path::PathBuf, AppConfig)> {
    let path = config_path();
    let config: AppConfig = read_config(&path)?;
    ensure!(
        !config.llm.profiles.is_empty(),
        "missing or invalid `llm` array in {}",
        path.display()
    );
    Ok((path, config))
}

fn write_editable_config(path: &Path, config: &AppConfig) -> Result<()> {
    ensure!(
        !config.llm.profiles.is_empty(),
        "missing or invalid `llm` array in {}",
        path.display()
    );
    write_config(path, config)
}

fn find_model(profiles: &[LLMConfig], model: &str) -> Result<usize> {
    profiles
        .iter()
        .position(|entry| entry.model == model)
        .ok_or_else(|| anyhow!("model {model:?} not found"))
}

/// Add an LLM profile to the app config.
///
/// # Errors
///
/// Returns an error if the config file is unreadable or malformed.
pub fn add_llm(config: &LLMConfig) -> Result<()> {
    let (path, mut app_config) = editable_config()?;
    app_config.llm.profiles.push(config.clone());
    write_editable_config(&path, &app_config)
}

/// Remove an LLM profile by model name
///
/// # Errors
///
/// Returns error if the model is not found or it is the only entry.
pub fn remove_llm(model: &str) -> Result<()> {
    let (path, mut app_config) = editable_config()?;
    let idx = find_model(&app_config.llm.profiles, model)?;
    ensure!(
        app_config.llm.profiles.len() > 1,
        "cannot remove the only LLM profile"
    );
    app_config.llm.profiles.remove(idx);
    write_editable_config(&path, &app_config)
}

/// Set an LLM profile as primary (move to index 0) by model name
///
/// # Errors
///
/// Returns error if the model is not found.
pub fn set_primary_llm(model: &str) -> Result<()> {
    let (path, mut app_config) = editable_config()?;
    let idx = find_model(&app_config.llm.profiles, model)?;
    if idx != 0 {
        let entry = app_config.llm.profiles.remove(idx);
        app_config.llm.profiles.insert(0, entry);
    }
    write_editable_config(&path, &app_config)
}

/// List all LLM profiles, primary first.
///
/// # Errors
///
/// Returns an error if the config file cannot be read or the `llm` array is invalid.
pub fn list_llm_profiles() -> Result<Vec<LLMConfig>> {
    let (_, app_config) = editable_config()?;
    Ok(app_config.llm.profiles)
}
