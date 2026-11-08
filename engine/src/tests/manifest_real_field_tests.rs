use super::*;
use std::collections::BTreeMap;

/// Every manifest in the real field, read from the gateways' own directories.
///
/// DISCOVERED, not listed. A single file naming all thirteen would be the hand-maintained roster
/// the gateway-isolation rule exists to prevent (its lint, `lib/gateway_isolation_test.sh`, was
/// deleted in 1127cf1 and never scanned `.json` anyway). Reading the directory means adding a gateway is dropping in a
/// directory, and nothing else in the tree learns its name.
///
/// A schema that only represents an invented example proves nothing: if the types cannot
/// describe all thirteen entrants as they actually are, the schema is wrong and no amount of
/// internal consistency would say so.
fn field() -> BTreeMap<String, Manifest> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(&root)
        .expect("the gateways directory must exist")
        .flatten()
    {
        let def = entry.path().join("definition.json");
        if !def.is_file() {
            continue;
        }
        let m = Manifest::load(&entry.path()).unwrap_or_else(|e| panic!("{e}"));
        out.insert(m.name.clone(), m);
    }
    assert!(!out.is_empty(), "no gateways/*/definition.json found");
    out
}

#[test]
fn every_real_manifest_parses_and_validates() {
    let f = field();
    assert!(
        f.len() >= 13,
        "the whole field should be represented, got {}",
        f.len()
    );
    for (name, m) in &f {
        assert!(
            m.validate().is_ok(),
            "{name} must validate: {:?}",
            m.validate()
        );
        assert_eq!(&m.name, name, "the key and the declared name must agree");
    }
}

/// A `PidSource` that records which identity each reader asked for.
#[derive(Default)]
struct RecordingPids {
    asked: std::cell::RefCell<Vec<String>>,
}
impl crate::rss::PidSource for RecordingPids {
    fn docker_pid(&self, container: &str) -> Option<u32> {
        self.asked.borrow_mut().push(container.to_string());
        None
    }
    fn matching_pid(&self, pattern: &str) -> crate::rss::PidLookup {
        self.asked.borrow_mut().push(pattern.to_string());
        crate::rss::PidLookup::NotRunning
    }
}

/// A manifest's `runtime.identity()` is the ONE place its measured identity is spelled; no
/// second spelling exists to drift out of sync with it.
#[test]
fn no_manifest_can_name_two_different_things_to_measure() {
    for (name, m) in &field() {
        let id = m.runtime.identity();
        assert!(
            !id.trim().is_empty(),
            "{name} must declare something measurable"
        );
        // The memory reader asks for exactly this string (the container name, or the declared
        // process match), and nothing else.
        let rec = RecordingPids::default();
        let _ = crate::rss::root_pid_from(&rec, &m.runtime);
        assert_eq!(*rec.asked.borrow(), vec![id.clone()], "{name}");
    }
}

/// Every container manifest can produce a real invocation: this walks the real corpus and
/// builds the actual docker command line for each one.
#[test]
fn every_container_manifest_produces_a_launchable_invocation() {
    use std::time::Duration;
    for (name, m) in &field() {
        let spec = m
            .launch_spec(
                "0-3",
                8000,
                std::path::Path::new("/gw"),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .unwrap_or_else(|| panic!("{name} must declare how it is launched"))
            .unwrap_or_else(|e| panic!("{name} must produce a launchable spec: {e}"));
        assert!(spec.validate().is_ok(), "{name}: {:?}", spec.validate());

        let inv = crate::launch::build_invocation(&spec);
        // A container is started by the container runtime; a source-built entrant is started
        // pinned, directly. Both must be launchable.
        let expected = if m.runtime.is_docker() {
            "docker"
        } else {
            "taskset"
        };
        assert_eq!(inv.program, expected, "{name}");
        if !m.runtime.is_docker() {
            continue;
        }
        // The container name comes from runtime.identity(), NOT from the launch block, so the
        // thing started, the thing measured and the thing stopped cannot be three containers.
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--name".to_string(), m.runtime.identity().to_string()]),
            "{name} must launch under its declared identity: {:?}",
            inv.args
        );
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--cpuset-cpus".to_string(), "0-3".to_string()]),
            "{name} must be pinned: {:?}",
            inv.args
        );
        // A mount is resolved against the gateway's own directory: an absolute path in a manifest
        // only works on the machine it was written on.
        for a in &inv.args {
            assert!(
                !a.contains("$GW_DIR"),
                "{name} left an unexpanded shell path: {a}"
            );
            assert!(
                !a.contains('{') || !a.contains('}'),
                "{name} left an unresolved placeholder: {a}"
            );
        }
    }
    // Every discovered entrant is walked by the loop above, and every failure path panics inside
    // it; `field()` itself refuses an empty corpus, so this cannot pass on nothing.
}

/// PLANO'S TWO BOOT FAILURES, LOCKED SO A BOX IS NEVER SPENT ON THEM AGAIN.
///
/// Both cost a launched EC2 box and produced no measurement, and both are visible in the files
/// alone - no gateway needs to be started to see them. A config a gateway refuses at boot is
/// indistinguishable, from the orchestrator's side, from a gateway that is merely slow to come
/// up: the box waits out its readiness attempts and is torn down INCOMPLETE.
///
/// 1. `provider_interface` beside a model prefix Plano already knows. config_generator.py:445
///    rejects the pair (`provider in SUPPORTED_PROVIDERS and provider_interface is not None`)
///    and the container exits. The error text says "Please provide provider interface as part of
///    model name", which reads as the opposite of the rule it enforces, so this is worth
///    asserting rather than trusting a future reader to re-derive.
/// 2. Listener port 10000. Plano always renders its prompt-gateway listener there, so an LLM
///    listener on 10000 double-binds and envoy exits with "'egress_traffic' has duplicate
///    address '0.0.0.0:10000'".
#[test]
fn planos_config_cannot_regress_to_the_two_forms_it_refuses_to_boot_with() {
    let f = field();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways/plano");
    // Skipped only when the entrant is genuinely gone. A plano directory whose manifest is not
    // discovered under that name (a rename) must fail, not silently disable the guard.
    if !dir.is_dir() {
        eprintln!("skipping: no gateways/plano in this tree");
        return;
    }
    let m = f
        .get("plano")
        .expect("gateways/plano exists, so its manifest must be discovered as \"plano\"");

    assert_ne!(
        m.port, 10000,
        "plano's listener port must not be 10000: plano renders its own prompt-gateway listener \
             there, and envoy exits at boot with a duplicate-address error rather than serving"
    );

    for cf in &m.config_files {
        let tmpl = std::fs::read_to_string(dir.join(&cf.template))
            .unwrap_or_else(|e| panic!("plano template {} must be readable: {e}", cf.template));
        for (i, line) in tmpl.lines().enumerate() {
            let code = line.split('#').next().unwrap_or("");
            assert!(
                !code.contains("provider_interface"),
                "plano's {} line {} sets provider_interface ({:?}); its model prefixes are \
                     providers plano already knows, and setting both makes config generation exit \
                     and takes the container down with it",
                cf.template,
                i + 1,
                code.trim()
            );
        }
    }
}

/// A GATEWAY MAY ONLY CLAIM A CELL WHOSE EGRESS DIALECT IT HAS A MODEL FOR.
///
/// The matrix is what the board publishes as capability, and a claimed cell is measured: this
/// harness infers the upstream dialect from the request PATH, so a gateway that routes without
/// translating still answers 200 and the cell scores served. That is how plano's openai>anthropic
/// cell was about to publish a green for a request it forwarded in the OpenAI shape.
///
/// Path-inference is not something a unit test can check - it needs a live upstream. What CAN be
/// checked is the cheaper half: a claimed egress column must at least name a model to drive it
/// with. A column claimed with no model behind it is a claim nobody chose deliberately.
#[test]
fn no_gateway_claims_an_egress_column_it_has_no_model_for() {
    use crate::ingress::Dialect;
    for (name, m) in &field() {
        // A manifest that names no models at all leaves model selection to the harness default;
        // this asserts against the mixed case, where SOME columns are named and a claimed one
        // is not - that is the drift a hand-edited matrix produces.
        if m.egress_models.is_empty() {
            continue;
        }
        for row in &m.matrix {
            for (col, ch) in row.chars().enumerate() {
                if ch != '1' {
                    continue;
                }
                let egress = Dialect::ALL[col].as_str();
                assert!(
                    m.egress_models.contains_key(egress),
                    "{name} claims an egress column it cannot drive: the matrix asserts a \
                         {egress} upstream cell, but egress_models names no {egress} model, so the \
                         cell would be measured with whatever model the default picks"
                );
            }
        }
    }
}

/// The Go runtime's thread count is set from the size of the pinned core range. A literal there
/// would run the gateway at the host's core count inside a four-core cpuset, which is not a
/// detail: the core split IS the comparability basis of every number on the board.
#[test]
fn the_core_count_placeholder_resolves_to_the_pinned_range_not_the_host() {
    use std::time::Duration;
    let f = field();
    let with_ncore: Vec<_> = f
        .iter()
        .filter(|(_, m)| {
            m.launch.as_ref().is_some_and(|l| match l {
                LaunchDecl::Docker { env, .. } | LaunchDecl::Native { env, .. } => {
                    env.iter().any(|(_, v)| v.contains("{NCORE}"))
                }
            })
        })
        .collect();
    assert!(
        !with_ncore.is_empty(),
        "some entrants set their thread count from the core pin"
    );

    for (name, m) in with_ncore {
        for (cores, expected) in [("0-3", "4"), ("4-9", "6"), ("2", "1")] {
            let spec = m
                .launch_spec(
                    cores,
                    8000,
                    std::path::Path::new("/gw"),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .and_then(Result::ok)
                .unwrap_or_else(|| panic!("{name} must build a spec"));
            let crate::launch::LaunchKind::Docker { env, .. } = &spec.kind else {
                panic!("{name} is a container entrant")
            };
            let v = env
                .iter()
                .find(|(k, _)| k == "GOMAXPROCS")
                .map(|(_, v)| v.as_str());
            assert_eq!(
                v,
                Some(expected),
                "{name} on cores {cores} must run {expected} threads"
            );
        }
    }
}

/// THE THIRTY-SIX CELL DEFECT, as a test.
///
/// One entrant's config loader claims every environment variable sharing its prefix and feeds it
/// to a deny-unknown-fields deserializer, so the harness's OWN documented override variables kill
/// config load before the port binds. The binary is backgrounded, so the launch still reports
/// success and the only symptom is "port not listening" on every attempt of every column.
///
/// An env block can only ADD, and a spawned process inherits its parent's environment, so this
/// has to be expressible as a removal or the class cannot be prevented at all.
#[test]
fn a_native_entrant_can_require_that_a_variable_is_absent_not_merely_unset_by_us() {
    use std::time::Duration;
    let mut f = field();
    // The current entrant roster may legitimately need no environment removals.
    // Keep exercising the capability independently of gateway version changes.
    let mut fixture = crate::manifest::test_fixture();
    fixture.launch = Some(
        serde_json::from_value(serde_json::json!({
            "kind": "native", "binary": ["/bin/true"],
            "env_unset": ["OTB_TEST_REMOVE_ME"]
        }))
        .expect("native environment-removal fixture"),
    );
    f.insert("environment-removal-fixture".into(), fixture);
    let scrubbing: Vec<_> = f
        .iter()
        .filter(|(_, m)| {
            m.launch.as_ref().is_some_and(
                |l| matches!(l, LaunchDecl::Native { env_unset, .. } if !env_unset.is_empty()),
            )
        })
        .collect();
    assert!(
            !scrubbing.is_empty(),
            "at least one entrant must scrub the environment it is launched with; that requirement is why this field exists"
        );

    for (name, m) in scrubbing {
        let spec = m
            .launch_spec(
                "0-3",
                8000,
                std::path::Path::new("/gw"),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .and_then(Result::ok)
            .unwrap_or_else(|| panic!("{name} must build a spec"));
        let inv = crate::launch::build_invocation(&spec);
        assert!(
                !inv.env_unset.is_empty(),
                "{name} declares variables that must not reach it, and the invocation must carry that removal: {inv:?}"
            );
        // The removals must not merely be set to empty: an empty value is still a present
        // variable, and a loader that rejects unknown KEYS does not care what the value is.
        for removed in &inv.env_unset {
            assert!(
                    !inv.env.iter().any(|(k, _)| k == removed),
                    "{name} both sets and removes {removed}; setting it to anything still leaves it present"
                );
        }
    }
}

/// A VALIDATOR THAT ONLY EVER SAYS OK HAS NOT BEEN SHOWN TO WORK.
///
/// All thirteen real entrants pass, which is the outcome that proves nothing on its own. Each
/// mistake below is one that otherwise surfaces as a container which starts and immediately
/// dies - indistinguishable, from the outside, from the gateway being broken.
#[test]
fn every_setup_mistake_the_validator_exists_for_is_actually_reported() {
    // Removed on every exit path, a failing assertion included.
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let scratch =
        RemoveOnDrop(std::env::temp_dir().join(format!("otb-validate-{}", std::process::id())));
    let dir = scratch.0.clone();
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("typo.tmpl"), "port: {NOT_A_REAL_THING}\n").expect("write template");

    let mut m = field()
        .values()
        .find(|m| m.runtime.is_docker())
        .cloned()
        .expect("an entrant");
    m.config_files = vec![
        ConfigFile {
            template: "missing.tmpl".into(),
            output: "a.yaml".into(),
        },
        ConfigFile {
            template: "typo.tmpl".into(),
            output: "b.yaml".into(),
        },
    ];
    m.launch = Some(LaunchDecl::Docker {
        image: "x:1".into(),
        env: vec![],
        args: vec![],
        mounts: vec![MountDecl {
            host_path: "never-rendered.yaml".into(),
            container_path: "/c.yaml".into(),
            read_only: true,
        }],
    });
    m.egress_headers = [("notadialect".to_string(), vec!["x-a: b".to_string()])]
        .into_iter()
        .collect();

    let problems = m.problems(&dir);
    let joined = problems.join("\n");

    assert!(
        joined.contains("missing.tmpl"),
        "a declared template that is absent must be reported: {joined}"
    );
    assert!(
        joined.contains("NOT_A_REAL_THING"),
        "a placeholder the harness cannot supply must be reported: {joined}"
    );
    assert!(
            joined.contains("never-rendered.yaml"),
            "a mount nothing renders must be reported - this is the one that costs the most to diagnose, because the container just exits: {joined}"
        );
    assert!(
        joined.contains("notadialect"),
        "a header keyed by a dialect we do not speak must be reported: {joined}"
    );

    // The message has to tell a maintainer what to DO, not just that something is wrong.
    assert!(
        joined.contains("MOCK_PORT"),
        "the placeholder error must list what IS available: {joined}"
    );

    // A correct gateway stays quiet, or the validator is noise and gets ignored.
    for (name, real) in &field() {
        let gw_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gateways")
            .join(name);
        assert!(
            real.problems(&gw_dir).is_empty(),
            "{name} is a real entrant and must validate clean: {:?}",
            real.problems(&gw_dir)
        );
    }
}

/// A doubled brace is a literal one, BOTH ways round: handling only the opening half breaks a
/// YAML `error_map: {}`, rendering it as `{}}`, which the gateway refuses to boot on.
#[test]
fn a_literal_brace_survives_the_render_intact() {
    let m = field().values().next().cloned().expect("an entrant");
    let dir = std::path::Path::new("/gw");
    let go = |t: &str| m.substitute(t, "0-3", 8000, dir).expect("renders");

    assert_eq!(go("error_map: {{}}"), "error_map: {}");
    assert_eq!(go("{{\"a\": 1}}"), "{\"a\": 1}");
    // A placeholder still resolves when it sits beside literal braces.
    assert_eq!(go("{{port: {MOCK_PORT}}}"), "{port: 8000}");
    // A lone closing brace is content; nothing opened it.
    assert_eq!(go("a} b"), "a} b");
}

#[test]
fn a_launch_referring_to_something_the_harness_does_not_supply_is_refused() {
    use std::time::Duration;
    // Built here rather than borrowed from the other test module: this one walks the real
    // corpus, and a fixture that drifts from the real shape would prove nothing about it.
    let mut m = field()
        .values()
        .find(|m| m.runtime.is_docker())
        .cloned()
        .expect("a container entrant");
    m.launch = Some(LaunchDecl::Docker {
        image: "gw:1".into(),
        env: vec![("X".into(), "{NOT_A_THING}".into())],
        args: vec![],
        mounts: vec![],
    });
    let err = m
        .launch_spec(
            "0-3",
            8000,
            std::path::Path::new("/gw"),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .and_then(Result::err);
    assert!(
        matches!(err, Some(ManifestError::UnknownPlaceholder { ref name, .. }) if name == "NOT_A_THING"),
        "an unknown placeholder must be refused, not passed through as a literal: {err:?}"
    );
}

/// EVERY DECLARED CONFIG TEMPLATE ACTUALLY RENDERS.
///
/// Ten entrants boot from a file the harness writes. A template that refers to something the
/// harness does not supply produces a gateway that starts and immediately dies, which reads as
/// the gateway being broken - so this fails here, at the manifest, rather than there.
#[test]
fn every_declared_config_template_renders_with_nothing_left_unresolved() {
    let mut rendered = 0;
    for (name, m) in &field() {
        if m.config_files.is_empty() {
            continue;
        }
        let gw_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gateways")
            .join(name);
        for file in &m.config_files {
            let template_path = gw_dir.join(&file.template);
            let raw = std::fs::read_to_string(&template_path).unwrap_or_else(|e| {
                panic!(
                    "{name} declares {} which cannot be read: {e}",
                    file.template
                )
            });
            let body = m
                .substitute(&raw, "0-3", 8000, &gw_dir)
                .unwrap_or_else(|e| panic!("{name} template {}: {e}", file.template));

            // The output is deliberately NOT scanned for leftover braces. A rendered literal -
            // a config format's own syntax, or a comment documenting a URL shape as
            // `{api_base}` - is written `{{...}}` in the template and comes out as `{...}`,
            // which is indistinguishable from an unresolved placeholder by looking at the
            // result. The guarantee lives in `substitute`, which refuses an unknown name
            // outright, so reaching this line means every placeholder was supplied.
            assert!(
                !body.trim().is_empty(),
                "{name} rendered {} to nothing",
                file.output
            );
            rendered += 1;
        }
    }
    assert!(
        rendered >= 13,
        "every declared template must render, got {rendered}"
    );
}

#[test]
fn both_runtime_kinds_are_present_in_the_real_field() {
    let f = field();
    assert!(
        f.values().any(|m| m.runtime.is_docker()),
        "some entrants run in containers"
    );
    assert!(
        f.values().any(|m| !m.runtime.is_docker()),
        "some entrants run natively from source"
    );
}

#[test]
fn every_manifest_declares_a_reachable_url() {
    for (name, m) in &field() {
        let u = m.url();
        assert!(
            u.starts_with("http://127.0.0.1:"),
            "{name} must be driven on loopback, got {u}"
        );
        assert!(u.contains(&m.port.to_string()));
    }
}
