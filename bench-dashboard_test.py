#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
#
# bench-dashboard.py never fails a build, so a wrong figure only shows up as an operator killing a
# box that was nearly done, or babysitting one that's wedged. This file pins the three defect
# classes that matter: a FABRICATED progress number (phase table entry for a metric the engine
# doesn't have), a label that MISDESCRIBES state (falsy-zero ETA/served rendering as "-"), and a
# gate that CANNOT FAIL (the phase table checked only against itself, not against
# `engine/src/metric.rs`).
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
# `PHASE_ORDER`/`PHASE_COST` are keyed by the group names the engine prints as `[phase] <cell> <group>`
# (from `metric::METRICS` and each metric's `name()`). Nothing at runtime connects the two, so a
# renamed/added/retired group leaves the table silently describing a run shape that no longer
# happens. The weights themselves can't be derived from source (they're measured wall-clock
# shares), so the table stays hand-written; only the key match is automated here.

_METRICS_RE = re.compile(r"pub const METRICS: &\[&dyn Metric\] = &\[(.*?)\];", re.S)
_STRUCT_RE = re.compile(r"&(\w+)")
# Only column-0 impls: metric.rs's test module declares its own `Metric` impls, indented, and those
# are not part of the shipped run.
_NAME_RE = re.compile(r"^impl Metric for (\w+) \{\s*\n\s*fn name\(&self\)[^\n]*\n\s*\"(\w+)\"", re.M)


def engine_phase_order(src):
    """The metric-group names the engine runs, in order, parsed from metric.rs's own source.

    Two steps since the facts live apart: METRICS gives the order as a list of types, `name()` gives
    the string each carries. Returns None (a failure, not []) when either shape can't be found, so a
    parse failure can't masquerade as an empty, vacuously-passing table."""
    m = _METRICS_RE.search(src)
    if not m:
        return None
    names = dict(_NAME_RE.findall(src))
    order = [names.get(s) for s in _STRUCT_RE.findall(m.group(1))]
    return None if any(o is None for o in order) else order


# Fixture with a different metric list, to prove the parser reads the source rather than echoing
# PHASE_ORDER (a parser that did that, or returned [] on anything unfamiliar, would never disagree).
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

# The real engine source and the real table must agree.
_METRIC_RS = os.path.join(HERE, "engine", "src", "metric.rs")
if not os.path.exists(_METRIC_RS):
    # Not a skip: a check that can't run has not passed.
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

# The fabricated-progress regression, tested as a property rather than a hardcoded number: a
# phantom phase-table entry (has happened twice — a stale `sustained_throughput`, then a retired
# `cpu_fps`) inflates `cell_fraction_done`'s accumulated weight for every later phase. Hardcoded
# expected sums caught the phantom but also broke on legitimate reweights, so instead assert the
# cumulative fraction equals the sum of exactly the phases before it in PHASE_ORDER, computed from
# the table itself — phantom-proof (caught by the keys check above) and reweight-proof.
for _i, _phase in enumerate(bd.PHASE_ORDER):
    _before = sum(bd.PHASE_COST[p] for p in bd.PHASE_ORDER[:_i])
    check(f"progress into {_phase} is exactly the phases before it, no invented work",
          round(bd.cell_fraction_done(_phase), 6), round(_before, 6))

# A retired phase name must claim nothing rather than silently matching another phase's weight.
check("the retired cpu_fps group contributes nothing", bd.cell_fraction_done("cpu_fps"), 0.0)
check("the retired sustained_throughput group contributes nothing",
      bd.cell_fraction_done("sustained_throughput"), 0.0)
check("an unrecognised phase claims NO progress (pessimistic, never confidently wrong)",
      bd.cell_fraction_done("some_new_group_2027"), 0.0)
check("no phase at all claims no progress", bd.cell_fraction_done(None), 0.0)


# ── parse_progress: a served count is a count only if we saw every cell ────────────────────────────
#
# The count comes from `[cell N/M]` lines in the box's log; a truncated log (e.g. the old 20 KB tail
# cap scrolling early cells out) silently becomes a floor. The engine numbers cells contiguously
# from 1, so completeness of the history is decidable.
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

# The same run with its first two cells scrolled off: positions {3,4} != {1,2,3,4}, so the count
# is known to be short.
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

# All declared cells (2) are already served -> left == 0 -> eta == 0.0, falsy in Python; must not
# render as "-" (no estimate).
row = _drive((2, 2, 2, "done", True))
check("eta of exactly 0.0 (all cells served) renders as an estimate, not '-'", row["eta"], "0m00s")
check("a complete history shows the served count bare", row["served"], "2/2")

# The same run read off a short log: every derived figure is a floor and says so.
row = _drive((2, 2, 1, "done", False))
check("an incomplete history marks the served count as a floor", row["served"], "≥1/2")
check("...and marks the ETA built on it as a floor too", row["eta"].startswith("≥"), True)

# A cell position of 0 must not print as "no cell information" (same falsy-zero class as the eta bug).
row = _drive((0, 36, 0, None, True))
check("a cell position of 0 prints the position, not '-'", row["cells"], "0/36")

bd.parse_progress = _orig_parse

# ── render() must survive a stdout codec that cannot encode its glyphs ──────────────────────────────
#
# `render()` unconditionally prints "·" (U+00B7); under LANG=C/PYTHONIOENCODING=ascii (cron, a
# minimal container, `> log.txt`) an unguarded ASCII stdout encoder would raise UnicodeEncodeError
# and kill the live monitor mid-run.
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
