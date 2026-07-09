//! Model catalog: maps task complexity tiers to concrete models, across any
//! provider claw supports (Anthropic, OpenAI, xAI, DashScope, Ollama).
//!
//! A model is usable when its provider's credential is present (API key env
//! var, subscription token, or a local Ollama host). The catalog validates
//! this up front so the run fails fast with a clear message instead of
//! halfway through a build.

use std::path::Path;

use api::{detect_provider_kind, resolve_model_alias, ProviderKind};
use serde::{Deserialize, Serialize};

use crate::contracts::Complexity;

/// Default tier assignments. Overridable via `.claw/multiagent.json` or CLI.
const DEFAULT_SIMPLE: &str = "claude-haiku-4-5";
const DEFAULT_MEDIUM: &str = "claude-sonnet-4-6";
const DEFAULT_COMPLEX: &str = "claude-opus-4-6";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCatalog {
    /// Simple tasks → small/fast model.
    pub simple: String,
    /// Medium tasks → intermediate model.
    pub medium: String,
    /// Complex tasks → premium model.
    pub complex: String,
    /// Director / Subdirector / Architects (planning quality matters most).
    pub director: String,
    /// Supervisor + fixer: the spec requires a superior model here.
    pub supervisor: String,
}

impl Default for ModelCatalog {
    fn default() -> Self {
        Self {
            simple: DEFAULT_SIMPLE.to_string(),
            medium: DEFAULT_MEDIUM.to_string(),
            complex: DEFAULT_COMPLEX.to_string(),
            director: DEFAULT_COMPLEX.to_string(),
            supervisor: DEFAULT_COMPLEX.to_string(),
        }
    }
}

impl ModelCatalog {
    /// Loads overrides from `<cwd>/.claw/multiagent.json` when present.
    #[must_use]
    pub fn load(cwd: &Path) -> Self {
        let path = cwd.join(".claw").join("multiagent.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn model_for(&self, complexity: Complexity) -> &str {
        match complexity {
            Complexity::Simple => &self.simple,
            Complexity::Medium => &self.medium,
            Complexity::Complex => &self.complex,
        }
    }

    /// Every distinct model the run may use.
    #[must_use]
    pub fn all_models(&self) -> Vec<&str> {
        let mut models = vec![
            self.simple.as_str(),
            self.medium.as_str(),
            self.complex.as_str(),
            self.director.as_str(),
            self.supervisor.as_str(),
        ];
        models.sort_unstable();
        models.dedup();
        models
    }

    /// Fails fast when a tier's provider has no credential configured.
    pub fn validate_credentials(&self) -> Result<(), String> {
        let mut missing: Vec<String> = Vec::new();
        for model in self.all_models() {
            let provider = provider_for_model(model);
            if !provider_credentials_present(provider) {
                missing.push(format!(
                    "{model} → {} (set {})",
                    provider_label(provider),
                    credential_hint(provider)
                ));
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "missing provider credentials for:\n  {}",
                missing.join("\n  ")
            ))
        }
    }
}

#[must_use]
pub fn provider_for_model(model: &str) -> ProviderKind {
    detect_provider_kind(&resolve_model_alias(model))
}

#[must_use]
pub fn provider_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::Xai => "xAI",
        ProviderKind::OpenAi => "OpenAI-compatible",
    }
}

fn env_non_empty(key: &str) -> bool {
    std::env::var(key).is_ok_and(|value| !value.trim().is_empty())
}

/// A provider is usable with an API key, a subscription token, or (for the
/// OpenAI-compatible family) a local Ollama host / DashScope key.
#[must_use]
pub fn provider_credentials_present(kind: ProviderKind) -> bool {
    match kind {
        ProviderKind::Anthropic => {
            env_non_empty("ANTHROPIC_API_KEY") || env_non_empty("ANTHROPIC_AUTH_TOKEN")
        }
        ProviderKind::Xai => env_non_empty("XAI_API_KEY"),
        ProviderKind::OpenAi => {
            env_non_empty("OPENAI_API_KEY")
                || env_non_empty("DASHSCOPE_API_KEY")
                || env_non_empty("OLLAMA_HOST")
                || env_non_empty("OPENAI_BASE_URL")
        }
    }
}

fn credential_hint(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN",
        ProviderKind::Xai => "XAI_API_KEY",
        ProviderKind::OpenAi => "OPENAI_API_KEY, DASHSCOPE_API_KEY, OLLAMA_HOST or OPENAI_BASE_URL",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalog_maps_tiers() {
        let catalog = ModelCatalog::default();
        assert_eq!(catalog.model_for(Complexity::Simple), DEFAULT_SIMPLE);
        assert_eq!(catalog.model_for(Complexity::Medium), DEFAULT_MEDIUM);
        assert_eq!(catalog.model_for(Complexity::Complex), DEFAULT_COMPLEX);
    }

    #[test]
    fn all_models_deduplicates() {
        let catalog = ModelCatalog::default();
        // complex, director and supervisor share the premium default.
        assert_eq!(catalog.all_models().len(), 3);
    }

    #[test]
    fn mixed_provider_catalog_detects_providers() {
        assert_eq!(
            provider_for_model("claude-sonnet-4-6"),
            ProviderKind::Anthropic
        );
        assert_eq!(provider_for_model("gpt-4o"), ProviderKind::OpenAi);
        assert_eq!(provider_for_model("grok-3"), ProviderKind::Xai);
        assert_eq!(provider_for_model("qwen-plus"), ProviderKind::OpenAi);
    }

    #[test]
    fn catalog_loads_overrides_from_json() {
        let dir = std::env::temp_dir().join(format!(
            "multiagent-catalog-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join(".claw")).expect("dir");
        std::fs::write(
            dir.join(".claw").join("multiagent.json"),
            r#"{"simple":"qwen-turbo","complex":"gpt-4o"}"#,
        )
        .expect("write");
        let catalog = ModelCatalog::load(&dir);
        assert_eq!(catalog.simple, "qwen-turbo");
        assert_eq!(catalog.complex, "gpt-4o");
        // Unspecified tiers keep defaults.
        assert_eq!(catalog.medium, DEFAULT_MEDIUM);
        let _ = std::fs::remove_dir_all(dir);
    }
}
