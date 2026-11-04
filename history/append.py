#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Append-only benchmark history: one JSONL line per (gateway, suite) per run.
#
# Reads every results/<suite>/<gateway>.json and appends a compact record to
# results/history/<gateway>.jsonl keyed by (suite, measured_at). Append-only by contract:
# an existing (suite, measured_at) pair for that gateway is skipped, never rewritten, so
# re-running after a partial field run only adds the new rows. History lives in git, so the
# file's own commit log is a second, tamper-evident record.
#
#   append.py                    every result on disk (manual backfill, e.g. after `harvest`)
#   append.py <snapshot.json>... ONLY these snapshot files - what run-on-ec2.sh passes, so a run's
#                                history holds exactly the snapshots its clean boxes pulled, never a
#                                harvested, held-back or set-aside file that happened to be on disk.
import json, os, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "results")
HIST = os.path.join(RES, "history")
SUITES = ["perf", "memory", "stream", "streamcpu", "xlate", "governed", "matrix"]
KEEP = {
    "perf": ["build", "rps_sustained_20ms", "rps_max_proxy", "added_latency_p50_us",
             "added_latency_p99_us", "server_timing_dur_p50_us", "server_timing_dur_p99_us"],
    "memory": ["build", "idle_rss_mib", "peak_rss_mib", "peak_rss_hwm_mib", "post_load_rss_mib"],
    "stream": ["build", "stream_served", "stream_added_ttft_p99_us", "stream_added_gap_p99_us",
               "stream_sustained_streams", "stream_sustained_fps"],
    "streamcpu": ["build", "stream_served", "streamcpu_frames_per_sec", "streamcpu_fps_per_core",
                  "streamcpu_direct_ceiling_fps", "streamcpu_mock_bound", "streamcpu_valid"],
    "xlate": ["build", "xlate_served", "xlate_added_latency_p99_us", "xlate_rps_sustained_20ms"],
    "governed": ["build", "governed_served", "governed_rps_sustained_20ms",
                 "plain_rps_sustained_20ms", "governed_vs_plain_sustained_pct"],
    # MATRIX IS NOW THE SOLE PRODUCER. After the perf/memory/stream/streamcpu/xlate/governed suites were
    # retired, this list was left at ["build"] alone — so the append-only history recorded NO
    # MEASUREMENT AT ALL for any run, and the regression-vs-history check had nothing to compare
    # against (audit P1-11). Restore the matrix-era metrics: the memory window's RSS lanes (top-level
    # "memory" object) plus, per served DIAGONAL cell, its perf + streaming numbers — flattened by
    # _matrix_extra below. Every value is copied VERBATIM, including a null: history must record
    # "not measured" as not-measured, never as 0.
    "matrix": ["build", "matrix_version", "served", "egress_configured", "sweep_ttft_ms",
               "p99_ceiling_ms", "sweep_rung_selection"],
}

# The matrix result's measurements live in nested objects, not at the top level, so KEEP alone cannot
# reach them. These are the fields worth a history row — the same ones the board ranks on.
MEM_KEEP = ["served", "load_cell", "idle_rss_mib", "peak_rss_mib", "peak_rss_hwm_mib",
            "recovered_rss_mib", "post_load_rss_mib", "idle_window_s", "recovery_window_s"]
# PER-CELL memory: the shape the producer writes now. The lanes the plateau design added are the whole
# point of keeping history for this metric - a leak rate and a time-to-plateau are only interpretable
# against the same gateway's earlier runs, which is exactly what history is for.
CELL_MEM_KEEP = ["served", "idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
                 "peak_rss_hwm_mib", "plateaued", "time_to_plateau_s", "growth_rate_mib_per_min",
                 "load_s", "idle_window_s", "recovery_window_s"]
# THE FIELDS THE ENGINE WRITES NOW (engine/src/record.rs CellPerf / CellStream). These lists used to
# name rps_sustained_20ms / rps_max_proxy / cpu_fps / *_mock_bound - all deleted from the engine - and
# omitted `frontier`, the board's throughput metric, so even a correctly-located cell yielded no
# throughput row at all.
CELL_PERF_KEEP = ["frontier", "added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us",
                  "direct_c1_p99_us", "cpu_us_per_request", "rps_per_cpu_second", "egress_reverified"]
CELL_STREAM_KEEP = ["stream_served", "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us",
                    "added_gap_p99_us", "streams_sustained", "streams_sustained_fps",
                    "streams_sustained_mock_ceiling", "streams_sustained_headroom"]


def _matrix_extra(data):
    """The matrix run's actual MEASUREMENTS, flattened into a compact history record.

    memory  -> the RSS window verbatim (nulls preserved: an unmeasured lane must stay unmeasured).
    perf    -> per DIAGONAL cell (<dialect> ingress on the <dialect> egress: the like-for-like cell the
               board headlines), that cell's perf block.
    stream  -> the same diagonal cells' streaming block, when the run measured one.
    """
    out = {}
    mem = data.get("memory")
    if isinstance(mem, dict):
        out["memory"] = {k: mem[k] for k in MEM_KEEP if k in mem}
    perf, stream = {}, {}
    ups = data.get("upstreams")
    if isinstance(ups, dict):
        for eg, up in ups.items():
            cell = ((up or {}).get("cells") or {}).get(eg)
            if not isinstance(cell, dict):
                continue
            p = cell.get("perf")
            if isinstance(p, dict):
                keep = {k: p[k] for k in CELL_PERF_KEEP if k in p}
                if keep:
                    perf[eg] = keep
            st = cell.get("stream")
            if isinstance(st, dict):
                keep = {k: st[k] for k in CELL_STREAM_KEEP if k in st}
                if keep:
                    stream[eg] = keep
    if perf:
        out["diagonal_perf"] = perf
    if stream:
        out["diagonal_stream"] = stream
    # PER-CELL MEMORY, harvested from the same grid. The top-level "memory" branch above reads a key the
    # per-cell redesign deleted, so every history row written since then has silently carried NO memory
    # data at all - and history is append-only, so each run that goes by is a row that can never be
    # backfilled from anything but the snapshots. Same diagonal rule as perf and streaming: the
    # like-for-like cell the board headlines.
    mem_cells = {}
    if isinstance(ups, dict):
        for eg, up in ups.items():
            cell = ((up or {}).get("cells") or {}).get(eg)
            if not isinstance(cell, dict):
                continue
            cm = cell.get("memory")
            if isinstance(cm, dict):
                keep = {k: cm[k] for k in CELL_MEM_KEEP if k in cm}
                if keep:
                    mem_cells[eg] = keep
    if mem_cells:
        out["diagonal_memory"] = mem_cells
    return out

def matrix_of(snapshot):
    """The matrix-shaped result inside an engine snapshot.

    THE ENGINE NESTS IT. A snapshot (`record::ResultSnapshot`) carries `upstreams`, `cells`, `served`,
    `matrix_version` and the sweep settings under `snapshot["matrix"]`, beside top-level metadata.
    Reading those keys off the TOP level - what this collector did since the per-suite directories were
    retired - found none of them, so every history row written from results/snapshots carried build,
    arch and hardware and NOT ONE MEASUREMENT. The top-level `measured_at`/`build`/`arch`/`hardware`
    win where both exist, so the (suite, measured_at) key matches the rows already recorded.
    """
    m = snapshot.get("matrix")
    if not isinstance(m, dict):
        return snapshot  # an un-nested (legacy results/matrix/<gw>.json) result is already the matrix
    out = dict(m)
    for k in ("measured_at", "build", "arch", "hardware"):
        if snapshot.get(k) is not None:
            out[k] = snapshot[k]
    return out


# What _write_record reports, so main() can tell a row it could not record from one already present.
WRITTEN, ALREADY_RECORDED, NO_TIMESTAMP, HISTORY_UNREADABLE = "written", "already", "no_ts", "bad_hist"


def _write_record(suite, gw, data):
    """Build one history row from a result JSON and append it if new. Returns one of the outcomes
    above: only WRITTEN appended anything; NO_TIMESTAMP and HISTORY_UNREADABLE are LOST rows."""
    measured = data.get("measured_at")
    if not measured:
        return NO_TIMESTAMP
    rec = {"suite": suite, "measured_at": measured,
           "arch": data.get("arch"), "hardware": data.get("hardware")}
    for k in KEEP[suite]:
        if k in data:
            rec[k] = data[k]
    if suite == "matrix":
        if isinstance(data.get("cells"), dict):
            rec["cells"] = {k: (v.get("served") if isinstance(v, dict) else None)
                            for k, v in data["cells"].items()}
        rec.update(_matrix_extra(data))
    hist_path = os.path.join(HIST, gw + ".jsonl")
    seen = set()
    if os.path.exists(hist_path):
        with open(hist_path) as fh:
            for n, line in enumerate(fh, 1):
                if not line.strip():
                    continue
                try:
                    j = json.loads(line)
                    seen.add((j.get("suite"), j.get("measured_at")))
                except Exception as e:
                    # A damaged line hides which (suite, measured_at) it held, so appending now could
                    # duplicate it in an append-only record. Refuse, loudly, and let main() fail.
                    print(f"history: {hist_path} line {n} is unreadable ({e}) - refusing to append "
                          f"to it", file=sys.stderr)
                    return HISTORY_UNREADABLE
    if (suite, measured) in seen:
        return ALREADY_RECORDED
    with open(hist_path, "a") as f:
        f.write(json.dumps(rec, separators=(",", ":")) + "\n")
    return WRITTEN


def _ingest_snapshot(src, label, skipped):
    """Append the history row for one engine snapshot file. Returns 1 if a row was written; records a
    LOST row in `skipped` when the file cannot be read, names no gateway, or cannot be recorded."""
    try:
        with open(src) as f:
            data = json.load(f)
    except Exception as e:
        print(f"history: SKIPPED corrupt {label}: {e}", file=sys.stderr)
        skipped.append(label)
        return 0
    gw = data.get("gateway") if isinstance(data, dict) else None
    if not gw or not isinstance(gw, str) or "/" in gw or gw.startswith("."):
        print(f"history: SKIPPED {label}: no `gateway` field to file it under (or not a plain name)",
              file=sys.stderr)
        skipped.append(label)
        return 0
    got = _write_record("matrix", gw, matrix_of(data))
    if got == WRITTEN:
        return 1
    if got != ALREADY_RECORDED:
        print(f"history: NOT RECORDED {label}: {got}", file=sys.stderr)
        skipped.append(label)
    return 0


def _finish(added, skipped):
    print(f"history: appended {added} record(s)")
    if skipped:
        print(f"history: WARNING {len(skipped)} result file(s) could not be recorded - "
              f"their history row(s) were LOST: {', '.join(skipped)}", file=sys.stderr)
        return 1
    return 0


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    os.makedirs(HIST, exist_ok=True)
    added = 0
    skipped = []   # result files that failed to parse (corrupt/truncated) - each is a LOST history row
    if argv:
        # NAMED SNAPSHOTS ONLY. A named file that is missing is a lost row, not an empty run.
        for src in argv:
            added += _ingest_snapshot(src, src, skipped)
        return _finish(added, skipped)
    for suite in SUITES:
        d = os.path.join(RES, suite)
        if not os.path.isdir(d):
            continue
        for fn in sorted(os.listdir(d)):
            if not fn.endswith(".json"):
                continue
            gw = fn[:-5]
            src = os.path.join(d, fn)
            try:
                with open(src) as f:
                    data = json.load(f)
            except Exception as e:
                # A corrupt/truncated result JSON (e.g. a here-doc that failed mid-write when the box
                # hit its shutdown timer, or a partially-pulled file) must NOT be silently skipped: this
                # is the append-only, git-tracked, tamper-evident history and skipping loses that
                # (gateway, suite) row forever. Record it and exit non-zero so the caller's
                # `if ! append.py` guard fires (audit R4-M3).
                print(f"history: SKIPPED corrupt {suite}/{fn}: {e}", file=sys.stderr)
                skipped.append(f"{suite}/{fn}")
                continue
            got = _write_record(suite, gw, data)
            if got == WRITTEN:
                added += 1
            elif got != ALREADY_RECORDED:
                print(f"history: NOT RECORDED {suite}/{fn}: {got}", file=sys.stderr)
                skipped.append(f"{suite}/{fn}")
    # THE ENGINE NO LONGER WRITES THE PER-SUITE DIRECTORIES ABOVE: it writes only
    # results/snapshots/<gw>.json (current) and results/snapshots/result_<gw>_<measured_at>.json
    # (timestamped). Both hold the matrix-shaped result, so ingest them the same way, keyed by the
    # JSON's own "gateway" field rather than the filename (the timestamped copies don't end in <gw>.json).
    snap_d = os.path.join(RES, "snapshots")
    if os.path.isdir(snap_d):
        for fn in sorted(os.listdir(snap_d)):
            if not fn.endswith(".json"):
                continue
            added += _ingest_snapshot(os.path.join(snap_d, fn), f"snapshots/{fn}", skipped)
    return _finish(added, skipped)

if __name__ == "__main__":
    sys.exit(main())
