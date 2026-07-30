# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T20:01:54Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 98 µs | 48,394 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [Busbar](https://github.com/GetBusbar/busbar) | 114 µs | 47,557 <sub>(+4% from 1 ms to no bound)</sub> | 7 MiB | 241 MiB | `getbusbar/busbar:1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 227 µs | 25,090 <sub>(+8% from 1 ms to no bound)</sub> | 25 MiB | 47 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 267 µs | 17,746 <sub>(+0% from 1 ms to no bound)</sub> | 67 MiB | 358 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 284 µs | 15,770 <sub>(+7% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 408 µs | 19,210 <sub>(+69% from 1 ms to no bound)</sub> | 403 MiB | 614 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 457 µs | 19,319 <sub>(+185% from 1 ms to no bound)</sub> | 180 MiB | 212 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 935 µs | 5,332 <sub>(+214% from 1 ms to no bound)</sub> | 251 MiB | 828 MiB | `maximhq/bifrost:v1.6.6` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2,146 µs | 1,865 <sub>(+51% from 5 ms to no bound)</sub> | 53 MiB | 87 MiB | `enterpilot/gomodel:0.1.63` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,494 µs | 880 <sub>(+0% from 5 ms to no bound)</sub> | 124 MiB | 244 MiB | `portkeyai/gateway:1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,893 µs | 146 <sub>(+29% from 10 ms to no bound)</sub> | 1079 MiB | 1103 MiB | `ghcr.io/berriai/litellm:v1.94.0` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41,448 µs | 0 | 49 MiB | 69 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 228,961 µs | 0 | 609 MiB | 1004 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 1,261,022 µs | 0 | 86 MiB | 145 MiB | `justsong/one-api:v0.6.10` |

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 &lt; 10 ms while failing none of the requests it accepted.

## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 47,849 | 48,394 | 48,394 | 48,394 | 48,394 | 48,394 | c=64, p99 2.07 ms, c=256 broke it |
| [Busbar](https://github.com/GetBusbar/busbar) | 45,856 | 47,557 | 47,557 | 47,557 | 47,557 | 47,557 | c=128, p99 4.44 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 23,297 | 25,090 | 25,090 | 25,090 | 25,090 | 25,090 | c=32, p99 2 ms, c=256 broke it |
| [AISIX (api7)](https://github.com/api7/aisix) | 17,672 | 17,746 | 17,746 | 17,746 | 17,746 | 17,746 | c=16, p99 1.41 ms, c=128 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,799 | 15,564 | 15,770 | 15,770 | 15,770 | 15,770 | c=64, p99 6.65 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 12,412 | 19,210 | 19,210 | 20,945 | 20,945 | 20,945 | c=16, p99 2.53 ms, c=64 broke it |
| [APISIX](https://github.com/apache/apisix) | 6,897 | 17,550 | 19,319 | 19,681 | 19,681 | 19,681 | c=64, p99 8.33 ms, c=128 broke it |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,868 | 5,332 | 5,332 | 5,348 | 5,348 | 5,860 | c=8, p99 4.84 ms, c=16 broke it |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 0 | 1,791 | 1,865 | 2,701 | 2,710 | 2,710 | c=4, p99 5.71 ms, c=8 broke it |
| [Portkey](https://github.com/Portkey-AI/gateway) | 0 | 880 | 880 | 883 | 883 | 883 | c=1, p99 3.35 ms, c=8 broke it |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 0 | 0 | 146 | 155 | 157 | 188 | c=1, p99 8.19 ms, c=2 broke it |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 0 | 0 | 0 | 6,321 | 11,962 | 13,512 | - |
| [Plano](https://github.com/katanemo/plano) | 0 | 0 | 0 | 0 | 0 | 19 | - |
| [One-API](https://github.com/songquanpeng/one-api) | 0 | 0 | 0 | 33 | 33 | 36 | - |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,680 at c=1 | 48,351 at c=64 | 5.6× / 64× | c=16 | 131 µs → 4.86 s | none | c=32768 |
| [Busbar](https://github.com/GetBusbar/busbar) | 7,572 at c=1 | 47,433 at c=64 | 6.3× / 64× | c=16 | 148 µs → 0 µs | c=512 | c=512 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,484 at c=1 | 24,993 at c=64 | 5.6× / 64× | c=16 | 279 µs → 595 ms | none | c=32768 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,293 at c=1 | 17,638 at c=16 | 4.1× / 16× | c=8 | 257 µs → 3.86 s | none | c=32768 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,443 at c=1 | 15,710 at c=64 | 4.6× / 64× | c=16 | 316 µs → 32.9 ms | c=64 | c=256 |
| [Kong](https://github.com/Kong/kong) | 3,976 at c=1 | 20,677 at c=128 | 5.2× / 128× | c=128 | 452 µs → 4.25 s | none | c=32768 |
| [APISIX](https://github.com/apache/apisix) | 4,031 at c=1 | 19,237 at c=64 | 4.8× / 64× | c=64 | 482 µs → 158 ms | c=4096 | c=16384 |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,846 at c=1 | 5,790 at c=2048 | 3.1× / 2048× | c=1024 | 986 µs → 3.71 s | none | c=32768 |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,405 at c=1 | 2,690 at c=64 | 1.9× / 64× | c=64 | 2.05 ms → 5.81 s | none | c=32768 |
| [Portkey](https://github.com/Portkey-AI/gateway) | 851 at c=1 | 879 at c=8 | 1.0× / 8× | c=1 | 3.4 ms → 1.01 s | c=1024 | c=8192 |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 145 at c=1 | 167 at c=128 | 1.2× / 128× | c=32 | 8.19 ms → 5.13 s | c=4096 | c=4096 |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 24 at c=1 | 13,110 at c=1024 | 546.2× / 1024× | c=1024 | 41 ms → 492 ms | none | c=32768 |
| [Plano](https://github.com/katanemo/plano) | 4 at c=1 | 19 at c=8 | 4.8× / 8× | c=8 | 225 ms → 5.77 s | none | c=256 |
| [One-API](https://github.com/songquanpeng/one-api) | 29 at c=1 | 42 at c=32 | 1.4× / 32× | c=32 | 42.4 ms → 3.18 s | c=16 | c=32 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 169 µs | 19 µs | 3,137 (12,960 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 246 µs | 11 µs | ✕ not measured | 40,747 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 372 µs | ≤ rig resolution | 2,479 (10,022 fps) | 22,972 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 535 µs | 39 µs | 4,232 (17,123 fps) | 16,630 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 461 µs | 14 µs | 255 (12,121 fps) | 15,578 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.5 ms | 168.7 ms | 403 (18,980 fps) | 18,874 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 10.0 ms | 6,912 (90,909 fps) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 885 µs | ≤ rig resolution | 518 (22,481 fps) | 5,247 (openai → gemini) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2.1 ms | ≤ rig resolution | 1,112 (41,371 fps) | 1,864 (openai → anthropic) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 28.7 ms | 487 µs | 1,712 (5,938 fps) | 865 (openai → anthropic) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 10.9 ms | ≤ rig resolution | 62 (1,103 fps) | 181 (openai → cohere) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 717 µs | ≤ rig resolution | 968 (15,183 fps) | 0 (openai → anthropic) |
| [Plano](https://github.com/katanemo/plano) | 191.9 ms | ≤ rig resolution | 47 (667 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 739 µs | 8 µs | 213 (9,518 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![frontier_shape](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shape.png?v=202607302240)

![frontier_shapes_key](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shapes_key.png?v=202607302240)

![frontier_climb](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_climb.png?v=202607302240)

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607302240)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_rps_at_bound.png?v=202607302240)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607302240)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607302240)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607302240)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607302240)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607302240)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607302240)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607302240)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_frontier_rps_at_bound.png?v=202607302240)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607302240)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 22:40 UTC** from the raw `results/*.json`.</sub>
