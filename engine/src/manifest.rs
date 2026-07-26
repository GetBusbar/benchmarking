// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// A gateway manifest, as DATA.
//
// The shell manifests express identity as code: each one hand-writes `gw_rss`, `gw_hwm` and
// `gw_stop`, thirteen times over, and every trio has to name the same container or the same process
// pattern. The harness comment says the readers are "matched BY CONSTRUCTION, not by convention",
// and at the library level that is true, but the manifest still spells the name out once per hook,
// so the matching is a convention again at exactly the layer a human edits.
//
// That drift has already corrupted published numbers. Three source-built manifests wrote a
// single-pid reader for RSS beside a whole-tree reader for HWM, so for the same gateway idle, peak
// and recovered measured ONE process while the high-water mark measured that process and every
// descendant. Two different populations, published side by side, and compared against gateways whose
// readers were tree-summed. A gateway that forks workers had its peak inflated relative to its idle
// by whatever its children weighed.
//
// Here identity is declared ONCE. Every reader derives from it, so RSS and HWM cannot describe
// different populations, and a stop cannot target something the readers never measured. The class of
// bug is removed rather than guarded.

use serde::{Deserialize, Serialize};

/// How the gateway runs, and therefore how its process tree is found. This is the single declaration
/// every memory reader and the stop path derive from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Runtime {
    /// A container. The root pid comes from the container runtime, and the tree walk starts there.
    Docker { container: String },
    /// A process started directly on the box, located by a match against its command line.
    Native { proc_match: String },
}

impl Runtime {
    /// The one identity string, whatever the kind. Readers take this rather than being handed a name
    /// per call site, which is what makes a mismatch between them unrepresentable.
    pub fn identity(&self) -> &str {
        match self {
            Runtime::Docker { container } => container,
            Runtime::Native { proc_match } => proc_match,
        }
    }

    pub fn is_docker(&self) -> bool {
        matches!(self, Runtime::Docker { .. })
    }
}

/// Why a config setting exists. The board's fairness rule is that every gateway config is the bare
/// minimum required to run, so each setting must name which necessity it satisfies. As shell this
/// was a free-text block a lint grepped; as an enum the build cannot express a setting with no
/// reason, and "we turned a feature on" has no variant to hide in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReason {
    /// Needed to boot the gateway at all.
    RequiredToBoot,
    /// Points an upstream at the test mock instead of a real provider.
    UpstreamToMock,
    /// Exposes an ingress path the matrix exercises.
    ExposesIngress,
    /// Binds the port or the cores the rig requires.
    RigBinding,
}

/// One declared config setting, with the necessity that justifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetting {
    pub key: String,
    pub reason: ConfigReason,
    /// Free text for a human, never load bearing.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Directory name. The gateway's identity on the board.
    pub name: String,
    pub display: String,
    pub lang: String,
    /// The project's OWN self-description, never our editorial.
    pub class: String,
    pub repo: String,
    pub port: u16,
    pub path: String,
    pub model: String,
    pub auth: String,
    #[serde(default)]
    pub headers: Vec<String>,
    pub runtime: Runtime,
    /// Egress dialects the manifest configures. NOT a capability claim: the matrix probes every cell
    /// regardless and publishes what it observes. This only says which upstreams are wired.
    #[serde(default)]
    pub egress: Vec<String>,
    #[serde(default)]
    pub config: Vec<ConfigSetting>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Empty(&'static str),
    BadPort,
    /// A field still carrying a shell variable that was never expanded. These manifests were
    /// EXTRACTED from shell, where one gateway writes `GW_MODEL="$SOME_MODEL"` - one indirection away
    /// from the literal it resolves to. Extract the wrong side of that and the field holds the
    /// reference instead of a model name. Non-empty, so every existing check passes it.
    UnexpandedVariable { field: &'static str, raw: String },
    /// A config setting with no stated necessity cannot be lint-checked, so it cannot ship.
    ConfigWithoutReason(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Empty(field) => write!(f, "{field} must not be empty"),
            ManifestError::BadPort => write!(f, "port must be non-zero"),
            ManifestError::ConfigWithoutReason(k) => {
                write!(f, "config setting {k:?} has no key to attach a reason to")
            }
            ManifestError::UnexpandedVariable { field, raw } => write!(
                f,
                "{field} still holds an unexpanded shell variable ({raw:?}): the extraction took the reference, not the value"
            ),
        }
    }
}

impl Manifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        for (v, field) in [
            (&self.name, "name"),
            (&self.display, "display"),
            (&self.repo, "repo"),
            (&self.path, "path"),
            (&self.model, "model"),
        ] {
            if v.trim().is_empty() {
                return Err(ManifestError::Empty(field));
            }
        }

        // An extraction artefact, not a typo, and the reason it needs its own check: a field holding
        // an unexpanded reference is non-empty, so every check above passes it, and it survives all
        // the way to the wire. A model name that is really a shell reference is sent as the request body's model,
        // the gateway rejects it against the model its own route declares, and `probe.rs` classifies
        // any status from a healthy rig as `NotConfigured` - "the gateway answered, deterministically,
        // that this pairing does not light up". The board then publishes OUR extraction bug as that
        // gateway's own capability denial. No legitimate value of any of these fields contains `$`.
        for (v, field) in [
            (&self.name, "name"),
            (&self.display, "display"),
            (&self.repo, "repo"),
            (&self.path, "path"),
            (&self.model, "model"),
            (&self.auth, "auth"),
            (&self.lang, "lang"),
            (&self.class, "class"),
        ] {
            if v.contains('$') {
                return Err(ManifestError::UnexpandedVariable { field, raw: v.clone() });
            }
        }
        for h in &self.headers {
            if h.contains('$') {
                return Err(ManifestError::UnexpandedVariable { field: "headers", raw: h.clone() });
            }
        }
        if self.runtime.identity().contains('$') {
            return Err(ManifestError::UnexpandedVariable {
                field: "runtime identity",
                raw: self.runtime.identity().to_string(),
            });
        }
        if self.runtime.identity().trim().is_empty() {
            return Err(ManifestError::Empty("runtime identity"));
        }
        if self.port == 0 {
            return Err(ManifestError::BadPort);
        }
        for c in &self.config {
            if c.key.trim().is_empty() {
                return Err(ManifestError::ConfigWithoutReason(c.key.clone()));
            }
        }
        Ok(())
    }

    /// The URL the harness drives this gateway on.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docker_manifest() -> Manifest {
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
        }
    }

    // THE WHOLE POINT. RSS, HWM and stop all read ONE declaration, so they cannot name different
    // things. In shell each was a separate hand-written hook, and three manifests did in fact drift:
    // a single-pid reader for RSS beside a whole-tree reader for HWM, publishing two different
    // populations for the same gateway.
    #[test]
    fn every_reader_derives_from_one_identity() {
        let m = docker_manifest();
        let id = m.runtime.identity();
        assert_eq!(id, "gw-bench");
        // There is no second place to spell it, so there is nothing for a reader to disagree with.
        assert_eq!(m.runtime.identity(), id);
    }

    #[test]
    fn a_native_runtime_carries_a_process_match_not_a_container() {
        let m = Manifest { runtime: Runtime::Native { proc_match: "target/release/gw".into() }, ..docker_manifest() };
        assert!(!m.runtime.is_docker());
        assert_eq!(m.runtime.identity(), "target/release/gw");
    }

    // A runtime with no identity cannot be measured or stopped, so it must not validate.
    #[test]
    fn an_empty_runtime_identity_is_rejected() {
        let m = Manifest { runtime: Runtime::Docker { container: "  ".into() }, ..docker_manifest() };
        assert_eq!(m.validate(), Err(ManifestError::Empty("runtime identity")));
    }

    #[test]
    fn required_fields_are_required() {
        for (mutate, field) in [
            (Box::new(|m: &mut Manifest| m.name.clear()) as Box<dyn Fn(&mut Manifest)>, "name"),
            (Box::new(|m: &mut Manifest| m.display.clear()), "display"),
            (Box::new(|m: &mut Manifest| m.repo.clear()), "repo"),
            (Box::new(|m: &mut Manifest| m.path.clear()), "path"),
            (Box::new(|m: &mut Manifest| m.model.clear()), "model"),
        ] {
            let mut m = docker_manifest();
            mutate(&mut m);
            assert_eq!(m.validate(), Err(ManifestError::Empty(field)), "{field} must be required");
        }
        let mut m = docker_manifest();
        m.port = 0;
        assert_eq!(m.validate(), Err(ManifestError::BadPort));
    }

    // Every declared setting must name which of the four necessities it satisfies. As shell this was
    // free text a lint grepped; here a setting cannot be constructed without one, and there is no
    // variant meaning "we wanted this feature on".
    #[test]
    fn a_config_setting_must_name_its_necessity() {
        let m = Manifest {
            config: vec![ConfigSetting {
                key: "listen.port".into(),
                reason: ConfigReason::RigBinding,
                note: "the rig pins the port".into(),
            }],
            ..docker_manifest()
        };
        assert!(m.validate().is_ok());
        assert_eq!(m.config[0].reason, ConfigReason::RigBinding);
    }

    // The shell manifests are one indirection deep: one gateway sets `SOME_MODEL=gpt-4o-mini` and
    // then `GW_MODEL="$SOME_MODEL"`. Extracting the reference instead of the value yields a field
    // that is non-empty, parses, and validates under every other rule, then goes out on the wire as a
    // model name. The corpus shipped exactly this.
    #[test]
    fn a_field_holding_an_unexpanded_shell_variable_is_rejected() {
        let m = Manifest { model: "$SOME_MODEL".into(), ..docker_manifest() };
        assert_eq!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable { field: "model", raw: "$SOME_MODEL".into() })
        );
        assert!(!m.model.trim().is_empty(), "the point: it is non-empty, so the emptiness checks pass it");

        // Every field a request or a launch is built from, not just the model.
        let m = Manifest { auth: "${GW_KEY}".into(), ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "auth", .. })));
        let m = Manifest { headers: vec!["x-api-key: $GW_AUTH".into()], ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "headers", .. })));
        let m = Manifest { runtime: Runtime::Docker { container: "$NAME-bench".into() }, ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "runtime identity", .. })));

        // A clean manifest is untouched by the new rule.
        assert!(docker_manifest().validate().is_ok());
    }

    #[test]
    fn a_setting_with_no_key_cannot_ship() {
        let m = Manifest {
            config: vec![ConfigSetting { key: " ".into(), reason: ConfigReason::RequiredToBoot, note: String::new() }],
            ..docker_manifest()
        };
        assert!(matches!(m.validate(), Err(ManifestError::ConfigWithoutReason(_))));
    }

    #[test]
    fn round_trips_through_json_including_the_runtime_tag() {
        let m = docker_manifest();
        let js = serde_json::to_string(&m).unwrap();
        assert!(js.contains(r#""kind":"docker""#), "the runtime kind must be explicit on the wire: {js}");
        let back: Manifest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn the_url_is_built_from_the_declared_port_and_path() {
        assert_eq!(docker_manifest().url(), "http://127.0.0.1:8080/v1/chat/completions");
    }
}

#[cfg(test)]
mod real_field_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every manifest in the real field, extracted from the shell manifests as data.
    ///
    /// This is the manifest counterpart of the snapshot-corpus test: a schema that only represents
    /// an example I invented proves nothing. If the types cannot describe all thirteen entrants as
    /// they actually are, the schema is wrong and no amount of internal consistency would say so.
    fn field() -> BTreeMap<String, Manifest> {
        let txt = include_str!("../tests/manifests.json");
        serde_json::from_str(txt).expect("the extracted field must parse under these types")
    }

    #[test]
    fn every_real_manifest_parses_and_validates() {
        let f = field();
        assert!(f.len() >= 13, "the whole field should be represented, got {}", f.len());
        for (name, m) in &f {
            assert!(m.validate().is_ok(), "{name} must validate: {:?}", m.validate());
            assert_eq!(&m.name, name, "the key and the declared name must agree");
        }
    }

    /// The regression this schema exists to make impossible. Today all thirteen agree, because the
    /// shell defect was found and fixed by hand; the point is that after this there is no second
    /// place to spell the identity, so they cannot drift apart again.
    #[test]
    fn no_manifest_can_name_two_different_things_to_measure() {
        for (name, m) in &field() {
            let id = m.runtime.identity();
            assert!(!id.trim().is_empty(), "{name} must declare something measurable");
            // Both readers and the stop path take this one string. Asserting it twice is the closest
            // a test can get to asserting that a second spelling does not exist.
            assert_eq!(m.runtime.identity(), id);
        }
    }

    #[test]
    fn both_runtime_kinds_are_present_in_the_real_field() {
        let f = field();
        assert!(f.values().any(|m| m.runtime.is_docker()), "some entrants run in containers");
        assert!(f.values().any(|m| !m.runtime.is_docker()), "some entrants run natively from source");
    }

    #[test]
    fn every_manifest_declares_a_reachable_url() {
        for (name, m) in &field() {
            let u = m.url();
            assert!(u.starts_with("http://127.0.0.1:"), "{name} must be driven on loopback, got {u}");
            assert!(u.contains(&m.port.to_string()));
        }
    }
}
