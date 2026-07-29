// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The protocol-dialect knowledge: for each of the six dialects a client of this benchmark can
// speak, what URL path carries it, what a probe body looks like, and whether the mock upstream can
// answer that dialect with a real SSE stream.
//
// Gemini and bedrock carry the model in the URL PATH, not the body: `path` takes the model as a
// parameter and returns a fresh `String` on every call, so there is no global and no stored value
// that a later step switching models per column could leave stale.
//
// Every dialect is a variant of `Dialect`, and every function below matches on it exhaustively (no
// wildcard arm), so a seventh dialect is a compile error here, not a silent fallthrough to whatever
// the default arm happened to return.

use std::fmt;
use std::str::FromStr;

/// One of the six request shapes this benchmark speaks, on either side of a gateway. These are
/// protocol dialects (what a client sends, what an upstream expects), not any one vendor's gateway
/// product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dialect {
    Openai,
    OpenaiResponses,
    Anthropic,
    Gemini,
    Cohere,
    Bedrock,
}

/// Which dialects a run should walk, from the value of `OTB_DIALECTS`.
///
/// UNSET AND EMPTY BOTH MEAN "ALL", and that distinction is the whole point. `std::env::var` returns
/// `Ok("")` for a variable that is SET BUT EMPTY, and the orchestrator always exports the variable,
/// writing an empty value for a full-grid run, so an empty string must resolve to every dialect, not
/// to zero.
///
/// A value that names something we do not know is still an ERROR, not a silent fallback to all: an
/// operator who asked for one dialect and got six would be publishing a grid they did not request.
pub fn dialects_from(value: Option<&str>) -> Result<Vec<Dialect>, String> {
    let Some(list) = value.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Dialect::ALL.to_vec());
    };
    let parsed: Vec<Dialect> = list
        .split(',')
        .filter_map(|d| d.trim().parse().ok())
        .collect();
    if parsed.is_empty() {
        return Err(format!(
            "OTB_DIALECTS={list:?} named no dialect this build knows"
        ));
    }
    Ok(parsed)
}

impl Dialect {
    /// Every dialect, in the canonical order used across the harness (lib/ingress.sh, matrix/run.sh).
    pub const ALL: [Dialect; 6] = [
        Dialect::Openai,
        Dialect::OpenaiResponses,
        Dialect::Anthropic,
        Dialect::Gemini,
        Dialect::Cohere,
        Dialect::Bedrock,
    ];

    /// The canonical short name used on the wire, in JSON, and in every shell script this ports.
    pub fn as_str(&self) -> &'static str {
        match self {
            Dialect::Openai => "openai",
            Dialect::OpenaiResponses => "openai-responses",
            Dialect::Anthropic => "anthropic",
            Dialect::Gemini => "gemini",
            Dialect::Cohere => "cohere",
            Dialect::Bedrock => "bedrock",
        }
    }

    /// The ingress URL path a client of this dialect would POST to. `model` is a PARAMETER, taken
    /// fresh on every call: gemini and bedrock embed it in the path, and freezing either of those
    /// into a stored value is exactly the defect this module exists to make impossible (see the
    /// module header). Matches lib/ingress.sh's canonical defaults (no manifest override
    /// modelled here: that belongs to whatever composes this module with a manifest).
    pub fn path(&self, model: &str) -> String {
        match self {
            Dialect::Openai => "/v1/chat/completions".to_string(),
            Dialect::OpenaiResponses => "/v1/responses".to_string(),
            Dialect::Anthropic => "/v1/messages".to_string(),
            Dialect::Gemini => format!("/v1beta/models/{model}:generateContent"),
            Dialect::Cohere => "/v2/chat".to_string(),
            Dialect::Bedrock => format!("/model/{model}/converse"),
        }
    }

    /// The probe request body for this dialect (matrix/run.sh's `ingress_body`). Gemini and bedrock
    /// carry no "model" key: for those two the model already rode in the path, and repeating it in
    /// the body is not part of the wire shape either script sends.
    pub fn body(&self, model: &str) -> String {
        match self {
            Dialect::Openai => {
                format!(
                    r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}],"max_tokens":16}}"#
                )
            }
            Dialect::OpenaiResponses => format!(r#"{{"model":"{model}","input":"hello"}}"#),
            Dialect::Anthropic => {
                format!(
                    r#"{{"model":"{model}","max_tokens":64,"messages":[{{"role":"user","content":"hello"}}]}}"#
                )
            }
            Dialect::Gemini => r#"{"contents":[{"parts":[{"text":"hello"}]}]}"#.to_string(),
            Dialect::Cohere => {
                format!(r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}]}}"#)
            }
            Dialect::Bedrock => {
                r#"{"messages":[{"role":"user","content":[{"text":"hello"}]}]}"#.to_string()
            }
        }
    }

    /// The path used to reach the mock DIRECTLY, for the added-latency baseline leg (matrix/run.sh's
    /// `mock_direct_path`). The mock routes on these same canonical paths regardless of any
    /// per-gateway ingress override, so this is deliberately not a function of any manifest: it is
    /// the mock's own fixed routing, independent of whatever path the gateway under test answers on.
    /// Numerically identical to `path` for every dialect (the mock's canonical route IS the
    /// canonical default), kept as its own function because the two answer different questions and
    /// a manifest override can make them diverge for the gateway-facing path but never for this one.
    pub fn mock_direct_path(&self, model: &str) -> String {
        self.path(model)
    }

    /// How a client of THIS dialect authenticates, and any version header the wire requires.
    ///
    /// The auth header is a property of the PROTOCOL, not of the gateway: a client speaking Anthropic
    /// sends `x-api-key` and `anthropic-version` whoever it is talking to. It lives here, once, for
    /// the same reason `path` does - thirteen copies of one table is thirteen chances to drift.
    ///
    /// Only three of six dialects use a bearer token; sending one to the other two draws a 401,
    /// which `probe.rs` maps to `NotConfigured` for any status from a healthy rig - "the gateway
    /// answered, deterministically, that this pairing does not light up" - so a wrong auth shape
    /// would mislabel whole ROWS of the grid as the gateway's own capability denial, grey on the
    /// board, when the cause is our request.
    pub fn auth_headers(&self, auth: &str) -> Vec<(String, String)> {
        match self {
            // Anthropic's own wire: the key is x-api-key, and the version header is mandatory.
            Dialect::Anthropic => vec![
                ("x-api-key".to_string(), auth.to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ],
            // Gemini carries the key in its own header.
            Dialect::Gemini => vec![("x-goog-api-key".to_string(), auth.to_string())],
            // Everything else is a bearer token. Bedrock is signed in the real world, but the mock
            // ignores the signature and the shell sent a bearer here too, so this matches what the
            // field has always driven.
            Dialect::Openai | Dialect::OpenaiResponses | Dialect::Cohere | Dialect::Bedrock => {
                vec![("authorization".to_string(), format!("Bearer {auth}"))]
            }
        }
    }

    /// The same probe body, asking for a stream.
    ///
    /// The mock decides whether to stream by looking for `"stream": true` in the request body (its
    /// own `wants_stream` dispatch), and so does every gateway under test, so the streaming probe
    /// must be the ORDINARY body plus that one flag. Building a separate hand-written streaming body
    /// per dialect would let the streaming and non-streaming legs drift into asking two different
    /// questions, and the difference between them is exactly what the added-latency numbers publish.
    ///
    /// The flag is inserted at the front of the object rather than appended, so no trailing-comma
    /// handling is needed for bodies that end in a nested structure.
    pub fn stream_body(&self, model: &str) -> String {
        let body = self.body(model);
        match body.strip_prefix('{') {
            Some(rest) => format!("{{\"stream\":true,{rest}"),
            // `body` is a JSON object for every dialect; this arm cannot be reached today and exists
            // so a future dialect whose body is not an object fails loudly rather than silently
            // sending an unstreamed request that would be published as "does not stream".
            None => body,
        }
    }

    /// Whether the mock upstream can answer this dialect's `"stream":true` request with a real SSE
    /// stream, rather than plain JSON. Read off mock/src/main.rs's own dispatch (`wants_stream(...)
    /// && (body is OPENAI or ANTHROPIC)`), not off a comment: only those two dialects get native SSE
    /// frames there, because bedrock's real streaming wire shape is AWS's binary event-stream
    /// framing (not SSE) and the mock does not synthesize responses/gemini/cohere streams at all. A
    /// dialect this returns false for must be reported as a rig limit (the mock cannot pose the
    /// question), never as the gateway failing to stream.
    pub fn streams_natively(&self) -> bool {
        matches!(self, Dialect::Openai | Dialect::Anthropic)
    }

    /// Whether a real client of this dialect authenticates by a scheme THE HARNESS CANNOT PRODUCE.
    ///
    /// Bedrock is signed with AWS SigV4 over the request. `auth_headers` sends a bearer token
    /// instead, which is what the field has always driven and is fine against the mock (it ignores
    /// the signature), but a GATEWAY that verifies credentials properly will reject it - correctly.
    /// Grading that rejection as the gateway failing publishes a red it did not earn, so a 401/403
    /// on this dialect is recorded as unprobed rather than as a refusal. Forging signatures is the
    /// alternative and is worse: it would make the harness assert an identity it does not hold.
    ///
    /// Deliberately about the DIALECT, never about a gateway: this is a limit of our instrument, and
    /// it applies identically to every entrant.
    pub fn auth_is_unforgeable_by_the_rig(&self) -> bool {
        matches!(self, Dialect::Bedrock)
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unparseable dialect name. Typed on purpose: there is no default dialect to fall back to, so a
/// caller must handle this rather than silently routing an unknown string somewhere plausible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownDialect(pub String);

impl fmt::Display for UnknownDialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown ingress dialect: {:?}", self.0)
    }
}

impl std::error::Error for UnknownDialect {}

impl FromStr for Dialect {
    type Err = UnknownDialect;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openai" => Ok(Dialect::Openai),
            "openai-responses" => Ok(Dialect::OpenaiResponses),
            "anthropic" => Ok(Dialect::Anthropic),
            "gemini" => Ok(Dialect::Gemini),
            "cohere" => Ok(Dialect::Cohere),
            "bedrock" => Ok(Dialect::Bedrock),
            other => Err(UnknownDialect(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── path(): asserted exactly against lib/ingress.sh's canonical defaults ──────────────────────

    #[test]
    fn path_matches_ingress_sh_defaults() {
        assert_eq!(Dialect::Openai.path("gpt-4o-mini"), "/v1/chat/completions");
        assert_eq!(
            Dialect::OpenaiResponses.path("gpt-4o-mini"),
            "/v1/responses"
        );
        assert_eq!(Dialect::Anthropic.path("gpt-4o-mini"), "/v1/messages");
        assert_eq!(
            Dialect::Gemini.path("gpt-4o-mini"),
            "/v1beta/models/gpt-4o-mini:generateContent"
        );
        assert_eq!(Dialect::Cohere.path("gpt-4o-mini"), "/v2/chat");
        assert_eq!(
            Dialect::Bedrock.path("gpt-4o-mini"),
            "/model/gpt-4o-mini/converse"
        );
    }

    // The model is a parameter, so a different model must produce a different path for the two
    // dialects that embed it, on every call, with no stored state anywhere to disagree with a later
    // call.
    #[test]
    fn a_different_model_produces_a_different_path_for_gemini_and_bedrock() {
        let a = Dialect::Gemini.path("gpt-4o-mini");
        let b = Dialect::Gemini.path("gpt-4o-mini-gemini");
        assert_ne!(a, b);
        assert!(b.contains("gpt-4o-mini-gemini"));
        assert!(
            !b.contains("models/gpt-4o-mini:"),
            "must not retain the stale model as a prefix match"
        );

        let a = Dialect::Bedrock.path("gpt-4o-mini");
        let b = Dialect::Bedrock.path("gpt-4o-mini-bedrock");
        assert_ne!(a, b);
        assert!(b.contains("gpt-4o-mini-bedrock"));
        assert!(!b.contains("/model/gpt-4o-mini/converse"));
    }

    // The model appears in the path for the two dialects that embed it, and in NEITHER other path
    // (it rides in the body there instead).
    #[test]
    fn only_gemini_and_bedrock_carry_the_model_in_the_path() {
        let model = "distinctive-marker-model";
        for d in Dialect::ALL {
            let carries = d.path(model).contains(model);
            let expected = matches!(d, Dialect::Gemini | Dialect::Bedrock);
            assert_eq!(carries, expected, "{d} path model-in-path mismatch");
        }
    }

    // Calling path() many times with different models never leaves any trace of a prior call: there
    // is nothing to freeze because there is nothing stored.
    #[test]
    fn repeated_calls_with_different_models_never_bleed_into_each_other() {
        let models = ["m1", "m2-with-a-suffix", "m3"];
        for d in [Dialect::Gemini, Dialect::Bedrock] {
            let mut seen = std::collections::BTreeSet::new();
            for m in models {
                let p = d.path(m);
                assert!(p.contains(m));
                seen.insert(p);
            }
            assert_eq!(
                seen.len(),
                models.len(),
                "{d} produced fewer distinct paths than distinct models"
            );
        }
    }

    // ── mock_direct_path(): asserted exactly against matrix/run.sh's mock_direct_path ─────────────

    #[test]
    fn mock_direct_path_matches_run_sh() {
        assert_eq!(
            Dialect::Openai.mock_direct_path("m"),
            "/v1/chat/completions"
        );
        assert_eq!(
            Dialect::OpenaiResponses.mock_direct_path("m"),
            "/v1/responses"
        );
        assert_eq!(Dialect::Anthropic.mock_direct_path("m"), "/v1/messages");
        assert_eq!(
            Dialect::Gemini.mock_direct_path("gpt-4o-mini"),
            "/v1beta/models/gpt-4o-mini:generateContent"
        );
        assert_eq!(Dialect::Cohere.mock_direct_path("m"), "/v2/chat");
        assert_eq!(
            Dialect::Bedrock.mock_direct_path("gpt-4o-mini"),
            "/model/gpt-4o-mini/converse"
        );
    }

    // ── body(): exact against lib/ingress_body ... and always valid, non-empty JSON ────────────────

    #[test]
    fn body_matches_ingress_body_exactly() {
        assert_eq!(
            Dialect::Openai.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}],"max_tokens":16}"#
        );
        assert_eq!(
            Dialect::OpenaiResponses.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","input":"hello"}"#
        );
        assert_eq!(
            Dialect::Anthropic.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","max_tokens":64,"messages":[{"role":"user","content":"hello"}]}"#
        );
        assert_eq!(
            Dialect::Gemini.body("gpt-4o-mini"),
            r#"{"contents":[{"parts":[{"text":"hello"}]}]}"#
        );
        assert_eq!(
            Dialect::Cohere.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#
        );
        assert_eq!(
            Dialect::Bedrock.body("gpt-4o-mini"),
            r#"{"messages":[{"role":"user","content":[{"text":"hello"}]}]}"#
        );
    }

    #[test]
    fn every_body_is_non_empty_valid_json() {
        for d in Dialect::ALL {
            let b = d.body("gpt-4o-mini");
            assert!(!b.is_empty(), "{d} produced an empty body");
            let parsed: serde_json::Value = serde_json::from_str(&b)
                .unwrap_or_else(|e| panic!("{d} body is not valid JSON: {e}"));
            assert!(parsed.is_object(), "{d} body must be a JSON object");
        }
    }

    // ── streams_natively(): matches mock/src/main.rs's real dispatch, not a comment ────────────────

    // Sending the wrong auth header to a dialect makes the gateway answer 401, which probe.rs reads
    // as the gateway's own deterministic refusal of the pairing. Two of the six dialects do not use
    // a bearer token at all, so hardcoding one shape mislabels two whole rows of every gateway's grid
    // as capability denials.
    #[test]
    fn each_dialect_authenticates_the_way_its_own_protocol_does() {
        let anthropic = Dialect::Anthropic.auth_headers("K");
        assert!(
            anthropic.iter().any(|(n, v)| n == "x-api-key" && v == "K"),
            "{anthropic:?}"
        );
        assert!(
            anthropic.iter().any(|(n, _)| n == "anthropic-version"),
            "the version header is mandatory: {anthropic:?}"
        );
        assert!(
            !anthropic.iter().any(|(n, _)| n == "authorization"),
            "anthropic does not take a bearer token"
        );

        let gemini = Dialect::Gemini.auth_headers("K");
        assert_eq!(
            gemini,
            vec![("x-goog-api-key".to_string(), "K".to_string())]
        );

        for d in [
            Dialect::Openai,
            Dialect::OpenaiResponses,
            Dialect::Cohere,
            Dialect::Bedrock,
        ] {
            let h = d.auth_headers("K");
            assert!(
                h.iter()
                    .any(|(n, v)| n == "authorization" && v == "Bearer K"),
                "{d}: {h:?}"
            );
        }

        // Every dialect must authenticate somehow: a dialect with no credential at all would be
        // driven anonymously and read as the gateway refusing it.
        for d in Dialect::ALL {
            assert!(!d.auth_headers("K").is_empty(), "{d} sends no credential");
        }
    }

    // The streaming probe must be the ORDINARY body plus the flag, or the streaming and
    // non-streaming legs are asking different questions and their difference means nothing.
    #[test]
    fn a_stream_body_is_the_probe_body_plus_the_flag_and_nothing_else() {
        for d in Dialect::ALL {
            let plain: serde_json::Value =
                serde_json::from_str(&d.body("m")).expect("every probe body must be valid JSON");
            let streamed: serde_json::Value = serde_json::from_str(&d.stream_body("m"))
                .expect("every stream body must be valid JSON");

            assert_eq!(
                streamed.get("stream"),
                Some(&serde_json::Value::Bool(true)),
                "{d} must ask for a stream"
            );

            // Every other key is untouched, and no key is lost: same question, streamed.
            let mut without_flag = streamed.clone();
            without_flag
                .as_object_mut()
                .expect("a JSON object")
                .remove("stream");
            assert_eq!(
                without_flag, plain,
                "{d}: the stream body must differ from the probe body ONLY by the flag"
            );
        }
    }

    #[test]
    fn only_openai_and_anthropic_stream_natively_in_the_mock() {
        for d in Dialect::ALL {
            let expected = matches!(d, Dialect::Openai | Dialect::Anthropic);
            assert_eq!(
                d.streams_natively(),
                expected,
                "{d} streams_natively mismatch"
            );
        }
    }

    // ── round trip: Display -> FromStr is stable, and an unknown name is a typed error ─────────────

    #[test]
    fn display_then_parse_round_trips_for_every_dialect() {
        for d in Dialect::ALL {
            let printed = d.to_string();
            let parsed: Dialect = printed
                .parse()
                .unwrap_or_else(|e| panic!("{printed:?} failed to parse back: {e:?}"));
            assert_eq!(parsed, d);
            assert_eq!(parsed.as_str(), d.as_str());
        }
    }

    #[test]
    fn an_unknown_dialect_name_is_a_typed_error_not_a_default() {
        let err = "openai-v2".parse::<Dialect>().unwrap_err();
        assert_eq!(err, UnknownDialect("openai-v2".to_string()));
        assert!(err.to_string().contains("openai-v2"));

        let err = "".parse::<Dialect>().unwrap_err();
        assert_eq!(err, UnknownDialect(String::new()));
    }

    // Guards against a copy/paste of the enum drifting from ALL, or ALL drifting from the match arms:
    // every canonical name parses back to the exact variant in ALL, in order.
    #[test]
    fn all_lists_exactly_the_six_canonical_dialects_in_order() {
        let names: Vec<&str> = Dialect::ALL.iter().map(Dialect::as_str).collect();
        assert_eq!(
            names,
            vec![
                "openai",
                "openai-responses",
                "anthropic",
                "gemini",
                "cohere",
                "bedrock"
            ]
        );
    }

    // std::env::var returns Ok("") for a variable that is set but empty, and the orchestrator always
    // exports OTB_DIALECTS, writing an empty value when the run is not narrowed: an empty string
    // must mean every dialect, not zero.
    #[test]
    fn an_empty_dialect_list_means_every_dialect_not_none() {
        assert_eq!(
            dialects_from(None),
            Ok(Dialect::ALL.to_vec()),
            "unset means the whole grid"
        );
        assert_eq!(
            dialects_from(Some("")),
            Ok(Dialect::ALL.to_vec()),
            "SET BUT EMPTY means the whole grid too"
        );
        assert_eq!(
            dialects_from(Some("   ")),
            Ok(Dialect::ALL.to_vec()),
            "whitespace is empty"
        );
    }

    // A value that names something unknown must still fail. Falling back to the whole grid there
    // would publish six columns to an operator who asked for one, which is a different run than the
    // one they requested.
    #[test]
    fn a_dialect_list_naming_nothing_we_know_is_an_error_not_a_fallback() {
        assert!(dialects_from(Some("nonsense")).is_err());
        assert_eq!(dialects_from(Some("openai")), Ok(vec![Dialect::Openai]));
    }
}
