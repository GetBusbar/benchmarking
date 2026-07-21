# SPDX-License-Identifier: Apache-2.0
# Capture a gateway's OWN self-reported Server-Timing `dur` at concurrency 1, in µs.
#
# This is neutral: we record whatever the gateway chooses to emit in `Server-Timing: <name>;dur=<ms>`.
# Only a gateway that self-reports produces samples (currently just busbar, via `busbar;dur`); everything
# else yields n=0 → null in the result. Fired on the SAME box, SAME c1 condition as the added-latency
# probe, so busbar's own compute and the end-to-end added latency come from ONE run and decompose cleanly
# (busbar;dur ⊂ end-to-end added latency = compute + the extra network hop).
#
#   python3 stdur.py <url> <n> <model> <auth> ["Header: Val" ...]
import sys, json, urllib.request

url, n, model, auth = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
extra = sys.argv[5:]  # "Key: Val" strings (e.g. a virtual-key header)

body = json.dumps({"model": model, "messages": [{"role": "user", "content": "ping"}],
                   "max_tokens": 16}).encode()
headers = {"content-type": "application/json"}
if auth:
    headers["authorization"] = f"Bearer {auth}"
for h in extra:
    if h and ":" in h:
        k, v = h.split(":", 1)
        headers[k.strip()] = v.strip()


def hit():
    req = urllib.request.Request(url, data=body, headers=headers)
    r = urllib.request.urlopen(req, timeout=5)
    r.read()
    return r.headers.get("Server-Timing", "")


for _ in range(min(300, n)):            # warm-up (discarded), same as every other measured path
    try:
        hit()
    except Exception:
        pass

durs = []
for _ in range(n):
    try:
        st = hit()
        for part in st.split(","):
            if "dur=" in part:
                try:
                    durs.append(float(part.split("dur=")[1].split(";")[0]))
                    break                # first dur metric = the gateway's own processing time
                except ValueError:
                    pass
    except Exception:
        pass

durs.sort()
if not durs:
    print(json.dumps({"n": 0}))
    sys.exit(0)
pc = lambda p: durs[min(len(durs) - 1, int(len(durs) * p))]
print(json.dumps({"n": len(durs),
                  "p50_us": round(pc(.50) * 1000, 1),
                  "p99_us": round(pc(.99) * 1000, 1)}))
