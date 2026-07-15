#![allow(clippy::cast_possible_truncation)]
#![allow(dead_code)]
use std::future::Future;
use std::pin::Pin;

use runtime::format_usd;
use serde::Serialize;
use serde_json::Value;
use telemetry::AnalyticsEvent;

use crate::error::ApiError;
use crate::types::{MessageRequest, MessageResponse, Usage};

pub mod anthropic;
pub mod openai_compat;

/// A retryable request failure about to be waited out, surfaced so the CLI
/// can tell the user why the turn is stalled (a rate-limited request can
/// legally back off for minutes and otherwise looks like a hang).
pub struct RetryNotice {
    pub attempt: u32,
    pub max_retries: u32,
    pub delay: std::time::Duration,
    pub error: String,
}

type RetryNotifierFn = dyn Fn(&RetryNotice) + Send + Sync;

static RETRY_NOTIFIER: std::sync::OnceLock<Box<RetryNotifierFn>> = std::sync::OnceLock::new();

/// Installs a process-global observer for retryable request failures.
/// First installation wins; later calls are ignored (the CLI installs it
/// once at interactive startup — print/JSON modes install nothing so
/// machine-readable output stays clean).
pub fn set_retry_notifier(notifier: impl Fn(&RetryNotice) + Send + Sync + 'static) {
    let _ = RETRY_NOTIFIER.set(Box::new(notifier));
}

/// Called by provider retry loops right before sleeping out the backoff.
pub(crate) fn notify_retry(notice: &RetryNotice) {
    if let Some(notifier) = RETRY_NOTIFIER.get() {
        notifier(notice);
    }
}

/// Builds the shared `message_usage` analytics event with the full
/// input/output/cache token breakdown consumed by `claw-dashboard`.
/// Used by every provider that traces usage.
pub(crate) fn message_usage_event(
    model: &str,
    request_id: Option<&str>,
    usage: &Usage,
) -> AnalyticsEvent {
    let cost_usd = usage.estimated_cost_usd(model).total_cost_usd();
    AnalyticsEvent::new(telemetry::API_NAMESPACE, telemetry::MESSAGE_USAGE_ACTION)
        .with_property("model", Value::String(model.to_string()))
        .with_property(
            "request_id",
            request_id.map_or(Value::Null, |id| Value::String(id.to_string())),
        )
        .with_property("input_tokens", Value::from(usage.input_tokens))
        .with_property("output_tokens", Value::from(usage.output_tokens))
        .with_property(
            "cache_creation_input_tokens",
            Value::from(usage.cache_creation_input_tokens),
        )
        .with_property(
            "cache_read_input_tokens",
            Value::from(usage.cache_read_input_tokens),
        )
        .with_property("total_tokens", Value::from(usage.total_tokens()))
        .with_property("estimated_cost_usd", Value::String(format_usd(cost_usd)))
        // Full-precision cost for consumers that aggregate across many
        // requests (the formatted string above rounds to 4 decimals).
        .with_property("estimated_cost_usd_value", Value::from(cost_usd))
}

#[allow(dead_code)]
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

#[allow(dead_code)]
pub trait Provider {
    type Stream;

    fn send_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, MessageResponse>;

    fn stream_message<'a>(
        &'a self,
        request: &'a MessageRequest,
    ) -> ProviderFuture<'a, Self::Stream>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProviderKind {
    Anthropic,
    Xai,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderMetadata {
    pub provider: ProviderKind,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelTokenLimit {
    pub max_output_tokens: u32,
    pub context_window_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderWireProtocol {
    AnthropicMessages,
    OpenAiChatCompletions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFeatureSupport {
    Supported,
    Unsupported,
    PassthroughAsTool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilityReport {
    pub provider: ProviderKind,
    pub wire_protocol: ProviderWireProtocol,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
    pub tool_calls: ProviderFeatureSupport,
    pub streaming: ProviderFeatureSupport,
    pub streaming_usage: ProviderFeatureSupport,
    pub prompt_cache: ProviderFeatureSupport,
    pub custom_parameters: ProviderFeatureSupport,
    pub reasoning_effort: ProviderFeatureSupport,
    pub reasoning_content_history: ProviderFeatureSupport,
    pub fixed_sampling_reasoning_models: ProviderFeatureSupport,
    pub web_search: ProviderFeatureSupport,
    pub web_fetch: ProviderFeatureSupport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDiagnosticSeverity {
    Info,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderDiagnostic {
    pub code: &'static str,
    pub severity: ProviderDiagnosticSeverity,
    pub message: String,
    pub action: String,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderDiagnostics {
    pub requested_model: String,
    pub resolved_model: String,
    pub provider: ProviderKind,
    pub auth_env: &'static str,
    pub base_url_env: &'static str,
    pub default_base_url: &'static str,
    pub openai_compatible: bool,
    pub reasoning_model: bool,
    pub preserves_reasoning_content_in_history: bool,
    pub strips_tuning_params: bool,
    pub supports_stream_usage: bool,
    pub honors_proxy_env: bool,
    pub supports_extra_body_params: bool,
    pub preserves_slash_model_ids_on_custom_base_url: bool,
}

const MODEL_REGISTRY: &[(&str, ProviderMetadata)] = &[
    (
        "opus",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "sonnet",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "haiku",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "grok",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-3",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-mini",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-3-mini",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "grok-2",
        ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
    ),
    (
        "kimi",
        ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: openai_compat::DEFAULT_DASHSCOPE_BASE_URL,
        },
    ),
    // Zhipu GLM rides the Anthropic protocol (ANTHROPIC_AUTH_TOKEN +
    // ANTHROPIC_BASE_URL → api.z.ai), so its alias routes like a Claude
    // model and the /provider zhipu preset supplies the endpoint.
    (
        "glm",
        ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
    ),
    (
        "deepseek",
        ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
        },
    ),
    (
        "qwen",
        ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: openai_compat::DEFAULT_DASHSCOPE_BASE_URL,
        },
    ),
];

#[must_use]
pub fn resolve_model_alias(model: &str) -> String {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    MODEL_REGISTRY
        .iter()
        .find_map(|(alias, metadata)| {
            (*alias == lower).then_some(match metadata.provider {
                ProviderKind::Anthropic => match *alias {
                    "opus" => "claude-opus-4-7",
                    "sonnet" => "claude-sonnet-4-6",
                    "haiku" => "claude-haiku-4-5-20251213",
                    "glm" => "glm-4.6",
                    _ => trimmed,
                },
                ProviderKind::Xai => match *alias {
                    "grok" | "grok-3" => "grok-3",
                    "grok-mini" | "grok-3-mini" => "grok-3-mini",
                    "grok-2" => "grok-2",
                    _ => trimmed,
                },
                ProviderKind::OpenAi => match *alias {
                    "kimi" => "kimi-k2.5",
                    "deepseek" => "deepseek-chat",
                    "qwen" => "qwen-max",
                    _ => trimmed,
                },
            })
        })
        .map_or_else(|| trimmed.to_string(), ToOwned::to_owned)
}

/// Canonical model IDs reachable through [`resolve_model_alias`]. Kept as a
/// static table so [`resolve_model_fuzzy`] can hand out `&'static str`
/// results; a unit test asserts it stays in sync with the alias resolver.
const CANONICAL_MODEL_IDS: &[&str] = &[
    "claude-opus-4-7",
    "claude-sonnet-4-6",
    "claude-haiku-4-5-20251213",
    "glm-4.6",
    "grok-3",
    "grok-3-mini",
    "grok-2",
    "kimi-k2.5",
    "deepseek-chat",
    "qwen-max",
];

/// Resolves a user-supplied model name case-insensitively against the model
/// registry:
///
/// 1. An exact match on a registry alias (e.g. `opus`, `grok-mini`) or on a
///    canonical model ID wins immediately.
/// 2. Otherwise the input is matched as a substring against the canonical
///    model IDs: exactly one candidate resolves to `Ok`, several candidates
///    return `Err` with the list so the caller can present them, and zero
///    candidates return `Err(vec![])`.
///
/// This function is not wired into request routing; it is a helper for
/// interactive callers (e.g. a CLI model picker) to suggest corrections.
pub fn resolve_model_fuzzy(input: &str) -> Result<&'static str, Vec<&'static str>> {
    let needle = input.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Err(Vec::new());
    }

    // Exact registry alias (resolve_model_alias lower-cases the input the
    // same way, so this mirrors the existing alias behavior).
    if MODEL_REGISTRY.iter().any(|(alias, _)| *alias == needle) {
        let canonical = resolve_model_alias(&needle);
        if let Some(id) = CANONICAL_MODEL_IDS
            .iter()
            .find(|id| **id == canonical.as_str())
        {
            return Ok(id);
        }
    }

    // Exact canonical ID match (case-insensitive).
    if let Some(id) = CANONICAL_MODEL_IDS
        .iter()
        .find(|id| id.eq_ignore_ascii_case(&needle))
    {
        return Ok(id);
    }

    // Substring match against canonical IDs.
    let candidates: Vec<&'static str> = CANONICAL_MODEL_IDS
        .iter()
        .filter(|id| id.to_ascii_lowercase().contains(&needle))
        .copied()
        .collect();
    match candidates.as_slice() {
        [single] => Ok(single),
        _ => Err(candidates),
    }
}

/// Extracts the `exp` (expiry, Unix seconds) claim from a JWT without
/// verifying its signature. Returns `None` for anything that is not a
/// three-part JWT with a base64url-encoded JSON payload carrying a numeric
/// `exp` claim.
#[must_use]
pub fn jwt_expiry_unix(token: &str) -> Option<i64> {
    let mut parts = token.trim().split('.');
    let (_header, payload) = (parts.next()?, parts.next()?);
    let _signature = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let decoded = base64url_decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let exp = claims.get("exp")?;
    exp.as_i64()
        .or_else(|| exp.as_f64().map(|value| value as i64))
}

/// Minimal base64url (RFC 4648 §5) decoder, tolerant of missing padding.
/// Hand-rolled so the API crate does not grow a `base64` dependency for a
/// single claim lookup.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    fn sextet(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    let trimmed = input.trim_end_matches('=');
    // A lone trailing sextet (len % 4 == 1) can never form a whole byte.
    if trimmed.len() % 4 == 1 {
        return None;
    }
    let mut output = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in trimmed.as_bytes() {
        buffer = (buffer << 6) | sextet(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
        }
    }
    Some(output)
}

#[must_use]
pub fn metadata_for_model(model: &str) -> Option<ProviderMetadata> {
    let canonical = resolve_model_alias(model);
    if canonical.starts_with("claude") || canonical.starts_with("anthropic/") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Anthropic,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        });
    }
    if canonical.starts_with("grok") {
        return Some(ProviderMetadata {
            provider: ProviderKind::Xai,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        });
    }
    // Explicit provider-namespaced models (e.g. "openai/gpt-4.1-mini") must
    // route to the correct provider regardless of which auth env vars are set.
    // Without this, detect_provider_kind falls through to the auth-sniffer
    // order and misroutes to Anthropic if ANTHROPIC_API_KEY is present.
    if canonical.starts_with("openai/") || canonical.starts_with("gpt-") {
        return Some(ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
        });
    }
    if canonical.starts_with("local/") {
        return Some(ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
        });
    }
    // Alibaba DashScope compatible-mode endpoint. Routes qwen/* and bare
    // qwen-* model names (qwen-max, qwen-plus, qwen-turbo, qwen-qwq, etc.)
    // to the OpenAI-compat client pointed at DashScope's /compatible-mode/v1.
    // Uses the OpenAi provider kind because DashScope speaks the OpenAI REST
    // shape — only the base URL and auth env var differ.
    if canonical.starts_with("qwen/") || canonical.starts_with("qwen-") {
        return Some(ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: openai_compat::DEFAULT_DASHSCOPE_BASE_URL,
        });
    }
    // Kimi models (kimi-k2.5, kimi-k1.5, etc.) via DashScope compatible-mode.
    // Routes kimi/* and kimi-* model names to DashScope endpoint.
    if canonical.starts_with("kimi/") || canonical.starts_with("kimi-") {
        return Some(ProviderMetadata {
            provider: ProviderKind::OpenAi,
            auth_env: "DASHSCOPE_API_KEY",
            base_url_env: "DASHSCOPE_BASE_URL",
            default_base_url: openai_compat::DEFAULT_DASHSCOPE_BASE_URL,
        });
    }
    None
}

#[must_use]
pub fn strip_provider_prefix(canonical_model: &str) -> String {
    if let Some(pos) = canonical_model.find('/') {
        canonical_model[pos + 1..].to_string()
    } else {
        canonical_model.to_string()
    }
}

#[must_use]
pub fn provider_diagnostics_for_model(model: &str) -> ProviderDiagnostics {
    let resolved_model = resolve_model_alias(model);
    let metadata =
        metadata_for_model(&resolved_model).unwrap_or_else(|| {
            match detect_provider_kind(&resolved_model) {
                ProviderKind::Anthropic => ProviderMetadata {
                    provider: ProviderKind::Anthropic,
                    auth_env: "ANTHROPIC_API_KEY",
                    base_url_env: "ANTHROPIC_BASE_URL",
                    default_base_url: anthropic::DEFAULT_BASE_URL,
                },
                ProviderKind::Xai => ProviderMetadata {
                    provider: ProviderKind::Xai,
                    auth_env: "XAI_API_KEY",
                    base_url_env: "XAI_BASE_URL",
                    default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
                },
                ProviderKind::OpenAi => ProviderMetadata {
                    provider: ProviderKind::OpenAi,
                    auth_env: "OPENAI_API_KEY",
                    base_url_env: "OPENAI_BASE_URL",
                    default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
                },
            }
        });
    let openai_compatible = matches!(metadata.provider, ProviderKind::OpenAi | ProviderKind::Xai);
    let reasoning_model = openai_compatible && openai_compat::is_reasoning_model(&resolved_model);

    ProviderDiagnostics {
        requested_model: model.to_string(),
        resolved_model: resolved_model.clone(),
        provider: metadata.provider,
        auth_env: metadata.auth_env,
        base_url_env: metadata.base_url_env,
        default_base_url: metadata.default_base_url,
        openai_compatible,
        reasoning_model,
        preserves_reasoning_content_in_history: openai_compatible
            && openai_compat::model_requires_reasoning_content_in_history(&resolved_model),
        strips_tuning_params: reasoning_model,
        supports_stream_usage: metadata.provider == ProviderKind::OpenAi
            && metadata.default_base_url == openai_compat::DEFAULT_OPENAI_BASE_URL,
        honors_proxy_env: true,
        supports_extra_body_params: openai_compatible,
        preserves_slash_model_ids_on_custom_base_url: metadata.provider == ProviderKind::OpenAi,
    }
}

fn looks_like_local_openai_model(model: &str) -> bool {
    model.contains(':') || model.contains('.')
}

#[must_use]
pub fn detect_provider_kind(model: &str) -> ProviderKind {
    // OLLAMA_HOST takes priority: if set, route all models through the local
    // OpenAI-compatible endpoint regardless of model name or other env vars.
    if std::env::var_os("OLLAMA_HOST").is_some() {
        return ProviderKind::OpenAi;
    }
    let resolved_model = resolve_model_alias(model);
    if let Some(metadata) = metadata_for_model(&resolved_model) {
        return metadata.provider;
    }
    // When OPENAI_BASE_URL is set and the unknown model name looks like a
    // local server tag (for example `llama3.2` or `qwen2.5-coder:7b`), prefer
    // the OpenAI-compatible endpoint over ambient Anthropic credentials.
    if std::env::var_os("OPENAI_BASE_URL").is_some()
        && looks_like_local_openai_model(&resolved_model)
    {
        return ProviderKind::OpenAi;
    }
    if anthropic::has_auth_from_env_or_saved().unwrap_or(false) {
        return ProviderKind::Anthropic;
    }
    if openai_compat::has_api_key("OPENAI_API_KEY") {
        return ProviderKind::OpenAi;
    }
    if openai_compat::has_api_key("XAI_API_KEY") {
        return ProviderKind::Xai;
    }
    // Last resort: if OPENAI_BASE_URL is set without OPENAI_API_KEY (some
    // local providers like Ollama don't require auth), still route there.
    if std::env::var_os("OPENAI_BASE_URL").is_some() {
        return ProviderKind::OpenAi;
    }
    ProviderKind::Anthropic
}

#[must_use]
pub const fn model_family_identity_for_kind(kind: ProviderKind) -> runtime::ModelFamilyIdentity {
    match kind {
        ProviderKind::Anthropic => runtime::ModelFamilyIdentity::Claude,
        ProviderKind::Xai | ProviderKind::OpenAi => runtime::ModelFamilyIdentity::Generic,
    }
}

#[must_use]
pub fn model_family_identity_for(model: &str) -> runtime::ModelFamilyIdentity {
    model_family_identity_for_kind(detect_provider_kind(model))
}

#[must_use]
pub fn provider_capabilities_for_model(model: &str) -> ProviderCapabilityReport {
    let metadata = metadata_for_model(model).unwrap_or_else(|| {
        let provider = detect_provider_kind(model);
        metadata_for_provider_kind(provider)
    });

    let (
        wire_protocol,
        streaming_usage,
        prompt_cache,
        custom_parameters,
        reasoning_effort,
        reasoning_content_history,
        fixed_sampling_reasoning_models,
    ) = match metadata.provider {
        ProviderKind::Anthropic => (
            ProviderWireProtocol::AnthropicMessages,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Supported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Unsupported,
        ),
        ProviderKind::Xai => (
            ProviderWireProtocol::OpenAiChatCompletions,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Supported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Supported,
        ),
        ProviderKind::OpenAi => (
            ProviderWireProtocol::OpenAiChatCompletions,
            ProviderFeatureSupport::Supported,
            ProviderFeatureSupport::Unsupported,
            ProviderFeatureSupport::Supported,
            ProviderFeatureSupport::Supported,
            if openai_compat::model_requires_reasoning_content_in_history(model) {
                ProviderFeatureSupport::Supported
            } else {
                ProviderFeatureSupport::Unsupported
            },
            ProviderFeatureSupport::Supported,
        ),
    };

    ProviderCapabilityReport {
        provider: metadata.provider,
        wire_protocol,
        auth_env: metadata.auth_env,
        base_url_env: metadata.base_url_env,
        default_base_url: metadata.default_base_url,
        tool_calls: ProviderFeatureSupport::Supported,
        streaming: ProviderFeatureSupport::Supported,
        streaming_usage,
        prompt_cache,
        custom_parameters,
        reasoning_effort,
        reasoning_content_history,
        fixed_sampling_reasoning_models,
        web_search: ProviderFeatureSupport::PassthroughAsTool,
        web_fetch: ProviderFeatureSupport::PassthroughAsTool,
    }
}

#[must_use]
pub fn provider_diagnostics_for_request(request: &MessageRequest) -> Vec<ProviderDiagnostic> {
    let capabilities = provider_capabilities_for_model(&request.model);
    let mut diagnostics = Vec::new();

    if request.reasoning_effort.is_some()
        && capabilities.reasoning_effort == ProviderFeatureSupport::Unsupported
    {
        diagnostics.push(ProviderDiagnostic {
            code: "reasoning_effort_unsupported",
            severity: ProviderDiagnosticSeverity::Warning,
            message: format!(
                "{} does not map `reasoning_effort` for model `{}`.",
                provider_label(capabilities.provider),
                request.model
            ),
            action: "Remove `reasoning_effort` or route to an OpenAI-compatible reasoning model such as `openai/o4-mini`.".to_string(),
        });
    }

    if openai_compat::is_reasoning_model(&request.model)
        && has_openai_tuning_parameters(request)
        && capabilities.fixed_sampling_reasoning_models == ProviderFeatureSupport::Supported
    {
        diagnostics.push(ProviderDiagnostic {
            code: "reasoning_model_fixed_sampling",
            severity: ProviderDiagnosticSeverity::Info,
            message: format!(
                "Model `{}` is treated as a fixed-sampling reasoning model; tuning parameters are omitted before the provider call.",
                request.model
            ),
            action: "Leave temperature/top_p/frequency_penalty/presence_penalty unset for reasoning models to match provider validation rules.".to_string(),
        });
    }

    if openai_compat::model_requires_reasoning_content_in_history(&request.model) {
        diagnostics.push(ProviderDiagnostic {
            code: "deepseek_v4_reasoning_history",
            severity: ProviderDiagnosticSeverity::Info,
            message: format!(
                "Model `{}` requires assistant thinking history to be echoed as `reasoning_content`.",
                request.model
            ),
            action: "Keep prior assistant Thinking blocks in history; the OpenAI-compatible serializer will emit `reasoning_content` for DeepSeek V4 models.".to_string(),
        });
    }

    if declares_tool(request, "web_search") {
        diagnostics.push(web_passthrough_diagnostic(
            "web_search_passthrough_tool",
            "web_search",
            capabilities.provider,
        ));
    }
    if declares_tool(request, "web_fetch") {
        diagnostics.push(web_passthrough_diagnostic(
            "web_fetch_passthrough_tool",
            "web_fetch",
            capabilities.provider,
        ));
    }

    diagnostics
}

#[must_use]
fn metadata_for_provider_kind(provider: ProviderKind) -> ProviderMetadata {
    match provider {
        ProviderKind::Anthropic => ProviderMetadata {
            provider,
            auth_env: "ANTHROPIC_API_KEY",
            base_url_env: "ANTHROPIC_BASE_URL",
            default_base_url: anthropic::DEFAULT_BASE_URL,
        },
        ProviderKind::Xai => ProviderMetadata {
            provider,
            auth_env: "XAI_API_KEY",
            base_url_env: "XAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_XAI_BASE_URL,
        },
        ProviderKind::OpenAi => ProviderMetadata {
            provider,
            auth_env: "OPENAI_API_KEY",
            base_url_env: "OPENAI_BASE_URL",
            default_base_url: openai_compat::DEFAULT_OPENAI_BASE_URL,
        },
    }
}

#[must_use]
const fn provider_label(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "Anthropic",
        ProviderKind::Xai => "xAI",
        ProviderKind::OpenAi => "OpenAI-compatible",
    }
}

#[must_use]
fn has_openai_tuning_parameters(request: &MessageRequest) -> bool {
    request.temperature.is_some()
        || request.top_p.is_some()
        || request.frequency_penalty.is_some()
        || request.presence_penalty.is_some()
}

#[must_use]
fn declares_tool(request: &MessageRequest, tool_name: &str) -> bool {
    request.tools.as_ref().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.name.eq_ignore_ascii_case(tool_name))
    })
}

#[must_use]
fn web_passthrough_diagnostic(
    code: &'static str,
    tool_name: &'static str,
    provider: ProviderKind,
) -> ProviderDiagnostic {
    ProviderDiagnostic {
        code,
        severity: ProviderDiagnosticSeverity::Info,
        message: format!(
            "`{tool_name}` is exposed to {} as a normal function tool, not as a provider-native web capability.",
            provider_label(provider)
        ),
        action: format!(
            "Provide a local `{tool_name}` tool implementation or route through a provider adapter that explicitly supports native web tools."
        ),
    }
}

#[must_use]
pub fn max_tokens_for_model(model: &str) -> u32 {
    let canonical = resolve_model_alias(model);
    let heuristic = if canonical.contains("opus") {
        32_000
    } else {
        64_000
    };

    model_token_limit(model).map_or(heuristic, |limit| heuristic.min(limit.max_output_tokens))
}

/// Returns the effective max output tokens for a model, preferring a plugin
/// override when present. Falls back to [`max_tokens_for_model`] when the
/// override is `None`.
#[must_use]
pub fn max_tokens_for_model_with_override(model: &str, plugin_override: Option<u32>) -> u32 {
    plugin_override.unwrap_or_else(|| max_tokens_for_model(model))
}

#[must_use]
pub fn model_token_limit(model: &str) -> Option<ModelTokenLimit> {
    let canonical = resolve_model_alias(model);
    let base_model = canonical.rsplit('/').next().unwrap_or(canonical.as_str());
    match base_model {
        "claude-opus-4-7" | "claude-opus-4-6" => Some(ModelTokenLimit {
            max_output_tokens: 32_000,
            context_window_tokens: 200_000,
        }),
        "claude-sonnet-4-6" | "claude-haiku-4-5-20251213" => Some(ModelTokenLimit {
            max_output_tokens: 64_000,
            context_window_tokens: 200_000,
        }),
        "grok-3" | "grok-3-mini" => Some(ModelTokenLimit {
            max_output_tokens: 64_000,
            context_window_tokens: 131_072,
        }),
        // GPT-4.1 family via the OpenAI API.
        "gpt-4.1" | "gpt-4.1-mini" | "gpt-4.1-nano" => Some(ModelTokenLimit {
            max_output_tokens: 32_768,
            context_window_tokens: 1_047_576,
        }),
        // GPT-5.4 family via the OpenAI API.
        "gpt-5.4" => Some(ModelTokenLimit {
            max_output_tokens: 128_000,
            context_window_tokens: 1_000_000,
        }),
        "gpt-5.4-mini" | "gpt-5.4-nano" => Some(ModelTokenLimit {
            max_output_tokens: 128_000,
            context_window_tokens: 400_000,
        }),
        // Kimi models via DashScope (Moonshot AI)
        // Source: https://platform.moonshot.cn/docs/intro
        "kimi-k2.5" | "kimi-k1.5" => Some(ModelTokenLimit {
            max_output_tokens: 16_384,
            context_window_tokens: 256_000,
        }),
        "qwen-max" => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 131_072,
        }),
        "qwen-plus" => Some(ModelTokenLimit {
            max_output_tokens: 8_192,
            context_window_tokens: 131_072,
        }),
        _ => None,
    }
}

pub fn preflight_message_request(request: &MessageRequest) -> Result<(), ApiError> {
    let Some(limit) = model_token_limit(&request.model) else {
        return Ok(());
    };

    let estimated_input_tokens = estimate_message_request_input_tokens(request);
    let estimated_total_tokens = estimated_input_tokens.saturating_add(request.max_tokens);
    if estimated_total_tokens > limit.context_window_tokens {
        return Err(ApiError::ContextWindowExceeded {
            model: resolve_model_alias(&request.model),
            estimated_input_tokens,
            requested_output_tokens: request.max_tokens,
            estimated_total_tokens,
            context_window_tokens: limit.context_window_tokens,
        });
    }

    Ok(())
}

fn estimate_message_request_input_tokens(request: &MessageRequest) -> u32 {
    let mut estimate = estimate_serialized_tokens(&request.messages);
    estimate = estimate.saturating_add(estimate_serialized_tokens(&request.system));
    estimate = estimate.saturating_add(estimate_serialized_tokens(&request.tools));
    estimate = estimate.saturating_add(estimate_serialized_tokens(&request.tool_choice));
    estimate
}

fn estimate_serialized_tokens<T: Serialize>(value: &T) -> u32 {
    serde_json::to_vec(value)
        .ok()
        .map_or(0, |bytes| (bytes.len() / 4 + 1) as u32)
}

/// Env var names used by other provider backends. When Anthropic auth
/// resolution fails we sniff these so we can hint the user that their
/// credentials probably belong to a different provider and suggest the
/// model-prefix routing fix that would select it.
const FOREIGN_PROVIDER_ENV_VARS: &[(&str, &str, &str)] = &[
    (
        "OPENAI_API_KEY",
        "OpenAI-compat",
        "prefix your model name with `openai/` (e.g. `--model openai/gpt-4.1-mini`) so prefix routing selects the OpenAI-compatible provider, and set `OPENAI_BASE_URL` if you are pointing at OpenRouter/Ollama/a local server",
    ),
    (
        "XAI_API_KEY",
        "xAI",
        "use an xAI model alias (e.g. `--model grok` or `--model grok-mini`) so the prefix router selects the xAI backend",
    ),
    (
        "DASHSCOPE_API_KEY",
        "Alibaba DashScope",
        "prefix your model name with `qwen/` or `qwen-` (e.g. `--model qwen-plus`) so prefix routing selects the DashScope backend",
    ),
];

/// Check whether an env var is set to a non-empty value either in the real
/// process environment or in the working-directory `.env` file. Mirrors the
/// credential discovery path used by `read_env_non_empty` so the hint text
/// stays truthful when users rely on `.env` instead of a real export.
fn env_or_dotenv_present(key: &str) -> bool {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => true,
        Ok(_) | Err(std::env::VarError::NotPresent) => {
            dotenv_value(key).is_some_and(|value| !value.is_empty())
        }
        Err(_) => false,
    }
}

/// Produce a hint string describing the first foreign provider credential
/// that is present in the environment when Anthropic auth resolution has
/// just failed. Returns `None` when no foreign credential is set, in which
/// case the caller should fall back to the plain `missing_credentials`
/// error without a hint.
pub(crate) fn anthropic_missing_credentials_hint() -> Option<String> {
    for (env_var, provider_label, fix_hint) in FOREIGN_PROVIDER_ENV_VARS {
        if env_or_dotenv_present(env_var) {
            return Some(format!(
                "I see {env_var} is set — if you meant to use the {provider_label} provider, {fix_hint}."
            ));
        }
    }
    None
}

/// Build an Anthropic-specific `MissingCredentials` error, attaching a
/// hint suggesting the probable fix whenever a different provider's
/// credentials are already present in the environment. Anthropic call
/// sites should prefer this helper over `ApiError::missing_credentials`
/// so users who mistyped a model name or forgot the prefix get a useful
/// signal instead of a generic "missing Anthropic credentials" wall.
pub(crate) fn anthropic_missing_credentials() -> ApiError {
    const PROVIDER: &str = "Anthropic";
    const ENV_VARS: &[&str] = &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"];
    match anthropic_missing_credentials_hint() {
        Some(hint) => ApiError::missing_credentials_with_hint(PROVIDER, ENV_VARS, hint),
        None => ApiError::missing_credentials(PROVIDER, ENV_VARS),
    }
}

/// Parse a `.env` file body into key/value pairs using a minimal `KEY=VALUE`
/// grammar. Lines that are blank, start with `#`, or do not contain `=` are
/// ignored. Surrounding double or single quotes are stripped from the value.
/// An optional leading `export ` prefix on the key is also stripped so files
/// shared with shell `source` workflows still parse cleanly.
pub(crate) fn parse_dotenv(content: &str) -> std::collections::HashMap<String, String> {
    let mut values = std::collections::HashMap::new();
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let trimmed_key = raw_key.trim();
        let key = trimmed_key
            .strip_prefix("export ")
            .map_or(trimmed_key, str::trim)
            .to_string();
        if key.is_empty() {
            continue;
        }
        let trimmed_value = raw_value.trim();
        let unquoted = if (trimmed_value.starts_with('"') && trimmed_value.ends_with('"')
            || trimmed_value.starts_with('\'') && trimmed_value.ends_with('\''))
            && trimmed_value.len() >= 2
        {
            &trimmed_value[1..trimmed_value.len() - 1]
        } else {
            trimmed_value
        };
        values.insert(key, unquoted.to_string());
    }
    values
}

/// Load and parse a `.env` file from the given path. Missing files yield
/// `None` instead of an error so callers can use this as a soft fallback.
pub(crate) fn load_dotenv_file(
    path: &std::path::Path,
) -> Option<std::collections::HashMap<String, String>> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse_dotenv(&content))
}

/// Look up `key` in a `.env` file located in the current working directory.
/// Returns `None` when the file is missing, the key is absent, or the value
/// is empty.
pub(crate) fn dotenv_value(key: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let values = load_dotenv_file(&cwd.join(".env"))?;
    values.get(key).filter(|value| !value.is_empty()).cloned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    use serde_json::json;

    use crate::error::ApiError;
    use crate::types::{
        InputContentBlock, InputMessage, MessageRequest, ToolChoice, ToolDefinition,
    };

    use super::{
        anthropic_missing_credentials, anthropic_missing_credentials_hint, detect_provider_kind,
        jwt_expiry_unix, load_dotenv_file, max_tokens_for_model,
        max_tokens_for_model_with_override, model_family_identity_for,
        model_family_identity_for_kind, model_token_limit, parse_dotenv, preflight_message_request,
        provider_capabilities_for_model, provider_diagnostics_for_request, resolve_model_alias,
        resolve_model_fuzzy, ProviderFeatureSupport, ProviderKind, ProviderWireProtocol,
        CANONICAL_MODEL_IDS, MODEL_REGISTRY,
    };

    /// Serializes every test in this module that mutates process-wide
    /// environment variables so concurrent test threads cannot observe
    /// each other's partially-applied state while probing the foreign
    /// provider credential sniffer.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Snapshot-restore guard for a single environment variable. Captures
    /// the original value on construction, applies the requested override
    /// (set or remove), and restores the original on drop so tests leave
    /// the process env untouched even when they panic mid-assertion.
    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var_os(key);
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn resolves_grok_aliases() {
        assert_eq!(resolve_model_alias("grok"), "grok-3");
        assert_eq!(resolve_model_alias("grok-mini"), "grok-3-mini");
        assert_eq!(resolve_model_alias("grok-2"), "grok-2");
    }

    #[test]
    fn detects_provider_from_model_name_first() {
        assert_eq!(detect_provider_kind("grok"), ProviderKind::Xai);
        assert_eq!(
            detect_provider_kind("claude-sonnet-4-6"),
            ProviderKind::Anthropic
        );
    }

    #[test]
    fn maps_provider_kind_to_model_family_identity() {
        // given: each supported provider kind
        let anthropic = ProviderKind::Anthropic;
        let openai = ProviderKind::OpenAi;
        let xai = ProviderKind::Xai;

        // when: converting provider kinds to prompt model family identities
        let anthropic_identity = model_family_identity_for_kind(anthropic);
        let openai_identity = model_family_identity_for_kind(openai);
        let xai_identity = model_family_identity_for_kind(xai);

        // then: Anthropic stays Claude and OpenAI-compatible providers are generic
        assert_eq!(anthropic_identity, runtime::ModelFamilyIdentity::Claude);
        assert_eq!(openai_identity, runtime::ModelFamilyIdentity::Generic);
        assert_eq!(xai_identity, runtime::ModelFamilyIdentity::Generic);
    }

    #[test]
    fn maps_model_name_to_model_family_identity() {
        // given: Anthropic, OpenAI-compatible, and xAI model names
        let claude_model = "claude-opus-4-6";
        let openai_model = "openai/gpt-4.1-mini";
        let xai_model = "grok-3";

        // when: detecting prompt model family identities from model names
        let claude_identity = model_family_identity_for(claude_model);
        let openai_identity = model_family_identity_for(openai_model);
        let xai_identity = model_family_identity_for(xai_model);

        // then: Anthropic stays Claude and OpenAI-compatible providers are generic
        assert_eq!(claude_identity, runtime::ModelFamilyIdentity::Claude);
        assert_eq!(openai_identity, runtime::ModelFamilyIdentity::Generic);
        assert_eq!(xai_identity, runtime::ModelFamilyIdentity::Generic);
    }

    #[test]
    fn provider_capability_matrix_snapshots_openai_compat_differences() {
        let openai = provider_capabilities_for_model("openai/gpt-4.1-mini");
        assert_eq!(openai.provider, ProviderKind::OpenAi);
        assert_eq!(
            openai.wire_protocol,
            ProviderWireProtocol::OpenAiChatCompletions
        );
        assert_eq!(openai.auth_env, "OPENAI_API_KEY");
        assert_eq!(openai.streaming_usage, ProviderFeatureSupport::Supported);
        assert_eq!(openai.reasoning_effort, ProviderFeatureSupport::Supported);
        assert_eq!(openai.web_search, ProviderFeatureSupport::PassthroughAsTool);
        assert_eq!(openai.web_fetch, ProviderFeatureSupport::PassthroughAsTool);

        let deepseek = provider_capabilities_for_model("openai/deepseek-v4-pro");
        assert_eq!(
            deepseek.reasoning_content_history,
            ProviderFeatureSupport::Supported
        );

        let xai = provider_capabilities_for_model("grok-3");
        assert_eq!(xai.provider, ProviderKind::Xai);
        assert_eq!(xai.auth_env, "XAI_API_KEY");
        assert_eq!(xai.reasoning_effort, ProviderFeatureSupport::Unsupported);
        assert_eq!(xai.streaming_usage, ProviderFeatureSupport::Unsupported);

        let anthropic = provider_capabilities_for_model("claude-sonnet-4-6");
        assert_eq!(anthropic.provider, ProviderKind::Anthropic);
        assert_eq!(
            anthropic.wire_protocol,
            ProviderWireProtocol::AnthropicMessages
        );
        assert_eq!(anthropic.prompt_cache, ProviderFeatureSupport::Supported);
        assert_eq!(
            anthropic.custom_parameters,
            ProviderFeatureSupport::Unsupported
        );
    }

    #[test]
    fn provider_diagnostics_explain_deepseek_reasoning_and_web_tool_passthrough() {
        let request = MessageRequest {
            model: "openai/deepseek-v4-pro".to_string(),
            max_tokens: 1024,
            messages: vec![InputMessage::user_text("research this")],
            tools: Some(vec![
                ToolDefinition {
                    name: "web_search".to_string(),
                    description: Some("Search the web".to_string()),
                    input_schema: json!({"type": "object"}),
                },
                ToolDefinition {
                    name: "web_fetch".to_string(),
                    description: Some("Fetch a URL".to_string()),
                    input_schema: json!({"type": "object"}),
                },
            ]),
            stream: true,
            ..Default::default()
        };

        let diagnostics = provider_diagnostics_for_request(&request);
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert!(codes.contains(&"deepseek_v4_reasoning_history"));
        assert!(codes.contains(&"web_search_passthrough_tool"));
        assert!(codes.contains(&"web_fetch_passthrough_tool"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.action.contains("provider adapter")));
    }

    #[test]
    fn provider_diagnostics_warn_for_unsupported_reasoning_effort() {
        let request = MessageRequest {
            model: "grok-3-mini".to_string(),
            max_tokens: 1024,
            messages: vec![InputMessage::user_text("think")],
            reasoning_effort: Some("high".to_string()),
            temperature: Some(0.7),
            ..Default::default()
        };

        let diagnostics = provider_diagnostics_for_request(&request);
        let codes = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert!(codes.contains(&"reasoning_effort_unsupported"));
        assert!(codes.contains(&"reasoning_model_fixed_sampling"));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("does not map `reasoning_effort`")));
    }

    #[test]
    fn openai_namespaced_model_routes_to_openai_not_anthropic() {
        // Regression: "openai/gpt-4.1-mini" was misrouted to Anthropic when
        // ANTHROPIC_API_KEY was set because metadata_for_model returned None
        // and detect_provider_kind fell through to auth-sniffer order.
        // The model prefix must win over env-var presence.
        let kind = super::metadata_for_model("openai/gpt-4.1-mini").map_or_else(
            || detect_provider_kind("openai/gpt-4.1-mini"),
            |m| m.provider,
        );
        assert_eq!(
            kind,
            ProviderKind::OpenAi,
            "openai/ prefix must route to OpenAi regardless of ANTHROPIC_API_KEY"
        );

        // Also cover bare gpt- prefix
        let kind2 = super::metadata_for_model("gpt-4o")
            .map_or_else(|| detect_provider_kind("gpt-4o"), |m| m.provider);
        assert_eq!(kind2, ProviderKind::OpenAi);
    }

    #[test]
    fn local_prefix_routes_to_openai_not_anthropic() {
        let meta = super::metadata_for_model("local/Qwen/Qwen3.6-27B-FP8")
            .expect("local/ prefix must resolve to OpenAI-compatible metadata");
        assert_eq!(meta.provider, ProviderKind::OpenAi);
        assert_eq!(meta.auth_env, "OPENAI_API_KEY");
        assert_eq!(meta.base_url_env, "OPENAI_BASE_URL");

        let kind = detect_provider_kind("local/Qwen/Qwen3.6-27B-FP8");
        assert_eq!(kind, ProviderKind::OpenAi);
    }

    #[test]
    fn qwen_prefix_routes_to_dashscope_not_anthropic() {
        // User request from Discord #clawcode-get-help: web3g wants to use
        // Qwen 3.6 Plus via native Alibaba DashScope API (not OpenRouter,
        // which has lower rate limits). metadata_for_model must route
        // qwen/* and bare qwen-* to the OpenAi provider kind pointed at
        // the DashScope compatible-mode endpoint, regardless of whether
        // ANTHROPIC_API_KEY is present in the environment.
        let meta = super::metadata_for_model("qwen/qwen-max")
            .expect("qwen/ prefix must resolve to DashScope metadata");
        assert_eq!(meta.provider, ProviderKind::OpenAi);
        assert_eq!(meta.auth_env, "DASHSCOPE_API_KEY");
        assert_eq!(meta.base_url_env, "DASHSCOPE_BASE_URL");
        assert!(meta.default_base_url.contains("dashscope.aliyuncs.com"));

        // Bare qwen- prefix also routes
        let meta2 = super::metadata_for_model("qwen-plus")
            .expect("qwen- prefix must resolve to DashScope metadata");
        assert_eq!(meta2.provider, ProviderKind::OpenAi);
        assert_eq!(meta2.auth_env, "DASHSCOPE_API_KEY");

        // detect_provider_kind must agree even if ANTHROPIC_API_KEY is set
        let kind = detect_provider_kind("qwen/qwen3-coder");
        assert_eq!(
            kind,
            ProviderKind::OpenAi,
            "qwen/ prefix must win over auth-sniffer order"
        );
    }

    #[test]
    fn kimi_prefix_routes_to_dashscope() {
        // Kimi models via DashScope (kimi-k2.5, kimi-k1.5, etc.)
        let meta = super::metadata_for_model("kimi-k2.5")
            .expect("kimi-k2.5 must resolve to DashScope metadata");
        assert_eq!(meta.auth_env, "DASHSCOPE_API_KEY");
        assert_eq!(meta.base_url_env, "DASHSCOPE_BASE_URL");
        assert!(meta.default_base_url.contains("dashscope.aliyuncs.com"));
        assert_eq!(meta.provider, ProviderKind::OpenAi);

        // With provider prefix
        let meta2 = super::metadata_for_model("kimi/kimi-k2.5")
            .expect("kimi/kimi-k2.5 must resolve to DashScope metadata");
        assert_eq!(meta2.auth_env, "DASHSCOPE_API_KEY");
        assert_eq!(meta2.provider, ProviderKind::OpenAi);

        // Different kimi variants
        let meta3 = super::metadata_for_model("kimi-k1.5")
            .expect("kimi-k1.5 must resolve to DashScope metadata");
        assert_eq!(meta3.auth_env, "DASHSCOPE_API_KEY");
    }

    #[test]
    fn kimi_alias_resolves_to_kimi_k2_5() {
        assert_eq!(super::resolve_model_alias("kimi"), "kimi-k2.5");
        assert_eq!(super::resolve_model_alias("KIMI"), "kimi-k2.5"); // case insensitive
    }

    #[test]
    fn chinese_provider_aliases_resolve_to_flagship_models() {
        assert_eq!(super::resolve_model_alias("glm"), "glm-4.6");
        assert_eq!(super::resolve_model_alias("deepseek"), "deepseek-chat");
        assert_eq!(super::resolve_model_alias("qwen"), "qwen-max");
        // Full model names pass through untouched.
        assert_eq!(super::resolve_model_alias("glm-4.5-air"), "glm-4.5-air");
    }

    #[test]
    fn provider_diagnostics_explain_openai_compatible_capabilities() {
        let diagnostics = super::provider_diagnostics_for_model("openai/deepseek-v4-pro");

        assert_eq!(diagnostics.provider, ProviderKind::OpenAi);
        assert_eq!(diagnostics.auth_env, "OPENAI_API_KEY");
        assert!(diagnostics.openai_compatible);
        assert!(diagnostics.preserves_reasoning_content_in_history);
        assert!(diagnostics.supports_extra_body_params);
        assert!(diagnostics.honors_proxy_env);
        assert!(diagnostics.preserves_slash_model_ids_on_custom_base_url);
    }

    #[test]
    fn keeps_existing_max_token_heuristic() {
        assert_eq!(max_tokens_for_model("opus"), 32_000);
        assert_eq!(max_tokens_for_model("grok-3"), 64_000);
        assert_eq!(max_tokens_for_model("gpt-5.4"), 64_000);
    }

    #[test]
    fn caps_default_max_tokens_to_openai_model_limits() {
        assert_eq!(max_tokens_for_model("gpt-4.1-mini"), 32_768);
        assert_eq!(max_tokens_for_model("openai/gpt-4.1-mini"), 32_768);
        assert_eq!(max_tokens_for_model("gpt-5.4"), 64_000);
        assert_eq!(max_tokens_for_model("openai/gpt-5.4"), 64_000);
    }

    #[test]
    fn plugin_config_max_output_tokens_overrides_model_default() {
        // given
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("api-plugin-max-tokens-{nanos}"));
        let cwd = root.join("project");
        let home = root.join("home").join(".claw");
        std::fs::create_dir_all(cwd.join(".claw")).expect("project config dir");
        std::fs::create_dir_all(&home).expect("home config dir");
        std::fs::write(
            home.join("settings.json"),
            r#"{
              "plugins": {
                "maxOutputTokens": 12345
              }
            }"#,
        )
        .expect("write plugin settings");

        // when
        let loaded = runtime::ConfigLoader::new(&cwd, &home)
            .load()
            .expect("config should load");
        let plugin_override = loaded.plugins().max_output_tokens();
        let effective = max_tokens_for_model_with_override("claude-opus-4-6", plugin_override);

        // then
        assert_eq!(plugin_override, Some(12345));
        assert_eq!(effective, 12345);
        assert_ne!(effective, max_tokens_for_model("claude-opus-4-6"));

        std::fs::remove_dir_all(root).expect("cleanup temp dir");
    }

    #[test]
    fn max_tokens_for_model_with_override_falls_back_when_plugin_unset() {
        // given
        let plugin_override: Option<u32> = None;

        // when
        let effective = max_tokens_for_model_with_override("claude-opus-4-6", plugin_override);

        // then
        assert_eq!(effective, max_tokens_for_model("claude-opus-4-6"));
        assert_eq!(effective, 32_000);
    }

    #[test]
    fn returns_context_window_metadata_for_supported_models() {
        assert_eq!(
            model_token_limit("claude-sonnet-4-6")
                .expect("claude-sonnet-4-6 should be registered")
                .context_window_tokens,
            200_000
        );
        assert_eq!(
            model_token_limit("grok-mini")
                .expect("grok-mini should resolve to a registered model")
                .context_window_tokens,
            131_072
        );
        assert_eq!(
            model_token_limit("openai/gpt-4.1-mini")
                .expect("openai/gpt-4.1-mini should be registered")
                .context_window_tokens,
            1_047_576
        );
        assert_eq!(
            model_token_limit("gpt-5.4")
                .expect("gpt-5.4 should be registered")
                .context_window_tokens,
            1_000_000
        );
    }

    #[test]
    fn preflight_blocks_requests_that_exceed_the_model_context_window() {
        let request = MessageRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(600_000),
                }],
            }],
            system: Some("Keep the answer short.".to_string()),
            tools: Some(vec![ToolDefinition {
                name: "weather".to_string(),
                description: Some("Fetches weather".to_string()),
                input_schema: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                }),
            }]),
            tool_choice: Some(ToolChoice::Auto),
            stream: true,
            ..Default::default()
        };

        let error = preflight_message_request(&request)
            .expect_err("oversized request should be rejected before the provider call");

        match error {
            ApiError::ContextWindowExceeded {
                model,
                estimated_input_tokens,
                requested_output_tokens,
                estimated_total_tokens,
                context_window_tokens,
            } => {
                assert_eq!(model, "claude-sonnet-4-6");
                assert!(estimated_input_tokens > 136_000);
                assert_eq!(requested_output_tokens, 64_000);
                assert!(estimated_total_tokens > context_window_tokens);
                assert_eq!(context_window_tokens, 200_000);
            }
            other => panic!("expected context-window preflight failure, got {other:?}"),
        }
    }

    #[test]
    fn preflight_blocks_oversized_requests_for_gpt_5_4() {
        let request = MessageRequest {
            model: "gpt-5.4".to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(3_900_000),
                }],
            }],
            system: Some("Keep the answer short.".to_string()),
            tools: None,
            tool_choice: None,
            stream: true,
            ..Default::default()
        };

        let error = preflight_message_request(&request)
            .expect_err("oversized gpt-5.4 request should be rejected before the provider call");

        match error {
            ApiError::ContextWindowExceeded {
                model,
                requested_output_tokens,
                context_window_tokens,
                ..
            } => {
                assert_eq!(model, "gpt-5.4");
                assert_eq!(requested_output_tokens, 64_000);
                assert_eq!(context_window_tokens, 1_000_000);
            }
            other => panic!("expected context-window preflight failure, got {other:?}"),
        }
    }

    #[test]
    fn preflight_skips_unknown_models() {
        let request = MessageRequest {
            model: "unknown-model".to_string(),
            max_tokens: 64_000,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(600_000),
                }],
            }],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            ..Default::default()
        };

        preflight_message_request(&request)
            .expect("models without context metadata should skip the guarded preflight");
    }

    #[test]
    fn returns_context_window_metadata_for_kimi_models() {
        // kimi-k2.5
        let k25_limit =
            model_token_limit("kimi-k2.5").expect("kimi-k2.5 should have token limit metadata");
        assert_eq!(k25_limit.max_output_tokens, 16_384);
        assert_eq!(k25_limit.context_window_tokens, 256_000);

        // kimi-k1.5
        let k15_limit =
            model_token_limit("kimi-k1.5").expect("kimi-k1.5 should have token limit metadata");
        assert_eq!(k15_limit.max_output_tokens, 16_384);
        assert_eq!(k15_limit.context_window_tokens, 256_000);
    }

    #[test]
    fn kimi_alias_resolves_to_kimi_k25_token_limits() {
        // The "kimi" alias resolves to "kimi-k2.5" via resolve_model_alias()
        let alias_limit =
            model_token_limit("kimi").expect("kimi alias should resolve to kimi-k2.5 limits");
        let direct_limit = model_token_limit("kimi-k2.5").expect("kimi-k2.5 should have limits");
        assert_eq!(
            alias_limit.max_output_tokens,
            direct_limit.max_output_tokens
        );
        assert_eq!(
            alias_limit.context_window_tokens,
            direct_limit.context_window_tokens
        );
    }

    #[test]
    fn preflight_blocks_oversized_requests_for_kimi_models() {
        let request = MessageRequest {
            model: "kimi-k2.5".to_string(),
            max_tokens: 16_384,
            messages: vec![InputMessage {
                role: "user".to_string(),
                content: vec![InputContentBlock::Text {
                    text: "x".repeat(1_000_000), // Large input to exceed context window
                }],
            }],
            system: Some("Keep the answer short.".to_string()),
            tools: None,
            tool_choice: None,
            stream: true,
            ..Default::default()
        };

        let error = preflight_message_request(&request)
            .expect_err("oversized request should be rejected for kimi models");

        match error {
            ApiError::ContextWindowExceeded {
                model,
                context_window_tokens,
                ..
            } => {
                assert_eq!(model, "kimi-k2.5");
                assert_eq!(context_window_tokens, 256_000);
            }
            other => panic!("expected context-window preflight failure, got {other:?}"),
        }
    }

    #[test]
    fn parse_dotenv_extracts_keys_handles_comments_quotes_and_export_prefix() {
        // given
        let body = "\
# this is a comment

ANTHROPIC_API_KEY=plain-value
XAI_API_KEY=\"quoted-value\"
OPENAI_API_KEY='single-quoted'
export GROK_API_KEY=exported-value
   PADDED_KEY  =  padded-value  
EMPTY_VALUE=
NO_EQUALS_LINE
";

        // when
        let values = parse_dotenv(body);

        // then
        assert_eq!(
            values.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("plain-value")
        );
        assert_eq!(
            values.get("XAI_API_KEY").map(String::as_str),
            Some("quoted-value")
        );
        assert_eq!(
            values.get("OPENAI_API_KEY").map(String::as_str),
            Some("single-quoted")
        );
        assert_eq!(
            values.get("GROK_API_KEY").map(String::as_str),
            Some("exported-value")
        );
        assert_eq!(
            values.get("PADDED_KEY").map(String::as_str),
            Some("padded-value")
        );
        assert_eq!(values.get("EMPTY_VALUE").map(String::as_str), Some(""));
        assert!(!values.contains_key("NO_EQUALS_LINE"));
        assert!(!values.contains_key("# this is a comment"));
    }

    #[test]
    fn load_dotenv_file_reads_keys_from_disk_and_returns_none_when_missing() {
        // given
        let temp_root = std::env::temp_dir().join(format!(
            "api-dotenv-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&temp_root).expect("create temp dir");
        let env_path = temp_root.join(".env");
        std::fs::write(
            &env_path,
            "ANTHROPIC_API_KEY=secret-from-file\n# comment\nXAI_API_KEY=\"xai-secret\"\n",
        )
        .expect("write .env");
        let missing_path = temp_root.join("does-not-exist.env");

        // when
        let loaded = load_dotenv_file(&env_path).expect("file should load");
        let missing = load_dotenv_file(&missing_path);

        // then
        assert_eq!(
            loaded.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("secret-from-file")
        );
        assert_eq!(
            loaded.get("XAI_API_KEY").map(String::as_str),
            Some("xai-secret")
        );
        assert!(missing.is_none());

        let _ = std::fs::remove_dir_all(&temp_root);
    }

    #[test]
    fn anthropic_missing_credentials_hint_is_none_when_no_foreign_creds_present() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let hint = anthropic_missing_credentials_hint();

        // then
        assert!(
            hint.is_none(),
            "no hint should be produced when every foreign provider env var is absent, got {hint:?}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_hint_detects_openai_api_key_and_recommends_openai_prefix() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("sk-openrouter-varleg"));
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let hint = anthropic_missing_credentials_hint()
            .expect("OPENAI_API_KEY presence should produce a hint");

        // then
        assert!(
            hint.contains("OPENAI_API_KEY is set"),
            "hint should name the detected env var so users recognize it: {hint}"
        );
        assert!(
            hint.contains("OpenAI-compat"),
            "hint should identify the target provider: {hint}"
        );
        assert!(
            hint.contains("openai/"),
            "hint should mention the `openai/` prefix routing fix: {hint}"
        );
        assert!(
            hint.contains("OPENAI_BASE_URL"),
            "hint should mention OPENAI_BASE_URL so OpenRouter users see the full picture: {hint}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_hint_detects_xai_api_key() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _xai = EnvVarGuard::set("XAI_API_KEY", Some("xai-test-key"));
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let hint = anthropic_missing_credentials_hint()
            .expect("XAI_API_KEY presence should produce a hint");

        // then
        assert!(
            hint.contains("XAI_API_KEY is set"),
            "hint should name XAI_API_KEY: {hint}"
        );
        assert!(
            hint.contains("xAI"),
            "hint should identify the xAI provider: {hint}"
        );
        assert!(
            hint.contains("grok"),
            "hint should suggest a grok-prefixed model alias: {hint}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_hint_detects_dashscope_api_key() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("sk-dashscope-test"));

        // when
        let hint = anthropic_missing_credentials_hint()
            .expect("DASHSCOPE_API_KEY presence should produce a hint");

        // then
        assert!(
            hint.contains("DASHSCOPE_API_KEY is set"),
            "hint should name DASHSCOPE_API_KEY: {hint}"
        );
        assert!(
            hint.contains("DashScope"),
            "hint should identify the DashScope provider: {hint}"
        );
        assert!(
            hint.contains("qwen"),
            "hint should suggest a qwen-prefixed model alias: {hint}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_hint_prefers_openai_when_multiple_foreign_creds_set() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("sk-openrouter-varleg"));
        let _xai = EnvVarGuard::set("XAI_API_KEY", Some("xai-test-key"));
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", Some("sk-dashscope-test"));

        // when
        let hint = anthropic_missing_credentials_hint()
            .expect("multiple foreign creds should still produce a hint");

        // then
        assert!(
            hint.contains("OPENAI_API_KEY"),
            "OpenAI should be prioritized because it is the most common misrouting pattern (OpenRouter users), got: {hint}"
        );
        assert!(
            !hint.contains("XAI_API_KEY"),
            "only the first detected provider should be named to keep the hint focused, got: {hint}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_builds_error_with_canonical_env_vars_and_no_hint_when_clean() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", None);
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let error = anthropic_missing_credentials();

        // then
        match &error {
            ApiError::MissingCredentials {
                provider,
                env_vars,
                hint,
            } => {
                assert_eq!(*provider, "Anthropic");
                assert_eq!(*env_vars, &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]);
                assert!(
                    hint.is_none(),
                    "clean environment should not generate a hint, got {hint:?}"
                );
            }
            other => panic!("expected MissingCredentials variant, got {other:?}"),
        }
        let rendered = error.to_string();
        assert!(
            !rendered.contains(" — hint: "),
            "rendered error should be a plain missing-creds message: {rendered}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_builds_error_with_hint_when_openai_key_is_set() {
        // given
        let _lock = env_lock();
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some("sk-openrouter-varleg"));
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let error = anthropic_missing_credentials();

        // then
        match &error {
            ApiError::MissingCredentials {
                provider,
                env_vars,
                hint,
            } => {
                assert_eq!(*provider, "Anthropic");
                assert_eq!(*env_vars, &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]);
                let hint_value = hint.as_deref().expect("hint should be populated");
                assert!(
                    hint_value.contains("OPENAI_API_KEY is set"),
                    "hint should name the detected env var: {hint_value}"
                );
            }
            other => panic!("expected MissingCredentials variant, got {other:?}"),
        }
        let rendered = error.to_string();
        assert!(
            rendered.starts_with("missing Anthropic credentials;"),
            "canonical base message should still lead the rendered error: {rendered}"
        );
        // #754: hint delimiter changed from " — hint: " to "\n" so split_error_hint works
        assert!(
            rendered.contains("I see OPENAI_API_KEY is set"),
            "rendered error should carry the env-driven hint: {rendered}"
        );
        assert!(
            rendered.contains('\n'),
            "rendered error must use newline separator (#754): {rendered}"
        );
    }

    #[test]
    fn anthropic_missing_credentials_hint_ignores_empty_string_values() {
        // given
        let _lock = env_lock();
        // An empty value is semantically equivalent to "not set" for the
        // credential discovery path, so the sniffer must treat it that way
        // to avoid false-positive hints for users who intentionally cleared
        // a stale export with `OPENAI_API_KEY=`.
        let _openai = EnvVarGuard::set("OPENAI_API_KEY", Some(""));
        let _xai = EnvVarGuard::set("XAI_API_KEY", None);
        let _dashscope = EnvVarGuard::set("DASHSCOPE_API_KEY", None);

        // when
        let hint = anthropic_missing_credentials_hint();

        // then
        assert!(
            hint.is_none(),
            "empty env var should not trigger the hint sniffer, got {hint:?}"
        );
    }

    #[test]
    fn openai_base_url_overrides_anthropic_fallback_for_unknown_model() {
        // given — user has OPENAI_BASE_URL + OPENAI_API_KEY but no Anthropic
        // creds, and a model name with no recognized prefix.
        let _lock = env_lock();
        let _base_url = EnvVarGuard::set("OPENAI_BASE_URL", Some("http://127.0.0.1:11434/v1"));
        let _api_key = EnvVarGuard::set("OPENAI_API_KEY", Some("dummy"));
        let _anthropic_key = EnvVarGuard::set("ANTHROPIC_API_KEY", None);
        let _anthropic_token = EnvVarGuard::set("ANTHROPIC_AUTH_TOKEN", None);

        // when
        let provider = detect_provider_kind("qwen2.5-coder:7b");

        // then — should route to OpenAI, not Anthropic
        assert_eq!(
            provider,
            ProviderKind::OpenAi,
            "OPENAI_BASE_URL should win over Anthropic fallback for unknown models"
        );
    }

    // NOTE: a "OPENAI_BASE_URL without OPENAI_API_KEY" test is omitted
    // because workspace-parallel test binaries can race on process env
    // (env_lock only protects within a single binary). The detection logic
    // is covered: OPENAI_BASE_URL alone routes to OpenAi as a last-resort
    // fallback in detect_provider_kind().

    #[test]
    fn canonical_model_ids_stay_in_sync_with_alias_resolver() {
        for (alias, _) in MODEL_REGISTRY {
            let canonical = resolve_model_alias(alias);
            assert!(
                CANONICAL_MODEL_IDS.contains(&canonical.as_str()),
                "alias `{alias}` resolves to `{canonical}`, which is missing \
                 from CANONICAL_MODEL_IDS — update the fuzzy resolver table"
            );
        }
    }

    #[test]
    fn resolve_model_fuzzy_exact_alias_and_id_win() {
        assert_eq!(resolve_model_fuzzy("opus"), Ok("claude-opus-4-7"));
        assert_eq!(resolve_model_fuzzy("OPUS"), Ok("claude-opus-4-7"));
        assert_eq!(resolve_model_fuzzy("  sonnet  "), Ok("claude-sonnet-4-6"));
        // Exact alias wins even though "grok" is a substring of several IDs.
        assert_eq!(resolve_model_fuzzy("grok"), Ok("grok-3"));
        assert_eq!(
            resolve_model_fuzzy("Claude-Opus-4-7"),
            Ok("claude-opus-4-7")
        );
    }

    #[test]
    fn resolve_model_fuzzy_unique_substring_resolves() {
        assert_eq!(resolve_model_fuzzy("k2"), Ok("kimi-k2.5"));
        assert_eq!(resolve_model_fuzzy("DEEP"), Ok("deepseek-chat"));
        assert_eq!(resolve_model_fuzzy("opus-4"), Ok("claude-opus-4-7"));
    }

    #[test]
    fn resolve_model_fuzzy_ambiguous_returns_candidates() {
        let candidates = resolve_model_fuzzy("gro").expect_err("`gro` matches several grok models");
        assert_eq!(candidates, vec!["grok-3", "grok-3-mini", "grok-2"]);

        let claude_candidates =
            resolve_model_fuzzy("claude").expect_err("`claude` matches several models");
        assert!(claude_candidates.len() > 1);
        assert!(claude_candidates.contains(&"claude-opus-4-7"));
    }

    #[test]
    fn resolve_model_fuzzy_no_match_returns_empty() {
        assert_eq!(resolve_model_fuzzy("gpt-99-ultra"), Err(vec![]));
        assert_eq!(resolve_model_fuzzy(""), Err(vec![]));
        assert_eq!(resolve_model_fuzzy("   "), Err(vec![]));
    }

    /// Builds a synthetic (unsigned) JWT from a JSON payload.
    fn synthetic_jwt(payload: &serde_json::Value) -> String {
        fn b64url(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let mut buffer = 0u32;
                for (index, byte) in chunk.iter().enumerate() {
                    buffer |= u32::from(*byte) << (16 - 8 * index);
                }
                let sextets = [
                    (buffer >> 18) & 0x3f,
                    (buffer >> 12) & 0x3f,
                    (buffer >> 6) & 0x3f,
                    buffer & 0x3f,
                ];
                let emit = match chunk.len() {
                    1 => 2,
                    2 => 3,
                    _ => 4,
                };
                for sextet in &sextets[..emit] {
                    out.push(char::from(ALPHABET[*sextet as usize]));
                }
            }
            out
        }
        let header = b64url(br#"{"alg":"none","typ":"JWT"}"#);
        let body = b64url(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    #[test]
    fn jwt_expiry_unix_reads_exp_claim() {
        let token = synthetic_jwt(&json!({"sub": "user-1", "exp": 1_764_000_000_i64}));
        assert_eq!(jwt_expiry_unix(&token), Some(1_764_000_000));
    }

    #[test]
    fn jwt_expiry_unix_handles_missing_exp_and_non_jwt_input() {
        let no_exp = synthetic_jwt(&json!({"sub": "user-1"}));
        assert_eq!(jwt_expiry_unix(&no_exp), None);

        assert_eq!(jwt_expiry_unix("sk-ant-not-a-jwt"), None);
        assert_eq!(jwt_expiry_unix(""), None);
        assert_eq!(jwt_expiry_unix("only.two"), None);
        assert_eq!(jwt_expiry_unix("a.b.c.d"), None);
        // Payload is not valid base64url JSON.
        assert_eq!(jwt_expiry_unix("head.!!invalid!!.sig"), None);
    }

    #[test]
    fn jwt_expiry_unix_accepts_padded_payload() {
        let token = synthetic_jwt(&json!({"exp": 42}));
        // Re-insert padding on the payload segment; decoders must tolerate it.
        let mut parts = token.split('.');
        let header = parts.next().unwrap();
        let payload = parts.next().unwrap();
        let padded = format!("{header}.{payload}==.sig");
        assert_eq!(jwt_expiry_unix(&padded), Some(42));
    }
}
