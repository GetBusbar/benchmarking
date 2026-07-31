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
instance type and vCPU count are recorded in every `results/*.json` and shown on the board.

**The board is at [onthebench.ai](https://onthebench.ai)** - the complete field, live, regenerated
from the committed snapshots in [`results/snapshots/`](results/snapshots/). Every gateway that could
not serve a pairing is marked there rather than hidden, and every absent number carries the reason it
is absent.

It ranks at any of the declared tail-latency bounds and re-sorts in front of you, which is the part a
static page cannot do: the ranking depends on the bound you accept, and there is no single answer to
bake in.

> Numbers land as runs complete. Re-run `run-on-ec2.sh` and the board updates from the new snapshots.

## Prerequisites

**To run locally on your own box:**
- **Rust** (`cargo`) - builds the mock (`mock/`, a hyper server that answers all six wire protocols and sustains 100s of k RPS, so it's never the bottleneck), the engine (including its own `otb loadgen` load generator), plus the gateways compiled from source (LiteLLM-Rust, Helicone). Source builds also need `cmake`, `clang`, and `protobuf-compiler`.
- **Docker** - for the container-based gateways (Bifrost, Kong, GoModel, One-API, …).
- **Python 3 + matplotlib** - draws the charts (`pip install matplotlib`). Optional; JSON results are written either way.
- Docker. Every gateway pulls its own pinned official image on first run; the pin lives in that gateway's own `gateways/<name>/definition.json`. A few build from pinned source because no arm64 image exists - each says so in its own manifest header.

**To run the one-click cloud version** (`run-on-ec2.sh`) the *only* extra dependency is **AWS CLI v2**, configured (`aws configure` - creds + a default region). The script launches a fresh Graviton box, installs everything on it, runs the full suite, pulls the results back, and **terminates the box** - nothing to set up, nothing to clean up.

## Run it - one command, every metric

Clone, then run one script. Everything is at the repo root, and **every gateway provisions itself**
from the ref pinned in its own [`gateways/<name>/definition.json`](gateways/) - Docker images, pip, source,
or (for a native gateway) its released image's binary. Nothing to fetch by hand for any of them.

```sh
git clone https://github.com/GetBusbar/benchmarking && cd benchmarking

./run-on-ec2.sh                      # every gateway, one fresh box each, in parallel
./run-on-ec2.sh litellm-rust bifrost # a subset
./run-on-ec2.sh harvest              # pull results from boxes a dead run left behind
./run-on-ec2.sh kill                 # terminate every box now
```

Requires awscli v2 (configured), `ssh` and `rsync`. Every gateway gets its own fresh
`m7g.4xlarge`, so no gateway inherits another's page cache or disk state, and the wall clock is
the slowest single gateway rather than the sum of all of them.

One run measures **latency, throughput, and memory** for every gateway on the same box, then
regenerates the charts and the report pages. Out comes `results/matrix/<gateway>.json` (the
passthrough/translation/streaming/memory measurements), `results/reports/{all,top5}/README.md`, and
the chart PNGs.

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
gateway, edit the pin in its own `gateways/<name>/definition.json` (or override the env var); the exact ref
is stamped into every result.

### How long it takes

Plan for it - this is a build-and-measure benchmark, not a quick script:

- **Full field, all metrics** (`run-on-ec2.sh` with the default gateways): **~60–75 min** on an
  `m7g.4xlarge`. Most of that is *building* - LiteLLM-Rust from source is the long pole (~15–20 min),
  plus the LiteLLM/Kong/Helicone images and busbar. The measurement itself is only ~5–6 min per
  gateway (latency + throughput sweep + a memory soak).
- **A single gateway** (e.g. `run-on-ec2.sh busbar`): **~8–12 min**, or ~2–3 min if it's already built.
- **Locally**, subtract the box provisioning (~2–3 min) but expect the same build/measure times.

The one-click EC2 script does all of this unattended and terminates the box when done, so the wall
clock is hands-off. First run is slowest (cold builds + image pulls); re-runs on a warm box are much
faster.

## What it measures

**Passthrough perf** (folded into `matrix/`) - what the system can *do* (the metrics that matter most):

- **added latency (µs)** - p99 the gateway adds over the upstream at concurrency 1
  (gateway p99 − direct-to-mock p99). Microseconds, because at this scale ms hides the story.
- **the throughput frontier** - requests/sec at each tail latency you are willing to accept:
  **1 ms, 5 ms, 10 ms, 50 ms, 100 ms**, plus one reading with no latency bound at all. Each is the
  most req/s the cell carried while 99% of requests finished under that bound AND it failed none it
  accepted. Read as: *"18,995 req/s while 99% of requests finished under 10 ms."*

  This replaced a single "RPS ceiling with p99 under a chosen bar", and the reason is that a scalar
  cannot express a tradeoff. Throughput and tail latency rise together with concurrency, so "the
  throughput" is a POINT ON A CURVE and picking the point for the reader hides the shape. Two real
  cells from the same board: agentgateway carries 23,630 req/s at a **1 ms** tail and gains only 7%
  by dropping the bound entirely, while apisix nearly DOUBLES between 1 ms and 5 ms. Published as one
  number those looked comparable; they are not the same machine.

  All six readings come off ONE concurrency sweep, published in full beside them, so every reading is
  re-derivable from the rungs rather than taken on trust. The sequence is monotone by construction:
  relaxing a bound only adds rungs to the set the maximum is taken over.

The matrix's best same-dialect diagonal cell IS this passthrough measurement (the retired standalone
`perf/` suite is gone; gen-data projects the board's headline perf from the matrix cell).

**`stream/`** (opt-in: `SUITES="stream matrix" ./run-on-ec2.sh`) - what the gateway adds to a
token stream. The mock answers `stream:true` with a valid SSE stream: a role chunk, then 64
content deltas paced at 20 ms, then finish + `[DONE]` (Anthropic event shape on `/messages`).
Against that fixed pace, per gateway:

- **added TTFT (µs)** - time to the first content frame through the gateway minus direct-to-mock,
  at concurrency 1. The delay a user waits before the first token appears.
- **added inter-frame latency (µs)** - p50/p99 of the gateway's content-frame gap minus the
  direct-to-mock gap. Both sides carry the mock's 20 ms pace and the same timer jitter, so the
  subtraction isolates the gateway's per-frame overhead.
- **streams sustained** - the highest concurrent stream count where every expected content frame
  arrives, no stream stalls past 10x the mock's pacing interval, and the stream error rate stays
  under 0.1%; plus the frames/sec carried there. A proxy that drops a frame has dropped a user's
  token, so the sustained ceiling is the last concurrency before anything is lost. The mock-ceiling guardrail applies here too: the
  mock's own frames/sec at top concurrency is recorded and a result within 10% is flagged
  mock-bound.

A gateway that answers 200 but buffers the stream (never frames) is recorded with its
`stream_served` naming the absence reason (`"untestable"`, `"rig_limited"`, and the rest of the
`Absent` vocabulary), not with a bare `false` - this engine never writes `false` here, since that
would assert the gateway does not stream, which no observation establishes. `false` is still
representable in `results/stream/<gateway>.json` only so older artifacts predating this vocabulary
still parse. The `stream_*` fields are additive; existing result files stay valid. Knobs: `STREAM_CHUNKS`,
`STREAM_INTERVAL_MS`, `STREAM_CHUNK_BYTES`, `STALL_X`, `SWEEP`, `SWEEP_DUR`.

**`xlate/`** (opt-in: `SUITES="xlate matrix" ./run-on-ec2.sh`) measures protocol translation.
The client speaks Anthropic (POST `/v1/messages`, a Messages body, `anthropic-version` and
`x-api-key` headers) while the upstream mock speaks OpenAI on the manifest's `GW_PATH`, so the
gateway must translate the request out and the response back. The mock is untouched; that is the
point. The lane repeats the c1 added-latency measurement and the sustained-RPS-@20ms sweep on the
translation path and writes `results/xlate/<gateway>.json` (`xlate_added_latency_p99_us`, and the
translation cell's own throughput frontier). One honest asymmetry, recorded in the JSON as
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
most need nothing. The same guard (rejecting an untranslated passthrough body as a false positive) is
covered directly against the mock's own `request_shape_ok` in `mock/src/main.rs`'s test suite, gated
in CI by the `mock-shape` job.

**Memory** (folded into `matrix/`) - resident memory across a request's life (matters most at GB scale):

- **idle RSS** - the MEDIAN over the cold-idle window on a freshly restarted process, sampled *before*
  it serves any request (a warm post-sweep process would measure "recovered", not "idle").
- **steady-state RSS** - the level the RSS settles at while the identical fixed load runs on that cell.
  A cell whose RSS never goes steady within the cap has no steady state, and publishes its **growth
  rate** (MiB/min over the final window) instead: at the cap that rate IS the leak rate.
- **plateaued / time to plateau / growth rate** - whether the RSS went steady on that cell, how long it
  took, and how fast it was still moving. The growth rate is published whether or not the cell
  plateaued, because any threshold admits a leak slower than itself.
- **recovered RSS** - the RSS at the END of the recovery window after the load stops: does it release,
  or stay pinned? A gateway that pools memory and never returns it looks bounded on a boot-time
  `docker stats` but stays pinned at peak under sustained load. (`post_load_rss_mib` is a back-compat
  alias of this field.)

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

**Throughput - a frontier, not a number.** A single throughput figure invites "you picked the
flattering metric", and it deserved the accusation: whichever tail latency you allow decides the
answer, and any one choice is arbitrary. So every cell reports its rate at **six declared bounds**
(1/5/10/50/100 ms and unbounded) off one sweep, and the reader ranks at whichever bound matters to
them. Nothing is suppressed and nothing is chosen on the reader's behalf.

This replaced two scalars that were the same sweep summarised twice under a chosen ceiling - and
which could **invert against each other**, because two different algorithms read one set of windows:
the "maximum" came out BELOW the "sustained" figure on real cells (aisix 16,232 vs 16,610; bifrost
5,113 vs 5,174). A maximum another reading of the same windows beats is not a maximum. Six maxima
over sets that only grow makes that unrepresentable rather than merely policed.

Each reading carries its own evidence: the concurrency it was observed at, the tail it **actually**
produced (never the bound - 4 ms under a 100 ms bound is a different finding from 99 ms), and the
concurrency above it that stopped qualifying. When nothing above it was probed at all, the rate is a
**floor** and says so, rather than being discarded for failing to prove maximality.

A rig limit is never charged to the gateway: connections **this host** could not open (ephemeral
ports or descriptors exhausted) are counted separately and the window is discarded unmeasured, so our
own port range can never be published as
the gateway's ceiling.

**Memory.** A **per-cell memory window**, not a synthetic burst (the old standalone 150 KB x 1500c
suite is deleted - it mislabelled itself as 6x6 provenance). **Every served cell gets its own window**
and no cell is selected: the gateway is **cold-started for that cell**, its **cold idle RSS** is sampled
before it serves a single request, then the **identical fixed load** (same concurrency and payload for
every gateway and every cell - `MATRIX_MEM_CONC` / `MATRIX_MEM_PAYLOAD`) runs **until the RSS is
steady** over a trailing window (`MEM_PLATEAU_WINDOW_S`, drift and spread both under their thresholds)
or until the `MEM_PLATEAU_CAP_S` cap, and RSS is sampled through a recovery window after the load stops.
Reaching the cap is a **published result** (`plateaued: false` plus the growth rate), not an error. A
fixed duration would have let the stopping point decide the answer for any gateway still climbing when
it expired; a plateau test holds every gateway to the same standard instead. One process lifecycle per
cell, so the idle -> steady -> recovered arc is a real curve. Windows are tunable (`MEM_IDLE_S` /
`MEM_SETTLE_S`) and travel in the result, so every published label renders the durations the run
actually used. Any RSS the sampler cannot obtain is **null, never a fabricated 0**, and a rig that
cannot read RSS at all withholds the plateau verdict as **null** rather than asserting `false`.

## Add a gateway

Drop a directory under [`gateways/`](gateways/) with a `gateway.sh` manifest - four variables, four
functions. The runners are gateway-agnostic; there is nothing else to edit. See
[`gateways/README.md`](gateways/README.md).

## Honesty notes (the receipts)

- **Source refs are config, not defaults buried in a script.** Each gateway's ref is pinned in its
  own `gateways/<name>/gateway.sh`, next to that manifest's disclosure of what it deviates from and
  why, and every pin is overridable from the environment; the *actual* version/commit built is
  written into each result's `build` field. "You used an old branch" is answerable by pointing at the
  manifest and the recorded commit.
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
