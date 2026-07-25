//! Property-based invariants for the api crate's pure parsing helpers.
//!
//! Cases are capped at 256 per property to keep the suite fast; the goal is
//! broad input coverage for clamp/normalization guarantees, not exhaustive
//! fuzzing.

use api::{
    jwt_expiry_unix, parse_net_timeout_ms, parse_retry_after, resolve_model_alias,
    resolve_model_fuzzy,
};
use proptest::prelude::*;

/// Inputs that exercise all three `resolve_model_fuzzy` paths: arbitrary
/// noise, model-ish slugs, registry aliases (mixed case), and substrings of
/// canonical model IDs.
fn model_input_strategy() -> impl Strategy<Value = String> {
    let canonical_substring = (
        prop_oneof![
            Just("claude-opus-4-7"),
            Just("claude-sonnet-4-6"),
            Just("claude-haiku-4-5-20251213"),
            Just("glm-4.6"),
            Just("grok-3-mini"),
            Just("kimi-k2.5"),
            Just("deepseek-chat"),
            Just("qwen-max"),
        ],
        any::<(usize, usize)>(),
        any::<bool>(),
    )
        .prop_map(|(id, (start, len), uppercase)| {
            let chars: Vec<char> = id.chars().collect();
            let start = start % chars.len();
            let len = 1 + len % (chars.len() - start);
            let slice: String = chars[start..start + len].iter().collect();
            if uppercase {
                slice.to_ascii_uppercase()
            } else {
                slice
            }
        });
    prop_oneof![
        "\\PC{0,24}",
        "[a-zA-Z0-9.\\-]{1,16}",
        prop_oneof![
            Just("opus".to_string()),
            Just("SONNET".to_string()),
            Just("  haiku  ".to_string()),
            Just("glm".to_string()),
            Just("grok-mini".to_string()),
            Just("Kimi".to_string()),
            Just("deepseek".to_string()),
            Just("qwen".to_string()),
        ],
        canonical_substring,
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `parse_retry_after` never yields a delay outside 1..=120 seconds,
    /// whatever the header carries.
    #[test]
    fn parse_retry_after_stays_within_clamp_for_arbitrary_headers(
        raw in proptest::option::of("\\PC{0,32}"),
    ) {
        if let Some(seconds) = parse_retry_after(raw.as_deref()) {
            prop_assert!(
                (1..=120).contains(&seconds),
                "retry-after {seconds}s escaped the clamp for {raw:?}"
            );
        }
    }

    /// Every numeric delta-seconds value parses and lands inside the clamp.
    #[test]
    fn parse_retry_after_clamps_all_numeric_deltas(value in any::<u64>()) {
        let seconds = parse_retry_after(Some(&value.to_string()))
            .expect("numeric delta-seconds must parse");
        prop_assert!((1..=120).contains(&seconds));
    }

    /// HTTP-date forms (IMF-fixdate) also stay inside the clamp — past dates
    /// saturate to the 1-second floor, far-future dates hit the 120s ceiling.
    #[test]
    fn parse_retry_after_clamps_http_date_forms(
        day in 1u8..=31,
        year in 1970u32..2200,
        hour in 0u8..24,
        minute in 0u8..60,
        second in 0u8..60,
    ) {
        let header = format!("Wed, {day:02} Oct {year} {hour:02}:{minute:02}:{second:02} GMT");
        if let Some(seconds) = parse_retry_after(Some(&header)) {
            prop_assert!(
                (1..=120).contains(&seconds),
                "retry-after {seconds}s escaped the clamp for `{header}`"
            );
        }
    }

    /// A fuzzy `Ok` resolution is never an arbitrary model: the resolved ID
    /// contains the (trimmed, case-folded) input as a substring, or the
    /// input is an exact registry alias for that ID.
    #[test]
    fn resolve_model_fuzzy_ok_contains_input_or_is_exact_alias(
        input in model_input_strategy(),
    ) {
        if let Ok(id) = resolve_model_fuzzy(&input) {
            let needle = input.trim().to_ascii_lowercase();
            prop_assert!(
                id.to_ascii_lowercase().contains(&needle) || resolve_model_alias(&input) == id,
                "`{input}` resolved to `{id}`, which neither contains the input \
                 nor is its exact alias resolution"
            );
        }
    }

    /// `jwt_expiry_unix` must be total: no panic for arbitrary garbage.
    #[test]
    fn jwt_expiry_unix_never_panics_on_arbitrary_strings(token in "\\PC{0,64}") {
        let _ = jwt_expiry_unix(&token);
    }

    /// Three-part dotted inputs (JWT-shaped, including near-miss base64url
    /// payloads) must not panic either.
    #[test]
    fn jwt_expiry_unix_never_panics_on_jwt_shaped_strings(
        header in "[A-Za-z0-9_=\\-]{0,24}",
        payload in "\\PC{0,48}",
        signature in "[A-Za-z0-9_=\\-]{0,24}",
    ) {
        let _ = jwt_expiry_unix(&format!("{header}.{payload}.{signature}"));
    }

    /// `parse_net_timeout_ms` never leaves its 5s..=600s clamp.
    #[test]
    fn parse_net_timeout_ms_stays_within_clamp(raw in proptest::option::of("\\PC{0,24}")) {
        let value = parse_net_timeout_ms(raw.as_deref());
        prop_assert!((5_000..=600_000).contains(&value));
    }

    /// Numeric strings — the parseable subset — are clamped, not passed through.
    #[test]
    fn parse_net_timeout_ms_clamps_all_numeric_inputs(value in any::<u64>()) {
        let parsed = parse_net_timeout_ms(Some(&value.to_string()));
        prop_assert!((5_000..=600_000).contains(&parsed));
    }
}
