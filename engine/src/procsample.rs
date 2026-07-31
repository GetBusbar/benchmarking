// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// WHAT A GATEWAY COST, not only what it delivered.
//
// The board could say a gateway served 46,031 rps. It could not say what that cost, and that gap
// produced three real failures on the 2026-07-30 field run:
//
//   1. Peak RPS is a SATURATION number. Once a gateway fills its pinned cores the ladder stops
//      measuring the gateway and starts measuring the box, so a faster build shows no gain at all.
//   2. A saturated run and an unsaturated one looked identical. A ceiling was inferred from the
//      shape of two p99 curves - wrongly, twice - because there was no utilisation figure to check
//      the inference against.
//   3. A swapping box publishes as a slow gateway. Nothing detected major faults, so a RIG problem
//      was attributable to the SUBJECT, which is the one thing this project exists to prevent.
//
// `cpu_us_per_request` is immune to the ceiling: at saturation two gateways deliver the same rps by
// definition, but the one doing less work per request still reads lower. It is also the number that
// maps to money, since a gateway costing half the CPU serves the same traffic on half the instance.
//
// EVERYTHING HERE IS A DELTA ACROSS THE WINDOW, NEVER AN ABSOLUTE. A process's absolute `utime`
// includes its startup, its config parse, and every load window that ran before this one; charging
// that to this window's requests would inflate whichever cell happened to run first and make cell
// order look like a gateway property.
//
// Counters are summed over the same PROCESS TREE `rss.rs` resolves, for the same reason it does: a
// gateway that forks workers keeps its CPU in the children, and a parent-only reading reports a busy
// gateway as idle.

use crate::measurement::{Absent, Measurement};
use crate::rss::{tree_pids, ProcSource};
use std::collections::BTreeMap;

/// Kernel USER_HZ. This is 100 on every Linux ABI regardless of the kernel's internal CONFIG_HZ -
/// the value `/proc/<pid>/stat` reports jiffies in is fixed by the userspace ABI, not by the tick
/// rate the kernel was built with. Hard-coding it avoids a libc dependency for `sysconf(_SC_CLK_TCK)`
/// in a binary that otherwise needs none; if this is ever wrong, cpu figures are off by a constant
/// factor across the whole board and would be caught by the first sanity check against wall time.
const USER_HZ: f64 = 100.0;

/// Raw counters read from one process tree at one instant. Absolute values - only differences of
/// two of these are ever published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// utime+stime in jiffies, summed across the tree.
    pub cpu_jiffies: u64,
    /// Major faults: a page that had to come from disk. NON-ZERO INVALIDATES THE WINDOW - the box
    /// was swapping, and what was measured is the disk, not the gateway.
    pub majflt: u64,
    /// Threads across the tree. Not a rate; sampled for shape (thread-per-connection vs async).
    pub threads: u64,
    /// Context switches. `nonvoluntary` is the interesting one: it means the scheduler took the CPU
    /// away, which is what a saturated core looks like from inside the process.
    pub vol_ctxt: u64,
    pub nonvol_ctxt: u64,
    /// Bytes through read()/write() syscalls (`rchar`/`wchar`), which counts buffered traffic a
    /// gateway copies rather than only what reached a device.
    pub rchar: u64,
    pub wchar: u64,
}

/// What a window cost, derived from two `Counters` readings and the window's own completed request
/// count. Every field is optional at the type level because a counter can be unreadable on its own.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowCost {
    pub cpu_us: Measurement<f64>,
    pub cpu_us_per_request: Measurement<f64>,
    pub rps_per_cpu_second: Measurement<f64>,
    pub majflt: Measurement<f64>,
    pub threads_end: Measurement<f64>,
    pub nonvol_ctxt_per_request: Measurement<f64>,
    pub vol_ctxt_per_request: Measurement<f64>,
    pub rchar_per_request: Measurement<f64>,
    pub wchar_per_request: Measurement<f64>,
    /// TRUE means the numbers above exist but must not be trusted: the box took major faults during
    /// the window. Published WITH the numbers rather than instead of them, so a reader sees why a row
    /// looks wrong instead of finding a hole where an explanation should be.
    pub swapped: bool,
}

/// Everything this module needs to read, abstracted so the whole thing is testable against fixture
/// text with no live process. `ppid_map`/`status` come from the RSS sampler's trait - the same tree
/// resolution, not a second implementation that could disagree with it.
pub trait CounterSource: ProcSource {
    /// Raw `/proc/<pid>/stat`, or `None` if the pid is gone or unreadable.
    fn stat(&self, pid: u32) -> Option<String>;
    /// Raw `/proc/<pid>/io`. `None` is ordinary, not exceptional: this file is unreadable without
    /// matching credentials on some kernels, so byte counters are best-effort and their absence must
    /// not take the CPU figure down with them.
    fn io(&self, _pid: u32) -> Option<String> {
        None
    }
}

/// The real filesystem. `RealProc` already implements the tree half of this (`ppid_map`/`status`)
/// for the RSS sampler, so extending it here keeps ONE resolution of "which processes are the
/// gateway" rather than a second one that could drift from it.
impl CounterSource for crate::rss::RealProc {
    fn stat(&self, pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
    }
    fn io(&self, pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/io")).ok()
    }
}

/// Sample the live process tree rooted at `root_pid`.
pub fn sample_live(root_pid: u32) -> Measurement<Counters> {
    sample(&crate::rss::RealProc, root_pid)
}

/// The field AFTER the command name, by zero-based index into what follows the LAST `)`.
///
/// Indexing from the last `)` rather than by splitting the whole line is not defensive tidiness: a
/// process is free to have `)` and whitespace in its comm, and `/proc/<pid>/stat` does not quote it.
/// Splitting the raw line shifts every field for such a process and would silently read `flags` as
/// `utime`. `rss.rs::read_ppid` locates ppid the same way for the same reason.
///
/// Offsets (stat is 1-based, this index is 0-based over the post-`)` tail):
///   idx 0 = state (f3), 1 = ppid (f4), 9 = majflt (f12), 11 = utime (f14), 12 = stime (f15),
///   17 = num_threads (f20).
fn stat_field(stat: &str, idx: usize) -> Option<u64> {
    let rparen = stat.rfind(')')?;
    stat[rparen + 1..].split_whitespace().nth(idx)?.parse().ok()
}

/// First numeric token after `<field>:` in `/proc/<pid>/status` or `/proc/<pid>/io`. Missing field,
/// non-numeric value and empty content are all `None` - "could not read", never a zero.
fn labelled_field(text: &str, field: &str) -> Option<u64> {
    let prefix = format!("{field}:");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Sum every counter across the tree rooted at `root_pid`.
///
/// A pid that cannot be read CONTRIBUTES NOTHING rather than failing the sample: processes legitimately
/// exit mid-scan, and one vanished worker must not void a window. But if NOTHING in the tree was
/// readable, that is an absence with a reason and not a zero - a live tree cannot consume no CPU.
pub fn sample<S: CounterSource>(source: &S, root_pid: u32) -> Measurement<Counters> {
    if root_pid == 0 {
        return Measurement::absent_because(Absent::NotMeasured, "no root pid to sample");
    }
    let ppid_map: BTreeMap<u32, u32> = source.ppid_map();
    let mut acc = Counters::default();
    let mut read_any = false;

    for pid in tree_pids(root_pid, &ppid_map) {
        if let Some(stat) = source.stat(pid) {
            // utime+stime are the metric; a stat line we cannot parse contributes nothing.
            if let (Some(u), Some(s)) = (stat_field(&stat, 11), stat_field(&stat, 12)) {
                acc.cpu_jiffies += u + s;
                read_any = true;
            }
            if let Some(m) = stat_field(&stat, 9) {
                acc.majflt += m;
            }
            if let Some(t) = stat_field(&stat, 17) {
                acc.threads += t;
            }
        }
        if let Some(status) = source.status(pid) {
            if let Some(v) = labelled_field(&status, "voluntary_ctxt_switches") {
                acc.vol_ctxt += v;
            }
            if let Some(v) = labelled_field(&status, "nonvoluntary_ctxt_switches") {
                acc.nonvol_ctxt += v;
            }
        }
        if let Some(io) = source.io(pid) {
            if let Some(v) = labelled_field(&io, "rchar") {
                acc.rchar += v;
            }
            if let Some(v) = labelled_field(&io, "wchar") {
                acc.wchar += v;
            }
        }
    }

    if !read_any {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!("no readable /proc/<pid>/stat in the tree rooted at pid {root_pid}"),
        );
    }
    Measurement::Measured(acc)
}

/// A counter that went BACKWARDS across the window.
///
/// This is the case the natural implementation gets wrong, because it subtracts and publishes a
/// negative. It is not hypothetical: pids are recycled, and a tree whose members changed between the
/// two samples can legitimately total less at the end than at the start. A negative cost is not a
/// small error, it is an impossible number - and by this project's own rule an impossible number is
/// an ENGINE bug, so it must never reach the board.
fn delta(start: u64, end: u64, what: &str) -> Result<u64, String> {
    end.checked_sub(start).ok_or_else(|| {
        format!("{what} went backwards across the window ({start} -> {end}), which means the process tree changed identity (pid reuse); this window cannot be charged")
    })
}

/// Turn two samples plus the window's completed request count into published cost.
///
/// `requests` comes from the window's OWN completed count, never from the published rps: rps is
/// already derived, and deriving cost from a derivation makes one number's error the other's too.
pub fn cost(
    start: &Measurement<Counters>,
    end: &Measurement<Counters>,
    requests: u64,
    window_s: f64,
) -> WindowCost {
    let absent = |a: Absent, m: &str| Measurement::absent_because(a, m.to_string());
    let (Measurement::Measured(a), Measurement::Measured(b)) = (start, end) else {
        let why = "a counter sample for this window was absent, so its cost cannot be derived";
        return WindowCost {
            cpu_us: absent(Absent::NotMeasured, why),
            cpu_us_per_request: absent(Absent::NotMeasured, why),
            rps_per_cpu_second: absent(Absent::NotMeasured, why),
            majflt: absent(Absent::NotMeasured, why),
            threads_end: absent(Absent::NotMeasured, why),
            nonvol_ctxt_per_request: absent(Absent::NotMeasured, why),
            vol_ctxt_per_request: absent(Absent::NotMeasured, why),
            rchar_per_request: absent(Absent::NotMeasured, why),
            wchar_per_request: absent(Absent::NotMeasured, why),
            swapped: false,
        };
    };

    // A backwards counter is a HARNESS failure, not a gateway property: the tree changed identity
    // under us. HarnessError, never a number.
    let per = |d: Result<u64, String>, what: &str| -> Measurement<f64> {
        match d {
            Err(e) => Measurement::absent_because(Absent::HarnessError, e),
            Ok(_) if requests == 0 => Measurement::absent_because(
                Absent::NotMeasured,
                format!("{what} cannot be charged per request: the window completed 0 requests"),
            ),
            Ok(v) => Measurement::Measured(v as f64 / requests as f64),
        }
    };

    let cpu_j = delta(a.cpu_jiffies, b.cpu_jiffies, "cpu jiffies");
    let cpu_us = match &cpu_j {
        Err(e) => Measurement::absent_because(Absent::HarnessError, e.clone()),
        Ok(j) => Measurement::Measured(*j as f64 / USER_HZ * 1_000_000.0),
    };
    let cpu_us_per_request = match (&cpu_j, requests) {
        (Err(e), _) => Measurement::absent_because(Absent::HarnessError, e.clone()),
        (Ok(_), 0) => absent(
            Absent::NotMeasured,
            "cpu cannot be charged per request: the window completed 0 requests",
        ),
        (Ok(j), n) => Measurement::Measured(*j as f64 / USER_HZ * 1_000_000.0 / n as f64),
    };
    // rps per CPU-second is how an operator sizes a box: requests served per second of CPU burned.
    let rps_per_cpu_second = match (&cpu_j, window_s > 0.0) {
        (Err(e), _) => Measurement::absent_because(Absent::HarnessError, e.clone()),
        (Ok(0), _) => absent(
            Absent::BelowResolution,
            "the window burned less than one jiffy of CPU, which this clock cannot divide by",
        ),
        (Ok(j), true) => Measurement::Measured(requests as f64 / (*j as f64 / USER_HZ)),
        (Ok(_), false) => absent(Absent::NotMeasured, "window length was not positive"),
    };

    let majflt_d = delta(a.majflt, b.majflt, "major faults");
    let swapped = matches!(majflt_d, Ok(n) if n > 0);

    WindowCost {
        cpu_us,
        cpu_us_per_request,
        rps_per_cpu_second,
        majflt: match &majflt_d {
            Err(e) => Measurement::absent_because(Absent::HarnessError, e.clone()),
            Ok(n) => Measurement::Measured(*n as f64),
        },
        // Threads is a LEVEL, not a delta: the question is how many the gateway ran with, not how
        // many it gained. Taken at the end, when the tree is at its most populated.
        threads_end: Measurement::Measured(b.threads as f64),
        nonvol_ctxt_per_request: per(
            delta(
                a.nonvol_ctxt,
                b.nonvol_ctxt,
                "nonvoluntary context switches",
            ),
            "nonvoluntary context switches",
        ),
        vol_ctxt_per_request: per(
            delta(a.vol_ctxt, b.vol_ctxt, "voluntary context switches"),
            "voluntary context switches",
        ),
        rchar_per_request: per(delta(a.rchar, b.rchar, "rchar"), "rchar"),
        wchar_per_request: per(delta(a.wchar, b.wchar, "wchar"), "wchar"),
        swapped,
    }
}

#[cfg(test)]
mod tests {
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
}
