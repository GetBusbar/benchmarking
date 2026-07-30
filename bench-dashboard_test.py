#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# THE STATUS BOARD IS ALSO A PUBLISHED NUMBER, and it is the one people act on fastest.
#
# Nothing here ends up on the website, which is exactly why it drifted: a wrong figure in
# `bench-dashboard.py` never fails a build, it just makes an operator kill a box that was nearly done
# or babysit one that has been wedged for an hour. Thirteen boxes billing by the hour is the whole
# reason the tool exists, so "the ETA was a bit off" is the failure mode it was written to prevent.
#
# The defect classes this file pins are the same three the rest of the repo polices:
#
#   a number that could be FABRICATED - the per-cell progress fraction was accumulated from a phase
#     table containing a metric group the engine does not have, so every cell in its last two phases
#     claimed 19.8 points of progress against work that does not exist;
#   a label that MISDESCRIBES a state - an eta of exactly 0.0 rendering as "no estimate", and a served
#     count derived from a truncated log presented as a count rather than as a floor;
#   a gate that CANNOT FAIL - a phase table asserted only against itself. The drift check below reads
#     `engine/src/metric.rs` and would have caught `sustained_throughput` the day it was retired.
#
# Run: python3 bench-dashboard_test.py
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_dashboard", os.path.join(HERE, "bench-dashboard.py"))
bd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bd)

_fail = 0


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got {got!r}, want {want!r}")
        _fail = 1


# ── the phase table must be the ENGINE'S phase list ───────────────────────────────────────────────
#
# `PHASE_ORDER`/`PHASE_COST` are keyed by the group names the engine prints as `[phase] <cell> <group>`,
# which come from `metric::METRICS` and the `name()` each metric declares. Nothing at runtime connects
# the two: the dashboard reads a log, not the engine's source, so a group that is renamed, added or
# retired leaves the table describing a run shape that no longer happens - and every consequence is a
# quietly wrong number rather than an error. `sustained_throughput` sat in this table after the engine
# folded it into `throughput`, and the only symptom was optimistic ETAs.
#
# Reading metric.rs from the test rather than from the tool is the deliberate line: the ETA weights
# cannot be derived from the source (they are measured wall-clock shares), so the table has to be
# hand-written, and the thing worth automating is the assertion that it still matches.

_METRICS_RE = re.compile(r"pub const METRICS: &\[&dyn Metric\] = &\[(.*?)\];", re.S)
_STRUCT_RE = re.compile(r"&(\w+)")
# Only column-0 impls: metric.rs's test module declares its own `Metric` impls, indented, and those
# are not part of the shipped run.
_NAME_RE = re.compile(r"^impl Metric for (\w+) \{\s*\n\s*fn name\(&self\)[^\n]*\n\s*\"(\w+)\"", re.M)


def engine_phase_order(src):
    """The metric-group names the engine runs, in order, parsed from metric.rs's own source.

    Two steps because the two facts live apart: METRICS gives the ORDER as a list of types, and each
    type's `name()` gives the STRING the log line carries. Returns None when either shape cannot be
    found, which the caller must treat as a failure - a parser that silently returns [] would turn
    this whole check into the vacuous pass it exists to prevent."""
    m = _METRICS_RE.search(src)
    if not m:
        return None
    names = dict(_NAME_RE.findall(src))
    order = [names.get(s) for s in _STRUCT_RE.findall(m.group(1))]
    return None if any(o is None for o in order) else order


# RED for the parser itself: point it at a source declaring a DIFFERENT metric list and it must report
# that list. A parser that echoed PHASE_ORDER, or that returned [] on anything unfamiliar, would agree
# with the dashboard forever.
_FIXTURE_RS = '''
pub const METRICS: &[&dyn Metric] = &[
    &Alpha,
    &Beta,
];

impl Metric for Alpha {
    fn name(&self) -> &'static str {
        "alpha"
    }
}
impl Metric for Beta {
    fn name(&self) -> &'static str {
        "beta_group"
    }
}
    impl Metric for TestOnly {
        fn name(&self) -> &'static str {
            "test_only"
        }
    }
'''
check("the metric.rs parser reads the ENGINE's list, not the dashboard's",
      engine_phase_order(_FIXTURE_RS), ["alpha", "beta_group"])
check("...and refuses to answer when METRICS is not there (never a silent empty list)",
      engine_phase_order("fn main() {}"), None)

# ACCEPT: the real engine source and the real table agree.
_METRIC_RS = os.path.join(HERE, "engine", "src", "metric.rs")
if not os.path.exists(_METRIC_RS):
    # Not a skip. A check that cannot run has not passed, and this one exists precisely because the
    # thing it compares against is easy to move.
    check("engine/src/metric.rs is present so the phase table can be checked against it", False, True)
else:
    with open(_METRIC_RS) as f:
        _engine_order = engine_phase_order(f.read())
    check("the engine's metric groups parse", _engine_order is not None, True)
    check("PHASE_ORDER is exactly the engine's metric groups, in the engine's order",
          bd.PHASE_ORDER, _engine_order)
    check("PHASE_COST covers exactly those groups and invents none",
          sorted(bd.PHASE_COST), sorted(_engine_order or []))

check("the phase weights still sum to one whole cell", round(sum(bd.PHASE_COST.values()), 6), 1.0)

# THE FABRICATED-PROGRESS REGRESSION, tested as the PROPERTY rather than as a number.
#
# It has now happened twice. First a phantom `sustained_throughput` sat in the table at 0.198 for a
# group metric.rs does not have, so a cell in `streams_sustained` counted that weight as already
# elapsed. Then `cpu_fps` was retired from the engine and its 0.102 became phantom in exactly the same
# way. Both times the real defect was the same: `cell_fraction_done` accumulates PHASE_ORDER weight
# until it reaches the phase it was given, so any entry for work that does not happen inflates every
# later phase's progress and shortens every ETA built on it.
#
# The first version of this test hardcoded the sums (0.845, and 1.0 - 0.102). That caught the phantom
# but ALSO broke on a legitimate reweight - which is what happened when the weights were re-measured
# after the memory phase became a fixed duration. A test that fires on a correct change is a test that
# gets edited until it stops firing. So: assert the cumulative fraction equals the sum of exactly the
# phases BEFORE it in PHASE_ORDER, computed from the table itself. That is phantom-weight-proof
# (a phantom is not in PHASE_ORDER, or is, and then the keys check above fails) and reweight-proof.
for _i, _phase in enumerate(bd.PHASE_ORDER):
    _before = sum(bd.PHASE_COST[p] for p in bd.PHASE_ORDER[:_i])
    check(f"progress into {_phase} is exactly the phases before it, no invented work",
          round(bd.cell_fraction_done(_phase), 6), round(_before, 6))

# And a RETIRED phase name claims nothing. `cpu_fps` ran in this harness for months; a log line naming
# it must now contribute 0.0 rather than silently matching some other phase's weight.
check("the retired cpu_fps group contributes nothing", bd.cell_fraction_done("cpu_fps"), 0.0)
check("the retired sustained_throughput group contributes nothing",
      bd.cell_fraction_done("sustained_throughput"), 0.0)
check("an unrecognised phase claims NO progress (pessimistic, never confidently wrong)",
      bd.cell_fraction_done("some_new_group_2027"), 0.0)
check("no phase at all claims no progress", bd.cell_fraction_done(None), 0.0)


# ── parse_progress: a served count is a count only if we saw every cell ────────────────────────────
#
# The count is derived by counting `[cell N/M]` lines out of the box's log. `remote_tail` used to cap
# that log at 20 KB, so on any real grid the earliest cells fell out of the window and the count
# silently became a floor - a gateway that had finished seven cells read "3/8", and the ETA divided by
# 3. The engine numbers cells contiguously from 1, so the completeness of the history is decidable.
_FULL_LOG = "\n".join([
    "[cell 1/4] openai>openai: served",
    "[phase] openai>openai throughput",
    "[phase] openai>openai memory",
    "[cell 2/4] openai>anthropic: not_configurable",
    "[cell 3/4] anthropic>openai: served",
    "[cell 4/4] anthropic>anthropic: no connection to the gateway - restarting the gateway",
    "[cell 4/4] anthropic>anthropic: after restart, it answers",
    "[cell 4/4] anthropic>anthropic: served",
    "[phase] anthropic>anthropic streams_sustained",
])
cells, total, served_done, phase, complete = bd.parse_progress(_FULL_LOG)
check("a complete log is reported complete", complete, True)
check("...with the engine's own absolute position", (cells, total), (4, 4))
check("...counting only terminal measured verdicts (a restart narrates under the same prefix)",
      served_done, 3)
check("...and the phase is the metric GROUP, which is what PHASE_COST is keyed by",
      phase.split()[0], "streams_sustained")
check("...so the in-flight fraction is a real one", bd.cell_fraction_done(phase.split()[0]) > 0, True)

# RED: the same run with its first two cells scrolled off. Position 3 is the earliest seen, so
# positions {3,4} != {1,2,3,4} and the count is known to be short.
_TRUNCATED = "\n".join(_FULL_LOG.split("\n")[4:])
_c, _t, _sd, _p, _complete = bd.parse_progress(_TRUNCATED)
check("a log that does not reach back to cell 1 is NOT reported complete", _complete, False)
check("...and its served count really is short (which is why it must not be shown as a count)",
      _sd < served_done, True)
check("a log with no cell lines at all is not called incomplete (there is nothing to be short of)",
      bd.parse_progress("[phase] x memory")[4], True)


# ── row_for: the two labels that used to misdescribe their state ──────────────────────────────────
def _drive(progress, declared=2, now=100):
    bd.local_state = lambda path: {"ip": "1.2.3.4", "start": 0, "terminal": None}
    bd.remote_tail = lambda ip: ""
    bd.declared_served = lambda gw: declared
    bd.parse_progress = lambda text: progress
    return bd.row_for("fakegw", "/tmp/fake", now)


_orig_parse = bd.parse_progress

# All declared cells (2) are already served -> left == 0 -> eta == 0.0, which is falsy in Python. The
# render expression used to be `hms(eta) if eta else (...)`, printing '-' ("no estimate") on a run
# that is finishing this second.
row = _drive((2, 2, 2, "done", True))
check("eta of exactly 0.0 (all cells served) renders as an estimate, not '-'", row["eta"], "0m00s")
check("a complete history shows the served count bare", row["served"], "2/2")

# The same run read off a short log: every derived figure is a floor and says so.
row = _drive((2, 2, 1, "done", False))
check("an incomplete history marks the served count as a floor", row["served"], "≥1/2")
check("...and marks the ETA built on it as a floor too", row["eta"].startswith("≥"), True)

# A cell position of 0 must not print as "no cell information". Same falsy-zero reading as the eta bug.
row = _drive((0, 36, 0, None, True))
check("a cell position of 0 prints the position, not '-'", row["cells"], "0/36")

bd.parse_progress = _orig_parse

# ── render() must survive a stdout codec that cannot encode its glyphs ──────────────────────────────
#
# `render()` prints "·" (U+00B7) unconditionally on its summary line, with no stdout.reconfigure()
# and no encoding safeguard anywhere in the file. That is fine on an interactive UTF-8 terminal, but a
# cron job, a minimal container, or a plain `> log.txt` redirection under LANG=C/LC_ALL=C/
# PYTHONIOENCODING=ascii gives Python an ASCII stdout encoder, and the very first refresh cycle raises
# UnicodeEncodeError and kills the live monitor mid-run.
import subprocess

_render_script = (
    "import importlib.util\n"
    f"spec = importlib.util.spec_from_file_location('bd', {os.path.join(HERE, 'bench-dashboard.py')!r})\n"
    "bd = importlib.util.module_from_spec(spec)\n"
    "spec.loader.exec_module(bd)\n"
    "bd.render([])\n"
)
_env = dict(os.environ)
_env["PYTHONIOENCODING"] = "ascii"
_env.pop("LANG", None)
_env.pop("LC_ALL", None)
_proc = subprocess.run([sys.executable, "-c", _render_script], env=_env, capture_output=True, text=True)
check("render() does not crash when stdout cannot encode non-ASCII glyphs (PYTHONIOENCODING=ascii)",
      _proc.returncode, 0)

if _fail == 0:
    print("all bench-dashboard tests passed")
    sys.exit(0)
print("BENCH-DASHBOARD TESTS FAILED")
sys.exit(1)
