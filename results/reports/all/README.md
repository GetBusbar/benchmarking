# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-31T10:27:43Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 46,187 <sub>(+1% from 1 ms to no bound)</sub> | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 214 µs | 25,668 <sub>(+2% from 1 ms to no bound)</sub> | 25 MiB | 48 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 256 µs | 17,463 <sub>(+1% from 1 ms to no bound)</sub> | 67 MiB | 354 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 319 µs | 14,504 <sub>(+5% from 1 ms to no bound)</sub> | 43 MiB | 56 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 408 µs | 20,575 <sub>(+76% from 1 ms to no bound)</sub> | 405 MiB | 618 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 444 µs | 21,091 <sub>(+93% from 1 ms to no bound)</sub> | 179 MiB | 210 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 904 µs | 5,341 <sub>(+213% from 1 ms to no bound)</sub> | 220 MiB | 819 MiB | `maximhq/bifrost:v1.6.6` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2,023 µs | 1,877 <sub>(+53% from 5 ms to no bound)</sub> | 53 MiB | 86 MiB | `enterpilot/gomodel:0.1.63` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,511 µs | 884 <sub>(+0% from 5 ms to no bound)</sub> | 124 MiB | 244 MiB | `portkeyai/gateway:1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 6,417 µs | 186 <sub>(+6% from 10 ms to no bound)</sub> | 1082 MiB | 1106 MiB | `ghcr.io/berriai/litellm:v1.94.0` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,993 µs | 0 | 49 MiB | 72 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 218,982 µs | 0 | 638 MiB | 965 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 1,556,903 µs | 0 | 88 MiB | 144 MiB | `justsong/one-api:v0.6.10` |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): Busbar. These land here as their runs complete - nothing is hidden.

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 &lt; 10 ms while failing none of the requests it accepted. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## The frontier: throughput at each tail you accept

The most req/s each gateway carried while 99% of requests finished under the column's bound **and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely changes gives you its full rate at a tight tail, and a row that climbs steeply is buying throughput with latency. The last column applies no latency bound at all, so it answers only "how much before it starts failing requests". Rates are non-decreasing left to right by construction - relaxing a bound can only add qualifying rungs, never remove one.

| Gateway | p99 &lt; 1 ms | p99 &lt; 5 ms | p99 &lt; 10 ms | p99 &lt; 50 ms | p99 &lt; 100 ms | no bound | at 10 ms: concurrency, observed tail |
|---|--:|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 45,614 | 46,187 | 46,187 | 46,187 | 46,187 | 46,187 | c=32, p99 1.07 ms, c=256 broke it |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 25,237 | 25,590 | 25,668 | 25,783 | 25,783 | 25,783 | c=128, p99 8.15 ms, c=256 broke it |
| [AISIX (api7)](https://github.com/api7/aisix) | 17,371 | 17,463 | 17,463 | 17,463 | 17,463 | 17,463 | c=16, p99 1.43 ms, c=128 broke it |
| [Helicone](https://github.com/Helicone/ai-gateway) | 13,793 | 14,412 | 14,504 | 14,504 | 14,504 | 14,504 | c=64, p99 7.24 ms, c=128 broke it |
| [Kong](https://github.com/Kong/kong) | 12,015 | 20,323 | 20,575 | 21,102 | 21,102 | 21,102 | c=64, p99 8.55 ms, c=128 broke it |
| [APISIX](https://github.com/apache/apisix) | 11,092 | 20,810 | 21,091 | 21,444 | 21,444 | 21,444 | c=64, p99 7.99 ms, c=128 broke it |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,900 | 5,341 | 5,341 | 5,375 | 5,375 | 5,938 | c=8, p99 4.8 ms, c=16 broke it |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 0 | 1,813 | 1,877 | 2,775 | 2,775 | 2,775 | c=4, p99 5.64 ms, c=8 broke it |
| [Portkey](https://github.com/Portkey-AI/gateway) | 0 | 881 | 884 | 884 | 884 | 884 | c=4, p99 6.97 ms, c=8 broke it |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 0 | 0 | 186 | 195 | 196 | 197 | c=1, p99 6.47 ms, c=2 broke it |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 0 | 0 | 0 | 6,335 | 13,303 | 13,303 | - |
| [Plano](https://github.com/katanemo/plano) | 0 | 0 | 0 | 0 | 0 | 19 | - |
| [One-API](https://github.com/songquanpeng/one-api) | 0 | 0 | 0 | 29 | 29 | 37 | - |

**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was established. **0** = the sweep ran and no rung held that bound while failing nothing. **n/a** = the record carries no reading at that bound. A **✕** cell names the record's own reason for the absence.

## The climb: what each gateway does as concurrency doubles

Every rung of the same sweep the frontier readings above are taken from, summarised. This is where "started low, took forever to climb, peaked early" is a number rather than an impression: **gain** is what the whole climb bought over the first rung, and **saturates** is the first concurrency reaching 95% of the gateway's own peak - which is the honest "peaked early" figure, since a peak's own concurrency can sit far above where the climb effectively ended. Rate figures are the median of the windows probed at that concurrency; the chart draws every window behind the median.

| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) | saturates (95% of peak) | p99 at lowest c → at top c | first c that failed a request | top c probed |
|---|--:|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 8,468 at c=1 | 46,085 at c=32 | 5.4× / 32× | c=16 | 134 µs → 3.56 s | none | c=32768 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 4,929 at c=1 | 25,725 at c=256 | 5.2× / 256× | c=16 | 243 µs → 529 ms | c=16384 | c=32768 |
| [AISIX (api7)](https://github.com/api7/aisix) | 4,226 at c=1 | 17,254 at c=16 | 4.1× / 16× | c=8 | 260 µs → 5.25 s | none | c=32768 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 3,239 at c=1 | 14,493 at c=64 | 4.5× / 64× | c=16 | 339 µs → 16.8 ms | c=128 | c=128 |
| [Kong](https://github.com/Kong/kong) | 4,191 at c=1 | 20,851 at c=128 | 5.0× / 128× | c=32 | 427 µs → 3.09 s | none | c=32768 |
| [APISIX](https://github.com/apache/apisix) | 4,211 at c=1 | 21,283 at c=128 | 5.1× / 128× | c=64 | 461 µs → 145 ms | c=4096 | c=16384 |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,896 at c=1 | 5,916 at c=2048 | 3.1× / 2048× | c=1024 | 932 µs → 3.68 s | none | c=32768 |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,407 at c=1 | 2,774 at c=64 | 2.0× / 64× | c=64 | 2.58 ms → 5.47 s | none | c=32768 |
| [Portkey](https://github.com/Portkey-AI/gateway) | 860 at c=1 | 883 at c=8 | 1.0× / 8× | c=1 | 3.37 ms → 2.34 s | c=2048 | c=2048 |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 185 at c=1 | 195 at c=32 | 1.1× / 32× | c=2 | 6.47 ms → 5.19 s | c=8192 | c=16384 |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 24 at c=1 | 13,128 at c=1024 | 547.0× / 1024× | c=1024 | 41 ms → 523 ms | none | c=32768 |
| [Plano](https://github.com/katanemo/plano) | 4 at c=1 | 19 at c=8 | 4.8× / 8× | c=8 | 229 ms → 4.24 s | none | c=256 |
| [One-API](https://github.com/songquanpeng/one-api) | 29 at c=1 | 41 at c=32 | 1.4× / 32× | c=32 | 42.5 ms → 3.06 s | c=8 | c=32 |

A rung that failed a request it had accepted qualifies for **no** frontier reading at any bound, so rate measured at or above the failing concurrency is not throughput the board will publish - the climb chart rules that region off. **none** in that column is a measured result across the whole ladder, not a missing one.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 217 µs | ≤ rig resolution | 2,553 (18,330 fps) | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 341 µs | ≤ rig resolution | 1,517 (30,558 fps) | 23,420 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 484 µs | 10 µs | ✕ not measured | 15,980 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 529 µs | 18 µs | 809 (36,603 fps) | 14,405 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.1 ms | 168.7 ms | 844 (38,367 fps) | 19,102 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 9.0 ms | 7,680 (85,220 fps) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 745 µs | 5 µs | 1,030 (44,582 fps) | 5,334 (openai → cohere) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2.4 ms | 5 µs | 1,975 (57,396 fps) | 1,871 (openai → anthropic) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 28.9 ms | 403 µs | 1,008 (6,398 fps) | 872 (openai → anthropic) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8.6 ms | 7 µs | 118 (1,317 fps) | 224 (openai → cohere) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 722 µs | 13 µs | 1,183 (4,761 fps) | 0 (openai → openai-responses) |
| [Plano](https://github.com/katanemo/plano) | 184.1 ms | 17 µs | 59 (770 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 965 µs | 0 µs | ✕ not measured | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![frontier_shape](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shape.png?v=202607311636)

![frontier_shapes_key](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_shapes_key.png?v=202607311636)

![frontier_climb](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_climb.png?v=202607311636)

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607311636)

![frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/frontier_rps_at_bound.png?v=202607311636)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607311636)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607311636)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607311636)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607311636)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607311636)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607311636)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607311636)

![xlate_frontier_rps_at_bound](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_frontier_rps_at_bound.png?v=202607311636)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607311636)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-31 16:36 UTC** from the raw `results/*.json`.</sub>
