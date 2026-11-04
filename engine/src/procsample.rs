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

/// Raw counters read from one process tree at one instant. Absolute values - with ONE deliberate
/// exception on the way out (`threads_end` publishes a LEVEL, because "how many threads did it hold"
/// is not a question a delta answers; see its own note) only differences of
/// two of these are ever published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    /// utime+stime in jiffies, summed across the tree, PLUS each member's cutime+cstime: the CPU of
    /// its children that have already exited and been reaped. Without that, a worker that exits
    /// mid-window takes its whole lifetime of CPU out of the end sample (it moves into its parent's
    /// cutime, which was not read), and a gateway that recycles workers published a cost too low by
    /// exactly the work those workers did.
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
    /// Which of the counters above NO process in the tree yielded. Such a counter is a 0 in this
    /// struct only because nothing was added to it, and `cost` must publish it as an absence, not as
    /// a measured zero. `Default` is "everything was read", so a sample built by hand states what it
    /// could not read rather than having it assumed.
    pub unread: Unread,
}

/// Which counter families could not be read from any process in the tree. See `Counters::unread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unread {
    /// `majflt` from `/proc/<pid>/stat`.
    pub majflt: bool,
    /// `num_threads` from `/proc/<pid>/stat`.
    pub threads: bool,
    /// The context-switch lines of `/proc/<pid>/status`.
    pub ctxt: bool,
    /// `rchar`/`wchar` from `/proc/<pid>/io`, unreadable without matching credentials on some kernels.
    pub io: bool,
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

/// Busy and total jiffies across a SET of cores, from `/proc/stat`.
///
/// THIS IS THE READING THAT SETTLES THE CEILING QUESTION. When two gateways converge on the same
/// peak, the ladder cannot say whether they are equally fast or equally stuck - and on 2026-07-30
/// that was inferred from the shape of two p99 curves, wrongly, twice. Utilisation of the cores the
/// gateway is PINNED to answers it directly: at ~100% the wall is the gateway's own CPU and the peak
/// is a real ceiling; well below it, the wall is somewhere else and the peak means something else.
///
/// Only the pinned cores are summed. The box also runs the mock and the load generator on their own
/// cores, so a machine-wide figure would blend three processes and describe none of them.
pub fn cpu_busy_total<S: SystemStatSource>(source: &S, cores: &[u32]) -> Option<(u64, u64)> {
    let text = source.system_stat()?;
    let mut busy = 0u64;
    let mut total = 0u64;
    let mut seen = false;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let Some(tag) = it.next() else { continue };
        // `cpu` with no index is the machine-wide aggregate and must NOT be summed alongside the
        // per-core lines: counting it would double every core it already contains.
        let Some(idx) = tag.strip_prefix("cpu").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if !cores.contains(&idx) {
            continue;
        }
        // EVERY TOKEN MUST PARSE. Dropping one that does not would shift the columns after it, so
        // `idle` and `iowait` below would be read from the wrong place.
        let Some(f) = it
            .map(|v| v.parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()
        else {
            continue;
        };
        // user nice system idle iowait irq softirq steal guest guest_nice
        if f.len() < 5 {
            continue;
        }
        seen = true;
        // GUEST AND GUEST_NICE ARE ALREADY INSIDE USER AND NICE (the kernel accounts guest time in
        // both), so only the first eight columns are summed. Summing all ten counted a core's guest
        // time twice in both busy and total. Steal stays in, as it always was: whether hypervisor
        // steal is "busy" for a pinned-core ceiling is a methodology question, not a parse one.
        let sum: u64 = f.iter().take(8).sum();
        // IDLE AND IOWAIT ARE BOTH NOT-BUSY. iowait is a core with nothing to run while a request is
        // outstanding; counting it as busy would report a blocked gateway as a saturated one, which
        // is the opposite of what this measurement exists to distinguish.
        let idle = f[3] + f[4];
        total += sum;
        busy += sum.saturating_sub(idle);
    }
    seen.then_some((busy, total))
}

/// Where the machine-wide CPU table comes from, split out so the arithmetic is fixture-testable.
pub trait SystemStatSource {
    fn system_stat(&self) -> Option<String>;
}

impl SystemStatSource for crate::rss::RealProc {
    fn system_stat(&self) -> Option<String> {
        std::fs::read_to_string("/proc/stat").ok()
    }
}

/// Utilisation of `cores` across a window, as a FRACTION (1.0 = every pinned core fully busy).
///
/// Absent rather than 0 when it cannot be taken, and absent rather than a number when the window
/// advanced no total jiffies at all - a zero-length window has no utilisation, and reporting 0%
/// would say the cores were idle when nothing was actually observed.
pub fn utilisation(start: Option<(u64, u64)>, end: Option<(u64, u64)>) -> Measurement<f64> {
    let (Some((b0, t0)), Some((b1, t1))) = (start, end) else {
        return Measurement::absent_because(
            Absent::NotMeasured,
            "the pinned cores could not be read from /proc/stat, so utilisation is unknown",
        );
    };
    let (Some(db), Some(dt)) = (b1.checked_sub(b0), t1.checked_sub(t0)) else {
        return Measurement::absent_because(
            Absent::HarnessError,
            format!("a /proc/stat counter went backwards across the window (busy {b0} -> {b1}, total {t0} -> {t1})"),
        );
    };
    if dt == 0 {
        return Measurement::absent_because(
            Absent::NotMeasured,
            "the pinned cores advanced no jiffies across this window, so there is no utilisation to report",
        );
    }
    Measurement::Measured(db as f64 / dt as f64)
}

/// The highest core index a CPU list may name. Far beyond any box this runs on, and low enough that a
/// typo like `0-4294967295` cannot expand into billions of entries and take the engine down mid-run.
const MAX_CORE_INDEX: u32 = 4095;

/// Parse a taskset-style CPU list ("4-7", "0,2,4", "4-5,8") into core indices.
///
/// The gateway's pinning is declared as exactly this string, so utilisation must read the SAME
/// declaration rather than a second list that could disagree with what taskset was actually given.
///
/// ALL OR NOTHING. A list with any part that does not parse (`4-7,x9`), or that names a core past
/// `MAX_CORE_INDEX`, yields NO cores - which the caller reports as utilisation it could not read -
/// rather than the parts that happened to parse, which would compute utilisation over fewer cores
/// than the gateway is pinned to and publish it as the whole set's.
pub fn parse_cores(spec: &str) -> Vec<u32> {
    let core = |t: &str| {
        t.trim()
            .parse::<u32>()
            .ok()
            .filter(|c| *c <= MAX_CORE_INDEX)
    };
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let range = match part.split_once('-') {
            Some((a, b)) => core(a).zip(core(b)),
            None => core(part).map(|c| (c, c)),
        };
        let Some((a, b)) = range else {
            return Vec::new();
        };
        // An inverted range is a typo in a manifest, not a request for zero cores; taking min..=max
        // keeps it meaning what it plainly says.
        out.extend(a.min(b)..=a.max(b));
    }
    out.sort_unstable();
    out.dedup();
    out
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
///   13 = cutime (f16), 14 = cstime (f17), 17 = num_threads (f20).
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
    // Per counter family: did ANY process in the tree yield it? One that none did is recorded in
    // `unread`, so it publishes as an absence instead of the 0 it was never added to.
    let (mut got_majflt, mut got_threads, mut got_ctxt, mut got_io) = (false, false, false, false);

    for pid in tree_pids(root_pid, &ppid_map) {
        if let Some(stat) = source.stat(pid) {
            // utime+stime are the metric; a stat line we cannot parse contributes nothing.
            if let (Some(u), Some(s)) = (stat_field(&stat, 11), stat_field(&stat, 12)) {
                // Reaped children's CPU (cutime+cstime) rides along; see `Counters::cpu_jiffies`.
                let reaped =
                    stat_field(&stat, 13).unwrap_or(0) + stat_field(&stat, 14).unwrap_or(0);
                acc.cpu_jiffies += u + s + reaped;
                read_any = true;
            }
            if let Some(m) = stat_field(&stat, 9) {
                acc.majflt += m;
                got_majflt = true;
            }
            if let Some(t) = stat_field(&stat, 17) {
                acc.threads += t;
                got_threads = true;
            }
        }
        if let Some(status) = source.status(pid) {
            if let (Some(v), Some(n)) = (
                labelled_field(&status, "voluntary_ctxt_switches"),
                labelled_field(&status, "nonvoluntary_ctxt_switches"),
            ) {
                acc.vol_ctxt += v;
                acc.nonvol_ctxt += n;
                got_ctxt = true;
            }
        }
        if let Some(io) = source.io(pid) {
            if let (Some(r), Some(w)) = (labelled_field(&io, "rchar"), labelled_field(&io, "wchar"))
            {
                acc.rchar += r;
                acc.wchar += w;
                got_io = true;
            }
        }
    }
    acc.unread = Unread {
        majflt: !got_majflt,
        threads: !got_threads,
        ctxt: !got_ctxt,
        io: !got_io,
    };

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
    // A counter neither sample could read is absent for that reason, before any arithmetic: its 0 is
    // the absence of a reading, and a delta of two such zeros is not a measured "none".
    let unread = |a_missing: bool, b_missing: bool, what: &str| -> Option<Measurement<f64>> {
        (a_missing || b_missing).then(|| {
            Measurement::absent_because(
                Absent::NotMeasured,
                format!(
                    "{what} could not be read from /proc for any process in the gateway's tree"
                ),
            )
        })
    };
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
    // Through `per`, the same rule every other per-request figure uses, then scaled from jiffies to
    // microseconds.
    let cpu_us_per_request = per(cpu_j.clone(), "cpu")
        .map(|jiffies_per_request| jiffies_per_request / USER_HZ * 1_000_000.0);
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
    // Unread faults are not "no faults": `swapped` stays false (nothing was observed), and `majflt`
    // itself says it was not measured.
    let majflt_unread = unread(a.unread.majflt, b.unread.majflt, "major faults");
    let swapped = majflt_unread.is_none() && matches!(majflt_d, Ok(n) if n > 0);
    let ctxt_unread = unread(a.unread.ctxt, b.unread.ctxt, "context switches");
    let io_unread = unread(a.unread.io, b.unread.io, "rchar/wchar");

    WindowCost {
        cpu_us,
        cpu_us_per_request,
        rps_per_cpu_second,
        majflt: majflt_unread.unwrap_or_else(|| match &majflt_d {
            Err(e) => Measurement::absent_because(Absent::HarnessError, e.clone()),
            Ok(n) => Measurement::Measured(*n as f64),
        }),
        // Threads is a LEVEL, not a delta: the question is how many the gateway ran with, not how
        // many it gained. Taken at the end, when the tree is at its most populated.
        threads_end: unread(false, b.unread.threads, "the thread count")
            .unwrap_or(Measurement::Measured(b.threads as f64)),
        nonvol_ctxt_per_request: ctxt_unread.clone().unwrap_or_else(|| {
            per(
                delta(
                    a.nonvol_ctxt,
                    b.nonvol_ctxt,
                    "nonvoluntary context switches",
                ),
                "nonvoluntary context switches",
            )
        }),
        vol_ctxt_per_request: ctxt_unread.unwrap_or_else(|| {
            per(
                delta(a.vol_ctxt, b.vol_ctxt, "voluntary context switches"),
                "voluntary context switches",
            )
        }),
        rchar_per_request: io_unread
            .clone()
            .unwrap_or_else(|| per(delta(a.rchar, b.rchar, "rchar"), "rchar")),
        wchar_per_request: io_unread
            .unwrap_or_else(|| per(delta(a.wchar, b.wchar, "wchar"), "wchar")),
        swapped,
    }
}

#[cfg(test)]
#[path = "tests/procsample_tests.rs"]
mod tests;
