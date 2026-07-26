// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Config necessity, as a lint over Manifest values.
//
// The shell counterpart, lib/gateway_config_lint.sh, enforces this standard: every gateway config is
// the bare minimum required to run. A setting appears only if it is required to boot, to point an
// upstream at the test mock, to expose an ingress path the matrix drives, or to bind the port or
// cores the rig requires. We do not turn features on, we do not turn them off, and we do not restate
// a default: a restated default is still a line we wrote, and it silently becomes an override the
// day upstream changes what the default is.
//
// The shell lint reads a free-text GW_CONFIG_WHY block and greps for one of four words. Here the
// four necessities are ConfigReason, an enum a ConfigSetting cannot be built without, so rule 1 below
// is not really a check, it is a comment explaining why the type already made the violation
// unrepresentable. What the type cannot catch is: a restated default, an empty key or value, and two
// settings fighting over one key. Those are what this module checks.

use crate::manifest::{ConfigReason, Manifest};
use std::collections::BTreeMap;

/// How serious a finding is. Only `Block` should stop a run; the others are worth surfacing but the
/// benchmark can still proceed (for example on a manifest whose config nobody has re-reviewed yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth a human's attention, does not by itself invalidate a run.
    Warn,
    /// The fairness claim is compromised. Do not run, or do not publish, until fixed.
    Block,
}

/// One rule the lint checked, so a finding can be filtered or explained without parsing its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// A setting's value equals the application's own documented default.
    RestatesDefault,
    /// A setting's key is empty (after trimming).
    EmptyKey,
    /// A setting's value is empty (after trimming).
    EmptyValue,
    /// Two settings in the same manifest claim the same key.
    DuplicateKey,
}

/// One thing wrong with one setting in one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub gateway: String,
    pub key: String,
    pub rule: Rule,
    pub severity: Severity,
    pub message: String,
}

/// Whether any finding in the list should stop a run. Everything this lint checks is a fairness
/// defect rather than a style nit, so today every rule blocks; this stays a function rather than a
/// blanket `!findings.is_empty()` call site so a future Warn-only rule does not have to touch callers.
pub fn blocks(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Block)
}

/// `key -> value` a gateway is known to ship with out of the box, pulled from the gateway's own
/// docs. Passed in rather than frozen into this lint, so the check is data-driven and does not go
/// stale the day upstream changes what the default is.
pub type Defaults = BTreeMap<String, String>;

/// Lint one manifest's declared config against the necessity standard. `defaults` is the known
/// default value for any setting key this gateway is documented to ship with; a setting whose value
/// matches its entry is a restated default rather than an override.
///
/// A manifest with an empty `config` is the ideal case, not a violation, so it returns no findings:
/// the bare minimum required to run was zero settings, which is exactly what this lint exists to
/// protect.
pub fn lint(manifest: &Manifest, defaults: &Defaults) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();

    for setting in &manifest.config {
        let key = setting.key.trim();
        // ConfigSetting carries no dedicated value field: the generated config file a gateway
        // actually reads lives outside this struct, and `note` is documented as free text for a
        // human, never load bearing. It is nonetheless the only place a recorded value can live
        // today, so this lint reads it from there. A struct with a real value field would be the
        // cleaner home for this check; until one exists, treat this as best-effort rather than
        // authoritative on manifests that leave `note` blank for a setting that does carry a value.
        let value = setting.note.trim();

        // Rule 1 is enforced by the type, not by this function: `ConfigSetting::reason` is a
        // `ConfigReason`, one of RequiredToBoot / UpstreamToMock / ExposesIngress / RigBinding, and
        // there is no way to construct a setting without choosing one. A build that compiles cannot
        // hold a setting whose necessity is "we wanted the feature on".
        let _: ConfigReason = setting.reason;

        if key.is_empty() {
            findings.push(Finding {
                gateway: manifest.name.clone(),
                key: setting.key.clone(),
                rule: Rule::EmptyKey,
                severity: Severity::Block,
                message: format!(
                    "{}: a config setting with key {:?} has no key once trimmed, so it cannot be claimed or acted on",
                    manifest.name, setting.key
                ),
            });
            // No key to index duplicates or defaults by, so there is nothing further to check on
            // this entry.
            continue;
        }

        if value.is_empty() {
            findings.push(Finding {
                gateway: manifest.name.clone(),
                key: key.to_string(),
                rule: Rule::EmptyValue,
                severity: Severity::Block,
                message: format!(
                    "{}: setting {:?} has an empty value, so it configures nothing and should be deleted",
                    manifest.name, key
                ),
            });
        }

        *seen.entry(key).or_insert(0) += 1;

        if let Some(default) = defaults.get(key) {
            if !value.is_empty() && value == default.trim() {
                findings.push(Finding {
                    gateway: manifest.name.clone(),
                    key: key.to_string(),
                    rule: Rule::RestatesDefault,
                    severity: Severity::Block,
                    message: format!(
                        "{}: setting {:?} writes {:?}, which is this gateway's own default, delete it before it silently becomes an override",
                        manifest.name, key, value
                    ),
                });
            }
        }
    }

    for (key, count) in seen {
        if count > 1 {
            findings.push(Finding {
                gateway: manifest.name.clone(),
                key: key.to_string(),
                rule: Rule::DuplicateKey,
                severity: Severity::Block,
                message: format!(
                    "{}: key {:?} is set {} times, two settings fighting over one key means one silently wins",
                    manifest.name, key, count
                ),
            });
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ConfigSetting, Runtime};

    fn base() -> Manifest {
        Manifest {
            name: "gw".into(),
            display: "GW".into(),
            lang: "Rust".into(),
            class: "AI gateway".into(),
            repo: "https://example.invalid/gw".into(),
            port: 8080,
            path: "/v1/chat/completions".into(),
            model: "m".into(),
            auth: "dummy".into(),
            headers: vec![],
            runtime: Runtime::Docker { container: "gw-bench".into() },
            egress: vec!["openai".into()],
            config: vec![],
            launch: None,
            config_files: vec![],
            constants: Default::default(),
            egress_headers: Default::default(),
        }
    }

    fn setting(key: &str, value: &str, reason: ConfigReason) -> ConfigSetting {
        ConfigSetting { key: key.into(), reason, note: value.into() }
    }

    // THE CASE A NAIVE LINT GETS WRONG. A gateway that needs no configuration is the ideal, not an
    // error, so an empty config list must come back clean.
    #[test]
    fn a_manifest_with_zero_settings_passes() {
        let m = base();
        assert!(m.config.is_empty());
        let findings = lint(&m, &Defaults::new());
        assert!(findings.is_empty(), "an empty config is the ideal case: {findings:?}");
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
        restated.config = vec![setting("metrics.enabled", "true", ConfigReason::RequiredToBoot)];
        let findings = lint(&restated, &defaults);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, Rule::RestatesDefault);
        assert!(findings[0].message.contains("metrics.enabled"));

        let mut overridden = base();
        overridden.config = vec![setting("metrics.enabled", "false", ConfigReason::RequiredToBoot)];
        let findings = lint(&overridden, &defaults);
        assert!(findings.is_empty(), "a value that differs from the default is a real override: {findings:?}");
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
        let f = findings.iter().find(|f| f.rule == Rule::EmptyValue).unwrap();
        assert!(f.message.contains("base_url"));
    }

    #[test]
    fn an_empty_value_is_silent_on_a_clean_setting() {
        let mut m = base();
        m.config = vec![setting("base_url", "http://mock", ConfigReason::UpstreamToMock)];
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
        let dupes: Vec<_> = findings.iter().filter(|f| f.rule == Rule::DuplicateKey).collect();
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
}

#[cfg(test)]
mod real_field_tests {
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
            if let Ok(m) = Manifest::load(&entry.path()) {
                out.insert(m.name.clone(), m);
            }
        }
        out
    }

    // No defaults table is claimed for the real field here: this test proves the lint runs over
    // every entrant without panicking, it does not assert the real field is clean. What it says is
    // reported by the caller of this module, not asserted away in a test.
    #[test]
    fn the_lint_runs_over_every_real_manifest_without_panicking() {
        let f = field();
        assert!(f.len() >= 13, "the whole field should be represented, got {}", f.len());
        for (name, m) in &f {
            let findings = lint(m, &Defaults::new());
            // Every entrant gets a result, whether or not it is empty.
            let _ = findings;
            assert_eq!(&m.name, name);
        }
    }
}
