# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-29T13:54:15Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | req/s @ p99 &lt; 10 ms, zero failures | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | n/a | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 215 µs | n/a | 25 MiB | 46 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 270 µs | n/a | 67 MiB | 402 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 284 µs | n/a | 43 MiB | 55 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 402 µs | n/a | 382 MiB | 596 MiB | `kong:3.9.3` |

**Throughput is a curve, not a number.** The column above is one reading of each gateway's concurrency sweep: the most req/s it carried while 99% of requests finished under **10 ms** and it failed **none** it accepted. The same sweep is published at 5 tail-latency bounds (1 ms, 5 ms, 10 ms, 50 ms, 100 ms) plus with no bound at all, and the shape across them is the comparison that matters: a gateway already at its ceiling at 1 ms is a different machine from one that doubles when given 5 ms. See the frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is a floor and no ceiling was established.
**n/a** = this gateway's record carries no frontier reading at that bound (distinct from a measured 0, which is a number).

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated req/s @ p99 &lt; 10 ms, 20 ms model delay |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 241 µs | ≤ rig resolution | ✕ not measured | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 356 µs | ≤ rig resolution | 257 (6,980 fps) | n/a - no frontier reading at this bound |
| [AISIX (api7)](https://github.com/api7/aisix) | 550 µs | 10 µs | 3,581 (14,613 fps) | n/a - no frontier reading at this bound |
| [Helicone](https://github.com/Helicone/ai-gateway) | 463 µs | ≤ rig resolution | ✕ not measured | n/a - no frontier reading at this bound |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 653 (17,532 fps) | n/a - no frontier reading at this bound |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607300252)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607300252)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607300252)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607300252)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607300252)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_ttft.png?v=202607300252)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_gap.png?v=202607300252)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607300252)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607300252)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier reading = the highest req/s any probed concurrency carried while 99% of requests finished under the STATED bound and the gateway failed none it accepted (readings are published at 1, 5, 10, 50, 100 ms and with no bound; the columns above use 10 ms, and every caption names the bound it used); cost figures divide that 10 ms reading by $0.1632/hr for the pinned 4-core (m7g.xlarge) slice; RSS idle = after first 200, steady state = the level the RSS settled at under load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 02:52 UTC** from the raw `results/*.json`.</sub>
