# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T02:36:30Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 44,363 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 215 µs | n/a | 25 MiB | 46 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 270 µs | n/a | 67 MiB | 402 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 291 µs | 15,108 <sub>(+5% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 396 µs | 22,659 <sub>(+79% from 1 ms to no bound)</sub> | 378 MiB | 591 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 451 µs | n/a | 180 MiB | 209 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 899 µs | n/a | 226 MiB | 822 MiB | `maximhq/bifrost:v1.6.6` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,952 µs | n/a | 54 MiB | 86 MiB | `enterpilot/gomodel:0.1.63` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,582 µs | n/a | 153 MiB | 243 MiB | `portkeyai/gateway:1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,223 µs | n/a | 1080 MiB | 1105 MiB | `ghcr.io/berriai/litellm:v1.94.0` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,993 µs | n/a | 47 MiB | 69 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 232,065 µs | 0 | 625 MiB | 1026 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 2,083,807 µs | 0 | 82 MiB | 144 MiB | `justsong/one-api:v0.6.10` |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): Busbar. These land here as their runs complete - nothing is hidden.

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 &lt; 10 ms while failing none of the requests it accepted. &nbsp; **n/a** = this gateway's record carries no frontier reading at that bound (distinct from a measured 0, which is a number). &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 43,876 | 44,363 | 44,363 | 44,363 | 44,363 | 44,363 | c=32, p99 1.12 ms, c=256 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,409 | 15,108 | 15,108 | 15,108 | 15,108 | 15,108 | c=32, p99 3.32 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 12,804 | 20,702 | 22,659 | 22,891 | 22,891 | 22,891 | c=64, p99 9.29 ms, c=128 broke it |
| [Plano](https://github.com/katanemo/plano) | 0 | 0 | 0 | 0 | 0 | 19 | - |
| [One-API](https://github.com/songquanpeng/one-api) | 0 | 0 | 0 | 28 | 28 | 36 | - |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,121 at c=1 | 44,299 at c=32 | 5.5× / 32× | c=8 | 140 µs → 5.23 s | c=32768 | c=32768 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,772 at c=1 | 25,239 at c=128 | 5.3× / 128× | c=16 | 246 µs → 20.1 ms | none | c=256 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,165 at c=1 | 17,203 at c=16 | 4.1× / 16× | c=8 | 263 µs → 31 ms | none | c=256 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,329 at c=1 | 15,091 at c=32 | 4.5× / 32× | c=8 | 323 µs → 16.2 ms | c=128 | c=128 |
| [Kong](https://github.com/Kong/kong) | 4,321 at c=1 | 22,593 at c=256 | 5.2× / 256× | c=64 | 419 µs → 104 ms | c=1024 | c=2048 |
| [APISIX](https://github.com/apache/apisix) | 3,988 at c=1 | 19,974 at c=128 | 5.0× / 128× | c=128 | 488 µs → 99.1 ms | none | c=1024 |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,878 at c=1 | 5,373 at c=16 | 2.9× / 16× | c=8 | 990 µs → 41 ms | none | c=64 |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,299 at c=1 | 2,561 at c=64 | 2.0× / 64× | c=64 | 1.94 ms → 320 ms | none | c=512 |
| [Portkey](https://github.com/Portkey-AI/gateway) | 886 at c=1 | 900 at c=8 | 1.0× / 8× | c=1 | 3.41 ms → 30.1 ms | none | c=16 |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 172 at c=1 | 179 at c=16 | 1.0× / 16× | c=1 | 7.11 ms → 111 ms | none | c=16 |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 24 at c=1 | 13,124 at c=2048 | 546.8× / 2048× | c=1024 | 41 ms → 65.9 ms | c=1024 | c=8192 |
| [Plano](https://github.com/katanemo/plano) | 4 at c=1 | 19 at c=8 | 4.8× / 8× | c=8 | 232 ms → 3.4 s | none | c=256 |
| [One-API](https://github.com/songquanpeng/one-api) | 28 at c=1 | 36 at c=16 | 1.3× / 16× | c=8 | 34.2 ms → 2.93 s | c=8 | c=16 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 281 µs | ≤ rig resolution | 1,012 (14,724 fps) | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 356 µs | ≤ rig resolution | 257 (6,980 fps) | n/a - no frontier reading at this bound |
| [AISIX (api7)](https://github.com/api7/aisix) | 550 µs | 10 µs | 3,581 (14,613 fps) | n/a - no frontier reading at this bound |
| [Helicone](https://github.com/Helicone/ai-gateway) | 524 µs | ≤ rig resolution | 491 (23,011 fps) | 15,418 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 652 (29,445 fps) | 21,141 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 9.0 ms | ✕ not measured | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 1.2 ms | 62 µs | ✕ not measured | n/a - no frontier reading at this bound |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1.8 ms | 24 µs | ✕ not measured | n/a - no frontier reading at this bound |
| [Portkey](https://github.com/Portkey-AI/gateway) | 28.8 ms | 408 µs | 1,837 (5,789 fps) | n/a - no frontier reading at this bound |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 9.7 ms | ≤ rig resolution | 62 (1,138 fps) | n/a - no frontier reading at this bound |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 782 µs | 85 µs | 675 (9,779 fps) | n/a - no frontier reading at this bound |
| [Plano](https://github.com/katanemo/plano) | 192.3 ms | ≤ rig resolution | 15 (452 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 772 µs | ≤ rig resolution | 106 (3,315 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![frontier_shape](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shape.png?v=202607300323)

![frontier_shapes_key](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shapes_key.png?v=202607300323)

![frontier_climb](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_climb.png?v=202607300323)

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607300323)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_rps_at_bound.png?v=202607300323)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607300323)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607300323)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607300323)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607300323)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607300323)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607300323)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607300323)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_frontier_rps_at_bound.png?v=202607300323)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607300323)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 03:23 UTC** from the raw `results/*.json`.</sub>
