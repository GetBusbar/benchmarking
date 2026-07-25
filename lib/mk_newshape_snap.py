#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Test fixture writer for lib/box_qualify_test.sh: a snapshot in the shape matrix/run.sh ACTUALLY
# writes today - a per-cell upstreams grid and NO top-level `memory` key. It lives in its own file
# rather than in a heredoc so the fixture shape is reviewable next to the producer it mirrors.
#
#   mk_newshape_snap.py <out.json> <measured_at> <arch> <floor_us> <verdict> <rps> <conc> <ing>eg> [mock_bound_rps]
#
# When mock_bound_rps is given, a SECOND cell carrying that HIGHER value is written with
# rps_max_proxy_mock_bound=true: the peak-replay reference must reject it, because a mock-bound
# sweep measures the rig's ceiling rather than the gateway's.
import json
import sys

path, at, arch, floor, verdict, rps, conc, cell = sys.argv[1:9]
mock_bound = sys.argv[9] if len(sys.argv) > 9 else ""

ingress, egress = cell.split(">")
upstreams = {
    egress: {"cells": {ingress: {"served": True, "perf": {
        "rps_max_proxy": float(rps),
        "rps_max_proxy_concurrency": int(conc),
        "rps_max_proxy_mock_bound": False,
    }}}},
}
if mock_bound:
    upstreams.setdefault("anthropic", {"cells": {}})
    upstreams["anthropic"]["cells"]["openai"] = {"served": True, "perf": {
        "rps_max_proxy": float(mock_bound),
        "rps_max_proxy_concurrency": 999,
        "rps_max_proxy_mock_bound": True,
    }}

json.dump({
    "gateway": "gw",
    "measured_at": at,
    "arch": arch,
    "rig": {"box_qualify": {"verdict": verdict,
                            "stage1": {"floor_p99_us_median": float(floor)}}},
    "matrix": {"upstreams": upstreams},
}, open(path, "w"))
