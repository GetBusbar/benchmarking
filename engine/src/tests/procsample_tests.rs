use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
struct Fixture {
    ppids: BTreeMap<u32, u32>,
    stats: BTreeMap<u32, String>,
    statuses: BTreeMap<u32, String>,
    ios: BTreeMap<u32, String>,
}
impl ProcSource for Fixture {
    fn ppid_map(&self) -> BTreeMap<u32, u32> {
        self.ppids.clone()
    }
    fn status(&self, pid: u32) -> Option<String> {
        self.statuses.get(&pid).cloned()
    }
}
impl CounterSource for Fixture {
    fn stat(&self, pid: u32) -> Option<String> {
        self.stats.get(&pid).cloned()
    }
    fn io(&self, pid: u32) -> Option<String> {
        self.ios.get(&pid).cloned()
    }
}

/// A stat line with the real field layout, including a comm containing `)` and a space so the
/// parser is exercised against the shape that breaks naive splitting.
fn stat_line(majflt: u64, utime: u64, stime: u64, threads: u64) -> String {
    let mut f = vec!["0".to_string(); 20];
    f[0] = "S".into(); // state
    f[1] = "1".into(); // ppid
    f[9] = majflt.to_string();
    f[11] = utime.to_string();
    f[12] = stime.to_string();
    f[17] = threads.to_string();
    format!("42 (gate way (x)) {}", f.join(" "))
}

#[test]
fn a_comm_containing_parens_and_spaces_does_not_shift_the_fields() {
    let s = stat_line(3, 100, 50, 7);
    assert_eq!(stat_field(&s, 9), Some(3), "majflt");
    assert_eq!(stat_field(&s, 11), Some(100), "utime");
    assert_eq!(stat_field(&s, 12), Some(50), "stime");
    assert_eq!(stat_field(&s, 17), Some(7), "num_threads");
}

#[test]
fn cpu_is_summed_over_the_whole_tree_including_a_child_that_holds_it() {
    // THE CASE A PARENT-ONLY READING GETS WRONG: the parent burns nothing, the worker burns it
    // all, and a parent-only sampler would report a busy gateway as idle.
    let mut f = Fixture::default();
    f.ppids.insert(200, 100);
    f.stats.insert(100, stat_line(0, 0, 0, 1));
    f.stats.insert(200, stat_line(0, 600, 400, 8));
    let Measurement::Measured(c) = sample(&f, 100) else {
        panic!("expected a sample")
    };
    assert_eq!(c.cpu_jiffies, 1000);
    assert_eq!(c.threads, 9);
}

#[test]
fn a_vanished_pid_contributes_nothing_but_does_not_void_the_window() {
    let mut f = Fixture::default();
    f.ppids.insert(200, 100); // 200 is in the tree but its stat is gone (exited mid-scan)
    f.stats.insert(100, stat_line(0, 700, 300, 2));
    let Measurement::Measured(c) = sample(&f, 100) else {
        panic!("a readable root must still yield a sample")
    };
    assert_eq!(c.cpu_jiffies, 1000);
}

#[test]
fn a_tree_with_nothing_readable_is_absent_not_zero() {
    let f = Fixture::default();
    // Assert on the TYPE, not on the debug string: `Absent::NotMeasured` contains the substring
    // "Measured", so a `!contains("Measured")` check fails on a correctly-absent value. That is
    // a test that reports a defect where there is none, which is worse than no test.
    match sample(&f, 100) {
        Measurement::Absent { reason, .. } => assert_eq!(reason, Absent::NotMeasured),
        other => panic!("a live tree cannot consume no CPU; expected absence, got {other:?}"),
    }
}

#[test]
fn pid_zero_is_absent() {
    let f = Fixture::default();
    assert!(matches!(sample(&f, 0), Measurement::Absent { .. }));
}

#[test]
fn cost_divides_by_the_windows_own_request_count() {
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 100,
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 1100,
        ..Default::default()
    });
    // 1000 jiffies = 10 CPU-seconds = 10_000_000 us over 10_000 requests = 1000 us/req
    let c = cost(&a, &b, 10_000, 10.0);
    assert_eq!(c.cpu_us, Measurement::Measured(10_000_000.0));
    assert_eq!(c.cpu_us_per_request, Measurement::Measured(1000.0));
    assert_eq!(c.rps_per_cpu_second, Measurement::Measured(1000.0));
    assert!(!c.swapped);
}

#[test]
fn a_counter_that_goes_backwards_is_a_harness_error_never_a_negative() {
    // PID REUSE. The natural implementation subtracts and publishes a negative CPU time, which is
    // an impossible number - and an impossible number is an engine bug, so it must not ship.
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 5000,
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 10,
        ..Default::default()
    });
    let c = cost(&a, &b, 100, 10.0);
    for m in [&c.cpu_us, &c.cpu_us_per_request, &c.rps_per_cpu_second] {
        match m {
            Measurement::Absent { reason, .. } => {
                assert_eq!(
                    *reason,
                    Absent::HarnessError,
                    "the rig changed, not the gateway"
                )
            }
            other => panic!("a backwards counter must not produce a number: {other:?}"),
        }
    }
}

#[test]
fn a_window_with_major_faults_publishes_its_numbers_with_the_flag_set() {
    // The row is DISCLOSED, not dropped: a reader must see why it looks wrong.
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 0,
        majflt: 0,
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 100,
        majflt: 12,
        ..Default::default()
    });
    let c = cost(&a, &b, 1000, 1.0);
    assert!(c.swapped, "major faults must raise the flag");
    assert_eq!(c.majflt, Measurement::Measured(12.0));
    assert!(
        matches!(c.cpu_us_per_request, Measurement::Measured(_)),
        "numbers still published"
    );
}

#[test]
fn zero_requests_cannot_be_divided_by_and_is_absent_not_infinite() {
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 0,
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 500,
        ..Default::default()
    });
    let c = cost(&a, &b, 0, 5.0);
    assert!(matches!(c.cpu_us_per_request, Measurement::Absent { .. }));
    assert!(
        matches!(c.cpu_us, Measurement::Measured(_)),
        "the window still burned CPU"
    );
}

#[test]
fn an_absent_sample_makes_every_derived_field_absent_rather_than_zero() {
    let a: Measurement<Counters> = Measurement::absent_because(Absent::NotMeasured, "gone");
    let b = Measurement::Measured(Counters::default());
    let c = cost(&a, &b, 100, 1.0);
    for m in [&c.cpu_us, &c.cpu_us_per_request, &c.rps_per_cpu_second] {
        assert!(matches!(m, Measurement::Absent { .. }), "got {m:?}");
    }
}

#[test]
fn context_switches_and_byte_counters_are_per_request_deltas() {
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 0,
        nonvol_ctxt: 100,
        vol_ctxt: 200,
        rchar: 1000,
        wchar: 2000,
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 10,
        nonvol_ctxt: 600,
        vol_ctxt: 1200,
        rchar: 11_000,
        wchar: 22_000,
        ..Default::default()
    });
    let c = cost(&a, &b, 100, 1.0);
    assert_eq!(c.nonvol_ctxt_per_request, Measurement::Measured(5.0));
    assert_eq!(c.vol_ctxt_per_request, Measurement::Measured(10.0));
    assert_eq!(c.rchar_per_request, Measurement::Measured(100.0));
    assert_eq!(c.wchar_per_request, Measurement::Measured(200.0));
}

struct Stat(String);
impl SystemStatSource for Stat {
    fn system_stat(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

#[test]
fn only_the_pinned_cores_are_summed_and_the_aggregate_line_is_ignored() {
    // The `cpu` line with no index already CONTAINS every core; summing it beside the per-core
    // lines would double-count the whole machine. The box also runs the mock and the load
    // generator on their own cores, so a machine-wide figure would describe none of the three.
    let s = Stat(
            "cpu  9999 0 9999 9999 0 0 0 0 0 0\n             cpu0 100 0 0 900 0 0 0 0 0 0\n             cpu4 300 0 0 700 0 0 0 0 0 0\n             cpu5 500 0 0 500 0 0 0 0 0 0\n"
                .to_string(),
        );
    // cpu4+cpu5 only: busy 300+500=800, total 1000+1000=2000
    assert_eq!(cpu_busy_total(&s, &[4, 5]), Some((800, 2000)));
}

#[test]
fn iowait_counts_as_not_busy() {
    // A core with nothing to run while a request is outstanding is not a saturated core.
    // Counting iowait as busy would report a BLOCKED gateway as a maxed-out one - the exact
    // opposite of the distinction this measurement exists to make.
    let s = Stat("cpu4 100 0 0 100 800 0 0 0 0 0\n".to_string());
    let (busy, total) = cpu_busy_total(&s, &[4]).expect("readable");
    assert_eq!(total, 1000);
    assert_eq!(busy, 100, "800 jiffies of iowait are idle, not work");
}

#[test]
fn utilisation_is_a_fraction_of_the_pinned_set() {
    // 900 busy of 1000 total across the window = 90% of the pinned cores.
    assert_eq!(
        utilisation(Some((100, 1000)), Some((1000, 2000))),
        Measurement::Measured(0.9)
    );
}

#[test]
fn an_unreadable_or_backwards_or_empty_window_is_absent_not_zero() {
    // Absent, never 0: reporting 0% would claim the cores were idle when nothing was observed.
    assert!(matches!(
        utilisation(None, Some((1, 2))),
        Measurement::Absent { .. }
    ));
    match utilisation(Some((500, 1000)), Some((10, 20))) {
        Measurement::Absent { reason, .. } => assert_eq!(reason, Absent::HarnessError),
        other => panic!("a backwards counter must not produce a number: {other:?}"),
    }
    // A window that advanced no jiffies has no utilisation to report.
    assert!(matches!(
        utilisation(Some((5, 10)), Some((5, 10))),
        Measurement::Absent { .. }
    ));
}

#[test]
fn a_taskset_cpu_list_parses_the_way_taskset_reads_it() {
    assert_eq!(parse_cores("4-7"), vec![4, 5, 6, 7]);
    assert_eq!(parse_cores("0,2,4"), vec![0, 2, 4]);
    assert_eq!(parse_cores("4-5,8"), vec![4, 5, 8]);
    // An inverted range is a manifest typo, not a request for zero cores.
    assert_eq!(parse_cores("7-4"), vec![4, 5, 6, 7]);
    assert_eq!(parse_cores(""), Vec::<u32>::new());
}

#[test]
fn io_is_best_effort_and_its_absence_does_not_take_cpu_down_with_it() {
    // /proc/<pid>/io is unreadable without matching credentials on some kernels. That must cost
    // the byte counters and nothing else.
    let mut f = Fixture::default();
    f.stats.insert(100, stat_line(0, 500, 500, 4));
    let Measurement::Measured(c) = sample(&f, 100) else {
        panic!("cpu must survive")
    };
    assert_eq!(c.cpu_jiffies, 1000);
    assert_eq!(c.rchar, 0);
}

// ---- a counter nothing could read is absent, never a measured zero ------------------------------

// THE DEFECT: `sample` only ADDED what it could read, so a tree whose /proc/<pid>/io (or status, or a
// stat field) was unreadable left a 0 in the struct, and `cost` published rchar/wchar, context
// switches and threads as Measured(0.0) - a gateway that "never loses the CPU" because we could not
// look.
#[test]
fn a_counter_no_process_yielded_publishes_as_absent_not_zero() {
    let mut f = Fixture::default();
    // stat readable (cpu, faults, threads); status and io unreadable for every pid.
    f.stats.insert(100, stat_line(0, 500, 500, 4));
    let Measurement::Measured(start) = sample(&f, 100) else {
        panic!("cpu is readable, so the sample exists")
    };
    assert!(start.unread.ctxt && start.unread.io);
    assert!(!start.unread.majflt && !start.unread.threads);
    let mut end = start;
    end.cpu_jiffies += 100;
    let c = cost(
        &Measurement::Measured(start),
        &Measurement::Measured(end),
        1000,
        1.0,
    );
    for (name, m) in [
        ("nonvol_ctxt_per_request", &c.nonvol_ctxt_per_request),
        ("vol_ctxt_per_request", &c.vol_ctxt_per_request),
        ("rchar_per_request", &c.rchar_per_request),
        ("wchar_per_request", &c.wchar_per_request),
    ] {
        assert_eq!(
            m.reason(),
            Some(&Absent::NotMeasured),
            "{name} was never read and must not publish as a number: {m:?}"
        );
    }
    assert!(c.cpu_us_per_request.is_measured(), "cpu itself was read");
    assert_eq!(c.threads_end, Measurement::Measured(4.0));
}

// Unread major faults are "not observed", never "no faults": the swap flag stays down and the count
// says why it is missing.
#[test]
fn unread_major_faults_are_absent_and_never_raise_or_clear_the_swap_flag_by_accident() {
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 0,
        unread: Unread {
            majflt: true,
            threads: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 100,
        majflt: 5,
        unread: Unread {
            majflt: true,
            threads: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let c = cost(&a, &b, 100, 1.0);
    assert_eq!(c.majflt.reason(), Some(&Absent::NotMeasured));
    assert!(!c.swapped);
    assert_eq!(c.threads_end.reason(), Some(&Absent::NotMeasured));
}

// A WORKER THAT EXITS MID-WINDOW KEEPS ITS CPU. Once reaped, its utime+stime move into the parent's
// cutime+cstime; reading only live members' utime+stime dropped its whole lifetime of CPU from the
// end sample and under-reported the window's cost.
#[test]
fn a_reaped_childs_cpu_is_still_counted_through_its_parents_cutime() {
    fn with_reaped(utime: u64, stime: u64, cutime: u64, cstime: u64) -> String {
        let mut f = vec!["0".to_string(); 20];
        f[0] = "S".into();
        f[1] = "1".into();
        f[11] = utime.to_string();
        f[12] = stime.to_string();
        f[13] = cutime.to_string();
        f[14] = cstime.to_string();
        f[17] = "1".into();
        format!("100 (gw) {}", f.join(" "))
    }
    // Start: a parent at 100 jiffies and a live worker at 400.
    let mut before = Fixture::default();
    before.ppids.insert(200, 100);
    before.stats.insert(100, with_reaped(100, 0, 0, 0));
    before.stats.insert(200, with_reaped(400, 0, 0, 0));
    // End: the worker burned 100 more, exited and was reaped - its 500 is now the parent's cutime.
    let mut after = Fixture::default();
    after.stats.insert(100, with_reaped(150, 0, 450, 50));
    let (Measurement::Measured(a), Measurement::Measured(b)) =
        (sample(&before, 100), sample(&after, 100))
    else {
        panic!("both samples are readable")
    };
    assert_eq!(a.cpu_jiffies, 500);
    assert_eq!(
        b.cpu_jiffies, 650,
        "the reaped worker's 500 jiffies must still be in the tree's total"
    );
}

// GUEST TIME IS ALREADY INSIDE USER/NICE. Summing all ten /proc/stat columns counted it twice.
#[test]
fn guest_columns_are_not_counted_twice() {
    // user 300 (of which guest 200), idle 700: busy 300 of 1000, not 500 of 1200.
    let s = Stat("cpu4 300 0 0 700 0 0 0 0 200 0\n".to_string());
    assert_eq!(cpu_busy_total(&s, &[4]), Some((300, 1000)));
}

// A /proc/stat line with a token that does not parse is skipped whole, not read with every later
// column shifted one place left.
#[test]
fn a_cpu_line_with_an_unparseable_token_is_skipped_rather_than_shifted() {
    let s = Stat("cpu4 100 x 0 900 0 0 0 0 0 0\n".to_string());
    assert_eq!(cpu_busy_total(&s, &[4]), None);
}

// ALL OR NOTHING: a partly-parseable list must not compute utilisation over fewer cores than the
// gateway is pinned to, and an absurd range must not expand into billions of entries.
#[test]
fn a_malformed_or_absurd_cpu_list_names_no_cores() {
    assert_eq!(parse_cores("4-7,x9"), Vec::<u32>::new());
    assert_eq!(parse_cores("4-7;8"), Vec::<u32>::new());
    assert_eq!(parse_cores("0-4294967295"), Vec::<u32>::new());
    assert_eq!(parse_cores("4-7, 9"), vec![4, 5, 6, 7, 9]);
}

// ---- the cost formulas, on a fixture where they cannot coincide --------------------------------

// The other fixture makes cpu_us_per_request and rps_per_cpu_second both 1000 and window_s equal to
// the CPU seconds, so swapping the formulas - or computing the headline efficiency as a wall-clock
// rate - passed. Here every candidate answer differs.
#[test]
fn rps_per_cpu_second_divides_requests_by_cpu_seconds_not_by_wall_time() {
    let a = Measurement::Measured(Counters::default());
    // 250 jiffies = 2.5 CPU-seconds, 1000 requests, a 4 s window.
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 250,
        ..Default::default()
    });
    let c = cost(&a, &b, 1000, 4.0);
    assert_eq!(c.rps_per_cpu_second, Measurement::Measured(400.0));
    assert_eq!(c.cpu_us_per_request, Measurement::Measured(2500.0));
    assert_eq!(c.cpu_us, Measurement::Measured(2_500_000.0));
}

// Under one jiffy there is nothing to divide by; a window length that is not positive has no rate.
#[test]
fn a_window_under_one_jiffy_or_of_no_length_has_no_efficiency_figure() {
    let a = Measurement::Measured(Counters {
        cpu_jiffies: 10,
        ..Default::default()
    });
    let c = cost(&a, &a, 1000, 4.0);
    assert_eq!(
        c.rps_per_cpu_second.reason(),
        Some(&Absent::BelowResolution)
    );
    let b = Measurement::Measured(Counters {
        cpu_jiffies: 20,
        ..Default::default()
    });
    let c = cost(&a, &b, 1000, 0.0);
    assert_eq!(c.rps_per_cpu_second.reason(), Some(&Absent::NotMeasured));
}
