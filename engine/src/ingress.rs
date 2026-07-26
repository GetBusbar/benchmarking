// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The protocol-dialect knowledge: for each of the six dialects a client of this benchmark can
// speak, what URL path carries it, what a probe body looks like, and whether the mock upstream can
// answer that dialect with a real SSE stream.
//
// THE DEFECT THIS EXISTS TO NOT REPEAT (published, real, shell's own postmortem in
// lib/ingress.sh / lib/ingress_path_test.sh): gemini and bedrock carry the model in the URL PATH,
// not the body. The shell engine once expanded that path ONCE at script init, into a global that
// every later call echoed back verbatim. When a later step switched models per column, the frozen
// path kept asking for the ORIGINAL model, so ten cells asked a live gateway for a model it was no
// longer configured to serve and the run published them red with a wrong-endpoint error. The fix
// there was "resolve at call time"; the fix here is stronger: there is no global to freeze, because
// `path` takes the model as a parameter and returns a fresh `String` on every call. There is no
// storage for a stale value to hide in.
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

impl Dialect {
    /// Every dialect, in the canonical order used across the harness (lib/ingress.sh, matrix/run.sh).
    pub const ALL: [Dialect; 6] =
        [Dialect::Openai, Dialect::OpenaiResponses, Dialect::Anthropic, Dialect::Gemini, Dialect::Cohere, Dialect::Bedrock];

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
                format!(r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}],"max_tokens":16}}"#)
            }
            Dialect::OpenaiResponses => format!(r#"{{"model":"{model}","input":"hello"}}"#),
            Dialect::Anthropic => {
                format!(r#"{{"model":"{model}","max_tokens":64,"messages":[{{"role":"user","content":"hello"}}]}}"#)
            }
            Dialect::Gemini => r#"{"contents":[{"parts":[{"text":"hello"}]}]}"#.to_string(),
            Dialect::Cohere => format!(r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}]}}"#),
            Dialect::Bedrock => r#"{"messages":[{"role":"user","content":[{"text":"hello"}]}]}"#.to_string(),
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
        assert_eq!(Dialect::OpenaiResponses.path("gpt-4o-mini"), "/v1/responses");
        assert_eq!(Dialect::Anthropic.path("gpt-4o-mini"), "/v1/messages");
        assert_eq!(Dialect::Gemini.path("gpt-4o-mini"), "/v1beta/models/gpt-4o-mini:generateContent");
        assert_eq!(Dialect::Cohere.path("gpt-4o-mini"), "/v2/chat");
        assert_eq!(Dialect::Bedrock.path("gpt-4o-mini"), "/model/gpt-4o-mini/converse");
    }

    // THE REGRESSION TEST for the frozen-model defect: the model is a parameter, so a different
    // model must produce a different path for the two dialects that embed it, on every call, with
    // no stored state anywhere to disagree with a later call.
    #[test]
    fn a_different_model_produces_a_different_path_for_gemini_and_bedrock() {
        let a = Dialect::Gemini.path("gpt-4o-mini");
        let b = Dialect::Gemini.path("gpt-4o-mini-gemini");
        assert_ne!(a, b);
        assert!(b.contains("gpt-4o-mini-gemini"));
        assert!(!b.contains("models/gpt-4o-mini:"), "must not retain the stale model as a prefix match");

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
            assert_eq!(seen.len(), models.len(), "{d} produced fewer distinct paths than distinct models");
        }
    }

    // ── mock_direct_path(): asserted exactly against matrix/run.sh's mock_direct_path ─────────────

    #[test]
    fn mock_direct_path_matches_run_sh() {
        assert_eq!(Dialect::Openai.mock_direct_path("m"), "/v1/chat/completions");
        assert_eq!(Dialect::OpenaiResponses.mock_direct_path("m"), "/v1/responses");
        assert_eq!(Dialect::Anthropic.mock_direct_path("m"), "/v1/messages");
        assert_eq!(Dialect::Gemini.mock_direct_path("gpt-4o-mini"), "/v1beta/models/gpt-4o-mini:generateContent");
        assert_eq!(Dialect::Cohere.mock_direct_path("m"), "/v2/chat");
        assert_eq!(Dialect::Bedrock.mock_direct_path("gpt-4o-mini"), "/model/gpt-4o-mini/converse");
    }

    // ── body(): exact against lib/ingress_body ... and always valid, non-empty JSON ────────────────

    #[test]
    fn body_matches_ingress_body_exactly() {
        assert_eq!(
            Dialect::Openai.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}],"max_tokens":16}"#
        );
        assert_eq!(Dialect::OpenaiResponses.body("gpt-4o-mini"), r#"{"model":"gpt-4o-mini","input":"hello"}"#);
        assert_eq!(
            Dialect::Anthropic.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","max_tokens":64,"messages":[{"role":"user","content":"hello"}]}"#
        );
        assert_eq!(Dialect::Gemini.body("gpt-4o-mini"), r#"{"contents":[{"parts":[{"text":"hello"}]}]}"#);
        assert_eq!(
            Dialect::Cohere.body("gpt-4o-mini"),
            r#"{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hello"}]}"#
        );
        assert_eq!(Dialect::Bedrock.body("gpt-4o-mini"), r#"{"messages":[{"role":"user","content":[{"text":"hello"}]}]}"#);
    }

    #[test]
    fn every_body_is_non_empty_valid_json() {
        for d in Dialect::ALL {
            let b = d.body("gpt-4o-mini");
            assert!(!b.is_empty(), "{d} produced an empty body");
            let parsed: serde_json::Value =
                serde_json::from_str(&b).unwrap_or_else(|e| panic!("{d} body is not valid JSON: {e}"));
            assert!(parsed.is_object(), "{d} body must be a JSON object");
        }
    }

    // ── streams_natively(): matches mock/src/main.rs's real dispatch, not a comment ────────────────

    // The streaming probe must be the ORDINARY body plus the flag, or the streaming and
    // non-streaming legs are asking different questions and their difference means nothing.
    #[test]
    fn a_stream_body_is_the_probe_body_plus_the_flag_and_nothing_else() {
        for d in Dialect::ALL {
            let plain: serde_json::Value =
                serde_json::from_str(&d.body("m")).expect("every probe body must be valid JSON");
            let streamed: serde_json::Value =
                serde_json::from_str(&d.stream_body("m")).expect("every stream body must be valid JSON");

            assert_eq!(streamed.get("stream"), Some(&serde_json::Value::Bool(true)), "{d} must ask for a stream");

            // Every other key is untouched, and no key is lost: same question, streamed.
            let mut without_flag = streamed.clone();
            without_flag.as_object_mut().expect("a JSON object").remove("stream");
            assert_eq!(without_flag, plain, "{d}: the stream body must differ from the probe body ONLY by the flag");
        }
    }

    #[test]
    fn only_openai_and_anthropic_stream_natively_in_the_mock() {
        for d in Dialect::ALL {
            let expected = matches!(d, Dialect::Openai | Dialect::Anthropic);
            assert_eq!(d.streams_natively(), expected, "{d} streams_natively mismatch");
        }
    }

    // ── round trip: Display -> FromStr is stable, and an unknown name is a typed error ─────────────

    #[test]
    fn display_then_parse_round_trips_for_every_dialect() {
        for d in Dialect::ALL {
            let printed = d.to_string();
            let parsed: Dialect = printed.parse().unwrap_or_else(|e| panic!("{printed:?} failed to parse back: {e:?}"));
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
        assert_eq!(names, vec!["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"]);
    }
}
