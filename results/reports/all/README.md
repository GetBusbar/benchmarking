# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T05:28:09Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 105 µs | 44,382 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [Busbar](https://github.com/GetBusbar/busbar) | 114 µs | 47,557 <sub>(+4% from 1 ms to no bound)</sub> | 7 MiB | 241 MiB | `getbusbar/busbar:1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 229 µs | 25,041 <sub>(+6% from 1 ms to no bound)</sub> | 25 MiB | 47 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 279 µs | 17,428 <sub>(+4% from 1 ms to no bound)</sub> | 67 MiB | 350 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 322 µs | 14,826 <sub>(+6% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 389 µs | 21,867 <sub>(+63% from 1 ms to no bound)</sub> | 382 MiB | 595 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 446 µs | 20,229 <sub>(+74% from 1 ms to no bound)</sub> | 180 MiB | 211 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 900 µs | 5,198 <sub>(+225% from 1 ms to no bound)</sub> | 235 MiB | 815 MiB | `maximhq/bifrost:v1.6.6` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2,020 µs | 1,828 <sub>(+52% from 5 ms to no bound)</sub> | 51 MiB | 86 MiB | `enterpilot/gomodel:0.1.63` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,476 µs | 877 <sub>(+0% from 5 ms to no bound)</sub> | 124 MiB | 244 MiB | `portkeyai/gateway:1.15.2` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,990 µs | 0 | 49 MiB | 70 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 201,977 µs | 0 | 614 MiB | 968 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 1,230,281 µs | 0 | 86 MiB | 144 MiB | `justsong/one-api:v0.6.10` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): LiteLLM · Python. These land here as their runs complete - nothing is hidden.

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 &lt; 10 ms while failing none of the requests it accepted. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 43,818 | 44,382 | 44,382 | 44,382 | 44,382 | 44,382 | c=32, p99 1.11 ms, c=256 broke it |
| [Busbar](https://github.com/GetBusbar/busbar) | 45,856 | 47,557 | 47,557 | 47,557 | 47,557 | 47,557 | c=128, p99 4.44 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 23,591 | 25,041 | 25,041 | 25,041 | 25,041 | 25,041 | c=32, p99 2 ms, c=256 broke it |
| [AISIX (api7)](https://github.com/api7/aisix) | 16,828 | 17,428 | 17,428 | 17,428 | 17,428 | 17,428 | c=16, p99 1.43 ms, c=128 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,007 | 14,768 | 14,826 | 14,826 | 14,826 | 14,826 | c=64, p99 7.03 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 13,539 | 21,283 | 21,867 | 22,033 | 22,033 | 22,033 | c=64, p99 9.84 ms, c=128 broke it |
| [APISIX](https://github.com/apache/apisix) | 11,929 | 18,704 | 20,229 | 20,775 | 20,775 | 20,775 | c=64, p99 7.99 ms, c=128 broke it |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,812 | 4,883 | 5,198 | 5,198 | 5,198 | 5,883 | c=8, p99 5.07 ms, c=16 broke it |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 0 | 1,746 | 1,828 | 2,654 | 2,656 | 2,656 | c=4, p99 5.83 ms, c=8 broke it |
| [Portkey](https://github.com/Portkey-AI/gateway) | 0 | 877 | 877 | 881 | 881 | 881 | c=1, p99 3.41 ms, c=8 broke it |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 0 | 0 | 0 | 6,351 | 13,332 | 13,332 | - |
| [Plano](https://github.com/katanemo/plano) | 0 | 0 | 0 | 0 | 0 | 21 | - |
| [One-API](https://github.com/songquanpeng/one-api) | 0 | 0 | 0 | 35 | 35 | 36 | - |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,152 at c=1 | 44,377 at c=32 | 5.4× / 32× | c=16 | 139 µs → 4.97 s | none | c=32768 |
| [Busbar](https://github.com/GetBusbar/busbar) | 7,572 at c=1 | 47,433 at c=64 | 6.3× / 64× | c=16 | 148 µs → 0 µs | c=512 | c=512 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,571 at c=1 | 25,004 at c=32 | 5.5× / 32× | c=16 | 262 µs → 43.3 ms | c=1024 | c=1024 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,278 at c=1 | 17,414 at c=16 | 4.1× / 16× | c=8 | 261 µs → 5.23 s | none | c=32768 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,253 at c=1 | 14,785 at c=64 | 4.5× / 64× | c=16 | 332 µs → 35.1 ms | c=128 | c=256 |
| [Kong](https://github.com/Kong/kong) | 4,392 at c=1 | 21,936 at c=128 | 5.0× / 128× | c=16 | 430 µs → 114 ms | c=1024 | c=2048 |
| [APISIX](https://github.com/apache/apisix) | 4,088 at c=1 | 20,754 at c=128 | 5.1× / 128× | c=64 | 461 µs → 153 ms | c=16384 | c=16384 |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,803 at c=1 | 5,803 at c=2048 | 3.2× / 2048× | c=1024 | 979 µs → 3.83 s | none | c=32768 |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,356 at c=1 | 2,651 at c=128 | 2.0× / 128× | c=64 | 2.74 ms → 5.49 s | none | c=32768 |
| [Portkey](https://github.com/Portkey-AI/gateway) | 861 at c=1 | 879 at c=8 | 1.0× / 8× | c=1 | 3.41 ms → 2.18 s | c=1024 | c=16384 |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 24 at c=1 | 13,747 at c=2048 | 572.8× / 2048× | c=1024 | 41 ms → 55 ms | c=1024 | c=2048 |
| [Plano](https://github.com/katanemo/plano) | 4 at c=1 | 21 at c=8 | 5.2× / 8× | c=8 | 199 ms → 5.68 s | none | c=128 |
| [One-API](https://github.com/songquanpeng/one-api) | 29 at c=1 | 40 at c=32 | 1.4× / 32× | c=32 | 42.4 ms → 3.26 s | c=4 | c=32 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 255 µs | 156 µs | ✕ not measured | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 246 µs | 11 µs | ✕ not measured | 40,747 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 381 µs | 5 µs | 501 (12,431 fps) | 22,688 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 521 µs | 26 µs | 2,799 (11,321 fps) | 16,301 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 582 µs | ≤ rig resolution | 405 (19,136 fps) | 14,182 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 856 (39,811 fps) | 20,032 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.3 ms | 9.1 ms | 13,942 (56,926 fps) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 984 µs | ≤ rig resolution | 1,027 (40,468 fps) | 5,037 (openai → cohere) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1.9 ms | ≤ rig resolution | 2,013 (60,430 fps) | 1,812 (openai → anthropic) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 29.3 ms | 448 µs | 952 (6,008 fps) | 876 (openai → cohere) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 716 µs | 50 µs | 642 (9,251 fps) | 0 (openai → openai-responses) |
| [Plano](https://github.com/katanemo/plano) | 169.2 ms | ≤ rig resolution | 81 (944 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 797 µs | ≤ rig resolution | 213 (6,526 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![frontier_shape](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shape.png?v=202607301739)

![frontier_shapes_key](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shapes_key.png?v=202607301739)

![frontier_climb](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_climb.png?v=202607301739)

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607301739)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_rps_at_bound.png?v=202607301739)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607301739)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607301739)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607301739)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607301739)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607301739)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607301739)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607301739)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_frontier_rps_at_bound.png?v=202607301739)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607301739)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 17:39 UTC** from the raw `results/*.json`.</sub>
