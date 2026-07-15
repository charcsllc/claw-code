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
    ///
    /// Precedence (lowest to highest): built-in defaults < config file <
    /// `CLAW_MA_*_MODEL` env vars < CLI flags — callers apply flags after
    /// this returns, so they win over everything.
    #[must_use]
    pub fn load(cwd: &Path) -> Self {
        let path = cwd.join(".claw").join("multiagent.json");
        let from_file = std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
        apply_model_env_overrides(from_file, |key| std::env::var(key).ok())
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
            if !credentials_present_for_model(model, provider) {
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

/// Applies per-tier `CLAW_MA_*_MODEL` env overrides on top of a loaded
/// catalog. Takes the lookup as a closure so tests never mutate process
/// env (parallel tests share it); blank values are ignored.
#[must_use]
pub fn apply_model_env_overrides(
    mut catalog: ModelCatalog,
    lookup: impl Fn(&str) -> Option<String>,
) -> ModelCatalog {
    for (key, slot) in [
        ("CLAW_MA_SIMPLE_MODEL", &mut catalog.simple),
        ("CLAW_MA_MEDIUM_MODEL", &mut catalog.medium),
        ("CLAW_MA_COMPLEX_MODEL", &mut catalog.complex),
        ("CLAW_MA_DIRECTOR_MODEL", &mut catalog.director),
        ("CLAW_MA_SUPERVISOR_MODEL", &mut catalog.supervisor),
    ] {
        if let Some(value) = lookup(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                *slot = trimmed.to_string();
            }
        }
    }
    catalog
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

/// Saved subscription login: the runtime keeps OAuth credentials under the
/// user config home (`CLAW_CONFIG_HOME` or `~/.claw`) in `settings.json`.
/// This crate must not depend on `runtime`, so probe the same file directly.
fn anthropic_saved_auth_present() -> bool {
    let config_home = std::env::var_os("CLAW_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".claw"))
        });
    let Some(config_home) = config_home else {
        return false;
    };
    std::fs::read_to_string(config_home.join("settings.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .is_some_and(|settings| {
            settings.get("oauth").is_some_and(|oauth| !oauth.is_null())
                || settings.get("provider").is_some_and(|p| !p.is_null())
        })
}

/// A provider is usable with an API key, a subscription token, saved
/// credentials on disk (Anthropic subscription login), or (for the
/// OpenAI-compatible family) a local Ollama host / DashScope key.
#[must_use]
pub fn provider_credentials_present(kind: ProviderKind) -> bool {
    credentials_present_for_model("", kind)
}

/// Like [`provider_credentials_present`], but within the OpenAI-compatible
/// family it matches the credential to the concrete model: a DashScope key
/// must not "validate" a `gpt-*` tier that would then die mid-build.
#[must_use]
pub fn credentials_present_for_model(model: &str, kind: ProviderKind) -> bool {
    match kind {
        ProviderKind::Anthropic => {
            env_non_empty("ANTHROPIC_API_KEY")
                || env_non_empty("ANTHROPIC_AUTH_TOKEN")
                // Subscription accounts log in once and keep credentials on
                // disk (`~/.claw/settings.json`, `oauth` section) with no env
                // var set — refusing them here would block exactly the users
                // the cost-ceiling docs target.
                || anthropic_saved_auth_present()
        }
        ProviderKind::Xai => env_non_empty("XAI_API_KEY"),
        ProviderKind::OpenAi => {
            // A custom endpoint routes any model in the family.
            if env_non_empty("OPENAI_BASE_URL") || env_non_empty("OLLAMA_HOST") {
                return true;
            }
            let lower = model.to_ascii_lowercase();
            if lower.starts_with("qwen") || lower.contains("dashscope") {
                env_non_empty("DASHSCOPE_API_KEY")
            } else if lower.starts_with("gpt") || lower.starts_with("o1") || lower.starts_with("o3")
            {
                env_non_empty("OPENAI_API_KEY")
            } else {
                env_non_empty("OPENAI_API_KEY") || env_non_empty("DASHSCOPE_API_KEY")
            }
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
    fn env_overrides_replace_tiers_and_ignore_blanks() {
        let catalog = apply_model_env_overrides(ModelCatalog::default(), |key| match key {
            "CLAW_MA_SIMPLE_MODEL" => Some("qwen-turbo".to_string()),
            "CLAW_MA_SUPERVISOR_MODEL" => Some("  gpt-4o  ".to_string()),
            "CLAW_MA_MEDIUM_MODEL" => Some("   ".to_string()),
            _ => None,
        });
        assert_eq!(catalog.simple, "qwen-turbo");
        assert_eq!(catalog.supervisor, "gpt-4o", "values are trimmed");
        // Blank values and unset vars keep the previous assignment.
        assert_eq!(catalog.medium, DEFAULT_MEDIUM);
        assert_eq!(catalog.complex, DEFAULT_COMPLEX);
        assert_eq!(catalog.director, DEFAULT_COMPLEX);
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
