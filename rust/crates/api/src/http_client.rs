use std::time::Duration;

use crate::error::ApiError;

const HTTP_PROXY_KEYS: [&str; 2] = ["HTTP_PROXY", "http_proxy"];
const HTTPS_PROXY_KEYS: [&str; 2] = ["HTTPS_PROXY", "https_proxy"];
const NO_PROXY_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];

/// Bounds and default for `CLAW_NET_TIMEOUT_MS` (overall request timeout in
/// milliseconds). The default matches the historical `TimeoutConfig` request
/// timeout of 300 seconds.
const NET_TIMEOUT_MS_MIN: u64 = 5_000;
const NET_TIMEOUT_MS_MAX: u64 = 600_000;
const NET_TIMEOUT_MS_DEFAULT: u64 = 300_000;

/// Parses a raw `CLAW_NET_TIMEOUT_MS` value into a request timeout in
/// milliseconds, clamped to `5_000..=600_000`. Missing, empty, or
/// unparseable values fall back to the default (300 000 ms, the same
/// request timeout the HTTP clients used before this knob existed).
#[must_use]
pub fn parse_net_timeout_ms(raw: Option<&str>) -> u64 {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .map_or(NET_TIMEOUT_MS_DEFAULT, |value| {
            value.clamp(NET_TIMEOUT_MS_MIN, NET_TIMEOUT_MS_MAX)
        })
}

/// Thin env wrapper over [`parse_net_timeout_ms`]: reads
/// `CLAW_NET_TIMEOUT_MS` from the process environment.
#[must_use]
pub fn net_timeout_ms() -> u64 {
    parse_net_timeout_ms(std::env::var("CLAW_NET_TIMEOUT_MS").ok().as_deref())
}

/// Timeout configuration for outbound HTTP requests.
///
/// When set, the `reqwest::Client` will abort requests that take longer
/// than the configured duration and return a timeout error (which is
/// retryable by the existing exponential backoff logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutConfig {
    /// Maximum time to wait for a connection to be established.
    /// Defaults to 30 seconds.
    pub connect_timeout: Duration,
    /// Maximum time for the entire request (including reading the response
    /// body). For streaming responses this is the timeout for the initial
    /// handshake only; the stream itself is governed by SSE parsing.
    /// Defaults to 5 minutes (300 seconds).
    pub request_timeout: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(300),
        }
    }
}

impl TimeoutConfig {
    /// Read timeout settings from the process environment.
    /// - `CLAW_API_CONNECT_TIMEOUT` — connect timeout in seconds
    /// - `CLAW_NET_TIMEOUT_MS` — overall request timeout in milliseconds
    ///   (clamped to 5 000..=600 000; takes precedence over
    ///   `CLAW_API_REQUEST_TIMEOUT` when set)
    /// - `CLAW_API_REQUEST_TIMEOUT` — overall request timeout in seconds
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Pure counterpart of [`TimeoutConfig::from_env`], driven by an
    /// arbitrary key lookup so tests never mutate the process environment.
    fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let connect_timeout = lookup("CLAW_API_CONNECT_TIMEOUT")
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));
        let net_timeout_ms = lookup("CLAW_NET_TIMEOUT_MS").filter(|v| !v.trim().is_empty());
        let request_timeout = if let Some(raw) = net_timeout_ms {
            Duration::from_millis(parse_net_timeout_ms(Some(&raw)))
        } else {
            lookup("CLAW_API_REQUEST_TIMEOUT")
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(Duration::from_secs(300))
        };
        Self {
            connect_timeout,
            request_timeout,
        }
    }

    /// Create from explicit second values (used by config file parsing).
    #[must_use]
    pub fn from_seconds(connect_secs: u64, request_secs: u64) -> Self {
        Self {
            connect_timeout: Duration::from_secs(connect_secs),
            request_timeout: Duration::from_secs(request_secs),
        }
    }
}

/// Snapshot of the proxy-related environment variables that influence the
/// outbound HTTP client. Captured up front so callers can inspect, log, and
/// test the resolved configuration without re-reading the process environment.
///
/// When `proxy_url` is set it acts as a single catch-all proxy for both
/// HTTP and HTTPS traffic, taking precedence over the per-scheme fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyConfig {
    pub http_proxy: Option<String>,
    pub https_proxy: Option<String>,
    pub no_proxy: Option<String>,
    /// Optional unified proxy URL that applies to both HTTP and HTTPS.
    /// When set, this takes precedence over `http_proxy` and `https_proxy`.
    pub proxy_url: Option<String>,
}

impl ProxyConfig {
    /// Read proxy settings from the live process environment, honouring both
    /// the upper- and lower-case spellings used by curl, git, and friends.
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Create a proxy configuration from a single URL that applies to both
    /// HTTP and HTTPS traffic. This is the config-file alternative to setting
    /// `HTTP_PROXY` and `HTTPS_PROXY` environment variables separately.
    #[must_use]
    pub fn from_proxy_url(url: impl Into<String>) -> Self {
        Self {
            proxy_url: Some(url.into()),
            ..Self::default()
        }
    }

    fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        Self {
            http_proxy: first_non_empty(&HTTP_PROXY_KEYS, &mut lookup),
            https_proxy: first_non_empty(&HTTPS_PROXY_KEYS, &mut lookup),
            no_proxy: first_non_empty(&NO_PROXY_KEYS, &mut lookup),
            proxy_url: None,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proxy_url.is_none() && self.http_proxy.is_none() && self.https_proxy.is_none()
    }
}

/// Build a `reqwest::Client` that honours the standard `HTTP_PROXY`,
/// `HTTPS_PROXY`, and `NO_PROXY` environment variables. When no proxy is
/// configured the client behaves identically to `reqwest::Client::new()`.
pub fn build_http_client() -> Result<reqwest::Client, ApiError> {
    build_http_client_with_opts(&ProxyConfig::from_env(), &TimeoutConfig::from_env())
}

/// Infallible counterpart to [`build_http_client`] for constructors that
/// historically returned `Self` rather than `Result<Self, _>`. When the proxy
/// configuration is malformed we fall back to a default client so that
/// callers retain the previous behaviour and the failure surfaces on the
/// first outbound request instead of at construction time.
#[must_use]
pub fn build_http_client_or_default() -> reqwest::Client {
    build_http_client_with_opts(&ProxyConfig::from_env(), &TimeoutConfig::from_env())
        .unwrap_or_else(|_| {
            reqwest::Client::builder()
                .user_agent("clawd-rust-tools/0.1")
                .build()
                .expect("default client with user_agent should always succeed")
        })
}

/// Build a `reqwest::Client` from an explicit [`ProxyConfig`]. Used by tests
/// and by callers that want to override process-level environment lookups.
///
/// When `config.proxy_url` is set it overrides the per-scheme `http_proxy`
/// and `https_proxy` fields and is registered as both an HTTP and HTTPS
/// proxy so a single value can route every outbound request.
pub fn build_http_client_with(config: &ProxyConfig) -> Result<reqwest::Client, ApiError> {
    build_http_client_with_opts(config, &TimeoutConfig::from_env())
}

/// Build a `reqwest::Client` from explicit [`ProxyConfig`] and [`TimeoutConfig`].
/// Used by callers that want to control both proxy routing and request timing.
pub fn build_http_client_with_opts(
    config: &ProxyConfig,
    timeout: &TimeoutConfig,
) -> Result<reqwest::Client, ApiError> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .user_agent("clawd-rust-tools/0.1")
        .connect_timeout(timeout.connect_timeout)
        .timeout(timeout.request_timeout);

    let no_proxy = config
        .no_proxy
        .as_deref()
        .and_then(reqwest::NoProxy::from_string);

    let (http_proxy_url, https_url) = match config.proxy_url.as_deref() {
        Some(unified) => (Some(unified), Some(unified)),
        None => (config.http_proxy.as_deref(), config.https_proxy.as_deref()),
    };

    if let Some(url) = https_url {
        let mut proxy = reqwest::Proxy::https(url)?;
        if let Some(filter) = no_proxy.clone() {
            proxy = proxy.no_proxy(Some(filter));
        }
        builder = builder.proxy(proxy);
    }

    if let Some(url) = http_proxy_url {
        let mut proxy = reqwest::Proxy::http(url)?;
        if let Some(filter) = no_proxy.clone() {
            proxy = proxy.no_proxy(Some(filter));
        }
        builder = builder.proxy(proxy);
    }

    Ok(builder.build()?)
}

fn first_non_empty<F>(keys: &[&str], lookup: &mut F) -> Option<String>
where
    F: FnMut(&str) -> Option<String>,
{
    keys.iter()
        .find_map(|key| lookup(key).filter(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{build_http_client_with, build_http_client_with_opts, ProxyConfig, TimeoutConfig};

    fn config_from_map(pairs: &[(&str, &str)]) -> ProxyConfig {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        ProxyConfig::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn proxy_config_is_empty_when_no_env_vars_are_set() {
        let config = config_from_map(&[]);
        assert!(config.is_empty());
        assert_eq!(config, ProxyConfig::default());
    }

    #[test]
    fn proxy_config_reads_uppercase_http_https_and_no_proxy() {
        let pairs = [
            ("HTTP_PROXY", "http://proxy.internal:3128"),
            ("HTTPS_PROXY", "http://secure.internal:3129"),
            ("NO_PROXY", "localhost,127.0.0.1,.corp"),
        ];
        let config = config_from_map(&pairs);
        assert_eq!(
            config.http_proxy.as_deref(),
            Some("http://proxy.internal:3128")
        );
        assert_eq!(
            config.https_proxy.as_deref(),
            Some("http://secure.internal:3129")
        );
        assert_eq!(
            config.no_proxy.as_deref(),
            Some("localhost,127.0.0.1,.corp")
        );
        assert!(!config.is_empty());
    }

    #[test]
    fn proxy_config_falls_back_to_lowercase_keys() {
        let pairs = [
            ("http_proxy", "http://lower.internal:3128"),
            ("https_proxy", "http://lower-secure.internal:3129"),
            ("no_proxy", ".lower"),
        ];
        let config = config_from_map(&pairs);
        assert_eq!(
            config.http_proxy.as_deref(),
            Some("http://lower.internal:3128")
        );
        assert_eq!(
            config.https_proxy.as_deref(),
            Some("http://lower-secure.internal:3129")
        );
        assert_eq!(config.no_proxy.as_deref(), Some(".lower"));
    }

    #[test]
    fn proxy_config_prefers_uppercase_over_lowercase_when_both_set() {
        let pairs = [
            ("HTTP_PROXY", "http://upper.internal:3128"),
            ("http_proxy", "http://lower.internal:3128"),
        ];
        let config = config_from_map(&pairs);
        assert_eq!(
            config.http_proxy.as_deref(),
            Some("http://upper.internal:3128")
        );
    }

    #[test]
    fn proxy_config_treats_empty_strings_as_unset() {
        let pairs = [("HTTP_PROXY", ""), ("http_proxy", "")];
        let config = config_from_map(&pairs);
        assert!(config.http_proxy.is_none());
    }

    #[test]
    fn build_http_client_succeeds_when_no_proxy_is_configured() {
        let config = ProxyConfig::default();
        let result = build_http_client_with(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_http_client_succeeds_with_valid_http_and_https_proxies() {
        let config = ProxyConfig {
            http_proxy: Some("http://proxy.internal:3128".to_string()),
            https_proxy: Some("http://secure.internal:3129".to_string()),
            no_proxy: Some("localhost,127.0.0.1".to_string()),
            proxy_url: None,
        };
        let result = build_http_client_with(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_http_client_returns_http_error_for_invalid_proxy_url() {
        let config = ProxyConfig {
            http_proxy: None,
            https_proxy: Some("not a url".to_string()),
            no_proxy: None,
            proxy_url: None,
        };
        let result = build_http_client_with(&config);
        let error = result.expect_err("invalid proxy URL must be reported as a build failure");
        assert!(
            matches!(error, crate::error::ApiError::Http(_)),
            "expected ApiError::Http for invalid proxy URL, got: {error:?}"
        );
    }

    #[test]
    fn from_proxy_url_sets_unified_field_and_leaves_per_scheme_empty() {
        let config = ProxyConfig::from_proxy_url("http://unified.internal:3128");
        assert_eq!(
            config.proxy_url.as_deref(),
            Some("http://unified.internal:3128")
        );
        assert!(config.http_proxy.is_none());
        assert!(config.https_proxy.is_none());
        assert!(!config.is_empty());
    }

    #[test]
    fn build_http_client_succeeds_with_unified_proxy_url() {
        let config = ProxyConfig {
            proxy_url: Some("http://unified.internal:3128".to_string()),
            no_proxy: Some("localhost".to_string()),
            ..ProxyConfig::default()
        };
        let result = build_http_client_with(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn proxy_url_takes_precedence_over_per_scheme_fields() {
        let config = ProxyConfig {
            http_proxy: Some("http://per-scheme.internal:1111".to_string()),
            https_proxy: Some("http://per-scheme.internal:2222".to_string()),
            no_proxy: None,
            proxy_url: Some("http://unified.internal:3128".to_string()),
        };
        let result = build_http_client_with(&config);
        assert!(result.is_ok());
    }

    #[test]
    fn build_http_client_returns_error_for_invalid_unified_proxy_url() {
        let config = ProxyConfig::from_proxy_url("not a url");
        let result = build_http_client_with(&config);
        assert!(
            matches!(result, Err(crate::error::ApiError::Http(_))),
            "invalid unified proxy URL should fail: {result:?}"
        );
    }

    #[test]
    fn timeout_config_defaults() {
        let config = TimeoutConfig::default();
        assert_eq!(config.connect_timeout, std::time::Duration::from_secs(30));
        assert_eq!(config.request_timeout, std::time::Duration::from_secs(300));
    }

    #[test]
    fn timeout_config_from_seconds() {
        let config = TimeoutConfig::from_seconds(10, 60);
        assert_eq!(config.connect_timeout, std::time::Duration::from_secs(10));
        assert_eq!(config.request_timeout, std::time::Duration::from_secs(60));
    }

    #[test]
    fn build_http_client_with_custom_timeouts() {
        let config = ProxyConfig::default();
        let timeout = TimeoutConfig::from_seconds(5, 120);
        let result = build_http_client_with_opts(&config, &timeout);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_net_timeout_ms_defaults_when_missing_or_invalid() {
        assert_eq!(super::parse_net_timeout_ms(None), 300_000);
        assert_eq!(super::parse_net_timeout_ms(Some("")), 300_000);
        assert_eq!(super::parse_net_timeout_ms(Some("   ")), 300_000);
        assert_eq!(super::parse_net_timeout_ms(Some("not-a-number")), 300_000);
        assert_eq!(super::parse_net_timeout_ms(Some("-5")), 300_000);
        assert_eq!(super::parse_net_timeout_ms(Some("12.5")), 300_000);
    }

    #[test]
    fn parse_net_timeout_ms_accepts_and_clamps_values() {
        assert_eq!(super::parse_net_timeout_ms(Some("120000")), 120_000);
        assert_eq!(super::parse_net_timeout_ms(Some(" 60000 ")), 60_000);
        assert_eq!(super::parse_net_timeout_ms(Some("1")), 5_000);
        assert_eq!(super::parse_net_timeout_ms(Some("0")), 5_000);
        assert_eq!(super::parse_net_timeout_ms(Some("999999999")), 600_000);
        assert_eq!(super::parse_net_timeout_ms(Some("5000")), 5_000);
        assert_eq!(super::parse_net_timeout_ms(Some("600000")), 600_000);
    }

    fn timeout_from_map(pairs: &[(&str, &str)]) -> TimeoutConfig {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        TimeoutConfig::from_lookup(|key| map.get(key).cloned())
    }

    #[test]
    fn timeout_config_lookup_honours_claw_net_timeout_ms() {
        let config = timeout_from_map(&[("CLAW_NET_TIMEOUT_MS", "45000")]);
        assert_eq!(
            config.request_timeout,
            std::time::Duration::from_millis(45_000)
        );
        assert_eq!(config.connect_timeout, std::time::Duration::from_secs(30));
    }

    #[test]
    fn timeout_config_lookup_prefers_net_timeout_ms_over_request_seconds() {
        let config = timeout_from_map(&[
            ("CLAW_NET_TIMEOUT_MS", "45000"),
            ("CLAW_API_REQUEST_TIMEOUT", "10"),
        ]);
        assert_eq!(
            config.request_timeout,
            std::time::Duration::from_millis(45_000)
        );
    }

    #[test]
    fn timeout_config_lookup_falls_back_to_request_seconds_when_ms_unset() {
        let config = timeout_from_map(&[("CLAW_API_REQUEST_TIMEOUT", "10")]);
        assert_eq!(config.request_timeout, std::time::Duration::from_secs(10));

        let default_config = timeout_from_map(&[]);
        assert_eq!(
            default_config.request_timeout,
            std::time::Duration::from_secs(300)
        );
    }
}
