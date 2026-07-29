# All gateways - full field

**Ran on:** unknown  ·  2026-07-28T23:01:09Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 105 µs | 42,634 | 46,959 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 121 µs | 43,190 | 46,934 | 7 MiB | 283 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 218 µs | 25,517 | 26,038 | 23 MiB | 41 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 263 µs | 15,627 | 17,360 | 67 MiB | 404 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 307 µs | 14,758 | 14,752 | 43 MiB | 53 MiB | `` |
| [Kong](https://github.com/Kong/kong) | 386 µs | 24,877 | 25,901 | 386 MiB | 592 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 415 µs | 20,570 | 20,797 | 180 MiB | 212 MiB | `` |
| [Bifrost](https://github.com/maximhq/bifrost) | 936 µs | 5,044 | 5,349 | 162 MiB | 824 MiB | `` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,731 µs | 2,100 | 2,764 | 56 MiB | 90 MiB | `` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,683 µs | 883 | 906 | 153 MiB | 243 MiB | `` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,079 µs | 184 | 180 | 1036 MiB | 1084 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41,382 µs | 0 | 13,855 | 49 MiB | 66 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | 226,939 µs | 0 | 21 | 584 MiB | 963 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 85 MiB | 143 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 189 µs | n/a | 2,367 (37,840 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | ✕ not measured (rig-limited) | 5,714 (openai → bedrock) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 350 µs | n/a | 502 (9,537 fps) | 23,670 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | 546 µs | 121 µs | 510 (13,777 fps) | 15,973 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 502 µs | 20 µs | ✕ not measured (rig-limited) | 14,856 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.3 ms | 168.7 ms | ✕ 0 - MEASURED: sustained no stall-free stream | 24,035 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | 10.9 ms | 9.0 ms | ✕ not measured (rig-limited) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 830 µs | 21 µs | ✕ not measured (rig-limited) | 4,196 (openai → anthropic) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1.8 ms | n/a | ✕ not measured (rig-limited) | 2,108 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 29.1 ms | 487 µs | 63 (2,856 fps) | 889 (openai → cohere) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8.5 ms | n/a | 31 (1,236 fps) | 177 (openai → bedrock) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 709 µs | 4 µs | 665 (9,658 fps) | 0 (openai → anthropic) |
| [Plano](https://github.com/katanemo/plano) | 195.8 ms | 11 µs | 7 (252 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607290157)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607290157)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607290157)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607290157)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607290157)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607290157)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607290157)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607290157)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607290157)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607290157)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607290157)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607290157)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607290157)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-29 01:57 UTC** from the raw `results/*.json`.</sub>
