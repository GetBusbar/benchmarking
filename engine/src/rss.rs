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
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
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
///
/// The children index is built once per call, so the walk is linear in the size of the map rather
/// than one full scan of it per tree member: the memory sampler runs this every ~100 ms on the box
/// under test.
pub(crate) fn tree_pids(root: u32, ppid_map: &BTreeMap<u32, u32>) -> BTreeSet<u32> {
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (&pid, &ppid) in ppid_map {
        children.entry(ppid).or_default().push(pid);
    }
    let mut seen = BTreeSet::new();
    seen.insert(root);
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &pid in children.get(&parent).map(Vec::as_slice).unwrap_or(&[]) {
            if seen.insert(pid) {
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
    let mut root_read = false;
    for pid in pids {
        let Some(status) = source.status(pid) else {
            continue;
        };
        let Some(kb) = parse_field_kb(&status, field) else {
            continue;
        };
        read_any = true;
        root_read |= pid == root_pid;
        total_kb += kb;
    }

    // THE ROOT ITSELF MUST BE READ. A worker that exits mid-scan legitimately drops out, but the
    // root is the gateway's own process: summing its children without it published a figure missing
    // the largest member as a complete tree total, and memory is ranked ascending, so the hole
    // flattered the gateway.
    if read_any && !root_read {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "{field} of the tree's root pid {root_pid} was unreadable while its descendants were \
                 readable, so the tree total would be missing its root"
            ),
        );
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
/// so it cannot miss a sub-interval spike between polls - FOR A SINGLE PROCESS.
///
/// For a multi-process tree this is the sum of each member's OWN peak, and those peaks need not have
/// happened at the same moment: it is an upper bound on the tree's simultaneous peak, not that peak,
/// and a member that already exited contributes nothing. Like `rss_tree_mib`, it also counts pages
/// shared between members once per member.
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
    /// The container runtime's reported root pid for a container name, `None` when there is none.
    fn docker_pid(&self, container: &str) -> Option<u32>;
    /// The same lookup, keeping WHY there is no pid. `Ok(None)` means the runtime answered that no
    /// such container is running; `Err` means the runtime itself could not be asked (no `docker`
    /// binary, daemon unreachable, permission denied), which is a rig fault and must not read as the
    /// gateway having died. Defaults to `docker_pid` for a source that cannot fail that way.
    fn docker_lookup(&self, container: &str) -> Result<Option<u32>, String> {
        Ok(self.docker_pid(container))
    }
    /// The root of the process tree whose command lines match, or why there is no single answer.
    fn matching_pid(&self, pattern: &str) -> PidLookup;
}

/// What looking a `proc_match` up on the live box produced. THREE OUTCOMES, NOT TWO: "several
/// processes match and none of them is the parent of the others" is not the same finding as "nothing
/// matches", and it must not be answered with a guess. The old code took the numerically smallest
/// matching pid, which on a box where an older unrelated process happened to match published THAT
/// process's tree as the gateway's memory - a real number, from the wrong process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PidLookup {
    /// Exactly one process matched, or several did and one of them is the ancestor of all the rest -
    /// which is a gateway that forked workers, and the ancestor is the root of its tree.
    Found(u32),
    /// Nothing matched. Includes an empty pattern, which names nothing and must never be read as
    /// naming everything.
    NotRunning,
    /// Several unrelated processes matched. Refusing to measure is the honest answer: any choice
    /// among them would be a coin toss published as this gateway's memory.
    Ambiguous(Vec<u32>),
}

/// The one match among `pids` that every other match descends from, if there is exactly one.
///
/// A forking gateway is the case this exists for: master and workers all carry the same command
/// line, so they all match, and the tree to sum is the master's. Two unrelated matches have no
/// common member and yield `None`, which the caller turns into an absence rather than a guess.
fn sole_tree_root(pids: &[u32], ppid_of: &BTreeMap<u32, u32>) -> Option<u32> {
    let is_ancestor_of = |candidate: u32, mut pid: u32| -> bool {
        let mut seen = BTreeSet::new();
        while seen.insert(pid) {
            if pid == candidate {
                return true;
            }
            match ppid_of.get(&pid).copied() {
                Some(0) | None => return false,
                Some(parent) => pid = parent,
            }
        }
        false // a cycle in the reported parent links: not an ancestry we can assert.
    };
    let mut roots = pids
        .iter()
        .copied()
        .filter(|&c| pids.iter().all(|&p| is_ancestor_of(c, p)));
    let first = roots.next()?;
    roots.next().is_none().then_some(first)
}

/// The real lookups: the container runtime for a container, a command-line match for a native
/// process. Kept thin; the logic worth testing is in `root_pid_from`.
pub struct RealPids;

impl PidSource for RealPids {
    fn docker_pid(&self, container: &str) -> Option<u32> {
        self.docker_lookup(container).ok().flatten()
    }

    fn docker_lookup(&self, container: &str) -> Result<Option<u32>, String> {
        let out = std::process::Command::new("docker")
            .args(["inspect", "-f", "{{.State.Pid}}", container])
            .output()
            .map_err(|e| format!("could not run `docker inspect`: {e}"))?;
        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
            // "No such object/container" is the runtime's answer about the container; anything else
            // (daemon unreachable, permission denied) is the runtime failing to answer at all.
            if why.contains("No such") {
                return Ok(None);
            }
            return Err(format!("`docker inspect` exited {}: {why}", out.status));
        }
        // A stopped container reports pid 0. That is not a process, and summing a tree from it would
        // read whatever pid 0 resolves to rather than nothing.
        match String::from_utf8_lossy(&out.stdout).trim().parse::<u32>() {
            Ok(0) | Err(_) => Ok(None),
            Ok(pid) => Ok(Some(pid)),
        }
    }

    /// THE SAME MATCH RULE THE STOP PATH USES, from `supervise::select_matches`: the harness, the
    /// shells running `commands` lines and a second engine are excluded, and an empty pattern names
    /// nothing. What is left is resolved by ANCESTRY rather than by pid order - the old
    /// smallest-pid rule was a guess dressed as a heuristic, and an older bystander that happened to
    /// match won it.
    fn matching_pid(&self, pattern: &str) -> PidLookup {
        let table = crate::supervise::process_table();
        let pids = crate::supervise::select_matches(&table, pattern, std::process::id());
        match pids.len() {
            0 => PidLookup::NotRunning,
            1 => PidLookup::Found(pids[0]),
            _ => {
                let ppid_of: BTreeMap<u32, u32> = table.iter().map(|e| (e.pid, e.ppid)).collect();
                match sole_tree_root(&pids, &ppid_of) {
                    Some(root) => PidLookup::Found(root),
                    None => PidLookup::Ambiguous(pids),
                }
            }
        }
    }
}

/// The root of the gateway's process tree, from the one identity the manifest declares.
pub fn root_pid(runtime: &Runtime) -> Measurement<u32> {
    root_pid_from(&RealPids, runtime)
}

/// The testable core.
pub fn root_pid_from<S: PidSource>(source: &S, runtime: &Runtime) -> Measurement<u32> {
    let found = match runtime {
        // The RUN-SCOPED name, so a concurrent run's container of the same gateway is never the one
        // measured here.
        Runtime::Docker { .. } => match source.docker_lookup(&runtime.identity()) {
            Ok(Some(pid)) => PidLookup::Found(pid),
            Ok(None) => PidLookup::NotRunning,
            // OUR LOOKUP FAILED, which is not the gateway being down: said as a harness error so a
            // reader does not go looking for a crashed container.
            Err(why) => {
                return Measurement::absent_because(
                    Absent::HarnessError,
                    format!(
                        "the container runtime could not be asked for {:?}'s pid: {why}",
                        runtime.identity()
                    ),
                )
            }
        },
        Runtime::Native { proc_match } => source.matching_pid(proc_match),
    };
    match found {
        PidLookup::Found(pid) => Measurement::Measured(pid),
        // SEVERAL UNRELATED PROCESSES ANSWER TO THIS IDENTITY. Not "the gateway is missing" and not
        // a number: naming the ambiguity is the only honest reading, and the pids are listed so the
        // operator can see what else on the box answers to the manifest's `proc_match`.
        PidLookup::Ambiguous(pids) => Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "{} unrelated processes match the declared process-match {:?} ({pids:?}) and none is the parent of the others; refusing to guess which tree is the gateway's",
                pids.len(),
                runtime.identity()
            ),
        ),
        // NotMeasured, not Untestable: the rig worked, we asked, and the process was not there. The
        // detail names the identity so a reader can tell "the gateway died" from "we looked for the
        // wrong thing", which are the two causes and they have different fixes.
        PidLookup::NotRunning => Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "no running process found for the declared {} identity {:?}",
                if runtime.is_docker() {
                    "container"
                } else {
                    "process-match"
                },
                runtime.identity()
            ),
        ),
    }
}

#[cfg(test)]
#[path = "tests/rss_pid_tests.rs"]
mod pid_tests;

#[cfg(test)]
#[path = "tests/rss_tests.rs"]
mod tests;
