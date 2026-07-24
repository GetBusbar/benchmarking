# AI gateway benchmarks

> **Browse the results:** [onthebench.ai](https://onthebench.ai) - sortable tables, protocol matrix, charts, methodology.

A fair, reproducible benchmark for self-hostable AI gateways - **LiteLLM (Rust & Python), Bifrost,
Portkey, Kong, Helicone, GoModel, Busbar, and whatever else you drop in.** Same box, same mock, same load,
same cpu pin, for every gateway. One command runs it; the charts regenerate from raw results; every
source ref is pinned in the open and the built commit is stamped into the output.

Chart bars are coloured by **implementation language** (a neutral property), never by rank or brand, so
the colour can't be misread as favouring the sponsor. Every number regenerates from committed JSON. If a
gateway can't serve the endpoint, the result says `served: false` with the evidence, instead of quietly
dropping it. Add your gateway (or fix how we run yours) with a one-file [manifest](gateways/README.md).

On the Bench is built and operated by the Busbar team, and busbar is one of the entrants. It stays honest
structurally: one shared harness with no per-gateway special-casing, every number from the committed JSON,
fully open source. Don't take our word for it - read the code and re-run it.

## Results

**Ran on:** AWS `m7g.2xlarge` Graviton3 (ARM64, 8 cores), Ubuntu 24.04 - the gateway under test
pinned to 4 of them (an `m7g.xlarge`-class slice, the same 4 vCPU / $0.04-per-vCPU machine class the
gateways-under-test benchmark themselves on); the mock + load generator get the other 4. The exact
instance type and vCPU count are recorded in every `results/*.json` and printed at the top of each
report page.

Full, auto-generated result pages (regenerated from the raw JSON on every run - no hand-typed numbers):

- **[Top 5 gateways →](results/reports/top5/)** - the top 5 by lowest added latency, the same five on every chart.
- **[All gateways →](results/reports/all/)** - the complete field, including any that couldn't serve the endpoint (marked, not hidden).

Each page shows added latency (µs), RPS ceiling, idle/peak memory, whether the gateway served, and the
exact build/commit measured, plus the charts below.

> Numbers land here as runs complete. Re-run `run-all.sh` and these pages + charts update in place.

## Prerequisites

**To run locally on your own box:**
- **Rust** (`cargo`) - builds the mock (`mock/`, a hyper server that answers all six wire protocols and sustains 100s of k RPS, so it's never the bottleneck), plus the gateways compiled from source (LiteLLM-Rust, Helicone). Source builds also need `cmake`, `clang`, and `protobuf-compiler`.
- **Go** - builds the load generator (`loadgen/`).
- **Docker** - for the container-based gateways (Bifrost, Kong, GoModel, One-API, …).
- **Python 3 + matplotlib** - draws the charts (`pip install matplotlib`). Optional; JSON results are written either way.
- Docker. Every gateway pulls its own pinned official image on first run (see `gateways/versions.env`); helicone and litellm-rust build from pinned source (no arm64 image exists — see their manifests).

**To run the one-click cloud version** (`run-on-ec2.sh`) the *only* extra dependency is **AWS CLI v2**, configured (`aws configure` - creds + a default region). The script launches a fresh Graviton box, installs everything on it, runs the full suite, pulls the results back, and **terminates the box** - nothing to set up, nothing to clean up.

## Run it - one command, every metric

Clone, then run one script. Everything is at the repo root, and **every gateway provisions itself**
from the ref pinned in [`gateways/versions.env`](gateways/versions.env) - Docker images, pip, source,
or (for a native gateway) its released image's binary. Nothing to fetch by hand for any of them.

```sh
git clone https://github.com/GetBusbar/benchmarking && cd benchmarking

./run-all.sh                     # every gateway, all metrics (latency + throughput + memory)
./run-all.sh litellm-rust bifrost   # a subset
```

One run measures **latency, throughput, and memory** for every gateway on the same box, then
regenerates the charts and the report pages. Out comes `results/perf/<gateway>.json`,
`results/memory/<gateway>.json`, `results/reports/{all,top5}/README.md`, and the chart PNGs.

### On a fresh cloud box (nothing to install)

`run-on-ec2.sh` launches a Graviton box, installs everything, runs the full suite, pulls results
back, and **terminates the box**. Only needs AWS CLI v2 configured.

```sh
./run-on-ec2.sh                     # every gateway, one-click (Graviton/arm64)
./run-on-ec2.sh litellm-rust bifrost   # a subset
ARCH=x86 ./run-on-ec2.sh            # the whole field on Intel instead - one flip
```

**Architecture is one knob.** `ARCH=arm64` (default) runs the field on Graviton (`m7g`); `ARCH=x86`
runs the same field on Intel (`m7i`). One switch picks the instance family *and* the matching Ubuntu
AMI; every gateway builds/pulls for that arch on its own box, and the arch is recorded in each result
so runs from different arches never get conflated. Every gateway here runs natively on **both** -
including Helicone and One-API, which publish x86-only images (we build Helicone from source and pin
One-API to its arm64 tag), so nothing is quietly arm64-only or x86-only.

A gateway that can't be stood up (unreachable, or needs infra a single container can't provide) is
recorded `served: false` and shown as such - never silently dropped. To pin a different build of any
gateway, edit its line in `gateways/versions.env` (or override the env var); the exact ref is stamped
into every result.

### How long it takes

Plan for it - this is a build-and-measure benchmark, not a quick script:

- **Full field, all metrics** (`run-on-ec2.sh` with the default gateways): **~60–75 min** on an
  `m7g.4xlarge`. Most of that is *building* - LiteLLM-Rust from source is the long pole (~15–20 min),
  plus the LiteLLM/Kong/Helicone images and busbar. The measurement itself is only ~5–6 min per
  gateway (latency + throughput sweep + a memory soak).
- **A single gateway** (e.g. `run-all.sh busbar`): **~8–12 min**, or ~2–3 min if it's already built.
- **Locally**, subtract the box provisioning (~2–3 min) but expect the same build/measure times.

The one-click EC2 script does all of this unattended and terminates the box when done, so the wall
clock is hands-off. First run is slowest (cold builds + image pulls); re-runs on a warm box are much
faster.

## What it measures

**`perf/`** - what the system can *do* (the metrics that matter most):

- **added latency (µs)** - p99 the gateway adds over the upstream at concurrency 1
  (gateway p99 − direct-to-mock p99). Microseconds, because at this scale ms hides the story.
- **RPS ceiling** - highest sustained requests/sec with p99 under 1 s and **a <0.1% error rate** -
  "how much can it carry before it falls over."

**`stream/`** (opt-in: `SUITES="perf memory stream" ./run-all.sh`) - what the gateway adds to a
token stream. The mock answers `stream:true` with a valid SSE stream: a role chunk, then 64
content deltas paced at 20 ms, then finish + `[DONE]` (Anthropic event shape on `/messages`).
Against that fixed pace, per gateway:

- **added TTFT (µs)** - time to the first content frame through the gateway minus direct-to-mock,
  at concurrency 1. The delay a user waits before the first token appears.
- **added inter-frame latency (µs)** - p50/p99 of the gateway's content-frame gap minus the
  direct-to-mock gap. Both sides carry the mock's 20 ms pace and the same timer jitter, so the
  subtraction isolates the gateway's per-frame overhead.
- **streams sustained** - the highest concurrent stream count where at least 99.9% of expected
  frames deliver, no stream stalls past 2x the pacing interval, and the stream error rate stays
  under 0.1%; plus the frames/sec carried there. The mock-ceiling guardrail applies here too: the
  mock's own frames/sec at top concurrency is recorded and a result within 10% is flagged
  mock-bound.

A gateway that answers 200 but buffers the stream (never frames) is recorded
`stream_served: false` in `results/stream/<gateway>.json` rather than crashing the run. The
`stream_*` fields are additive; existing result files stay valid. Knobs: `STREAM_CHUNKS`,
`STREAM_INTERVAL_MS`, `STREAM_CHUNK_BYTES`, `STALL_X`, `SWEEP`, `SWEEP_DUR`.

**`governed/`** (opt-in: `SUITES="perf memory governed" ./run-all.sh`) measures what governance
costs. Every published gateway number in `perf/` is an ungoverned pass-through; production traffic
usually runs behind per-caller keys, rate limits, and budgets. This lane repeats the c1
added-latency measurement and the sustained-RPS-@20ms sweep with the gateway's native key/limit
governance active, so every request pays virtual-key resolution, rate-limit accounting, and the
budget check on the hot path. The same run then repeats the identical sweep against the plain
launch, so `results/governed/<gateway>.json` self-contains the overhead
(`governed_vs_plain_sustained_pct`, `governed_vs_plain_added_p99_delta_us`) from one box in one
sitting, never a cross-day subtraction. The minted key carries no caps (unlimited RPM/TPM/budget,
all pools): nothing can trip at benchmark rates, so the number is the cost of the check, not a
limit. A gateway opts in through two optional manifest hooks (`gw_governed_launch`,
`gw_governed_token`); a manifest without them gets a valid `governed_served: false` result, never a
crash. Today busbar is wired (governance activates when `governance.admin_token` is set; the run
mints a virtual key over `POST /api/v1/admin/keys` and uses the once-shown secret as the bench
token). LiteLLM-Rust is recorded `governed_served: false` because its key mint path requires the
Python proxy plus a Postgres database, which this single-box harness does not provision.

**`xlate/`** (opt-in: `SUITES="perf memory xlate" ./run-all.sh`) measures protocol translation.
The client speaks Anthropic (POST `/v1/messages`, a Messages body, `anthropic-version` and
`x-api-key` headers) while the upstream mock speaks OpenAI on the manifest's `GW_PATH`, so the
gateway must translate the request out and the response back. The mock is untouched; that is the
point. The lane repeats the c1 added-latency measurement and the sustained-RPS-@20ms sweep on the
translation path and writes `results/xlate/<gateway>.json` (`xlate_added_latency_p99_us`,
`xlate_rps_sustained_20ms`). One honest asymmetry, recorded in the JSON as
`xlate_baseline_shape: openai`: the mock does not translate, so the direct baseline is the OpenAI
shape straight to the mock, and the added-latency figure therefore includes the translation work,
which is exactly what this lane exists to price. Many gateways cannot serve Anthropic ingress
against an OpenAI upstream at all; one probe decides, and a non-2xx, a non-Anthropic body, or the
mock's own canned `/messages` body (proof the path was proxied verbatim, not translated) is
recorded `xlate_served: false` with the probe status and body snippet as evidence, never a crash.
Manifests may override `GW_ANTHROPIC_PATH` (default `/v1/messages`) and add
`GW_ANTHROPIC_AUTH_HEADER`; the load generator sends the token as both `Authorization: Bearer` and
`x-api-key`, so most manifests need nothing.

**`matrix/`** (opt-in: `GATEWAY=<name> matrix/run.sh`) is the protocol support matrix, a
capability suite rather than a latency suite. One gateway is probed across six ingress protocol
shapes (OpenAI chat completions, OpenAI Responses, Anthropic Messages, Gemini `generateContent`,
Cohere v2 chat with a v1 fallback, Bedrock Converse) while the upstream mock stays fixed on the
OpenAI shape, so every non-OpenAI cell is a translation claim: the gateway must convert the request
out and the response back. One probe per cell validates the response envelope, not just the status
code (`choices[0].message`, a Responses envelope, `"type":"message"` plus a content array,
`candidates[0].content`, `message.content`, `output.message.content`). The xlate passthrough guard
generalizes to every cell: the mock answers all six protocols by path, so a gateway that proxies an
ingress path verbatim gets a plausible 200 from the mock's canned constant; every translation cell
rejects that canned body as untranslated passthrough. Bedrock gets one extra honesty rule: real
Bedrock clients sign with AWS SigV4, and a gateway that answers 401/403 to the probe's bearer token
records `"unprobed_auth"` (distinct from false) with the evidence, because the harness does not
forge signatures and a red it did not earn would be a lie. Each cell writes
`{served, status, verdict_note, body_snippet}` to `results/matrix/<gateway>.json`, valid JSON
always, exit 0 always. v1 records no per-cell latency (the load generator only speaks the OpenAI
and Anthropic shapes today) and fixes the upstream to the OpenAI dialect; the full six-by-six grid
with every upstream dialect is future work. Manifests may override `GW_MATRIX_PATH_OPENAI`,
`GW_MATRIX_PATH_RESPONSES`, `GW_MATRIX_PATH_ANTHROPIC` (defaults to the shared
`GW_ANTHROPIC_PATH`), `GW_MATRIX_PATH_GEMINI`, `GW_MATRIX_PATH_COHERE`, `GW_MATRIX_PATH_BEDROCK`;
most need nothing. A self-test fixture lives at `matrix/mock-gateway/` (outside `gateways/` so
discovery never fields it): a second mock posing as the gateway, expected to score OpenAI true
incidentally and every translation cell false as passthrough. If the fixture ever goes green on a
translation cell, the guard has a hole.

**`memory/`** - resident memory across a request's life (matters most at GB scale):

- **idle RSS** - right after the gateway first answers `200`, before any load.
- **peak RSS** - highest RSS under sustained large-payload load.
- **post-load RSS** - 60 s after load stops: does it release, or stay pinned? A gateway that pools
  memory and never returns it looks bounded on a boot-time `docker stats` but stays pinned at peak
  under sustained load.

## Methodology - the choices, explained

**Machine.** `m7g.2xlarge` - 8 real Graviton3 cores (Graviton doesn't hyperthread: 1 vCPU = 1 core).
The **gateway under test is pinned to 4 cores** (= an `m7g.xlarge`, the 4-vCPU class AIGatewayBench
uses); the **mock + load generator get the other 4**, isolated. That's stricter than a co-located
4-vCPU run where the load tool steals cycles from the gateway - here the gateway gets a clean 4 cores
and the harness can't bottleneck it. All loopback; no network noise.

**The mock.** A deterministic Rust server (`mock/`) that answers all six wire protocols by path and
holds hundreds of thousands of concurrent requests so it's never the limit. One knob: `MOCK_TTFT_MS`,
a per-request delay simulating the model doing work.

**Latency - instant mock.** Added latency is `gateway p99 − direct-to-mock p99` at concurrency 1
against a **zero-delay** mock. Zero base keeps the overhead a clean microsecond delta; a 20 ms base
would just add noise to a sub-millisecond number.

**Throughput - two honest numbers, not one.** A single throughput figure invites "you picked the
flattering metric," so we report both, same 20 ms delay for every gateway:
- **Max proxy throughput** (instant mock): raw forwarding speed - trivial requests/sec the gateway
  pushes, its CPU-bound ceiling.
- **Sustained RPS @ 20 ms** (delayed mock): **AIGatewayBench's exact metric** - how many concurrent
  in-flight requests the gateway holds while the model takes 20 ms, at p99 < 1 s with <0.1% errors.
  Production-shaped (a gateway's real job is holding thousands of slow calls) and directly comparable
  to their published numbers.

A **mock-ceiling guardrail** measures the mock's own throughput each sweep and flags (⚠) any result
within 10% of it - so a number that's really the *harness's* limit is marked a floor, never sold as
the gateway's ceiling.

**Memory.** Sustained 150 KB payloads at high concurrency, sampling idle / peak / post-load RSS - the
arc that separates a bounded working set from an unbounded pool that eats the node.

## Add a gateway

Drop a directory under [`gateways/`](gateways/) with a `gateway.sh` manifest - four variables, four
functions. The runners are gateway-agnostic; there is nothing else to edit. See
[`gateways/README.md`](gateways/README.md).

## Honesty notes (the receipts)

- **Source refs are config, not defaults buried in a script.** Everything is pinned in
  [`gateways/versions.env`](gateways/versions.env) and overridable; the *actual* version/commit
  built is written into each result's `build` field. "You used an old branch" is answerable by
  pointing at the file and the recorded commit.
- **Each gateway is launched the only way it actually serves the endpoint.** For example,
  LiteLLM-Rust's `/v1/messages` route only serves the `azure_ai` provider *and* only serves at all
  under its `python-config` reader (the lean env config returns `400`) - verified against its own
  source. We launch it that way and record what it costs, rather than quoting an idle number from a
  config that doesn't serve. The reasoning is in
  [`gateways/litellm-rust/gateway.sh`](gateways/litellm-rust/gateway.sh).
- **The mock is deterministic and dumb** - it answers any path with a fixed small body (OpenAI shape,
  or Anthropic shape for `/messages`), so the number is the *gateway's* cost, not the upstream's.
- **The chart colors by measurement, not by name.** Green goes to whichever gateway measured lowest.
  If Busbar loses a metric, Busbar isn't green on it.

## Why this exists

Published gateway numbers are often hard to reproduce - the hardware isn't disclosed, the config may
not actually serve the endpoint, and the chart can't be regenerated from raw data. This repo is built
to be the opposite: disclosed hardware, configs that serve (or are recorded as not serving), and every
number regenerating from committed JSON. Clone it, run it, and check the work - including ours.
