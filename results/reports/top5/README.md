# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-30T02:36:30Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 0 | 0 | - | - | `litellm-ai-gateway` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 215 µs | 0 | 0 | 25 MiB | 46 MiB | `ghcr.io/agentgateway/agentgateway:v1.4.0` |
| [AISIX (api7)](https://github.com/api7/aisix) | 270 µs | 0 | 0 | 67 MiB | 402 MiB | `target/release/aisix` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 284 µs | 0 | 0 | 43 MiB | 55 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 402 µs | 0 | 0 | 382 MiB | 596 MiB | `kong:3.9.3` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 281 µs | ≤ rig resolution | 1,012 (14,724 fps) | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 356 µs | ≤ rig resolution | 257 (6,980 fps) | ✕ not measured (openai → bedrock) |
| [AISIX (api7)](https://github.com/api7/aisix) | 550 µs | 10 µs | 3,581 (14,613 fps) | ✕ not measured (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 463 µs | ≤ rig resolution | ✕ not measured | ✕ not measured (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 653 (17,532 fps) | ✕ not measured (openai → gemini) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607300259)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_max_proxy.png?v=202607300259)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_sustained_20ms.png?v=202607300259)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607300259)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607300259)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607300259)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607300259)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_ttft.png?v=202607300259)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_gap.png?v=202607300259)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607300259)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_streamcpu_fps.png?v=202607300259)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_rps_sustained_20ms.png?v=202607300259)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607300259)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-30 02:59 UTC** from the raw `results/*.json`.</sub>
