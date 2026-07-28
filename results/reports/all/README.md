# All gateways - full field

**Ran on:** unknown  ·  2026-07-28T23:01:09Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 105 µs | 42,634 | 46,959 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 121 µs | 43,190 | 46,934 | 7 MiB | 283 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 225 µs | 24,944 | 25,158 | 23 MiB | 39 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 247 µs | 16,876 | 18,463 | 67 MiB | 426 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 298 µs | 14,214 | 14,594 | 43 MiB | 55 MiB | `` |
| [Kong](https://github.com/Kong/kong) | 386 µs | 24,877 | 25,901 | 386 MiB | 592 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 443 µs | 20,205 | 20,837 | 177 MiB | 207 MiB | `` |
| [Bifrost](https://github.com/maximhq/bifrost) | 925 µs | 5,378 | 5,389 | 149 MiB | 807 MiB | `` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,865 µs | 2,066 | 2,804 | 56 MiB | 90 MiB | `` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,569 µs | 875 | 903 | 153 MiB | 243 MiB | `` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8,445 µs | 148 | 167 | 1034 MiB | 1082 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41,007 µs | 0 | 13,991 | 49 MiB | 66 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | 226,939 µs | 0 | 21 | 584 MiB | 963 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 87 MiB | 139 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 189 µs | n/a | 2,367 (37,840 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | ✕ not measured (rig-limited) | 5,714 (openai → bedrock) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 358 µs | 5 µs | 501 (12,527 fps) | 23,220 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 476 µs | 0 µs | 950 (13,780 fps) | 16,976 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 519 µs | 0 µs | ✕ not measured (rig-limited) | 14,299 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.3 ms | 168.7 ms | ✕ 0 - MEASURED: sustained no stall-free stream | 24,035 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 9.9 ms | ✕ not measured (rig-limited) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 865 µs | 15 µs | ✕ not measured (rig-limited) | 5,339 (openai → gemini) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | n/a | n/a | 925 (35,335 fps) | 1,991 (openai → bedrock) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 29.4 ms | 456 µs | 127 (5,036 fps) | 907 (openai → cohere) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 10.3 ms | 114 µs | 27 (1,069 fps) | 158 (openai → gemini) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 718 µs | 0 µs | 511 (9,553 fps) | 0 (openai → openai-responses) |
| [Plano](https://github.com/katanemo/plano) | 195.8 ms | 11 µs | 7 (252 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607282336)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607282336)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607282336)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607282336)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607282336)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607282336)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607282336)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607282336)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607282336)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607282336)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607282336)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607282336)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607282336)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-28 23:36 UTC** from the raw `results/*.json`.</sub>
