#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Append-only benchmark history: one JSONL line per (gateway, suite) per run.
#
# Reads every results/<suite>/<gateway>.json and appends a compact record to
# results/history/<gateway>.jsonl keyed by (suite, measured_at). Append-only by contract:
# an existing (suite, measured_at) pair for that gateway is skipped, never rewritten, so
# re-running after a partial field run only adds the new rows. History lives in git, so the
# file's own commit log is a second, tamper-evident record.
import json, os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "results")
HIST = os.path.join(RES, "history")
SUITES = ["perf", "memory", "stream", "xlate", "governed", "matrix"]
KEEP = {
    "perf": ["build", "rps_sustained_20ms", "rps_max_proxy", "added_latency_p50_us",
             "added_latency_p99_us", "server_timing_dur_p50_us", "server_timing_dur_p99_us"],
    "memory": ["build", "idle_rss_mib", "peak_rss_mib", "peak_rss_hwm_mib", "post_load_rss_mib"],
    "stream": ["build", "stream_served", "stream_added_ttft_p99_us", "stream_added_gap_p99_us",
               "stream_sustained_streams", "stream_sustained_fps"],
    "xlate": ["build", "xlate_served", "xlate_added_latency_p99_us", "xlate_rps_sustained_20ms"],
    "governed": ["build", "governed_served", "governed_rps_sustained_20ms",
                 "plain_rps_sustained_20ms", "governed_vs_plain_sustained_pct"],
    "matrix": ["build"],
}

def main():
    os.makedirs(HIST, exist_ok=True)
    added = 0
    for suite in SUITES:
        d = os.path.join(RES, suite)
        if not os.path.isdir(d):
            continue
        for fn in sorted(os.listdir(d)):
            if not fn.endswith(".json"):
                continue
            gw = fn[:-5]
            try:
                data = json.load(open(os.path.join(d, fn)))
            except Exception:
                continue
            measured = data.get("measured_at")
            if not measured:
                continue
            rec = {"suite": suite, "measured_at": measured,
                   "arch": data.get("arch"), "hardware": data.get("hardware")}
            for k in KEEP[suite]:
                if k in data:
                    rec[k] = data[k]
            if suite == "matrix" and isinstance(data.get("cells"), dict):
                rec["cells"] = {k: v.get("served") for k, v in data["cells"].items()}
            hist_path = os.path.join(HIST, gw + ".jsonl")
            seen = set()
            if os.path.exists(hist_path):
                for line in open(hist_path):
                    try:
                        j = json.loads(line)
                        seen.add((j.get("suite"), j.get("measured_at")))
                    except Exception:
                        pass
            if (suite, measured) in seen:
                continue
            with open(hist_path, "a") as f:
                f.write(json.dumps(rec, separators=(",", ":")) + "\n")
            added += 1
    print(f"history: appended {added} record(s)")

if __name__ == "__main__":
    main()
