// A HEADER WITH NO COLON WOULD NEVER REACH THE WIRE, SILENTLY.
//
// `headers_for` splits each declared line on ':' with no else arm, so a line missing its colon is
// dropped without a word and the gateway is measured WITHOUT a header its own manifest declared.
// A gateway missing its auth header answers 401 on every probe and publishes as one that serves
// nothing - indistinguishable from a gateway that genuinely does not. `otb validate` reports a
// gateway ok whenever problems() is empty, so this had to become a problem to be caught at all.
#[test]
fn a_header_line_without_a_colon_is_a_validate_problem() {
    let dir = std::env::temp_dir();
    let mut m = test_fixture();
    m.headers = vec!["x-api-key bench-token".to_string()];
    let found = m.problems(&dir);
    assert!(
        found
            .iter()
            .any(|p| p.contains("no colon") && p.contains("x-api-key")),
        "a colonless header must be reported, got: {found:?}"
    );

    // The wire-shaped form is fine, and so is a value that itself contains a colon (a URL).
    m.headers = vec![
        "x-api-key: bench-token".to_string(),
        "x-base-url: http://127.0.0.1:8000".to_string(),
    ];
    let found = m.problems(&dir);
    assert!(
        !found.iter().any(|p| p.contains("no colon")),
        "a properly formed header must not be reported, got: {found:?}"
    );
}

use super::*;
use std::time::Duration;

fn docker_manifest() -> Manifest {
    test_fixture()
}

// THE UNEARNED RED THIS PREVENTS. A cell path built from a declared constant reached the probe
// as the literal `{NAME}` while the gateway's config template rendered the real route, so the
// probe asked for a path made of our own template syntax, got a truthful 404, and the board
// published a declared-capable pairing as one the gateway does not serve. Nesting is exercised
// because the real case is a path constant that refers to a model constant.
#[test]
fn a_cell_path_resolves_its_placeholders_before_it_ever_reaches_a_probe() {
    let mut m = docker_manifest();
    m.constants
        .insert("BEDROCK_MODEL".into(), "vendor.model-v1:0".into());
    m.constants.insert(
        "MATRIX_PATH_BEDROCK".into(),
        "/model/{BEDROCK_MODEL}/converse".into(),
    );
    m.cell_paths
        .insert("bedrock>bedrock".into(), "{MATRIX_PATH_BEDROCK}".into());

    let got = m.cell_paths_for("0-3", 8000, std::path::Path::new("."));
    assert_eq!(
        got.get("bedrock>bedrock").map(String::as_str),
        Some("/model/vendor.model-v1:0/converse"),
        "a nested constant must be fully resolved, never passed through as template syntax"
    );
}

// An unresolvable key is DROPPED, so `path_for` falls back to the dialect's standard path: a
// wrong-but-honest probe of a real endpoint beats asking a gateway for `{TYPO}`.
#[test]
fn an_unresolvable_cell_path_is_dropped_rather_than_sent_half_substituted() {
    let mut m = docker_manifest();
    m.cell_paths.insert(
        "bedrock>bedrock".into(),
        "/model/{NO_SUCH_CONSTANT}/converse".into(),
    );
    assert!(m
        .cell_paths_for("0-3", 8000, std::path::Path::new("."))
        .is_empty());
}

// Axis order is `Dialect::ALL` both ways: row 0 = openai ingress, col 0 = openai egress.
#[test]
fn matrix_reads_row_ingress_col_egress_in_dialect_order() {
    let matrix = vec![
        "100000".to_string(), // openai ingress: only openai egress capable
        "000000".to_string(),
        "001000".to_string(), // anthropic ingress: only anthropic egress capable
        "000000".to_string(),
        "000000".to_string(),
        "000000".to_string(),
    ];
    assert_eq!(
        matrix_declared_capable(&matrix, "openai", "openai"),
        Some(true)
    );
    assert_eq!(
        matrix_declared_capable(&matrix, "openai", "anthropic"),
        Some(false)
    );
    assert_eq!(
        matrix_declared_capable(&matrix, "anthropic", "anthropic"),
        Some(true)
    );
    assert_eq!(
        matrix_declared_capable(&matrix, "gemini", "openai"),
        Some(false)
    );
}

// Empty matrix means undeclared: every cell probes normally, unchanged from before this field
// existed - a gateway that has not been researched yet must not be silently treated as
// incapable of everything.
#[test]
fn an_empty_matrix_means_undeclared_not_incapable() {
    assert_eq!(matrix_declared_capable(&[], "openai", "openai"), None);
}

#[test]
fn a_dialect_name_the_matrix_does_not_recognise_is_also_undeclared() {
    let matrix = vec!["1".repeat(6); 6];
    assert_eq!(
        matrix_declared_capable(&matrix, "not-a-real-dialect", "openai"),
        None
    );
}

#[test]
fn untestable_cells_match_the_exact_ingress_egress_pair_only() {
    let untestable = vec!["openai/bedrock".to_string()];
    assert!(is_untestable_cell(&untestable, "openai", "bedrock"));
    assert!(
        !is_untestable_cell(&untestable, "bedrock", "openai"),
        "direction matters"
    );
    assert!(!is_untestable_cell(&untestable, "openai", "anthropic"));
}

// THE WHOLE POINT. RSS, HWM and stop all read ONE declaration, so they cannot name different
// things: a reader spelled out separately per hook could drift, reading a single pid for RSS
// beside a whole tree for HWM and publishing two different populations for the same gateway.
#[test]
fn every_reader_derives_from_one_identity() {
    let m = docker_manifest();
    let id = m.runtime.identity();
    assert_eq!(id, "gw-bench");
    // The memory reader asks the container runtime for EXACTLY this name, and nothing else.
    let rec = RecordingPids::default();
    assert_eq!(
        crate::rss::root_pid_from(&rec, &m.runtime).copied(),
        Some(7)
    );
    assert_eq!(*rec.asked.borrow(), vec![id]);
}

/// A `PidSource` that records which identity each reader asked for.
#[derive(Default)]
struct RecordingPids {
    asked: std::cell::RefCell<Vec<String>>,
}
impl crate::rss::PidSource for RecordingPids {
    fn docker_pid(&self, container: &str) -> Option<u32> {
        self.asked.borrow_mut().push(container.to_string());
        Some(7)
    }
    fn matching_pid(&self, pattern: &str) -> crate::rss::PidLookup {
        self.asked.borrow_mut().push(pattern.to_string());
        crate::rss::PidLookup::Found(7)
    }
}

#[test]
fn a_native_runtime_carries_a_process_match_not_a_container() {
    let m = Manifest {
        runtime: Runtime::Native {
            proc_match: "target/release/gw".into(),
        },
        ..docker_manifest()
    };
    assert!(!m.runtime.is_docker());
    assert_eq!(m.runtime.identity(), "target/release/gw");
}

// ---- run-scoped container identity ------------------------------------------------------------

// THE DEFECT: a container's `--name` was the manifest's name alone, identical on every run of
// this gateway on this box. Two overlapping runs then named ONE container, and the second run's
// boot-retry `docker rm -f` deleted the first run's container mid-measurement.
#[test]
fn two_runs_of_one_gateway_cannot_name_the_same_container() {
    let declared = Runtime::Docker {
        container: "gw-bench".into(),
        run_scope: None,
    };
    let a = declared.scoped_to_run("20260729-101500-4242");
    let b = declared.scoped_to_run("20260729-101500-4243");
    assert_ne!(
        a.identity(),
        b.identity(),
        "two concurrent runs must not name the same container"
    );
    assert!(a.identity().starts_with("gw-bench-"), "{}", a.identity());
    // And teardown still finds its OWN: the stop path takes this same identity, and the declared
    // name stays available for the label a cross-run sweep filters on.
    assert_eq!(a.declared_identity(), "gw-bench");
    assert_eq!(a.run_scope(), Some("20260729-101500-4242"));
}

// A NATIVE IDENTITY IS UNCHANGED BY SCOPING: `proc_match` matches a command line the gateway
// itself produces, and no run id appears in it. Scoping it would match nothing at all.
#[test]
fn scoping_a_native_identity_leaves_the_process_match_alone() {
    let rt = Runtime::Native {
        proc_match: "target/release/gw".into(),
    };
    assert_eq!(rt.scoped_to_run("run-1").identity(), "target/release/gw");
    assert_eq!(rt.scoped_to_run("run-1").run_scope(), None);
}

// A run id a container runtime would reject must not produce a name that cannot be launched.
#[test]
fn a_run_id_is_reduced_to_what_a_container_name_accepts() {
    let declared = Runtime::Docker {
        container: "gw".into(),
        run_scope: None,
    };
    assert_eq!(declared.scoped_to_run("a b/c:d").identity(), "gw-abcd");
    assert_eq!(
        declared.scoped_to_run("///").identity(),
        "gw",
        "a run id that survives as nothing leaves the name unscoped, never trailing a separator"
    );
}

// ---- a proc_match must name one process --------------------------------------------------------

// THE DEFECT: matching is a SUBSTRING against every command line on the box, so a short or
// generic pattern selects a crowd, and whichever member the stop path signals or the memory
// reader sums is a coin toss published as this gateway's.
#[test]
fn an_indistinct_proc_match_is_refused_at_load_not_discovered_at_match_time() {
    for bad in ["gw", "node", "server", "sh", "otb-run"] {
        let m = Manifest {
            runtime: Runtime::Native {
                proc_match: bad.into(),
            },
            ..docker_manifest()
        };
        assert!(
            matches!(m.validate(), Err(ManifestError::IndistinctProcMatch(_))),
            "{bad:?} must not validate: it matches command lines that are not this gateway"
        );
    }
}

#[test]
fn a_binary_path_is_a_distinctive_enough_proc_match() {
    for good in [
        "target/release/aisix",
        "target/release/ai-gateway",
        "litellm-ai-gateway",
    ] {
        assert_eq!(proc_match_problem(good), None, "{good:?} was refused");
    }
}

// A runtime with no identity cannot be measured or stopped, so it must not validate.
#[test]
fn an_empty_runtime_identity_is_rejected() {
    let m = Manifest {
        runtime: Runtime::Docker {
            container: "  ".into(),
            run_scope: None,
        },
        ..docker_manifest()
    };
    assert_eq!(m.validate(), Err(ManifestError::Empty("runtime identity")));
}

#[test]
fn required_fields_are_required() {
    for (mutate, field) in [
        (
            Box::new(|m: &mut Manifest| m.name.clear()) as Box<dyn Fn(&mut Manifest)>,
            "name",
        ),
        (Box::new(|m: &mut Manifest| m.display.clear()), "display"),
        (Box::new(|m: &mut Manifest| m.repo.clear()), "repo"),
        (Box::new(|m: &mut Manifest| m.path.clear()), "path"),
        (Box::new(|m: &mut Manifest| m.model.clear()), "model"),
    ] {
        let mut m = docker_manifest();
        mutate(&mut m);
        assert_eq!(
            m.validate(),
            Err(ManifestError::Empty(field)),
            "{field} must be required"
        );
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
}

// The shell manifests are one indirection deep: one gateway sets `SOME_MODEL=gpt-4o-mini` and
// then `GW_MODEL="$SOME_MODEL"`. Extracting the reference instead of the value yields a field
// that is non-empty, parses, and validates under every other rule, then goes out on the wire as a
// model name. The corpus shipped exactly this.
#[test]
fn a_field_holding_an_unexpanded_shell_variable_is_rejected() {
    let m = Manifest {
        model: "$SOME_MODEL".into(),
        ..docker_manifest()
    };
    assert_eq!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable {
            field: "model",
            raw: "$SOME_MODEL".into()
        })
    );

    // Every field a request or a launch is built from, not just the model.
    let m = Manifest {
        auth: "${GW_KEY}".into(),
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable { field: "auth", .. })
    ));
    let m = Manifest {
        headers: vec!["x-api-key: $GW_AUTH".into()],
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable {
            field: "headers",
            ..
        })
    ));
    let m = Manifest {
        runtime: Runtime::Docker {
            container: "$NAME-bench".into(),
            run_scope: None,
        },
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable {
            field: "runtime identity",
            ..
        })
    ));

    // A clean manifest is untouched by the new rule.
    assert!(docker_manifest().validate().is_ok());
}

#[test]
fn a_setting_with_no_key_cannot_ship() {
    let m = Manifest {
        config: vec![ConfigSetting {
            key: " ".into(),
            reason: ConfigReason::RequiredToBoot,
            note: String::new(),
        }],
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::ConfigWithoutReason(_))
    ));
}

#[test]
fn round_trips_through_json_including_the_runtime_tag() {
    let m = docker_manifest();
    let js = serde_json::to_string(&m).unwrap();
    assert!(
        js.contains(r#""kind":"docker""#),
        "the runtime kind must be explicit on the wire: {js}"
    );
    let back: Manifest = serde_json::from_str(&js).unwrap();
    assert_eq!(back, m);
}

#[test]
fn the_url_is_built_from_the_declared_port_and_path() {
    assert_eq!(
        docker_manifest().url(),
        "http://127.0.0.1:8080/v1/chat/completions"
    );
}

// EVERY GATEWAY IS TOLD ITS CORE COUNT, BY THE HARNESS, THE SAME WAY.
//
// A cpuset restricts which cores a process may use; it does not change what a runtime believes
// is available. A runtime that reads the machine's online CPU count sizes its thread pools for a
// sixteen core box while confined to four, contends with itself, and publishes a number that
// describes a configuration nobody would deploy.
//
// It is the harness that decides the core count, so it is the harness that states it,
// identically, everywhere, rather than leaving it to each gateway's own env, where a dropped
// setting still lets the gateway boot with nothing to signal the mistake.
#[test]
fn every_launched_gateway_is_told_how_many_cores_it_was_pinned_to() {
    // Both launch kinds, because a native entrant is pinned with taskset rather than a cpuset
    // and needs telling just as much.
    let mut container = docker_manifest();
    container.launch = Some(LaunchDecl::Docker {
        image: "gw:1".into(),
        env: vec![("GOMAXPROCS".into(), "64".into())],
        args: vec![],
        mounts: vec![],
    });
    let mut native = docker_manifest();
    native.runtime = Runtime::Native {
        proc_match: "gw".into(),
    };
    native.launch = Some(LaunchDecl::Native {
        build: None,
        binary: vec!["gw".into()],
        args: vec![],
        env: vec![],
        env_unset: vec![],
    });
    for (label, m) in [("container", container), ("native", native)] {
        let dir = std::path::Path::new(".");
        let spec = m
            .launch_spec(
                "0-3",
                8000,
                dir,
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .expect("this manifest declares a launch")
            .expect("and it resolves");
        let env = match &spec.kind {
            crate::launch::LaunchKind::Docker { env, .. } => env,
            crate::launch::LaunchKind::Native { env, .. } => env,
        };
        let got = |k: &str| {
            env.iter()
                .rev()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
        };
        // "0-3" is four cores, and that is what the runtime must be told, not the host's count.
        // Read the LAST assignment for each name: the harness appends its values after the
        // gateway's, so a gateway that sets its own GOMAXPROCS cannot escape the pinning.
        assert_eq!(got("GOMAXPROCS"), Some("4"), "{label}: GOMAXPROCS");
        assert_eq!(got("UV_THREADPOOL_SIZE"), Some("4"), "{label}: node pool");
        assert_eq!(got("RAYON_NUM_THREADS"), Some("4"), "{label}: rayon");
        assert_eq!(got("OMP_NUM_THREADS"), Some("4"), "{label}: openmp");
    }
}

// A MANIFEST THAT RESTATES A HEADER THE RIG ALREADY SENDS IS DISCLOSED, NOT SILENT.
//
// Ledger RIG-12's remainder. `run::headers_for` composed the dialect's credential header and
// then appended the manifest's verbatim, and `gateways/litellm-rust/definition.json` declares
// `Authorization: Bearer {GW_AUTH}` today - so two `authorization` headers went out on one
// request and HTTP does not define which a server honours. The wire is resolved there (the rig's
// copy wins); the collision is named here, so a precedence rule nobody is told about is not the
// same silence as the ambiguity it replaced.
#[test]
fn a_manifest_restating_a_header_the_rig_owns_is_reported() {
    let m = Manifest {
        headers: vec!["Authorization: Bearer {GW_AUTH}".into()],
        ..docker_manifest()
    };
    let found = m.rig_owned_headers_declared();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].contains("authorization") && found[0].contains("headers"),
        "the report must name the header and where it was declared: {found:?}"
    );
    // Reported case-insensitively, because HTTP header names are: the live manifest spells it
    // with a capital A, and a rule that only caught the lowercase one would not have caught the
    // file that motivated it. `found[0]` above is the lowercased name.

    // Every dialect's, not just the ones this gateway wires up: a header in `headers` is sent on
    // every ingress dialect the matrix probes, and the matrix probes all six regardless.
    for name in ["x-api-key", "x-goog-api-key", "anthropic-version"] {
        let m = Manifest {
            egress_headers: [("openai".to_string(), vec![format!("{name}: whatever")])]
                .into_iter()
                .collect(),
            ..docker_manifest()
        };
        assert_eq!(
            m.rig_owned_headers_declared().len(),
            1,
            "{name} is a header the rig sends itself"
        );
    }

    // A routing header the harness never sends is the normal case and must stay silent.
    let m = Manifest {
        headers: vec!["x-llm-provider: anthropic".into()],
        egress_headers: [(
            "openai".to_string(),
            vec!["x-portkey-custom-host: http://127.0.0.1:{MOCK_PORT}/v1".into()],
        )]
        .into_iter()
        .collect(),
        ..docker_manifest()
    };
    assert!(m.rig_owned_headers_declared().is_empty());
}

/* A MALFORMED MATRIX IS REFUSED, NOT READ AS DECLARED ZEROS. `matrix_declared_capable` reads every
byte other than '1' as a declared '0', and a '0' cell is never probed and publishes
`not_configurable` - so "1x1111" or a five-character row silently erased cells. */
#[test]
fn a_matrix_that_is_not_the_documented_grid_is_refused() {
    let with = |matrix: Vec<String>| Manifest {
        matrix,
        ..docker_manifest()
    };
    assert!(with(vec![]).validate().is_ok(), "empty is undeclared");
    assert!(with(vec!["101010".to_string(); 6]).validate().is_ok());
    let mut typo = vec!["111111".to_string(); 6];
    typo[2] = "11x111".to_string();
    for bad in [
        typo,
        vec!["111111".to_string(); 5],
        vec!["11111".to_string(); 6],
        vec!["1111111".to_string(); 6],
    ] {
        assert!(
            matches!(
                with(bad.clone()).validate(),
                Err(ManifestError::BadMatrix(_))
            ),
            "{bad:?} must be refused"
        );
    }
}

/* A HEADER WITH NO COLON STOPS THE RUN. `headers_for` dropped it without a word, so the gateway was
measured without a header its manifest declared and published as serving nothing. */
#[test]
fn a_header_without_a_colon_is_refused_at_resolution() {
    let m = Manifest {
        headers: vec!["x-api-key bench-token".to_string()],
        ..docker_manifest()
    };
    assert!(matches!(
        m.headers_for("", "0-3", 9000, std::path::Path::new("/tmp")),
        Err(ManifestError::HeaderWithoutColon(_))
    ));
}

/// Per-egress headers and per-cell paths reach the wire like `headers`, so an unexpanded shell
/// reference in them is refused the same way.
#[test]
fn an_unexpanded_variable_in_egress_headers_or_cell_paths_is_rejected() {
    let m = Manifest {
        egress_headers: [("openai".to_string(), vec!["x-key: $KEY".to_string()])]
            .into_iter()
            .collect(),
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable {
            field: "egress_headers",
            ..
        })
    ));
    let m = Manifest {
        cell_paths: [("openai>openai".to_string(), "/v1/$P".to_string())]
            .into_iter()
            .collect(),
        ..docker_manifest()
    };
    assert!(matches!(
        m.validate(),
        Err(ManifestError::UnexpandedVariable {
            field: "cell_paths",
            ..
        })
    ));
}

/// The cpuset grammar taskset and `--cpuset-cpus` accept: a comma list of cores and ranges. A list
/// used to count as ONE core, pinning GOMAXPROCS and friends below what the gateway was given.
#[test]
fn the_core_count_reads_a_cpuset_list_not_only_one_range() {
    assert_eq!(core_count("0-3"), 4);
    assert_eq!(core_count("0,2,4,6"), 4);
    assert_eq!(core_count("0-3,8-11"), 8);
    assert_eq!(core_count("2"), 1);
    assert_eq!(core_count(""), 1);
    assert_eq!(core_count("garbage"), 1);
}

/* AN ENV FILE LINE THAT SETS NOTHING IS REFUSED, and so is one whose declaration could not land.
`parse_env` skipped a line that was neither NAME=value nor -NAME (and took `export FOO=bar` as a
variable named "export FOO"); `apply_env` dropped -NAME on a docker launch, the whole file with no
launch, and replaced env already declared in definition.json. Each was a variable the gateway's own
directory says it runs with, silently absent. */
#[test]
fn an_env_sidecar_line_that_cannot_land_fails_the_load() {
    assert!(parse_env("A=1\n# c\n\n-B\n").is_ok());
    assert_eq!(
        parse_env("A=\"q\"\n").expect("parses").0,
        vec![("A".to_string(), "\"q\"".to_string())],
        "values are verbatim, as docker --env-file takes them"
    );
    for bad in ["JUSTANAME", "export FOO=bar", "=v", "-", "- two words"] {
        assert!(parse_env(bad).is_err(), "{bad:?} must be refused");
    }

    let docker = || Manifest {
        launch: Some(LaunchDecl::Docker {
            image: "x:1".into(),
            env: vec![],
            args: vec![],
            mounts: vec![],
        }),
        ..docker_manifest()
    };
    assert!(docker()
        .apply_env(vec![("A".into(), "1".into())], vec![])
        .is_ok());
    assert!(
        docker().apply_env(vec![], vec!["B".into()]).is_err(),
        "a removal a container cannot honour"
    );
    let mut declared = docker();
    if let Some(LaunchDecl::Docker { env, .. }) = declared.launch.as_mut() {
        env.push(("X".into(), "1".into()));
    }
    assert!(
        declared
            .apply_env(vec![("A".into(), "1".into())], vec![])
            .is_err(),
        "two places declaring the environment"
    );
    let mut no_launch = Manifest {
        launch: None,
        ..docker_manifest()
    };
    assert!(no_launch.apply_env(vec![], vec![]).is_ok());
    assert!(no_launch
        .apply_env(vec![("A".into(), "1".into())], vec![])
        .is_err());
}

/// The nested launch/config declarations refuse unknown keys like `Manifest` and `Runtime` do.
#[test]
fn an_unknown_key_inside_a_launch_or_config_declaration_is_refused() {
    for bad in [
        r#"{"kind":"docker","image":"x:1","enviroment":[]}"#,
        r#"{"kind":"native","binary":["b"],"img":"x"}"#,
    ] {
        assert!(serde_json::from_str::<LaunchDecl>(bad).is_err(), "{bad}");
    }
    assert!(serde_json::from_str::<MountDecl>(
        r#"{"host_path":"a","container_path":"/a","readonly":true}"#
    )
    .is_err());
    assert!(serde_json::from_str::<ConfigFile>(r#"{"template":"a","output":"b","x":1}"#).is_err());
    assert!(serde_json::from_str::<ConfigSetting>(
        r#"{"key":"k","reason":"rig_binding","nte":"typo"}"#
    )
    .is_err());
}
