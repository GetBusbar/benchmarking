// SPDX-License-Identifier: Apache-2.0
use std::process::Command;

#[test]
fn provenance_queries_exit_without_starting_the_mock_listener() {
    for (query, expected) in [
        ("engine-commit", env!("OTB_ENGINE_COMMIT")),
        ("build-commit", env!("OTB_SOURCE_COMMIT")),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_mock"))
            .arg(query)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), expected);
        assert!(!expected.is_empty());
    }
}
