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

Clone, then run one script. Everything is at the repo root.

```sh
git clone https://github.com/GetBusbar/benchmarking && cd benchmarking

# Every gateway that builds/pulls itself (LiteLLM, Bifrost, Portkey, Kong, Helicone), all metrics:
./run-all.sh

# A subset:
./run-all.sh litellm-rust bifrost

# Include the busbar row — point BUSBAR_BIN at a busbar binary. Get one with either:
#   docker create --name b getbusbar/busbar:1.4.1 && docker cp b:/busbar ./busbar && docker rm b
#   (or download it from https://github.com/GetBusbar/busbar/releases)
BUSBAR_BIN=./busbar ./run-all.sh
```

One run measures **latency, throughput, and memory** for every gateway on the same box, then
regenerates the charts and the report pages. Out comes `results/perf/<gateway>.json`,
`results/memory/<gateway>.json`, `results/reports/{all,top5}/README.md`, and the chart PNGs.

### On a fresh cloud box (nothing to install)

`run-on-ec2.sh` launches a Graviton box, installs everything, runs the full suite, pulls results
back, and **terminates the box**. Only needs AWS CLI v2 configured.

```sh
./run-on-ec2.sh                                              # every self-building gateway
# also build + include busbar at a released tag (from a local busbar checkout):
BUSBAR_REF=v1.4.1 BUSBAR_REPO=/path/to/busbar-checkout ./run-on-ec2.sh
```

A gateway that can't be stood up (missing binary, unreachable, or needs infra a single container
can't provide) is recorded `served: false` and shown as such — never silently dropped.

### How long it takes

Plan for it — this is a build-and-measure benchmark, not a quick script:

- **Full field, all metrics** (`run-on-ec2.sh` with the default gateways): **~60–75 min** on an
  `m7g.4xlarge`. Most of that is *building* — LiteLLM-Rust from source is the long pole (~15–20 min),
  plus the LiteLLM/Kong/Helicone images and busbar. The measurement itself is only ~5–6 min per
  gateway (latency + throughput sweep + a memory soak).
- **A single gateway** (e.g. `run-all.sh busbar`): **~8–12 min**, or ~2–3 min if it's already built.
- **Locally**, subtract the box provisioning (~2–3 min) but expect the same build/measure times.

The one-click EC2 script does all of this unattended and terminates the box when done, so the wall
clock is hands-off. First run is slowest (cold builds + image pulls); re-runs on a warm box are much
faster.

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
