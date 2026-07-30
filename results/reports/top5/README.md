# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T20:01:54Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 98 µs | 48,394 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [Busbar](https://github.com/GetBusbar/busbar) | 114 µs | 47,557 <sub>(+4% from 1 ms to no bound)</sub> | 7 MiB | 241 MiB | `getbusbar/busbar:1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 229 µs | 25,041 <sub>(+6% from 1 ms to no bound)</sub> | 25 MiB | 47 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 279 µs | 17,428 <sub>(+4% from 1 ms to no bound)</sub> | 67 MiB | 350 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 322 µs | 14,826 <sub>(+6% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 47,849 | 48,394 | 48,394 | 48,394 | 48,394 | 48,394 | c=64, p99 2.07 ms, c=256 broke it |
| [Busbar](https://github.com/GetBusbar/busbar) | 45,856 | 47,557 | 47,557 | 47,557 | 47,557 | 47,557 | c=128, p99 4.44 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 23,591 | 25,041 | 25,041 | 25,041 | 25,041 | 25,041 | c=32, p99 2 ms, c=256 broke it |
| [AISIX (api7)](https://github.com/api7/aisix) | 16,828 | 17,428 | 17,428 | 17,428 | 17,428 | 17,428 | c=16, p99 1.43 ms, c=128 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,007 | 14,768 | 14,826 | 14,826 | 14,826 | 14,826 | c=64, p99 7.03 ms, c=128 broke it |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,680 at c=1 | 48,351 at c=64 | 5.6× / 64× | c=16 | 131 µs → 4.86 s | none | c=32768 |
| [Busbar](https://github.com/GetBusbar/busbar) | 7,572 at c=1 | 47,433 at c=64 | 6.3× / 64× | c=16 | 148 µs → 0 µs | c=512 | c=512 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,571 at c=1 | 25,004 at c=32 | 5.5× / 32× | c=16 | 262 µs → 43.3 ms | c=1024 | c=1024 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,278 at c=1 | 17,414 at c=16 | 4.1× / 16× | c=8 | 261 µs → 5.23 s | none | c=32768 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,253 at c=1 | 14,785 at c=64 | 4.5× / 64× | c=16 | 332 µs → 35.1 ms | c=128 | c=256 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 169 µs | 19 µs | 3,137 (12,960 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 246 µs | 11 µs | ✕ not measured | 40,747 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 381 µs | 5 µs | 501 (12,431 fps) | 22,688 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 521 µs | 26 µs | 2,799 (11,321 fps) | 16,301 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 582 µs | ≤ rig resolution | 405 (19,136 fps) | 14,182 (openai → anthropic) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607302025)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_frontier_rps_at_bound.png?v=202607302025)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607302025)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607302025)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607302025)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607302025)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_ttft.png?v=202607302025)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_gap.png?v=202607302025)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607302025)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_frontier_rps_at_bound.png?v=202607302025)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607302025)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 20:25 UTC** from the raw `results/*.json`.</sub>
