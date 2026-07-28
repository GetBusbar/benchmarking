# All gateways - full field

**Ran on:** unknown  ·  2026-07-28T07:07:07Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 33,974 | 45,965 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 121 µs | 43,190 | 46,934 | 7 MiB | 283 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 233 µs | 25,042 | 25,009 | 23 MiB | 40 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 254 µs | 16,392 | 17,209 | 67 MiB | 413 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 294 µs | 15,388 | 15,642 | 43 MiB | 55 MiB | `` |
| [Kong](https://github.com/Kong/kong) | 360 µs | 25,455 | 20,753 | 398 MiB | 594 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 469 µs | 18,675 | 19,277 | 179 MiB | 210 MiB | `` |
| [Bifrost](https://github.com/maximhq/bifrost) | 896 µs | 5,146 | 5,433 | 212 MiB | 839 MiB | `` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,865 µs | 2,066 | 2,804 | 56 MiB | 90 MiB | `` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,390 µs | 899 | 910 | 154 MiB | 141 MiB | `` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,980 µs | 145 | 0 | 1036 MiB | 1083 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41,362 µs | 0 | 10,989 | 49 MiB | 66 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | 218,981 µs | 0 | 21 | 599 MiB | 952 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 80 MiB | 139 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | n/a | n/a | 1,122 (28,198 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | ✕ not measured (rig-limited) | 5,714 (openai → bedrock) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | n/a | n/a | 501 (13,658 fps) | 22,599 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | n/a | n/a | ✕ not measured (rig-limited) | 15,873 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | n/a | n/a | ✕ not measured (rig-limited) | 15,184 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | n/a | n/a | ✕ 0 - MEASURED: sustained no stall-free stream | 24,936 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | n/a | n/a | ✕ not measured (rig-limited) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | n/a | n/a | ✕ not measured (rig-limited) | 5,382 (openai → cohere) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | n/a | n/a | 925 (35,335 fps) | 1,991 (openai → bedrock) |
| [Portkey](https://github.com/Portkey-AI/gateway) | n/a | n/a | 127 (5,105 fps) | 884 (openai → anthropic) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | n/a | n/a | 27 (1,080 fps) | 125 (openai → openai-responses) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | n/a | n/a | 1,151 (8,195 fps) | 0 (openai → bedrock) |
| [Plano](https://github.com/katanemo/plano) | n/a | n/a | 1 (43 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607280852)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607280852)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607280852)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607280852)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607280852)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607280852)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607280852)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607280852)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607280852)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607280852)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607280852)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607280852)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607280852)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-28 08:52 UTC** from the raw `results/*.json`.</sub>
