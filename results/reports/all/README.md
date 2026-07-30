# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T02:36:30Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 44,363 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 216 µs | 24,298 <sub>(+7% from 1 ms to no bound)</sub> | 25 MiB | 47 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 291 µs | 15,108 <sub>(+5% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 396 µs | 22,659 <sub>(+79% from 1 ms to no bound)</sub> | 378 MiB | 591 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 448 µs | 20,119 <sub>(+77% from 1 ms to no bound)</sub> | 180 MiB | 211 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 922 µs | 5,170 <sub>(+218% from 1 ms to no bound)</sub> | 217 MiB | 830 MiB | `maximhq/bifrost:v1.6.6` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,997 µs | 0 | 49 MiB | 66 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 232,065 µs | 0 | 625 MiB | 1026 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 2,083,807 µs | 0 | 82 MiB | 144 MiB | `justsong/one-api:v0.6.10` |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | *pending measurement* |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | *pending measurement* |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | ⏳ *pending* | - | - | - | *pending measurement* |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | *pending measurement* |
| [Portkey](https://github.com/Portkey-AI/gateway) | ⏳ *pending* | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): AISIX (api7), Busbar, GoModel, LiteLLM · Python, Portkey. These land here as their runs complete - nothing is hidden.

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 &lt; 10 ms while failing none of the requests it accepted. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 43,876 | 44,363 | 44,363 | 44,363 | 44,363 | 44,363 | c=32, p99 1.12 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 22,864 | 24,184 | 24,298 | 24,530 | 24,530 | 24,530 | c=128, p99 8.67 ms, c=256 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 14,409 | 15,108 | 15,108 | 15,108 | 15,108 | 15,108 | c=32, p99 3.32 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 12,804 | 20,702 | 22,659 | 22,891 | 22,891 | 22,891 | c=64, p99 9.29 ms, c=128 broke it |
| [APISIX](https://github.com/apache/apisix) | 11,487 | 17,560 | 20,119 | 20,389 | 20,389 | 20,389 | c=64, p99 7.58 ms, c=128 broke it |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,783 | 4,875 | 5,170 | 5,176 | 5,176 | 5,669 | c=8, p99 5.06 ms, c=16 broke it |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 0 | 0 | 0 | 11,875 | 11,936 | 11,936 | - |
| [Plano](https://github.com/katanemo/plano) | 0 | 0 | 0 | 0 | 0 | 19 | - |
| [One-API](https://github.com/songquanpeng/one-api) | 0 | 0 | 0 | 28 | 28 | 36 | - |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,121 at c=1 | 44,299 at c=32 | 5.5× / 32× | c=8 | 140 µs → 5.23 s | c=32768 | c=32768 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,812 at c=1 | 24,489 at c=256 | 5.1× / 256× | c=16 | 248 µs → 44.3 ms | c=1024 | c=1024 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,329 at c=1 | 15,091 at c=32 | 4.5× / 32× | c=8 | 323 µs → 16.2 ms | c=128 | c=128 |
| [Kong](https://github.com/Kong/kong) | 4,321 at c=1 | 22,593 at c=256 | 5.2× / 256× | c=64 | 419 µs → 104 ms | c=1024 | c=2048 |
| [APISIX](https://github.com/apache/apisix) | 4,071 at c=1 | 20,214 at c=128 | 5.0× / 128× | c=64 | 483 µs → 148 ms | c=4096 | c=16384 |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,783 at c=1 | 5,621 at c=2048 | 3.2× / 2048× | c=1024 | 1 ms → 3.64 s | none | c=32768 |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 24 at c=1 | 12,821 at c=1024 | 534.2× / 1024× | c=1024 | 41 ms → 53.8 ms | c=1024 | c=1024 |
| [Plano](https://github.com/katanemo/plano) | 4 at c=1 | 19 at c=8 | 4.8× / 8× | c=8 | 232 ms → 3.4 s | none | c=256 |
| [One-API](https://github.com/songquanpeng/one-api) | 28 at c=1 | 36 at c=16 | 1.3× / 16× | c=8 | 34.2 ms → 2.93 s | c=8 | c=16 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 281 µs | ≤ rig resolution | 1,012 (14,724 fps) | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 347 µs | 37 µs | 501 (12,605 fps) | 22,032 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 524 µs | ≤ rig resolution | 491 (23,011 fps) | 15,418 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 652 (29,445 fps) | 21,141 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | 10.9 ms | 8.9 ms | 14,466 (56,838 fps) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 822 µs | 224 µs | 512 (22,248 fps) | 5,164 (openai → cohere) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 869 µs | ≤ rig resolution | 648 (12,225 fps) | 0 (openai → bedrock) |
| [Plano](https://github.com/katanemo/plano) | 192.3 ms | ≤ rig resolution | 15 (452 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 772 µs | ≤ rig resolution | 106 (3,315 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![frontier_shape](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shape.png?v=202607300407)

![frontier_shapes_key](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shapes_key.png?v=202607300407)

![frontier_climb](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_climb.png?v=202607300407)

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607300407)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_rps_at_bound.png?v=202607300407)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607300407)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607300407)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607300407)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607300407)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607300407)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607300407)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607300407)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_frontier_rps_at_bound.png?v=202607300407)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607300407)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 04:07 UTC** from the raw `results/*.json`.</sub>
