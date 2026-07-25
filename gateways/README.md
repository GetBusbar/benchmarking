# Gateways — drop-in benchmark targets

Every gateway the benchmark can measure is a directory here. **Adding a gateway = adding a
directory.** The runners (`memory/run.sh`, and friends) are gateway-agnostic: they `source`
`gateways/<name>/gateway.sh` and call a fixed contract. No runner edits, no branching.

## The contract

A gateway is defined **entirely by its own directory** — nothing about it is hard-coded in the
runners, the charts, or the run lists. `run-all.sh`, `run-on-ec2.sh`, and `charts.py` all discover the
field by scanning `gateways/*/gateway.sh`, so **adding a dir adds the gateway everywhere and deleting a
dir removes it everywhere.** No list to keep in sync.

`gateways/<name>/gateway.sh` sets these variables and defines four functions:

```sh
GW_KIND=native|docker      # informational

# Self-describing metadata — charts.py + the report tables read these straight from the manifest.
GW_DISPLAY="Busbar"        # label shown in charts and the report table
GW_LANG=Rust               # implementation language → bar color bucket (Rust|Go|Python|Node|Other)
GW_REPO=https://github.com/GetBusbar/busbar   # the gateway name in the table links here

GW_PORT=8080               # port the gateway listens on
GW_PATH=/v1/chat/completions   # request path used to probe + load it
GW_MODEL=gpt-4o-mini       # model string put in the request body
GW_AUTH=bench-token        # bearer token the gateway accepts

gw_build()  { :; }         # build/pull/install — idempotent; may be empty
gw_launch() { :; }         # start it, pinned to $CORES, upstream = mock at 127.0.0.1:$MOCK_PORT
gw_rss()    { :; }         # echo current resident memory in MiB
gw_stop()   { :; }         # stop + clean up
```

`GW_LANG` colors the bars by language (Rust / Go / Python / Node / Other — anything else, e.g.
OpenResty/Lua or an Envoy/C++ data plane, folds into Other). There is **no winner highlight**: charts
sort by the measured value, so the best is already the top bar. A gateway that didn't serve is drawn
grey regardless of language.

The runner exports for you: `$MOCK_PORT` (deterministic mock upstream), `$CORES` (cpu pin),
`$GW_DIR` (this gateway's directory, for config files).

**Optional per-lane hooks** (all generic — the shared runners branch on a hook's presence, never on
a gateway's name; per-gateway values live only in the manifest):

```sh
GW_MATRIX_CAP="…"           # 6x6 declared capability grid (matrix/run.sh header documents it)
GW_MATRIX_CAP_NOTE="…"      # cited reason shown on declared-0 (grey) cells
GW_MATRIX_UNTESTABLE="ing/eg …"   # pairs the gateway serves in production but whose real cloud
GW_MATRIX_UNTESTABLE_NOTE="…"     # host is hardcoded (no base-URL override): served:"untestable",
                                  # a mock-reachability limit, distinct from declared-incapable
gw_xlate_env() { :; }       # adjust manifest knobs for the translation lane (runs before launch)
GW_XLATE_HEADERS=(…)        # header set replacing GW_HEADERS for the xlate lane only
GW_XLATE_CAP=0              # gateway does not claim anthropic-in -> openai-out translation;
GW_XLATE_CAP_NOTE="…"       # recorded as xlate_declared=false with this citation, never probed
GW_STREAM_NOTE="…"          # cited note attached to stream/streamcpu results (e.g. a link to the
                            # project's own open issue when a stream failure is a known upstream bug)
```

The load body is `{"model","messages":[…],"max_tokens":16}` — valid for both OpenAI
`/v1/chat/completions` and Anthropic `/v1/messages`, so a gateway picks its `GW_PATH`/`GW_MODEL`
and it just works. The mock answers both shapes (OpenAI by default, Anthropic for `/messages`).

## Shipped gateways

**In the default run** (serve the mock as a single-box drop-in). This table is illustrative — the
actual field is whatever dirs exist here; alphabetical, no gateway seated first.

| dir | what | notes |
|---|---|---|
| `agentgateway/` | agentgateway (Rust data plane, docker) | `ai` backend `hostOverride`/`pathOverride` → mock; no backendAuth/backendTLS; observability off |
| `apisix/` | Apache APISIX + `ai-proxy` (docker, DB-less standalone) | `override.endpoint` → mock; no etcd; access log off, workers = pinned cores |
| `arch/` | Arch (Katanemo, `archgw` CLI) | Envoy + Arch services in one arm64 container; egress-only config → mock; containers pinned to the gateway cores |
| `bifrost/` | maximhq/bifrost (docker) | openai provider base_url → mock; runs its stock config |
| `busbar/` | Busbar single binary | pulls the RELEASED image, extracts the binary, runs native |
| `gomodel/` | GoModel (ENTERPILOT/GOModel, Go, docker) | `OPENAI_BASE_URL` → mock; discovers routable models from the mock's `/v1/models` |
| `helicone/` | Helicone AI Gateway (Rust) — **built from source, run native** | no arm64 image published, so we compile it (pinned commit in `versions.env`); `openai` base-url → mock |
| `kong/` | Kong Gateway + `ai-proxy` (docker, DB-less) | `upstream_url` → mock |
| `litellm-python/` | LiteLLM `[proxy]` CLI | pip-installed; multi-worker to its pinned cores |
| `litellm-rust/` | BerriAI compiled AI-gateway beta | **only serves `/v1/messages` via `azure_ai` + the `python-config` reader** — see its `gateway.sh` header (verified against their source) |
| `one-api/` | One-API (songquanpeng/one-api, docker) | pinned to `v0.6.10` (arm64 tag); channel + token bootstrapped over the admin API in `gw_launch` |
| `portkey/` | Portkey OSS gateway (npx) | routes via `x-portkey-*` headers |
| `tensorzero/` | TensorZero (Rust, docker) | arm64 multiarch image; observability off; provider base_url → mock |

**Out of scope:** Envoy AI Gateway is Kubernetes-native (Envoy Gateway + CRDs, a full cluster), not a
single-box drop-in, so it is intentionally not in this harness.

## Fairness

Same box, same mock, same load profile, same cpu pin for every gateway. Each is launched the only
way it actually serves the endpoint — no strawmen, no idle-only snapshots. If a gateway can't serve
the endpoint, that's recorded (`served:false`) rather than hidden.

### Memory is measured the same way for every gateway

`gw_rss` and `gw_hwm` must always describe the **same process set**: the gateway's whole process tree,
summed from `/proc` (`VmRSS` / `VmHWM`). Manifests never spell that walk out themselves — they call a
matched pair from `lib/harness.sh`:

| gateway kind | rss reader | hwm reader |
|---|---|---|
| docker (10 manifests) | `container_rss_mib <container>` | `container_hwm_mib <container>` |
| native (aisix, helicone, litellm-rust) | `native_rss_mib '<pgrep -f pattern>'` | `native_hwm_mib '<same pattern>'` |

The three native manifests used to hand-roll `gw_rss` as an `awk` over a **single** pid's
`/proc/<pid>/status` while `gw_hwm` walked the whole tree, so `idle/peak/recovered_rss_mib` and
`peak_rss_hwm_mib` described different populations of the same gateway and were then compared against
ten tree-summed docker gateways. `lib/mem_rss_test.sh` now checks all 13 manifests for the matched
pair on every push, so this cannot drift back.

### Environment parity: `gw_prereqs` and the build/measure boundary

Ten gateways run an official image; three (aisix, helicone, litellm-rust) have no usable arm64 image
and are built from source, so their manifests declare `gw_prereqs`. Disposition:

| gateway | `gw_prereqs` installs | why | resident during measurement? |
|---|---|---|---|
| aisix | `git build-essential pkg-config libssl-dev protobuf-compiler` + rustup | no arm64 image; `rust-toolchain.toml` pins rustc 1.93.1, `protoc` is a build dep of the vertex/bedrock tonic/prost crates | no — build phase only |
| helicone | `git build-essential pkg-config libssl-dev` + rustup | no arm64 image; `cargo build --release -p ai-gateway` | no — build phase only |
| litellm-rust | the above plus `python3-venv python3-pip` and a ~564 MB `litellm[proxy]` venv | Rust build, **plus** the gateway's `python-config` feature loads the `litellm` package at runtime to read its config | toolchain: no. The venv: **yes, deliberately** — `gw_launch` runs the gateway with `PYTHONPATH` into it, so that cost is the gateway's own and lands inside its measured process tree |
| the other ten | nothing | official image | n/a |

Why the extra toolchain on three boxes is not a parity break:

1. **One gateway per box.** `run-on-ec2.sh` launches a dedicated `m7g`/`m7i.4xlarge` per gateway from
   the same bare AMI (docker + curl + jq + psutil, no build toolchain). Nothing another gateway
   installed is ever present while this one is measured.
2. **Build phase only, and verified.** Every `gw_prereqs` is called from that manifest's `gw_build`,
   which `matrix/run.sh` runs before the mock, the warm-up, the 36 probes, the sweeps and the memory
   window. At the boundary the runner calls `harness_build_quiesce` — it waits for compilers/package
   managers to exit and publishes the result as `build_env.quiesce` in the run JSON — and then
   `harness_seal_prereqs`, which makes `gw_prereqs` inert so no later hook can install anything while
   measurements are in flight. **Every** gateway crosses the same boundary and records the same
   field; the ten with no prereqs simply record `quiesced`. That uniformity is the parity statement.
3. **Not a resident cost.** Published memory is per-process `VmRSS`/`VmHWM` over the gateway's own
   tree — never host free memory or cgroup usage — so an idle toolchain on disk, or a page cache the
   build left warm, cannot enter it. Published latency/throughput come from a gateway pinned to
   `$CORES` with loadgen and mock on disjoint cores; the build has exited before any of that starts,
   and `m7g`/`m7i` are fixed-performance instances with no burst credits a long build could spend.

A new source-built gateway may add `gw_prereqs` — it must call it from `gw_build` and nowhere else.
