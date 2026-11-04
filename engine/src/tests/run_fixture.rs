use super::*;

/// A minimal `RunConfig` for tests across this crate, mirroring `manifest::test_fixture` (one place
/// to add a field, not one per call site). `matrix`/`untestable_cells` empty: undeclared, so every
/// cell is probed unless a test overrides them via struct-update syntax.
pub(crate) fn test_fixture(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
    RunConfig {
        // The test fixture pins nothing, so it declares no cores: utilisation is absent rather than
        // a figure borrowed from whatever else the test host happens to be running.
        gw_cores: String::new(),
        gateway_addr: gw,
        mock_addr: mock,
        model: "m".into(),
        egress_models: Default::default(),
        auth: "dummy".into(),
        dialects: vec![Dialect::Openai],
        sweep_duration_s: 1,
        probe_timeout: Duration::from_secs(2),
        load_cores: None,
        static_headers: Vec::new(),
        egress_headers: Default::default(),
        runtime: crate::manifest::Runtime::Native {
            proc_match: "test-fixture".into(),
        },
        declared_path: String::new(),
        cell_paths: Default::default(),
        matrix: Vec::new(),
        matrix_note: String::new(),
        untestable_cells: Vec::new(),
        untestable_note: String::new(),
        relaunch: None,
        relaunch_commands: Vec::new(),
        relaunch_launcher: Default::default(),
    }
}
