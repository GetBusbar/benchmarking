use super::*;

/// A minimal, valid `Manifest` for tests across this crate (config_lint.rs, suite.rs, and this
/// module's own tests all built their own copy of this same field list; one adding a field meant
/// three edits agreeing by hand). Callers override only the fields their test cares about with
/// struct-update syntax (`Manifest { egress: vec![...], ..test_fixture() }`), so a new field is one
/// edit here, not one per call site.
pub(crate) fn test_fixture() -> Manifest {
    Manifest {
        name: "gw".into(),
        display: "GW".into(),
        lang: "Rust".into(),
        class: "AI gateway".into(),
        repo: "https://example.invalid/gw".into(),
        port: 8080,
        path: "/v1/chat/completions".into(),
        model: "m".into(),
        egress_models: Default::default(),
        auth: "dummy".into(),
        headers: vec![],
        runtime: Runtime::Docker {
            container: "gw-bench".into(),
            run_scope: None,
        },
        egress: vec!["openai".into()],
        matrix: vec![],
        matrix_note: String::new(),
        untestable: vec![],
        untestable_note: String::new(),
        commands: vec![],
        cell_paths: Default::default(),
        config: vec![],
        launch: None,
        config_files: vec![],
        constants: Default::default(),
        egress_headers: Default::default(),
    }
}
