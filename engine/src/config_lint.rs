// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Config necessity, as a lint over Manifest values.
//
// The standard (the shell lint that also enforced it, lib/gateway_config_lint.sh, was deleted in
// 1127cf1; this module is what remains): every gateway config is the bare minimum required to run. A setting appears only if it is required to boot, to point an
// upstream at the test mock, to expose an ingress path the matrix drives, or to bind the port or
// cores the rig requires. We do not turn features on, we do not turn them off, and we do not restate
// a default: a restated default is still a line we wrote, and it silently becomes an override the
// day upstream changes what the default is.
//
// The deleted shell lint read a free-text GW_CONFIG_WHY block and greps for one of four words. Here the
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
    /// A setting records no value (its `note` is empty after trimming). A WARNING, not a block: see the
    /// rule's own comment in `lint`.
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

        // A WARNING, NOT A BLOCK. The "value" here is `note`, which the manifest schema documents as
        // free text for a human, never load bearing, and lets a manifest omit. A blank note says the
        // manifest did not RECORD a value - not that the setting configures nothing - so blocking a
        // run on it, with advice to delete a setting that may be required to boot, was the lint
        // overreaching its own evidence. What a blank note does cost is real and is said: the
        // restated-default check below cannot run on it.
        if value.is_empty() {
            findings.push(Finding {
                gateway: manifest.name.clone(),
                key: key.to_string(),
                rule: Rule::EmptyValue,
                severity: Severity::Warn,
                message: format!(
                    "{}: setting {:?} records no value in its note, so whether it restates a default \
                     cannot be checked",
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
#[path = "tests/config_lint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/config_lint_real_field_tests.rs"]
mod real_field_tests;
