# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** unknown  ·  2026-07-27T03:50:03Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 113 µs | 42,280 | 42,737 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 123 µs | 38,345 | 46,821 | 7 MiB | 280 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 214 µs | 24,732 | 24,974 | 23 MiB | 39 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 282 µs | 16,061 | 18,377 | 67 MiB | 413 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 286 µs | 15,224 | 15,250 | 43 MiB | 53 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | n/a | n/a | ✕ not measured (rig-limited) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | 785 (21,376 fps) | 34,179 (openai → openai-responses) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | n/a | n/a | ✕ not measured (rig-limited) | 22,472 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | n/a | n/a | 494 (13,445 fps) | 17,023 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | n/a | n/a | ✕ not measured (rig-limited) | 15,093 (openai → anthropic) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607271011)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_max_proxy.png?v=202607271011)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_sustained_20ms.png?v=202607271011)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607271011)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607271011)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607271011)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607271011)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607271011)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_streamcpu_fps.png?v=202607271011)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_rps_sustained_20ms.png?v=202607271011)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607271011)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/gateway.sh`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-27 10:11 UTC** from the raw `results/*.json`.</sub>
