# AI gateway benchmarks

A fair, reproducible benchmark for self-hostable AI gateways — **LiteLLM (Rust & Python), Bifrost,
Portkey, Kong, Helicone, busbar, and whatever else you drop in.** Same box, same mock, same load,
same cpu pin, for every gateway. One command runs it; the charts regenerate from raw results; every
source ref is pinned in the open and the built commit is stamped into the output.

The chart colors the winner **by measurement, not by name** — whichever gateway measures best on a
metric is green, full stop. No cherry-picked idle snapshots, no "believe us," no numbers you can't
regenerate. If a gateway can't serve the endpoint, the result says `served: false` instead of quietly
dropping it. Add your gateway (or fix how we run yours) with a one-file [manifest](gateways/README.md).

## Results

**Ran on:** AWS `m7g.xlarge`-class Graviton3 (ARM64), Ubuntu 24.04 — the same 4 vCPU / $0.04-per-vCPU
machine class the gateways-under-test benchmark themselves on. The exact instance type and vCPU count
are recorded in every `results/*.json` and printed at the top of each report page.

Full, auto-generated result pages (regenerated from the raw JSON on every run — no hand-typed numbers):

- **[Top 5 gateways →](results/reports/top5/)** — the field leaders by throughput ceiling.
- **[All gateways →](results/reports/all/)** — the complete field, including any that couldn't serve the endpoint (marked, not hidden).

Each page shows added latency (µs), RPS ceiling, idle/peak memory, whether the gateway served, and the
exact build/commit measured, plus the charts below.

> Numbers land here as runs complete. Re-run `run-all.sh` and these pages + charts update in place.

## Prerequisites

**To run locally on your own box:**
- **Rust** (`cargo`) — builds the mock (`mock/`, a hyper server that answers all six wire protocols and sustains 100s of k RPS, so it's never the bottleneck).
- **Go** — builds the load generator (`loadgen/`).
- **Docker** — for the container-based gateways (Bifrost, Kong, Helicone, …).
- **Python 3 + matplotlib** — draws the charts (`pip install matplotlib`). Optional; JSON results are written either way.
- A gateway binary/image for whatever you're testing (e.g. `BUSBAR_BIN=/path/to/busbar`). Competitor gateways build/pull themselves on first run.

**To run the one-click cloud version** (`run-on-ec2.sh`) the *only* extra dependency is **AWS CLI v2**, configured (`aws configure` — creds + a default region). The script launches a fresh Graviton box, installs everything on it, runs the full suite, pulls the results back, and **terminates the box** — nothing to set up, nothing to clean up.

## Run it — one command, every metric

```sh
BUSBAR_BIN=/path/to/busbar bench/run-all.sh                 # all gateways, all metrics
BUSBAR_BIN=/path/to/busbar bench/run-all.sh busbar litellm-rust   # a subset
```

One run measures **latency, throughput, and memory** for every gateway on the same box, then
regenerates the charts. On a fresh cloud box (builds every gateway, pulls results back, terminates
the box — nothing to set up):

```sh
BUSBAR_REPO=/path/to/busbarAI bench/run-on-ec2.sh          # one-click, Graviton
```

Out comes `results/perf/<gateway>.json`, `results/memory/<gateway>.json`, and the chart PNGs
(`results/added_latency.png`, `results/rps_ceiling.png`, `results/memory_rss.png`).

## What it measures

**`perf/`** — what the system can *do* (the metrics that matter most):

- **added latency (µs)** — p99 the gateway adds over the upstream at concurrency 1
  (gateway p99 − direct-to-mock p99). Microseconds, because at this scale ms hides the story.
- **RPS ceiling** — highest sustained requests/sec with p99 under 1 s and **zero errors** —
  "how much can it carry before it falls over."

**`memory/`** — resident memory across a request's life (matters most at GB scale):

- **idle RSS** — right after the gateway first answers `200`, before any load.
- **peak RSS** — highest RSS under sustained large-payload load.
- **post-load RSS** — 15 s after load stops: does it release, or stay pinned? A gateway that pools
  memory and never returns it looks fine on a boot-time `docker stats` and then eats your node.

## Add a gateway

Drop a directory under [`gateways/`](gateways/) with a `gateway.sh` manifest — four variables, four
functions. The runners are gateway-agnostic; there is nothing else to edit. See
[`gateways/README.md`](gateways/README.md).

## Honesty notes (the receipts)

- **Source refs are config, not defaults buried in a script.** Everything is pinned in
  [`gateways/versions.env`](gateways/versions.env) and overridable; the *actual* version/commit
  built is written into each result's `build` field. "You used an old branch" is answerable by
  pointing at the file and the recorded commit.
- **Each gateway is launched the only way it actually serves the endpoint.** For example,
  LiteLLM-Rust's `/v1/messages` route only serves the `azure_ai` provider *and* only serves at all
  under its `python-config` reader (the lean env config returns `400`) — verified against its own
  source. We launch it that way and record what it costs, rather than quoting an idle number from a
  config that doesn't serve. The reasoning is in
  [`gateways/litellm-rust/gateway.sh`](gateways/litellm-rust/gateway.sh).
- **The mock is deterministic and dumb** — it answers any path with a fixed small body (OpenAI shape,
  or Anthropic shape for `/messages`), so the number is the *gateway's* cost, not the upstream's.
- **The chart colors by measurement, not by name.** Green goes to whichever gateway measured lowest.
  If busbar loses a metric, busbar isn't green on it.

## Why this exists

Gateway vendors publish memory and latency numbers that don't survive a re-run — measured on
undisclosed hardware, from configs that don't serve the endpoint, with the winner hardcoded. This
repo is the opposite: click, run, get the answer, check our work. That's the whole point.
