//! Provider presets: one-command configuration for every supported backend
//! (`/provider use zhipu <token>`) and the startup glue that makes the
//! provider saved in `~/.claw/settings.json` actually take effect.
//!
//! Historically `/setup` persisted `provider.{kind,apiKey,baseUrl}` but
//! nothing consumed it: the API clients read only environment variables, so
//! the wizard's promise was empty. [`apply_saved_provider_settings`] closes
//! that gap by exporting the saved credentials as env vars at startup —
//! with the environment always winning over the file, so `ANTHROPIC_API_KEY=x
//! claw` still behaves as expected.

use runtime::{save_user_provider_settings, ConfigLoader, RuntimeProviderConfig};

/// A supported provider preset: which env vars carry its credentials and
/// the defaults that make `use <preset> <key>` a one-liner.
pub(crate) struct ProviderPreset {
    /// The `provider.kind` value persisted in settings.json.
    pub kind: &'static str,
    /// Human-readable label for `/provider` output.
    pub label: &'static str,
    /// Env var that receives the API key/token (empty = keyless, e.g. ollama).
    pub key_env: &'static str,
    /// Env var that receives the base URL (empty = provider has none).
    pub base_url_env: &'static str,
    /// Base URL applied when the user does not pass one (empty = none).
    pub default_base_url: &'static str,
    /// Suggested default model (persisted unless the user names one).
    pub default_model: &'static str,
}

/// Every preset `/provider use` understands. The Chinese coding providers
/// (zhipu/kimi) speak the Anthropic protocol, so their tokens ride
/// `ANTHROPIC_AUTH_TOKEN` + `ANTHROPIC_BASE_URL`; deepseek is
/// OpenAI-compatible.
pub(crate) const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        kind: "anthropic",
        label: "Anthropic",
        key_env: "ANTHROPIC_API_KEY",
        base_url_env: "ANTHROPIC_BASE_URL",
        default_base_url: "",
        default_model: "",
    },
    ProviderPreset {
        kind: "zhipu",
        label: "Zhipu / Z.ai (GLM)",
        key_env: "ANTHROPIC_AUTH_TOKEN",
        base_url_env: "ANTHROPIC_BASE_URL",
        default_base_url: "https://api.z.ai/api/anthropic",
        default_model: "glm-4.6",
    },
    ProviderPreset {
        kind: "kimi",
        label: "Moonshot / Kimi",
        key_env: "ANTHROPIC_AUTH_TOKEN",
        base_url_env: "ANTHROPIC_BASE_URL",
        default_base_url: "https://api.moonshot.ai/anthropic",
        default_model: "kimi-k2-0905-preview",
    },
    ProviderPreset {
        kind: "deepseek",
        label: "DeepSeek",
        key_env: "OPENAI_API_KEY",
        base_url_env: "OPENAI_BASE_URL",
        default_base_url: "https://api.deepseek.com",
        default_model: "deepseek-chat",
    },
    ProviderPreset {
        kind: "dashscope",
        label: "Alibaba DashScope (Qwen)",
        key_env: "DASHSCOPE_API_KEY",
        base_url_env: "",
        default_base_url: "",
        default_model: "qwen-max",
    },
    ProviderPreset {
        kind: "openai",
        label: "OpenAI",
        key_env: "OPENAI_API_KEY",
        base_url_env: "OPENAI_BASE_URL",
        default_base_url: "",
        default_model: "",
    },
    ProviderPreset {
        kind: "xai",
        label: "xAI / Grok",
        key_env: "XAI_API_KEY",
        base_url_env: "XAI_BASE_URL",
        default_base_url: "",
        default_model: "grok",
    },
    ProviderPreset {
        kind: "ollama",
        label: "Ollama (local, keyless)",
        key_env: "",
        base_url_env: "OLLAMA_HOST",
        default_base_url: "http://127.0.0.1:11434",
        default_model: "",
    },
];

pub(crate) fn preset_for(kind: &str) -> Option<&'static ProviderPreset> {
    let normalized = kind.trim().to_ascii_lowercase();
    // Common aliases users will reasonably try.
    let normalized = match normalized.as_str() {
        "z.ai" | "zai" | "glm" => "zhipu",
        "moonshot" => "kimi",
        "qwen" | "alibaba" => "dashscope",
        "grok" => "xai",
        other => other,
    };
    PROVIDER_PRESETS
        .iter()
        .find(|preset| preset.kind == normalized)
}

fn env_is_set(name: &str) -> bool {
    !name.is_empty()
        && std::env::var(name)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

/// Exports a provider's credentials as env vars. With `force` false (startup
/// path) an already-set env var always wins over the saved file; with
/// `force` true (`/provider use` in-session) the new choice takes effect
/// immediately.
pub(crate) fn apply_provider_env(
    preset: &ProviderPreset,
    api_key: Option<&str>,
    base_url: Option<&str>,
    force: bool,
) {
    if !preset.key_env.is_empty() {
        if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
            if force || !env_is_set(preset.key_env) {
                std::env::set_var(preset.key_env, key);
            }
        }
    }
    if !preset.base_url_env.is_empty() {
        let url = base_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(preset.default_base_url);
        if !url.is_empty() && (force || !env_is_set(preset.base_url_env)) {
            std::env::set_var(preset.base_url_env, url);
        }
    }
}

/// Startup glue: reads the provider saved in the merged settings and exports
/// it to the environment so the API clients (which are env-driven) see it.
/// Env vars already present always win. Silent on every failure — a broken
/// settings file must not stop the CLI from starting (it is reported by
/// /doctor and the config loader elsewhere).
pub(crate) fn apply_saved_provider_settings() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(config) = ConfigLoader::default_for(&cwd).load() else {
        return;
    };
    let provider: &RuntimeProviderConfig = config.provider();
    let Some(kind) = provider.kind() else {
        return;
    };
    let Some(preset) = preset_for(kind) else {
        return;
    };
    apply_provider_env(preset, provider.api_key(), provider.base_url(), false);
}

/// Handles the `/provider` REPL command. Returns the message to print.
pub(crate) fn handle_provider_command(args: Option<&str>) -> String {
    let args = args.unwrap_or("").trim();
    let mut tokens = args.split_whitespace();
    match tokens.next() {
        None | Some("show") => render_provider_show(),
        Some("list") => render_provider_list(),
        Some("clear") => match runtime::clear_user_provider_settings() {
            Ok(()) => "Provider\n  Action           clear\n  Status           ok\n  Note             saved provider removed; env vars (if any) still apply".to_string(),
            Err(error) => format!("Provider\n  Error            {error}"),
        },
        Some("use") => {
            let Some(kind) = tokens.next() else {
                return format!("Provider\n  Error            'use' expects a provider\n{}", provider_usage());
            };
            let Some(preset) = preset_for(kind) else {
                return format!(
                    "Provider\n  Error            unknown provider '{kind}'\n{}",
                    provider_usage()
                );
            };
            let key = tokens.next().map(ToString::to_string);
            let model = tokens.next().map(ToString::to_string);
            if preset.key_env.is_empty() {
                // Keyless providers (ollama): the "key" slot, if present,
                // is actually a base URL override.
                let base_override = key.clone();
                apply_provider_env(preset, None, base_override.as_deref(), true);
                let base = base_override.unwrap_or_else(|| preset.default_base_url.to_string());
                match save_user_provider_settings(preset.kind, "", Some(&base), model.as_deref()) {
                    Ok(()) => {}
                    Err(error) => return format!("Provider\n  Error            {error}"),
                }
                return format!(
                    "Provider\n  Action           use\n  Status           ok\n  Provider         {} ({})\n  Base URL         {}\n  Applied          now + persisted (~/.claw/settings.json)",
                    preset.label, preset.kind, base
                );
            }
            let Some(key) = key.filter(|key| !key.trim().is_empty()) else {
                return format!(
                    "Provider\n  Error            'use {}' expects an API key/token\n{}",
                    preset.kind,
                    provider_usage()
                );
            };
            let model_to_save = model.clone().or_else(|| {
                (!preset.default_model.is_empty()).then(|| preset.default_model.to_string())
            });
            let base_url = (!preset.default_base_url.is_empty())
                .then_some(preset.default_base_url);
            if let Err(error) =
                save_user_provider_settings(preset.kind, &key, base_url, model_to_save.as_deref())
            {
                return format!("Provider\n  Error            {error}");
            }
            apply_provider_env(preset, Some(&key), base_url, true);
            format!(
                "Provider\n  Action           use\n  Status           ok\n  Provider         {} ({})\n  Credential       {} = {}\n  Base URL         {}\n  Model            {}\n  Applied          now + persisted (~/.claw/settings.json, 0600)",
                preset.label,
                preset.kind,
                preset.key_env,
                mask_key(&key),
                base_url.unwrap_or("provider default"),
                model_to_save.as_deref().unwrap_or("(unchanged)")
            )
        }
        Some(other) => format!(
            "Provider\n  Error            unknown action '{other}'\n{}",
            provider_usage()
        ),
    }
}

fn provider_usage() -> String {
    let kinds = PROVIDER_PRESETS
        .iter()
        .map(|preset| preset.kind)
        .collect::<Vec<_>>()
        .join("|");
    format!(
        "  Usage            /provider [show|list]\n                   /provider use <{kinds}> <api-key> [model]\n                   /provider use ollama [base-url] [model]\n                   /provider clear"
    )
}

fn render_provider_list() -> String {
    let mut out = String::from("Provider\n  Presets:\n");
    for preset in PROVIDER_PRESETS {
        out.push_str(&format!(
            "    {:<10} {} — {}{}\n",
            preset.kind,
            preset.label,
            if preset.key_env.is_empty() {
                "keyless".to_string()
            } else {
                preset.key_env.to_string()
            },
            if preset.default_base_url.is_empty() {
                String::new()
            } else {
                format!(" @ {}", preset.default_base_url)
            }
        ));
    }
    out.push_str(&provider_usage());
    out
}

fn render_provider_show() -> String {
    let saved = std::env::current_dir()
        .ok()
        .and_then(|cwd| ConfigLoader::default_for(&cwd).load().ok())
        .map(|config| config.provider().clone());
    let mut out = String::from("Provider\n");
    match saved {
        Some(provider) if provider.kind().is_some() => {
            let kind = provider.kind().unwrap_or("?");
            let label = preset_for(kind).map_or(kind, |preset| preset.label);
            out.push_str(&format!("  Saved            {label} ({kind})\n"));
            if let Some(url) = provider.base_url() {
                out.push_str(&format!("  Base URL         {url}\n"));
            }
            if let Some(model) = provider.model() {
                out.push_str(&format!("  Model            {model}\n"));
            }
            if let Some(key) = provider.api_key() {
                out.push_str(&format!("  Key              {}\n", mask_key(key)));
            }
        }
        _ => out.push_str("  Saved            (none)\n"),
    }
    // Live environment view: what the API clients will actually use.
    out.push_str("  Active env:\n");
    for env in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "DASHSCOPE_API_KEY",
        "XAI_API_KEY",
        "OLLAMA_HOST",
    ] {
        if let Ok(value) = std::env::var(env) {
            if !value.trim().is_empty() {
                let shown = if env.ends_with("URL") || env == "OLLAMA_HOST" {
                    value
                } else {
                    mask_key(&value)
                };
                out.push_str(&format!("    {env} = {shown}\n"));
            }
        }
    }
    out.push_str(&provider_usage());
    out
}

/// Masks a credential to its last four characters (char-safe).
fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 4 {
        return "****".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("****{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_resolve_by_kind_and_alias() {
        assert_eq!(preset_for("zhipu").expect("zhipu").kind, "zhipu");
        assert_eq!(preset_for("z.ai").expect("alias").kind, "zhipu");
        assert_eq!(preset_for("GLM").expect("case+alias").kind, "zhipu");
        assert_eq!(preset_for("qwen").expect("qwen").kind, "dashscope");
        assert_eq!(preset_for("moonshot").expect("moonshot").kind, "kimi");
        assert!(preset_for("nope").is_none());
    }

    #[test]
    fn zhipu_and_kimi_ride_the_anthropic_protocol() {
        for kind in ["zhipu", "kimi"] {
            let preset = preset_for(kind).expect(kind);
            assert_eq!(preset.key_env, "ANTHROPIC_AUTH_TOKEN");
            assert_eq!(preset.base_url_env, "ANTHROPIC_BASE_URL");
            assert!(!preset.default_base_url.is_empty());
        }
    }

    #[test]
    fn mask_key_is_char_safe_and_short() {
        assert_eq!(mask_key("abc"), "****");
        assert_eq!(mask_key("sk-ant-123456"), "****3456");
        // Multibyte: must not panic and must keep the last 4 chars.
        assert_eq!(mask_key("clave€€€€"), "****€€€€");
    }

    #[test]
    fn usage_mentions_every_preset() {
        let usage = provider_usage();
        for preset in PROVIDER_PRESETS {
            assert!(usage.contains(preset.kind), "usage lists {}", preset.kind);
        }
    }
}
