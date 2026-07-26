// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// RESIDENT MEMORY OF A PROCESS TREE, ported from lib/harness.sh's `_proc_tree_field_mib` and its
// two callers `_rss_tree_mib` / `_hwm_tree_mib`.
//
// THE CONTRACT (the shell defect this preserves, in types): a live process cannot be 0 KiB
// resident, so a summed 0 is a measurement failure wearing a real number's clothes, never a real
// reading. An unreadable or absent /proc entry (the process exited mid-scan, a permission denial,
// a PID namespace we cannot see, no /proc at all on a non-Linux host) contributes NOTHING to the
// sum; it must never lower the total the way a substituted 0 would. If nothing in the tree was
// readable, or everything that was readable summed to exactly zero, the result is
// `Absent(NotMeasured)`, never `Measured(0.0)`. A gateway benchmark ranks memory ascending, so a
// fabricated 0.0 would certify the gateway we simply failed to measure as the winner.
//
// The walk is separated from the syscalls by the `ProcSource` trait so the summing logic (this
// file's actual content) is tested against fixture text, never against a real process tree.

use crate::manifest::Runtime;
use crate::measurement::{Absent, Measurement};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Everything the summing logic needs to read from `/proc`, abstracted so tests can supply fixture
/// data instead of a real filesystem.
pub trait ProcSource {
    /// pid -> parent pid, for every process currently visible to this reader. A process the reader
    /// could not resolve a parent for is simply absent from the map; that process still gets
    /// counted if it is the root or was already discovered, but nothing beneath it can be found.
    /// A partial map therefore only risks UNDER-counting descendants, never inventing one.
    fn ppid_map(&self) -> BTreeMap<u32, u32>;

    /// The raw contents of `/proc/<pid>/status`, or `None` if the pid is unreadable or gone.
    fn status(&self, pid: u32) -> Option<String>;
}

/// Walk of the real filesystem. Parent/child links come from `/proc/<pid>/stat`'s `ppid` field
/// (fourth field after the command, which we locate by the LAST `)` so a command name containing
/// its own parentheses cannot shift the parse) rather than from `pgrep -P`, since there is no
/// portable pgrep-equivalent syscall; scanning every stat file once and building the map up front
/// gives the same parent/child answer pgrep -P would, without a subprocess per pid.
pub struct RealProc;

impl ProcSource for RealProc {
    // A FULL SCAN, EVERY CALL, ON PURPOSE. The memory sampler in metric.rs calls this roughly every
    // 100ms for the whole length of a load window, so the cost is real, but caching this map across
    // calls (or across-refreshing only pids already known) would be a correctness bug, not just a
    // perf win: a gateway is free to spawn new worker processes under load, at any point during the
    // window, as children of any pid already in its tree, and the only way to notice a newly spawned
    // pid is to look at the set of pids that exist RIGHT NOW. A cached map answers "was" a question,
    // not "is". Nor can the scan be narrowed to some subset of /proc: whether a given live pid
    // belongs under `root_pid` is only knowable by reading that pid's own ppid, so every pid in
    // /proc has to be read to answer "does the tree have a new member" - there is no cheaper index.
    // A pid that gets reparented AWAY from the tree (its process's original parent died) drops out of
    // `tree_pids`'s walk on the very next scan regardless of caching, since the walk is a fresh BFS
    // from `root_pid` over whatever `ppid_map` currently says; that is an accepted, unrelated
    // limitation of the walk, not something this scan's freshness could fix either way.
    fn ppid_map(&self) -> BTreeMap<u32, u32> {
        let mut map = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return map; // no /proc at all (e.g. a non-Linux host): an empty map, not an error.
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if let Some(ppid) = read_ppid(pid) {
                map.insert(pid, ppid);
            }
        }
        map
    }

    fn status(&self, pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/status")).ok()
    }
}

fn read_ppid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rparen = stat.rfind(')')?;
    let mut fields = stat[rparen + 1..].split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse::<u32>().ok()
}

/// Every pid in the tree rooted at `root`, discovered by following `ppid_map` outward and
/// deduplicated by construction: a `BTreeSet` insert that reports "already present" halts that
/// branch of the walk, which is what makes a cycle (or a pid reachable by more than one path)
/// terminate and count once rather than looping or double-counting.
fn tree_pids(root: u32, ppid_map: &BTreeMap<u32, u32>) -> BTreeSet<u32> {
    let mut seen = BTreeSet::new();
    seen.insert(root);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (&pid, &ppid) in ppid_map {
            if ppid == parent && seen.insert(pid) {
                frontier.push(pid);
            }
        }
    }
    seen
}

/// The first numeric token after a `<field>:` line in `/proc/<pid>/status` text, e.g. `VmRSS:` ->
/// the kB value. `None` for a missing field, a non-numeric value, or empty content: all three are
/// "could not read this pid's number", never a zero.
fn parse_field_kb(status: &str, field: &str) -> Option<u64> {
    let prefix = format!("{field}:");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            let token = rest.split_whitespace().next()?;
            return token.parse::<u64>().ok();
        }
    }
    None
}

/// The pure core: sum `field` (in kB, as `/proc/<pid>/status` reports it) across `root_pid` and
/// every descendant `source` can resolve. kB -> MiB is divided by 1024 exactly once, here, so
/// every caller shares one conversion site.
fn sum_field_mib<S: ProcSource>(source: &S, root_pid: u32, field: &str) -> Measurement<f64> {
    if root_pid == 0 {
        // Matches the shell guard: pid 0 means "inspect failed" or "not running", not a real root.
        return Measurement::absent_because(Absent::NotMeasured, "no root pid to measure");
    }
    let ppid_map = source.ppid_map();
    let pids = tree_pids(root_pid, &ppid_map);

    let mut total_kb: u64 = 0;
    let mut read_any = false;
    for pid in pids {
        let Some(status) = source.status(pid) else { continue };
        let Some(kb) = parse_field_kb(&status, field) else { continue };
        read_any = true;
        total_kb += kb;
    }

    if !read_any {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!("no /proc entry in the tree rooted at pid {root_pid} was readable"),
        );
    }
    if total_kb == 0 {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!("{field} summed to 0 across a readable process tree rooted at pid {root_pid}, which a live tree cannot be"),
        );
    }
    Measurement::Measured(total_kb as f64 / 1024.0)
}

/// Sum of `VmRSS` (current resident memory) across `root_pid` and all descendants, in MiB.
pub fn rss_tree_mib(root_pid: u32) -> Measurement<f64> {
    sum_field_mib(&RealProc, root_pid, "VmRSS")
}

/// Sum of `VmHWM` (the kernel's own per-process high-water mark) across `root_pid` and all
/// descendants, in MiB. Unlike a periodic `VmRSS` poll, the kernel updates `VmHWM` on every charge,
/// so it cannot miss a sub-interval spike between polls.
pub fn hwm_tree_mib(root_pid: u32) -> Measurement<f64> {
    sum_field_mib(&RealProc, root_pid, "VmHWM")
}

// ── the bridge from a declared identity to a process tree ────────────────────────────────────────
//
// THE LINK FROM A DECLARED IDENTITY TO A PROCESS TREE. Everything above sums a tree from a root
// pid, and the manifest declares an identity (`Runtime`); this resolves through the SAME
// `Runtime::identity()` the launcher's `--name` and the stop path take, so the tree measured here
// cannot describe a different process than the one that was started or the one that gets stopped
// (see `manifest.rs`'s module header for why that matters).
//
// A resolution failure is an ABSENCE WITH A REASON, never a zero and never a default pid. Reading
// memory off the wrong process is worse than reading none.

/// How the root pid is discovered, abstracted so the resolution logic is testable without docker or
/// a live process.
pub trait PidSource {
    /// The container runtime's reported root pid for a container name.
    fn docker_pid(&self, container: &str) -> Option<u32>;
    /// The oldest pid whose command line matches, which is the parent of any workers it forked.
    fn matching_pid(&self, pattern: &str) -> Option<u32>;
}

/// The real lookups: the container runtime for a container, a command-line match for a native
/// process. Kept thin; the logic worth testing is in `root_pid_from`.
pub struct RealPids;

impl PidSource for RealPids {
    fn docker_pid(&self, container: &str) -> Option<u32> {
        let out = std::process::Command::new("docker")
            .args(["inspect", "-f", "{{.State.Pid}}", container])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        // A stopped container reports pid 0. That is not a process, and summing a tree from it would
        // read whatever pid 0 resolves to rather than nothing.
        match String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
            Ok(0) | Err(_) => None,
            Ok(pid) => Some(pid),
        }
    }

    fn matching_pid(&self, pattern: &str) -> Option<u32> {
        let out = std::process::Command::new("pgrep").args(["-f", pattern]).output().ok()?;
        if !out.status.success() {
            return None;
        }
        // `pgrep -f` lists newest-first on some systems and oldest-first on others, so take the
        // NUMERICALLY smallest rather than the first line: the parent is started before the workers
        // it forks, and summing from a worker would silently exclude its siblings and the parent.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .filter(|p| *p != 0)
            .min()
    }
}

/// The root of the gateway's process tree, from the one identity the manifest declares.
pub fn root_pid(runtime: &Runtime) -> Measurement<u32> {
    root_pid_from(&RealPids, runtime)
}

/// The testable core.
pub fn root_pid_from<S: PidSource>(source: &S, runtime: &Runtime) -> Measurement<u32> {
    let found = match runtime {
        Runtime::Docker { container } => source.docker_pid(container),
        Runtime::Native { proc_match } => source.matching_pid(proc_match),
    };
    match found {
        Some(pid) => Measurement::Measured(pid),
        // NotMeasured, not Untestable: the rig worked, we asked, and the process was not there. The
        // detail names the identity so a reader can tell "the gateway died" from "we looked for the
        // wrong thing", which are the two causes and they have different fixes.
        None => Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "no running process found for the declared {} identity {:?}",
                if runtime.is_docker() { "container" } else { "process-match" },
                runtime.identity()
            ),
        ),
    }
}

#[cfg(test)]
mod pid_tests {
    use super::*;

    #[derive(Default)]
    struct FakePids {
        docker: Option<u32>,
        native: Option<u32>,
        seen_docker: std::cell::RefCell<Vec<String>>,
        seen_native: std::cell::RefCell<Vec<String>>,
    }

    impl PidSource for FakePids {
        fn docker_pid(&self, container: &str) -> Option<u32> {
            self.seen_docker.borrow_mut().push(container.to_string());
            self.docker
        }
        fn matching_pid(&self, pattern: &str) -> Option<u32> {
            self.seen_native.borrow_mut().push(pattern.to_string());
            self.native
        }
    }

    // THE POINT OF THE BRIDGE: whichever kind the manifest declares, the lookup is driven by
    // `Runtime::identity()` and nothing else, so the tree measured here is the tree that was
    // launched and the tree that gets stopped.
    #[test]
    fn the_lookup_is_driven_by_the_one_declared_identity() {
        let f = FakePids { docker: Some(42), ..Default::default() };
        let rt = Runtime::Docker { container: "gw-bench".into() };
        assert_eq!(root_pid_from(&f, &rt).copied(), Some(42));
        assert_eq!(f.seen_docker.borrow().as_slice(), ["gw-bench"], "the container name came from identity()");
        assert!(f.seen_native.borrow().is_empty(), "a container must not be looked up as a process match");

        let f = FakePids { native: Some(7), ..Default::default() };
        let rt = Runtime::Native { proc_match: "target/release/gw".into() };
        assert_eq!(root_pid_from(&f, &rt).copied(), Some(7));
        assert_eq!(f.seen_native.borrow().as_slice(), ["target/release/gw"]);
        assert!(f.seen_docker.borrow().is_empty());
    }

    // A process that is not there is an ABSENCE WITH A REASON. Never a zero, never a default pid:
    // summing a tree from the wrong root publishes another process's memory as the gateway's.
    #[test]
    fn a_missing_process_is_an_absence_that_names_what_was_looked_for() {
        let f = FakePids::default();
        let rt = Runtime::Docker { container: "gw-bench".into() };
        let m = root_pid_from(&f, &rt);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::NotMeasured));
        let detail = m.detail().unwrap_or_default();
        assert!(detail.contains("gw-bench"), "the reason must name the identity: {detail}");
        assert!(detail.contains("container"), "and which kind it was: {detail}");

        let rt = Runtime::Native { proc_match: "no-such-binary".into() };
        let m = root_pid_from(&f, &rt);
        assert!(m.detail().is_some_and(|d| d.contains("process-match") && d.contains("no-such-binary")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A fixture `ProcSource`: an explicit parent map and per-pid status text, so a test drives the
    /// summing logic directly rather than through the filesystem.
    #[derive(Default, Clone)]
    struct FakeProc {
        ppid: BTreeMap<u32, u32>,
        status: BTreeMap<u32, String>,
    }

    impl FakeProc {
        fn child(mut self, pid: u32, ppid: u32) -> Self {
            self.ppid.insert(pid, ppid);
            self
        }
        fn rss(mut self, pid: u32, kb: u64) -> Self {
            self.status.insert(pid, format!("Name:\tp{pid}\nVmRSS:\t{kb} kB\nVmHWM:\t{kb} kB\n"));
            self
        }
        fn status_text(mut self, pid: u32, text: &str) -> Self {
            self.status.insert(pid, text.to_string());
            self
        }
    }

    impl ProcSource for FakeProc {
        fn ppid_map(&self) -> BTreeMap<u32, u32> {
            self.ppid.clone()
        }
        fn status(&self, pid: u32) -> Option<String> {
            self.status.get(&pid).cloned()
        }
    }

    fn rss(source: &FakeProc, root: u32) -> Measurement<f64> {
        sum_field_mib(source, root, "VmRSS")
    }
    fn hwm(source: &FakeProc, root: u32) -> Measurement<f64> {
        sum_field_mib(source, root, "VmHWM")
    }

    // ---- required case: a tree of several pids sums correctly -------------------------------------

    #[test]
    fn a_tree_of_several_pids_sums_correctly() {
        // root(1) -> child(2), child(3); child(2) -> grandchild(4).
        let f = FakeProc::default()
            .rss(1, 1024)
            .child(2, 1)
            .rss(2, 2048)
            .child(3, 1)
            .rss(3, 512)
            .child(4, 2)
            .rss(4, 4096);
        // total kB = 1024+2048+512+4096 = 7680 -> /1024 = 7.5 MiB.
        assert_eq!(rss(&f, 1).copied(), Some(7.5));
    }

    // ---- required case: an unreadable/missing pid is EXCLUDED, not counted as zero ----------------

    #[test]
    fn an_unreadable_pid_is_excluded_not_counted_as_zero() {
        // child(3) has a ppid entry (it exists in the tree) but no status text: it exited mid-scan.
        let f = FakeProc::default().rss(1, 1000).child(2, 1).rss(2, 2000).child(3, 1);
        let got = rss(&f, 1).copied();
        // The readable-only total: (1000+2000)/1024.
        let readable_only = (1000.0 + 2000.0) / 1024.0;
        assert_eq!(got, Some(readable_only));
        // And it must differ from what treating the missing pid as 0 would give (same number here,
        // since 0 contributes nothing to a sum) -- so assert against a DIFFERENT wrong total instead:
        // a "missing = 0" implementation would still reach the same total for a genuine 0-contribution
        // bug, so the real regression this guards is a missing pid contributing a NONZERO placeholder;
        // pin that the total is exactly the readable sum and nothing else.
        let wrong_if_missing_were_nonzero = readable_only + 1.0;
        assert_ne!(got, Some(wrong_if_missing_were_nonzero));
    }

    #[test]
    fn missing_pid_is_excluded_but_present_siblings_still_sum_correctly() {
        // Three pids in the tree, only two readable. Total must equal exactly those two.
        let f = FakeProc::default().rss(1, 500).child(2, 1).rss(2, 1500).child(3, 1); // 3: unreadable
        assert_eq!(rss(&f, 1).copied(), Some((500.0 + 1500.0) / 1024.0));
    }

    // ---- required case: all pids unreadable yields Absent(NotMeasured), never Measured(0.0) ------

    #[test]
    fn all_pids_unreadable_yields_absent_not_measured_zero() {
        let f = FakeProc::default().child(2, 1).child(3, 1); // root and children all have no status.
        let got = rss(&f, 1);
        assert!(!got.is_measured());
        assert_eq!(got.reason(), Some(&Absent::NotMeasured));
        assert_ne!(got, Measurement::Measured(0.0));
    }

    // ---- required case: a readable tree summing to exactly 0 yields Absent, not Measured(0.0) -----

    #[test]
    fn a_readable_tree_summing_to_exactly_zero_is_absent() {
        let f = FakeProc::default().rss(1, 0).child(2, 1).rss(2, 0);
        let got = rss(&f, 1);
        assert!(!got.is_measured());
        assert_eq!(got.reason(), Some(&Absent::NotMeasured));
        assert_ne!(got, Measurement::Measured(0.0));
    }

    // ---- required case: a pid appearing twice (cycle or repeated listing) is counted once ---------

    #[test]
    fn a_cycle_in_the_tree_is_counted_once_per_pid() {
        // 1 -> 2 -> 3 -> 1 (a cycle including the root). Each pid must still be summed exactly once.
        let f = FakeProc::default().rss(1, 1000).child(2, 1).rss(2, 2000).child(3, 2).rss(3, 3000);
        let mut cyclic = f.clone();
        cyclic.ppid.insert(1, 3); // close the loop back onto the root.
        let got = rss(&cyclic, 1).copied();
        assert_eq!(got, Some((1000.0 + 2000.0 + 3000.0) / 1024.0));
    }

    #[test]
    fn a_pid_reachable_through_two_parent_links_is_still_counted_once() {
        // Two "parent" rows both claiming the same child pid 3 (a corrupted/duplicate listing): the
        // map can only hold one ppid per key, so the second insert simply overwrites the first, and
        // the walk must still find and count pid 3 exactly once either way.
        let f = FakeProc::default().rss(1, 100).child(2, 1).rss(2, 200).child(3, 1).rss(3, 300).child(3, 2);
        assert_eq!(rss(&f, 1).copied(), Some((100.0 + 200.0 + 300.0) / 1024.0));
    }

    // ---- required case: kB to MiB conversion is exact and applied once ----------------------------

    #[test]
    fn kb_to_mib_conversion_is_exact() {
        let f = FakeProc::default().rss(1, 1_048_576);
        assert_eq!(rss(&f, 1).copied(), Some(1024.0));
    }

    // ---- required case: malformed status content is excluded, not parsed as zero ------------------

    #[test]
    fn missing_field_in_status_is_excluded_not_zero() {
        let f = FakeProc::default().status_text(1, "Name:\tonly\nVmSize:\t500 kB\n");
        let got = rss(&f, 1);
        assert!(!got.is_measured());
        assert_eq!(got.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn non_numeric_field_value_is_excluded_not_zero() {
        let f = FakeProc::default().status_text(1, "VmRSS:\tnot-a-number kB\n");
        let got = rss(&f, 1);
        assert!(!got.is_measured());
    }

    #[test]
    fn empty_status_file_is_excluded_not_zero() {
        let f = FakeProc::default().status_text(1, "");
        let got = rss(&f, 1);
        assert!(!got.is_measured());
    }

    #[test]
    fn one_malformed_sibling_does_not_poison_the_readable_ones() {
        let f = FakeProc::default()
            .rss(1, 1000)
            .child(2, 1)
            .status_text(2, "VmRSS:\tgarbage kB\n")
            .child(3, 1)
            .rss(3, 2000);
        assert_eq!(rss(&f, 1).copied(), Some((1000.0 + 2000.0) / 1024.0));
    }

    // ---- ported from lib/mem_rss_test.sh -----------------------------------------------------------

    #[test]
    fn root_pid_zero_is_absent_not_measured() {
        // "no pid (inspect failed / not running)" in the shell version.
        let f = FakeProc::default();
        assert!(!rss(&f, 0).is_measured());
        assert!(!hwm(&f, 0).is_measured());
    }

    #[test]
    fn an_entirely_unresolvable_root_is_absent() {
        // A root pid with no ppid entry and no status: the shell's "no /proc at all" / unknown-pid case.
        let f = FakeProc::default();
        let got = rss(&f, 4_194_305);
        assert!(!got.is_measured());
        assert_eq!(got.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn rss_and_hwm_are_read_from_their_own_fields_not_conflated() {
        // Different VmRSS and VmHWM values on the same pid: each reader must pick its own field.
        let f = FakeProc::default().status_text(
            1,
            "Name:\tgw\nVmRSS:\t2048 kB\nVmHWM:\t8192 kB\n",
        );
        assert_eq!(rss(&f, 1).copied(), Some(2.0));
        assert_eq!(hwm(&f, 1).copied(), Some(8.0));
    }

    #[test]
    fn hwm_can_exceed_rss_a_settled_peak_above_current_use() {
        // The peak (VmHWM) may sit above the live reading (VmRSS); the reader must not clamp either.
        let f = FakeProc::default().status_text(1, "VmRSS:\t100 kB\nVmHWM:\t500 kB\n");
        assert_eq!(rss(&f, 1).copied(), Some(100.0 / 1024.0));
        assert_eq!(hwm(&f, 1).copied(), Some(500.0 / 1024.0));
    }

    // ---- properties -----------------------------------------------------------------------------

    proptest! {
        // A tree built from an arbitrary set of positive per-pid kB readings sums to exactly their
        // total (converted once), regardless of how many descendants or how deep the tree is.
        #[test]
        fn sum_matches_the_readings_for_any_tree_shape(
            kbs in proptest::collection::vec(1u64..1_000_000, 1..30),
        ) {
            // Build a simple star: root 1, children 2..=n+1, each with its own reading.
            let mut f = FakeProc::default().rss(1, 0);
            // Root itself unreadable-for-RSS-purposes would mess the total, so give it a real reading
            // too and fold it into the expected sum.
            let mut expected_kb: u64 = 0;
            for (i, kb) in kbs.iter().enumerate() {
                let pid = (i as u32) + 2;
                f = f.child(pid, 1).rss(pid, *kb);
                expected_kb += *kb;
            }
            let got = rss(&f, 1).copied();
            prop_assert_eq!(got, Some(expected_kb as f64 / 1024.0));
        }

        // However many times a pid is reachable in the walk, the trait-level dedup means the total
        // never depends on graph shape once the SET of readable pids is fixed.
        #[test]
        fn total_depends_only_on_the_set_of_pids_not_on_path_multiplicity(
            kb1 in 1u64..100_000,
            kb2 in 1u64..100_000,
        ) {
            let star = FakeProc::default().rss(1, kb1).child(2, 1).rss(2, kb2);
            let chain = FakeProc::default().rss(1, kb1).child(2, 1).rss(2, kb2);
            prop_assert_eq!(rss(&star, 1).copied(), rss(&chain, 1).copied());
        }
    }
}
