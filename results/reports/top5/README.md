# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T02:36:30Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 44,363 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 216 µs | 24,298 <sub>(+7% from 1 ms to no bound)</sub> | 25 MiB | 47 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 267 µs | 17,009 <sub>(+1% from 1 ms to no bound)</sub> | 67 MiB | 349 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 291 µs | 15,108 <sub>(+5% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 396 µs | 22,659 <sub>(+79% from 1 ms to no bound)</sub> | 378 MiB | 591 MiB | `kong:3.9.3` |

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 43,876 | 44,363 | 44,363 | 44,363 | 44,363 | 44,363 | c=32, p99 1.12 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 22,864 | 24,184 | 24,298 | 24,530 | 24,530 | 24,530 | c=128, p99 8.67 ms, c=256 broke it |
| [AISIX (api7)](https://github.com/api7/aisix) | 16,835 | 17,009 | 17,009 | 17,009 | 17,009 | 17,009 | c=16, p99 1.47 ms, c=128 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,409 | 15,108 | 15,108 | 15,108 | 15,108 | 15,108 | c=32, p99 3.32 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 12,804 | 20,702 | 22,659 | 22,891 | 22,891 | 22,891 | c=64, p99 9.29 ms, c=128 broke it |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,121 at c=1 | 44,299 at c=32 | 5.5× / 32× | c=8 | 140 µs → 5.23 s | c=32768 | c=32768 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,812 at c=1 | 24,489 at c=256 | 5.1× / 256× | c=16 | 248 µs → 44.3 ms | c=1024 | c=1024 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,107 at c=1 | 16,936 at c=16 | 4.1× / 16× | c=8 | 270 µs → 4.5 s | none | c=32768 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,329 at c=1 | 15,091 at c=32 | 4.5× / 32× | c=8 | 323 µs → 16.2 ms | c=128 | c=128 |
| [Kong](https://github.com/Kong/kong) | 4,321 at c=1 | 22,593 at c=256 | 5.2× / 256× | c=64 | 419 µs → 104 ms | c=1024 | c=2048 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 281 µs | ≤ rig resolution | 1,012 (14,724 fps) | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 347 µs | 37 µs | 501 (12,605 fps) | 22,032 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 584 µs | 12 µs | ✕ not measured | 15,964 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 524 µs | ≤ rig resolution | 491 (23,011 fps) | 15,418 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 652 (29,445 fps) | 21,141 (openai → gemini) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607300452)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_frontier_rps_at_bound.png?v=202607300452)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607300452)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607300452)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607300452)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607300452)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_ttft.png?v=202607300452)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_gap.png?v=202607300452)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607300452)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_frontier_rps_at_bound.png?v=202607300452)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607300452)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 04:52 UTC** from the raw `results/*.json`.</sub>
