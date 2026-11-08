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
    // EVERY UNKNOWN NAME IS AN ERROR, not just a list where they ALL are.
    //
    // This was `filter_map(|d| d.trim().parse().ok())` with an emptiness check after it, so a single
    // typo was discarded in silence: `OTB_DIALECTS="openai,anthorpic"` parsed to one dialect, the
    // emptiness check passed, and the run walked a 1-dialect grid the operator believed was 2 - with
    // nothing on stdout, stderr, or in the artifact saying a name had been rejected. The caller in
    // bin/otb.rs only speaks when this returns Err, so that path printed nothing either.
    //
    // The module already stated the right rule two lines up ("a value that names something we do not
    // know is still an ERROR, not a silent fallback"); it was only true when every name was bad.
    let mut parsed: Vec<Dialect> = Vec::new();
    let mut unknown: Vec<&str> = Vec::new();
    let mut repeated: Vec<&str> = Vec::new();
    for name in list.split(',') {
        let name = name.trim();
        match name.parse::<Dialect>() {
            // A NAME TWICE IS A TYPO TOO. It walked every row and column of that dialect twice, the
            // second result overwriting the first in the keyed cell maps - box time spent on a grid
            // that differs from the one a reader of the list expects.
            Ok(d) if parsed.contains(&d) => repeated.push(name),
            Ok(d) => parsed.push(d),
            // An empty element is a stray comma rather than a misspelled dialect, and reporting it as
            // an unknown name would send a reader hunting for a typo that is not there.
            Err(_) if name.is_empty() => {}
            Err(_) => unknown.push(name),
        }
    }
    if !unknown.is_empty() {
        return Err(format!(
            "OTB_DIALECTS={list:?} names {} this build does not know: {}. A grid quietly missing a \
             dialect measures something other than what was asked for",
            if unknown.len() == 1 { "a dialect" } else { "dialects" },
            unknown.join(", ")
        ));
    }
    if !repeated.is_empty() {
        return Err(format!(
            "OTB_DIALECTS={list:?} names {} more than once: {}",
            if repeated.len() == 1 {
                "a dialect"
            } else {
                "dialects"
            },
            repeated.join(", ")
        ));
    }
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
    /// Four of six dialects use a bearer token; Anthropic and Gemini carry the key in headers of their
    /// own, and sending them a bearer draws a 401. `probe.rs` publishes a 401 from a healthy rig as
    /// `Failed` (only 404/501 are `NotConfigured`), so a wrong auth shape would mislabel whole ROWS of
    /// the grid as the gateway's own failure - red on the board - when the cause is our request.
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
            // `body` is a JSON object for every dialect, so this arm cannot be reached today. It is NOT
            // a loud failure: a future dialect whose body is not an object would send this
            // unstreamed body, so such a dialect must add its own streaming body here.
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

    /// Does this SSE `data:` payload carry MODEL OUTPUT, or is it protocol scaffolding?
    ///
    /// Ledger RIG-11. The stream reader counted every dispatched event as a delivered frame, and the
    /// two dialects that stream do not spend the same number of events on scaffolding. Verified
    /// against `mock/src/main.rs`'s own `StreamFrames::build`, which is what actually produces the
    /// frames this rig measures: openai sends 1 head (a `delta` carrying only `role`) + N content
    /// deltas + 2 tail (an empty `delta` with `finish_reason`, then `data: [DONE]`) = N+3 events;
    /// anthropic sends 2 head (`message_start`, `content_block_start`) + N deltas + 3 tail
    /// (`content_block_stop`, `message_delta`, `message_stop`) = N+5.
    ///
    /// That offset CANCELS in the added-TTFT and added-gap figures, because both legs of those speak
    /// the same dialect and the difference subtracts it away. It does NOT cancel in
    /// `run::StreamWindow::delivery_ratio`, which is a fraction of a fixed frame budget: there, a
    /// stream that delivered no tokens at all still satisfied 1 (openai) or 2 (anthropic) frames of
    /// the budget before its first content delta, and the two dialects differed from each other by
    /// exactly that. A delivery gate whose numerator counts `message_start` is not measuring
    /// delivery.
    ///
    /// OWNED BY THE DIALECT, deliberately. The alternative is a heuristic inside `http::SseReader`
    /// sniffing for `[DONE]`, and that decoder is transport-agnostic AND protocol-agnostic on
    /// purpose - it is fed by two lanes and knows nothing about who it is talking to. A taxonomy of
    /// events is a property of the wire dialect, so it lives with the rest of that dialect's wire
    /// knowledge, beside `body`, `path` and `auth_headers`.
    ///
    /// The rule per dialect is the real protocol's, not the mock's spelling:
    /// - openai: a chunk is content iff a `delta` carries a non-empty `content` string. The role
    ///   head, the `finish_reason` tail and the `[DONE]` sentinel all fail that, and `[DONE]` is not
    ///   even JSON.
    /// - anthropic: an event is content iff its `type` is `content_block_delta`. Every other typed
    ///   event in that protocol is framing.
    /// - everything else: `true`, which is what `frames` already counts. NOT a taxonomy we invented
    ///   for a wire we cannot test: `streams_natively` is false for all four, so the mock answers
    ///   them with plain JSON and no SSE event of theirs ever reaches this. If one ever does, it
    ///   counts exactly as it did before, rather than being silently reclassified by a rule nobody
    ///   validated against that protocol.
    pub fn sse_event_is_content(&self, data: &str) -> bool {
        match self {
            Dialect::Openai => {
                // `[DONE]` is a sentinel, not JSON, so it fails at the parse and needs no special
                // case - which is the point of asking the dialect rather than sniffing the string.
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    return false;
                };
                v.get("choices")
                    .and_then(|c| c.as_array())
                    .is_some_and(|choices| {
                        choices.iter().any(|c| {
                            c.get("delta")
                                .and_then(|d| d.get("content"))
                                .and_then(|t| t.as_str())
                                .is_some_and(|t| !t.is_empty())
                        })
                    })
            }
            Dialect::Anthropic => serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                .is_some_and(|t| t == "content_block_delta"),
            Dialect::OpenaiResponses | Dialect::Gemini | Dialect::Cohere | Dialect::Bedrock => true,
        }
    }

    /// How many events this dialect spends BEFORE its first content frame.
    ///
    /// The half of ledger RIG-11 that a classifier alone cannot fix. A stream is read to a fixed
    /// frame budget (`metric::STREAM_FRAME_BUDGET`), and the reader stops counting at that many
    /// events whether or not they carried tokens - so the most content frames a budget can possibly
    /// hold is the budget minus this. Comparing content frames against the raw budget would make
    /// every clean openai stream read as 63/64 delivered and every clean anthropic one as 62/64,
    /// failing `STREAM_MIN_DELIVERY_RATIO` (1.0, deliberate) on gateways that lost nothing.
    ///
    /// Read off `mock/src/main.rs`'s `StreamFrames::build` head vectors, the same source
    /// `sse_event_is_content` is: openai_head is one frame, anthropic_head is two. The TAIL is not
    /// counted here - at the rig's own settings (`MOCK_STREAM_CHUNKS` 64, budget 64) the budget is
    /// reached inside the deltas and no tail event ever arrives, and a stream that DOES reach its
    /// tail ended early, which is a delivery shortfall this gate should see rather than excuse.
    pub fn stream_prelude_frames(&self) -> u64 {
        match self {
            Dialect::Anthropic => 2,
            Dialect::Openai => 1,
            // No SSE reaches these (`streams_natively`), so there is no prelude to discount and
            // nothing is invented for a wire this rig cannot pose the question to.
            Dialect::OpenaiResponses | Dialect::Gemini | Dialect::Cohere | Dialect::Bedrock => 0,
        }
    }

    /// The header names the RIG ITSELF puts on every request of this dialect.
    ///
    /// Derived from `auth_headers` rather than listed again, so it cannot go stale the day a dialect
    /// changes how it authenticates. Used by `manifest::Manifest` to refuse a manifest that declares
    /// one of these itself: see the duplicate-credential refusal there for why sending both is a
    /// measurement fault rather than an untidiness.
    pub fn rig_owned_header_names(&self) -> Vec<String> {
        self.auth_headers("")
            .into_iter()
            .map(|(n, _)| n.to_ascii_lowercase())
            .collect()
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
#[path = "tests/ingress_tests.rs"]
mod tests;
