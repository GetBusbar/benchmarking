use super::*;
use std::collections::BTreeMap as Map;

/// Discovered from the gateways' own directories, not listed in one file. Same reason as
/// `manifest::real_field_tests::field`: a single file naming every entrant is the roster the
/// isolation lint exists to prevent.
fn field() -> Map<String, Manifest> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
    let mut out = Map::new();
    for entry in std::fs::read_dir(&root).expect("gateways dir").flatten() {
        let def = entry.path().join("definition.json");
        if !def.is_file() {
            continue;
        }
        // A manifest that does not load is a failure of this test, not an entrant to skip.
        let m = Manifest::load(&entry.path())
            .unwrap_or_else(|e| panic!("{} does not load: {e}", def.display()));
        out.insert(m.name.clone(), m);
    }
    out
}

// No defaults table is claimed for the real field here: this test proves every entrant loads and
// the lint runs over it, and that nothing it finds would BLOCK a run (`otb run` refuses on a
// blocking finding, so one here is an entrant the field cannot measure). Warnings are reported by
// the caller of this module, not asserted away in a test.
#[test]
fn the_lint_runs_over_every_real_manifest_without_panicking() {
    let f = field();
    assert!(
        f.len() >= 13,
        "the whole field should be represented, got {}",
        f.len()
    );
    for m in f.values() {
        // Every entrant gets a result; what it says is reported by the caller, but nothing in the
        // real field may BLOCK a run, or `otb run` would refuse that gateway.
        let findings = lint(m, &Defaults::new());
        assert!(!blocks(&findings), "{}: {findings:?}", m.name);
    }
}
