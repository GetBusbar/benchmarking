#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for lib/promote_guard.py — the collector guard that decides whether an INCOMING
# result may overwrite the EXISTING committed one. A neutral board must never let a boot/build failure
# from OUR environment clobber previously-good served data (audit R4-M2), and must never republish a
# regression as an honest failure. Focus: MEDIUM-R2-3 (matrix has no top-level last_http_status, so the
# guard's "000" connection-level branch is dead for the sole producer — the all-dead per-cell path must
# cover it) plus the pre-existing anchored-marker + status-000 + honest-not-served behaviours.
#
# Run: python3 lib/promote_guard_test.py
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(HERE, "promote_guard.py")

_fail = 0


def _w(doc):
    f = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    json.dump(doc, f)
    f.close()
    return f.name


def guard(suite, existing, incoming):
    """Return the guard's exit code. 0 = PROMOTE incoming, 1 = KEEP existing."""
    ex = _w(existing) if existing is not None else os.path.join(tempfile.gettempdir(), "no-such-file.json")
    inc = _w(incoming) if incoming is not None else os.path.join(tempfile.gettempdir(), "no-such-file.json")
    return subprocess.run([sys.executable, GUARD, suite, ex, inc]).returncode


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got exit={got}, want {want}")
        _fail = 1


SERVED_MATRIX = {
    "served": True,
    "upstreams": {"openai": {"served": True, "cells": {"openai": {"served": True, "status": "200"}}}},
}


# ── MEDIUM-R2-3: matrix all-dead (every cell 000, none served) is a boot/transport failure ───────────
all_dead = {
    "served": False,
    "serve_error": "HTTP 000 on POST /v1/chat/completions",  # NO anchored "failed to boot after" marker
    "upstreams": {
        "openai": {"served": False, "cells": {"openai": {"served": False, "status": "000"}}},
        "anthropic": {"served": False, "cells": {"anthropic": {"served": False, "status": "000"}}},
    },
}
check("all-dead matrix (every cell 000, no marker) must NOT overwrite a served result", guard("matrix", SERVED_MATRIX, all_dead), 1)

# A legitimately-partial matrix: one cell answered with a REAL HTTP status (a booted gateway that refused
# a dialect) — that is an honest not-served, NOT an environment boot failure, so it must PROMOTE.
partial = {
    "served": False,
    "serve_error": "some cells refused",
    "upstreams": {
        "openai": {"served": False, "cells": {"openai": {"served": False, "status": "000"}}},
        "anthropic": {"served": False, "cells": {"anthropic": {"served": False, "status": "501"}}},
    },
}
check("partial matrix with a real HTTP status (booted, honest not-served) must PROMOTE", guard("matrix", SERVED_MATRIX, partial), 0)

# A matrix with one served cell is not all-dead → promote (a fresh partial success).
one_served = {
    "served": True,
    "upstreams": {
        "openai": {"served": True, "cells": {"openai": {"served": True, "status": "200"}}},
        "anthropic": {"served": False, "cells": {"anthropic": {"served": False, "status": "000"}}},
    },
}
check("matrix with a served cell is not all-dead → PROMOTE", guard("matrix", SERVED_MATRIX, one_served), 0)

# No per-cell status anywhere (nothing to judge connection-level): be conservative, do NOT invent a boot
# failure — promote the honest not-served (a real result absent statuses still ages honestly).
no_status = {"served": False, "serve_error": "opaque", "upstreams": {}}
check("matrix with no per-cell status is NOT treated as all-dead → PROMOTE", guard("matrix", SERVED_MATRIX, no_status), 0)


# ── pre-existing behaviour still holds ───────────────────────────────────────────────────────────────
# Anchored "failed to boot after" marker in serve_error → boot failure, keep prior.
marker = {"served": False, "serve_error": "failed to boot after 5 attempts: port 8080 not listening", "upstreams": {}}
check("anchored 'failed to boot after' marker still KEEPS a served result", guard("matrix", SERVED_MATRIX, marker), 1)

# A non-matrix suite with top-level last_http_status 000 → boot failure (branch unchanged).
served_perf = {"served": True, "rps_max_proxy": 40000}
perf_000 = {"served": False, "last_http_status": "000", "serve_error": "connection refused"}
check("non-matrix suite status 000 still KEEPS a served result", guard("perf", served_perf, perf_000), 1)

# A booted gateway that honestly refused the probe (real HTTP status, no marker) → PROMOTE.
perf_refused = {"served": False, "last_http_status": "403", "serve_error": "auth required"}
check("non-matrix honest not-served (real status, no marker) PROMOTES", guard("perf", served_perf, perf_refused), 0)

# The guard never REFUSES when the existing file was not itself a served result (nothing good to protect).
not_served_existing = {"served": False}
check("all-dead does not block when existing was ALSO not served → PROMOTE", guard("matrix", not_served_existing, all_dead), 0)


if _fail == 0:
    print("all promote-guard tests passed")
    sys.exit(0)
print("PROMOTE-GUARD TESTS FAILED")
sys.exit(1)
