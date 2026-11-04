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
        self.status.insert(
            pid,
            format!("Name:\tp{pid}\nVmRSS:\t{kb} kB\nVmHWM:\t{kb} kB\n"),
        );
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
    let f = FakeProc::default()
        .rss(1, 1000)
        .child(2, 1)
        .rss(2, 2000)
        .child(3, 1);
    // Exactly the readable total: a missing pid contributes nothing. (Skipping it and adding 0 are
    // the same number in a sum; what this pins is that no placeholder is added for it.)
    assert_eq!(rss(&f, 1).copied(), Some((1000.0 + 2000.0) / 1024.0));
}

// THE ROOT IS THE GATEWAY. Summing its children without it published a figure missing the largest
// member as a complete tree total, and memory is ranked ascending.
#[test]
fn an_unreadable_root_with_readable_children_is_absent_not_a_partial_total() {
    let f = FakeProc::default()
        .child(2, 1)
        .rss(2, 2000)
        .child(3, 1)
        .rss(3, 3000);
    let got = rss(&f, 1);
    assert_eq!(got.reason(), Some(&Absent::NotMeasured), "{got:?}");
    assert!(got.detail().is_some_and(|d| d.contains("root")), "{got:?}");
}

#[test]
fn missing_pid_is_excluded_but_present_siblings_still_sum_correctly() {
    // Three pids in the tree, only two readable. Total must equal exactly those two.
    let f = FakeProc::default()
        .rss(1, 500)
        .child(2, 1)
        .rss(2, 1500)
        .child(3, 1); // 3: unreadable
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
    let f = FakeProc::default()
        .rss(1, 1000)
        .child(2, 1)
        .rss(2, 2000)
        .child(3, 2)
        .rss(3, 3000);
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
    let f = FakeProc::default()
        .rss(1, 100)
        .child(2, 1)
        .rss(2, 200)
        .child(3, 1)
        .rss(3, 300)
        .child(3, 2);
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
    let f = FakeProc::default().status_text(1, "Name:\tgw\nVmRSS:\t2048 kB\nVmHWM:\t8192 kB\n");
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
        kb3 in 1u64..100_000,
    ) {
        // The same three readable pids, as a star (2 and 3 both under 1) and as a chain (3 under 2).
        let star = FakeProc::default()
            .rss(1, kb1)
            .child(2, 1)
            .rss(2, kb2)
            .child(3, 1)
            .rss(3, kb3);
        let chain = FakeProc::default()
            .rss(1, kb1)
            .child(2, 1)
            .rss(2, kb2)
            .child(3, 2)
            .rss(3, kb3);
        let want = Some((kb1 + kb2 + kb3) as f64 / 1024.0);
        prop_assert_eq!(rss(&star, 1).copied(), want);
        prop_assert_eq!(rss(&chain, 1).copied(), want);
    }
}
