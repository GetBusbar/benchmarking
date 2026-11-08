use super::*;

/* A MANIFEST KEY NOBODY READS MUST FAIL AT LOAD. Serde ignored unknown fields, so writing
`runtime.image` - a field `Runtime::Docker` does not have, because the image lives on `launch`
- parsed cleanly and changed nothing. The box ran getbusbar/busbar:1.4.1 for 25 minutes under
the gateway key `busbar-150`, and the only reason it was caught was a human asking to log in
and look at the container rather than trusting the config.

The cost of the silent version is a run; the cost of the loud version is a parse error naming
the field. */
#[test]
fn an_image_written_where_the_schema_has_none_is_refused() {
    let json = r#"{
            "name": "x", "display": "X", "lang": "Rust", "class": "Gateway",
            "port": 8000, "model": "m", "auth": "t",
            "runtime": { "kind": "docker", "container": "c", "image": "getbusbar/busbar:9.9.9" },
            "egress": ["openai"]
        }"#;
    let err = serde_json::from_str::<Manifest>(json)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("image"),
        "the error must name the field that was ignored, got: {err}"
    );
}

/// And a stray top-level key is refused for the same reason - a typo'd or stale field is a
/// defect whichever level it sits at.
#[test]
fn a_stray_top_level_key_is_refused() {
    let json = r#"{
            "name": "x", "display": "X", "lang": "Rust", "class": "Gateway",
            "port": 8000, "model": "m", "auth": "t",
            "runtime": { "kind": "docker", "container": "c" },
            "egress": ["openai"],
            "verison": "1.5.0"
        }"#;
    let err = serde_json::from_str::<Manifest>(json)
        .unwrap_err()
        .to_string();
    assert!(err.contains("verison"), "got: {err}");
}

/// The real manifests must still parse - a guard that rejects the field is worthless if it also
/// rejects the board.
#[test]
fn every_shipped_manifest_still_parses_under_the_stricter_schema() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).expect("gateways/") {
        let p = e.expect("entry").path().join("definition.json");
        if !p.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&p).expect("read");
        serde_json::from_str::<Manifest>(&text)
            .unwrap_or_else(|err| panic!("{} no longer parses: {err}", p.display()));
        n += 1;
    }
    assert!(n >= 14, "expected the whole field, saw {n}");
}
