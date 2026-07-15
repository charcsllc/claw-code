//! Model listing over plain HTTP.
//!
//! Resolves the active provider endpoint from the environment with the same
//! env-first precedence the rest of the crate uses (Anthropic credentials
//! win over OpenAI-compatible ones), queries the provider's models endpoint,
//! and returns the sorted model IDs. Synchronous by design so the CLI can
//! call it from non-async command handlers.

use std::time::Duration;

const MODELS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How to authenticate the models request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelsAuth {
    /// Anthropic `x-api-key` header (plus `anthropic-version`).
    XApiKey(String),
    /// `Authorization: Bearer <token>` (Anthropic auth tokens and every
    /// OpenAI-compatible provider).
    Bearer {
        token: String,
        /// Anthropic Bearer requests still need the `anthropic-version`
        /// header; OpenAI-compatible ones must not send it.
        anthropic: bool,
    },
}

/// Fully resolved models endpoint: URL plus credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelsEndpoint {
    url: String,
    auth: ModelsAuth,
}

/// Resolves the active models endpoint from an environment lookup.
///
/// Mirrors the crate's env-first provider precedence:
/// - `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` (+ optional
///   `ANTHROPIC_BASE_URL`) → `GET {base}/v1/models` with `x-api-key` or
///   `Authorization: Bearer`.
/// - otherwise `OPENAI_API_KEY` (+ optional `OPENAI_BASE_URL`) →
///   `GET {base}/models` with `Authorization: Bearer` (`OPENAI_BASE_URL`
///   conventionally already ends in `/v1`).
fn resolve_models_endpoint<F>(mut lookup: F) -> Result<ModelsEndpoint, String>
where
    F: FnMut(&str) -> Option<String>,
{
    let non_empty = |value: Option<String>| -> Option<String> {
        value
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    let anthropic_api_key = non_empty(lookup("ANTHROPIC_API_KEY"));
    let anthropic_auth_token = non_empty(lookup("ANTHROPIC_AUTH_TOKEN"));
    if anthropic_api_key.is_some() || anthropic_auth_token.is_some() {
        let base_url = non_empty(lookup("ANTHROPIC_BASE_URL"))
            .unwrap_or_else(|| crate::providers::anthropic::DEFAULT_BASE_URL.to_string());
        let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
        // Match AuthSource precedence: an API key wins over a bearer token.
        let auth = match (anthropic_api_key, anthropic_auth_token) {
            (Some(api_key), _) => ModelsAuth::XApiKey(api_key),
            (None, Some(token)) => ModelsAuth::Bearer {
                token,
                anthropic: true,
            },
            (None, None) => unreachable!("guarded by the is_some() check above"),
        };
        return Ok(ModelsEndpoint { url, auth });
    }

    if let Some(api_key) = non_empty(lookup("OPENAI_API_KEY")) {
        let base_url = non_empty(lookup("OPENAI_BASE_URL")).unwrap_or_else(|| {
            crate::providers::openai_compat::DEFAULT_OPENAI_BASE_URL.to_string()
        });
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        return Ok(ModelsEndpoint {
            url,
            auth: ModelsAuth::Bearer {
                token: api_key,
                anthropic: false,
            },
        });
    }

    Err(String::from(
        "no provider credentials found in the environment; set ANTHROPIC_API_KEY / \
         ANTHROPIC_AUTH_TOKEN (optionally ANTHROPIC_BASE_URL) or OPENAI_API_KEY \
         (optionally OPENAI_BASE_URL) to list models",
    ))
}

/// Parses a models-list response body into model IDs.
///
/// Both the Anthropic (`GET /v1/models`) and OpenAI (`GET /models`) protocols
/// answer with `{"data": [{"id": "..."}]}`, so one parser covers both.
/// Malformed bodies and entries without a string `id` yield no IDs rather
/// than an error. IDs are returned sorted and deduplicated.
#[must_use]
pub fn parse_models_response(body: &str) -> Vec<String> {
    let mut ids: Vec<String> = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("data").and_then(|data| data.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Lists the models available on the active provider endpoint, resolved
/// env-first (see [`resolve_models_endpoint`]). Blocking: wraps the crate's
/// async `reqwest` client in a local single-threaded Tokio runtime, with a
/// 10-second request timeout. Returns the sorted model IDs.
pub fn list_models_via_http() -> Result<Vec<String>, String> {
    let endpoint = resolve_models_endpoint(|key| std::env::var(key).ok())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start blocking HTTP runtime: {error}"))?;
    runtime.block_on(async move {
        let client = crate::http_client::build_http_client_with_opts(
            &crate::http_client::ProxyConfig::from_env(),
            &crate::http_client::TimeoutConfig {
                connect_timeout: MODELS_REQUEST_TIMEOUT,
                request_timeout: MODELS_REQUEST_TIMEOUT,
            },
        )
        .map_err(|error| format!("failed to build HTTP client: {error}"))?;

        let mut request = client.get(&endpoint.url);
        request = match &endpoint.auth {
            ModelsAuth::XApiKey(api_key) => request
                .header("x-api-key", api_key)
                .header("anthropic-version", telemetry::DEFAULT_ANTHROPIC_VERSION),
            ModelsAuth::Bearer { token, anthropic } => {
                let mut request = request.bearer_auth(token);
                if *anthropic {
                    request =
                        request.header("anthropic-version", telemetry::DEFAULT_ANTHROPIC_VERSION);
                }
                request
            }
        };

        let response = request
            .send()
            .await
            .map_err(|error| format!("models request to {} failed: {error}", endpoint.url))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| format!("failed to read models response body: {error}"))?;
        if !status.is_success() {
            let snippet: String = body.chars().take(300).collect();
            return Err(format!(
                "models request to {} returned HTTP {}: {snippet}",
                endpoint.url,
                status.as_u16()
            ));
        }
        Ok(parse_models_response(&body))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{parse_models_response, resolve_models_endpoint, ModelsAuth};

    fn lookup_from(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn parse_models_response_handles_anthropic_format() {
        let body = r#"{
            "data": [
                {"type": "model", "id": "claude-opus-4-6", "display_name": "Claude Opus"},
                {"type": "model", "id": "claude-haiku-4-5"}
            ],
            "has_more": false
        }"#;
        assert_eq!(
            parse_models_response(body),
            vec!["claude-haiku-4-5", "claude-opus-4-6"]
        );
    }

    #[test]
    fn parse_models_response_handles_openai_format() {
        let body = r#"{
            "object": "list",
            "data": [
                {"id": "gpt-4.1-mini", "object": "model", "owned_by": "openai"},
                {"id": "gpt-4.1", "object": "model"},
                {"id": "gpt-4.1", "object": "model"}
            ]
        }"#;
        // Sorted and deduplicated.
        assert_eq!(parse_models_response(body), vec!["gpt-4.1", "gpt-4.1-mini"]);
    }

    #[test]
    fn parse_models_response_tolerates_malformed_bodies() {
        assert!(parse_models_response("").is_empty());
        assert!(parse_models_response("not json").is_empty());
        assert!(parse_models_response("{}").is_empty());
        assert!(parse_models_response(r#"{"data": "nope"}"#).is_empty());
        assert!(parse_models_response(r#"{"data": [{"name": "no-id"}, {"id": 7}]}"#).is_empty());
    }

    #[test]
    fn resolves_anthropic_api_key_endpoint() {
        // Fixture credential built at runtime so repository scanners never
        // see a literal token-shaped string.
        let key = format!("sk-ant-{}", "a".repeat(24));
        let endpoint = resolve_models_endpoint(lookup_from(&[("ANTHROPIC_API_KEY", &key)]))
            .expect("anthropic key resolves");
        assert_eq!(endpoint.url, "https://api.anthropic.com/v1/models");
        assert_eq!(endpoint.auth, ModelsAuth::XApiKey(key));
    }

    #[test]
    fn resolves_anthropic_bearer_endpoint_with_custom_base_url() {
        let token = format!("oat-{}", "b".repeat(32));
        let endpoint = resolve_models_endpoint(lookup_from(&[
            ("ANTHROPIC_AUTH_TOKEN", &token),
            ("ANTHROPIC_BASE_URL", "https://gateway.example.com/"),
        ]))
        .expect("anthropic bearer resolves");
        assert_eq!(endpoint.url, "https://gateway.example.com/v1/models");
        assert_eq!(
            endpoint.auth,
            ModelsAuth::Bearer {
                token,
                anthropic: true,
            }
        );
    }

    #[test]
    fn anthropic_credentials_take_precedence_over_openai() {
        let anthropic_key = format!("sk-ant-{}", "c".repeat(24));
        let openai_key = format!("sk-{}", "d".repeat(24));
        let endpoint = resolve_models_endpoint(lookup_from(&[
            ("ANTHROPIC_API_KEY", &anthropic_key),
            ("OPENAI_API_KEY", &openai_key),
        ]))
        .expect("resolves");
        assert_eq!(endpoint.url, "https://api.anthropic.com/v1/models");
    }

    #[test]
    fn resolves_openai_endpoint_when_only_openai_is_configured() {
        let openai_key = format!("sk-{}", "e".repeat(24));
        let endpoint = resolve_models_endpoint(lookup_from(&[
            ("OPENAI_API_KEY", &openai_key),
            ("OPENAI_BASE_URL", "http://127.0.0.1:11434/v1"),
        ]))
        .expect("openai resolves");
        assert_eq!(endpoint.url, "http://127.0.0.1:11434/v1/models");
        assert_eq!(
            endpoint.auth,
            ModelsAuth::Bearer {
                token: openai_key,
                anthropic: false,
            }
        );
    }

    #[test]
    fn openai_endpoint_defaults_to_public_base_url() {
        let openai_key = format!("sk-{}", "f".repeat(24));
        let endpoint = resolve_models_endpoint(lookup_from(&[("OPENAI_API_KEY", &openai_key)]))
            .expect("openai resolves");
        assert_eq!(endpoint.url, "https://api.openai.com/v1/models");
    }

    #[test]
    fn errors_when_no_credentials_are_configured() {
        let error =
            resolve_models_endpoint(lookup_from(&[])).expect_err("no credentials must not resolve");
        assert!(error.contains("ANTHROPIC_API_KEY"));
        assert!(error.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn empty_and_whitespace_credentials_are_treated_as_unset() {
        let error = resolve_models_endpoint(lookup_from(&[
            ("ANTHROPIC_API_KEY", "  "),
            ("OPENAI_API_KEY", ""),
        ]))
        .expect_err("blank credentials must not resolve");
        assert!(error.contains("no provider credentials"));
    }
}
