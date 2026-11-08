// A TYPO MUST NOT QUIETLY SHRINK THE GRID.
//
// `filter_map(...parse().ok())` dropped an unknown name in silence and only errored when EVERY
// name was bad, so `OTB_DIALECTS="openai,anthorpic"` ran a 1-dialect grid the operator believed
// was 2 - nothing on stderr, nothing in the artifact. The module's own rule ("a value that names
// something we do not know is still an ERROR, not a silent fallback") held only in the all-bad
// case.
#[test]
fn one_unknown_dialect_is_an_error_not_a_silently_smaller_grid() {
    let e = super::dialects_from(Some("openai,anthorpic"))
        .expect_err("a misspelled dialect must not be dropped in silence");
    assert!(
        e.contains("anthorpic"),
        "the error must name the rejected value: {e}"
    );

    // The all-bad case still errors, and a clean list still parses.
    assert!(super::dialects_from(Some("nope,also-nope")).is_err());
    assert_eq!(
        super::dialects_from(Some("openai,anthropic")).expect("both are real"),
        vec![Dialect::Openai, Dialect::Anthropic]
    );
    // An empty element is a stray comma, not a misspelling: reporting it would send a reader
    // hunting for a typo that is not there.
    assert_eq!(
        super::dialects_from(Some("openai,,anthropic")).expect("a stray comma is not a typo"),
        vec![Dialect::Openai, Dialect::Anthropic]
    );
    // Unset still means the whole field.
    assert_eq!(
        super::dialects_from(None).expect("unset"),
        Dialect::ALL.to_vec()
    );
}

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
        let parsed: serde_json::Value =
            serde_json::from_str(&b).unwrap_or_else(|e| panic!("{d} body is not valid JSON: {e}"));
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

// A list of nothing but separators names no dialect - the branch the unknown-name check above never
// reaches, because nothing in it is a name at all.
#[test]
fn a_list_of_only_separators_is_an_error_not_an_empty_grid() {
    let e = dialects_from(Some(",")).expect_err("a zero-dialect grid is not a run");
    assert!(e.contains("named no dialect"), "{e}");
    assert!(dialects_from(Some(" , ,")).is_err());
}

// A NAME TWICE walked that dialect's rows and columns twice, the second result overwriting the first.
#[test]
fn a_repeated_dialect_is_an_error_not_a_doubled_grid() {
    let e = dialects_from(Some("openai,anthropic,openai"))
        .expect_err("a repeated dialect must not double the grid");
    assert!(e.contains("more than once") && e.contains("openai"), "{e}");
}

// ── the SSE content taxonomy (ledger RIG-11) ────────────────────────────────────────────────

// THE FRAMES ARE THE MOCK'S OWN, copied from mock/src/main.rs's `StreamFrames::build` rather
// than paraphrased, because a classifier validated against invented strings classifies invented
// streams. openai: 1 head + N deltas + 2 tail. anthropic: 2 head + N deltas + 3 tail.
#[test]
fn only_the_deltas_that_carry_a_token_count_as_content() {
    let openai_head = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#;
    let openai_delta = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{"content":"xxxx"},"finish_reason":null}]}"#;
    let openai_finish = r#"{"id":"chatcmpl-x","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#;
    let d = Dialect::Openai;
    assert!(d.sse_event_is_content(openai_delta));
    assert!(
        !d.sse_event_is_content(openai_head),
        "the role chunk carries no token"
    );
    assert!(!d.sse_event_is_content(openai_finish));
    // The sentinel is not JSON, so it fails at the parse - no `[DONE]` string match anywhere,
    // which is exactly why this lives on the dialect and not inside the transport decoder.
    assert!(!d.sse_event_is_content("[DONE]"));

    let a = Dialect::Anthropic;
    assert!(a.sse_event_is_content(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"xxxx"}}"#
    ));
    for framing in [
        r#"{"type":"message_start","message":{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":64}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        assert!(
            !a.sse_event_is_content(framing),
            "{framing} is framing, not a token"
        );
    }

    // Nothing is claimed about a wire this rig cannot pose the question to: those dialects get
    // no SSE from the mock at all, and inventing a taxonomy for them would be a rule nobody
    // validated against the real protocol.
    for other in Dialect::ALL {
        if other.streams_natively() {
            continue;
        }
        assert!(other.sse_event_is_content("anything at all"));
        assert_eq!(other.stream_prelude_frames(), 0);
    }
}

// THE PRELUDE IS WHY A DELIVERY RATIO NEEDS A DENOMINATOR OF ITS OWN. A fixed frame budget is
// spent on the head events before a single token arrives, so the most content a budget can hold
// is the budget minus this - and the two streaming dialects differ by exactly the two the ledger
// names.
#[test]
fn the_two_streaming_dialects_spend_different_budget_before_the_first_token() {
    assert_eq!(Dialect::Openai.stream_prelude_frames(), 1);
    assert_eq!(Dialect::Anthropic.stream_prelude_frames(), 2);
    assert_eq!(
        Dialect::Anthropic.stream_prelude_frames() - Dialect::Openai.stream_prelude_frames(),
        1,
        "the head difference; the tails differ by one more, which the budget never reaches"
    );
}

// The names the rig puts on the wire itself are DERIVED from `auth_headers`, so a dialect that
// changes how it authenticates cannot leave the duplicate-header rule guarding a stale name.
#[test]
fn the_rig_owned_header_names_are_exactly_what_the_dialect_sends() {
    for d in Dialect::ALL {
        let sent: Vec<String> = d
            .auth_headers("tok")
            .into_iter()
            .map(|(n, _)| n.to_ascii_lowercase())
            .collect();
        assert_eq!(d.rig_owned_header_names(), sent, "{d}");
    }
    assert!(Dialect::Openai
        .rig_owned_header_names()
        .contains(&"authorization".to_string()));
    assert!(Dialect::Anthropic
        .rig_owned_header_names()
        .contains(&"x-api-key".to_string()));
}
