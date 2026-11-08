use super::*;

// A HEADER THAT CANNOT RESOLVE MUST NOT SILENTLY BECOME NO HEADERS.
//
// `validate()` rejects a header containing a shell-style `$`, but it never exercises `{...}`
// substitution - so a manifest declaring `Authorization: Bearer {NOT_A_PLACEHOLDER}` passes the
// gate `otb run` actually calls, and only fails later inside `headers_for`. That failure used to be
// swallowed by `.unwrap_or_default()` in `suite::run_suite_with`, which measured the entire run
// with an EMPTY header set: every cell fails to serve, and the board publishes `served: false` -
// reporting that the GATEWAY does not work when the harness dropped its credentials.
//
// This pins both halves: validate() does NOT catch it (so the later refusal is load-bearing rather
// than belt-and-braces), and headers_for DOES return Err (so there is something to refuse on).
#[test]
fn an_unknown_placeholder_in_a_header_escapes_validate_and_fails_resolution() {
    let m = Manifest {
        headers: vec!["Authorization: Bearer {NOT_A_REAL_PLACEHOLDER}".to_string()],
        ..test_fixture()
    };
    assert!(
        m.validate().is_ok(),
        "validate() only rejects shell-style `$`; if it ever learns to resolve braces, the \
             refusal in run_suite_with becomes redundant and this test should say so"
    );
    let resolved = m.headers_for("", "0-3", 9000, std::path::Path::new("/tmp"));
    assert!(
        resolved.is_err(),
        "an unknown placeholder must fail resolution - otherwise the run proceeds with a header \
             whose value still contains literal braces"
    );
}

// The clean path must still resolve, or the refusal above would be indistinguishable from a
// manifest system that simply never works.
#[test]
fn a_manifest_with_no_placeholders_resolves_its_headers() {
    let m = Manifest {
        headers: vec!["X-Fixed: value".to_string()],
        ..test_fixture()
    };
    let got = m
        .headers_for("", "0-3", 9000, std::path::Path::new("/tmp"))
        .expect("a header with no placeholder resolves");
    assert!(
        got.iter().any(|(k, v)| k == "X-Fixed" && v == "value"),
        "the resolved set must contain the declared header, got {got:?}"
    );
}
