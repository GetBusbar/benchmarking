use super::*;
use crate::manifest::ConfigSetting;

fn base() -> Manifest {
    crate::manifest::test_fixture()
}

fn setting(key: &str, value: &str, reason: ConfigReason) -> ConfigSetting {
    ConfigSetting {
        key: key.into(),
        reason,
        note: value.into(),
    }
}

// THE CASE A NAIVE LINT GETS WRONG. A gateway that needs no configuration is the ideal, not an
// error, so an empty config list must come back clean.
#[test]
fn a_manifest_with_zero_settings_passes() {
    let m = base();
    assert!(m.config.is_empty());
    let findings = lint(&m, &Defaults::new());
    assert!(
        findings.is_empty(),
        "an empty config is the ideal case: {findings:?}"
    );
}

#[test]
fn a_clean_manifest_with_settings_passes() {
    let mut m = base();
    m.config = vec![
        setting("listen.port", "8080", ConfigReason::RigBinding),
        setting("base_url", "http://mock", ConfigReason::UpstreamToMock),
    ];
    let findings = lint(&m, &Defaults::new());
    assert!(findings.is_empty(), "{findings:?}");
}

#[test]
fn restating_a_default_fires_only_when_the_value_matches() {
    let mut defaults = Defaults::new();
    defaults.insert("metrics.enabled".into(), "true".into());

    let mut restated = base();
    restated.config = vec![setting(
        "metrics.enabled",
        "true",
        ConfigReason::RequiredToBoot,
    )];
    let findings = lint(&restated, &defaults);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule, Rule::RestatesDefault);
    assert!(findings[0].message.contains("metrics.enabled"));

    let mut overridden = base();
    overridden.config = vec![setting(
        "metrics.enabled",
        "false",
        ConfigReason::RequiredToBoot,
    )];
    let findings = lint(&overridden, &defaults);
    assert!(
        findings.is_empty(),
        "a value that differs from the default is a real override: {findings:?}"
    );
}

#[test]
fn an_empty_key_fires() {
    let mut m = base();
    m.config = vec![setting("  ", "x", ConfigReason::RequiredToBoot)];
    let findings = lint(&m, &Defaults::new());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule, Rule::EmptyKey);
}

#[test]
fn an_empty_key_is_silent_on_a_clean_setting() {
    let mut m = base();
    m.config = vec![setting("listen.port", "8080", ConfigReason::RigBinding)];
    let findings = lint(&m, &Defaults::new());
    assert!(findings.iter().all(|f| f.rule != Rule::EmptyKey));
}

#[test]
fn an_empty_value_fires() {
    let mut m = base();
    m.config = vec![setting("base_url", "  ", ConfigReason::UpstreamToMock)];
    let findings = lint(&m, &Defaults::new());
    assert!(findings.iter().any(|f| f.rule == Rule::EmptyValue));
    let f = findings
        .iter()
        .find(|f| f.rule == Rule::EmptyValue)
        .unwrap();
    assert!(f.message.contains("base_url"));
    // The note is free text the schema lets a manifest omit: a blank one is worth saying, never a
    // reason to refuse the run (or to advise deleting a setting that may be needed to boot).
    assert_eq!(f.severity, Severity::Warn);
    assert!(!blocks(&findings), "{findings:?}");
}

#[test]
fn an_empty_value_is_silent_on_a_clean_setting() {
    let mut m = base();
    m.config = vec![setting(
        "base_url",
        "http://mock",
        ConfigReason::UpstreamToMock,
    )];
    let findings = lint(&m, &Defaults::new());
    assert!(findings.iter().all(|f| f.rule != Rule::EmptyValue));
}

#[test]
fn duplicate_keys_fire_even_when_both_entries_are_otherwise_valid() {
    let mut m = base();
    m.config = vec![
        setting("listen.port", "8080", ConfigReason::RigBinding),
        setting("listen.port", "8081", ConfigReason::RequiredToBoot),
    ];
    let findings = lint(&m, &Defaults::new());
    let dupes: Vec<_> = findings
        .iter()
        .filter(|f| f.rule == Rule::DuplicateKey)
        .collect();
    assert_eq!(dupes.len(), 1, "{findings:?}");
    assert!(dupes[0].message.contains("listen.port"));
}

#[test]
fn duplicate_keys_are_silent_when_every_key_is_unique() {
    let mut m = base();
    m.config = vec![
        setting("listen.port", "8080", ConfigReason::RigBinding),
        setting("base_url", "http://mock", ConfigReason::UpstreamToMock),
    ];
    let findings = lint(&m, &Defaults::new());
    assert!(findings.iter().all(|f| f.rule != Rule::DuplicateKey));
}

#[test]
fn blocks_is_true_only_when_a_blocking_finding_exists() {
    assert!(!blocks(&[]));
    let f = Finding {
        gateway: "gw".into(),
        key: "k".into(),
        rule: Rule::EmptyKey,
        severity: Severity::Block,
        message: "x".into(),
    };
    assert!(blocks(&[f]));
}
