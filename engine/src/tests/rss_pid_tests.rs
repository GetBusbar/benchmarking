use super::*;

#[derive(Default)]
struct FakePids {
    docker: Option<u32>,
    docker_error: Option<String>,
    native: Option<u32>,
    /// Set to script the "several unrelated processes answer to this pattern" lookup.
    ambiguous: Option<Vec<u32>>,
    seen_docker: std::cell::RefCell<Vec<String>>,
    seen_native: std::cell::RefCell<Vec<String>>,
}

impl PidSource for FakePids {
    fn docker_pid(&self, container: &str) -> Option<u32> {
        self.seen_docker.borrow_mut().push(container.to_string());
        self.docker
    }
    fn docker_lookup(&self, container: &str) -> Result<Option<u32>, String> {
        match &self.docker_error {
            Some(why) => {
                self.seen_docker.borrow_mut().push(container.to_string());
                Err(why.clone())
            }
            None => Ok(self.docker_pid(container)),
        }
    }
    fn matching_pid(&self, pattern: &str) -> PidLookup {
        self.seen_native.borrow_mut().push(pattern.to_string());
        match (&self.native, &self.ambiguous) {
            (_, Some(pids)) => PidLookup::Ambiguous(pids.clone()),
            (Some(pid), None) => PidLookup::Found(*pid),
            (None, None) => PidLookup::NotRunning,
        }
    }
}

// THE POINT OF THE BRIDGE: whichever kind the manifest declares, the lookup is driven by
// `Runtime::identity()` and nothing else, so the tree measured here is the tree that was
// launched and the tree that gets stopped.
#[test]
fn the_lookup_is_driven_by_the_one_declared_identity() {
    let f = FakePids {
        docker: Some(42),
        ..Default::default()
    };
    let rt = Runtime::Docker {
        container: "gw-bench".into(),
        run_scope: None,
    };
    assert_eq!(root_pid_from(&f, &rt).copied(), Some(42));
    assert_eq!(
        f.seen_docker.borrow().as_slice(),
        ["gw-bench"],
        "the container name came from identity()"
    );
    assert!(
        f.seen_native.borrow().is_empty(),
        "a container must not be looked up as a process match"
    );

    let f = FakePids {
        native: Some(7),
        ..Default::default()
    };
    let rt = Runtime::Native {
        proc_match: "target/release/gw".into(),
    };
    assert_eq!(root_pid_from(&f, &rt).copied(), Some(7));
    assert_eq!(f.seen_native.borrow().as_slice(), ["target/release/gw"]);
    assert!(f.seen_docker.borrow().is_empty());
}

// THE RUN-SCOPED NAME, not the declared one. With `run_scope: None` the two are the same string, so
// the test above cannot tell them apart; a lookup by the bare container name would read a concurrent
// run's container of the same gateway.
#[test]
fn a_run_scoped_container_is_looked_up_by_its_scoped_identity() {
    let f = FakePids {
        docker: Some(42),
        ..Default::default()
    };
    let rt = Runtime::Docker {
        container: "gw-bench".into(),
        run_scope: None,
    }
    .scoped_to_run("4242");
    let scoped = rt.identity();
    assert_ne!(scoped, "gw-bench", "the fixture must actually be scoped");
    assert_eq!(root_pid_from(&f, &rt).copied(), Some(42));
    assert_eq!(f.seen_docker.borrow().as_slice(), [scoped]);
}

// THE RUNTIME FAILING TO ANSWER IS NOT THE GATEWAY BEING DOWN. A missing docker binary or an
// unreachable daemon used to read as "no running process found for the declared container".
#[test]
fn a_container_runtime_that_cannot_be_asked_is_a_harness_error_not_a_dead_gateway() {
    let f = FakePids {
        docker_error: Some("Cannot connect to the Docker daemon".into()),
        ..Default::default()
    };
    let rt = Runtime::Docker {
        container: "gw-bench".into(),
        run_scope: None,
    };
    let got = root_pid_from(&f, &rt);
    assert_eq!(got.reason(), Some(&Absent::HarnessError), "{got:?}");
    assert!(
        got.detail().is_some_and(|d| d.contains("Docker daemon")),
        "{got:?}"
    );
}

// A FORKING GATEWAY: master and workers all carry the same command line, so they all match, and
// the tree to sum is the master's. Resolved by ANCESTRY, not by pid order - the old rule took the
// numerically smallest match, which is only the master by coincidence.
#[test]
fn several_matches_in_one_tree_resolve_to_the_ancestor_of_them_all() {
    // 40 is the master; 61 and 12 are its workers. The smallest pid is a WORKER here, so a rule
    // that took the smallest would sum a worker's subtree and miss the master and its sibling.
    let ppid: BTreeMap<u32, u32> = [(40, 1), (61, 40), (12, 40)].into_iter().collect();
    assert_eq!(sole_tree_root(&[40, 61, 12], &ppid), Some(40));
    assert_ne!(
        sole_tree_root(&[40, 61, 12], &ppid),
        Some(12),
        "the smallest pid is not the root of anything"
    );
}

// TWO UNRELATED PROCESSES ANSWER TO THE SAME PATTERN. There is no honest choice between them.
#[test]
fn unrelated_matches_have_no_sole_root() {
    let ppid: BTreeMap<u32, u32> = [(40, 1), (900, 1)].into_iter().collect();
    assert_eq!(sole_tree_root(&[40, 900], &ppid), None);
}

#[test]
fn a_cycle_in_the_reported_parent_links_does_not_hang_the_root_search() {
    let ppid: BTreeMap<u32, u32> = [(7, 8), (8, 7)].into_iter().collect();
    let _ = sole_tree_root(&[7, 8], &ppid);
}

// THE DEFECT THIS REPLACES: the smallest matching pid was measured as the gateway, so an older
// bystander that happened to match published ITS memory under the gateway's name. Refusing is the
// honest answer, and the absence names what else matched.
#[test]
fn several_unrelated_matches_refuse_to_measure_rather_than_guess() {
    let f = FakePids {
        ambiguous: Some(vec![317, 4820]),
        ..Default::default()
    };
    let rt = Runtime::Native {
        proc_match: "gateway-binary".into(),
    };
    let m = root_pid_from(&f, &rt);
    assert!(
        !m.is_measured(),
        "an ambiguous match must not resolve to a pid at all, got {m:?}"
    );
    assert_ne!(
        m.copied(),
        Some(317),
        "the smallest matching pid must no longer be chosen"
    );
    assert_eq!(m.reason(), Some(&Absent::NotMeasured));
    let detail = m.detail().unwrap_or_default();
    assert!(
        detail.contains("317") && detail.contains("4820"),
        "the absence must name what else answered to the pattern: {detail}"
    );
}

// AN UNDECLARED IDENTITY RESOLVES TO NOTHING. `smoke` and the rig-reference configs pass an empty
// `proc_match` meaning "there is no gateway process here"; `pgrep -f ""` answered with every
// process on the box, so init's tree could be summed and published as a gateway's RSS. Driven
// against the REAL box, because that is where the old behaviour lived.
#[test]
fn an_empty_declared_identity_measures_nothing_on_the_real_box() {
    assert_eq!(
        RealPids.matching_pid(""),
        PidLookup::NotRunning,
        "an empty match must name no process at all"
    );
    let m = root_pid_from(
        &RealPids,
        &Runtime::Native {
            proc_match: String::new(),
        },
    );
    assert!(!m.is_measured());
    assert_eq!(m.reason(), Some(&Absent::NotMeasured));
}

// A process that is not there is an ABSENCE WITH A REASON. Never a zero, never a default pid:
// summing a tree from the wrong root publishes another process's memory as the gateway's.
#[test]
fn a_missing_process_is_an_absence_that_names_what_was_looked_for() {
    let f = FakePids::default();
    let rt = Runtime::Docker {
        container: "gw-bench".into(),
        run_scope: None,
    };
    let m = root_pid_from(&f, &rt);
    assert!(!m.is_measured());
    assert_eq!(m.reason(), Some(&Absent::NotMeasured));
    let detail = m.detail().unwrap_or_default();
    assert!(
        detail.contains("gw-bench"),
        "the reason must name the identity: {detail}"
    );
    assert!(
        detail.contains("container"),
        "and which kind it was: {detail}"
    );

    let rt = Runtime::Native {
        proc_match: "no-such-binary".into(),
    };
    let m = root_pid_from(&f, &rt);
    assert!(m
        .detail()
        .is_some_and(|d| d.contains("process-match") && d.contains("no-such-binary")));
}
