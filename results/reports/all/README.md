# All gateways - full field

**Ran on:** unknown  ·  2026-07-28T05:09:47Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 103 µs | 42,089 | 46,484 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 114 µs | 47,463 | 49,440 | 7 MiB | 291 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 233 µs | 23,923 | 24,793 | 23 MiB | 41 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 324 µs | 14,208 | 14,475 | 43 MiB | 55 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 353 µs | 11,504 | 14,719 | 67 MiB | 337 MiB | `` |
| [Kong](https://github.com/Kong/kong) | 369 µs | 26,098 | 20,871 | 387 MiB | 590 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 436 µs | 19,368 | 20,921 | 179 MiB | 209 MiB | `` |
| [Bifrost](https://github.com/maximhq/bifrost) | 941 µs | 4,923 | 5,128 | 157 MiB | 806 MiB | `` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2,214 µs | 2,086 | 2,729 | 56 MiB | 89 MiB | `` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,367 µs | 909 | 634 | 153 MiB | 244 MiB | `` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 6,695 µs | 192 | 193 | 1036 MiB | 1083 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,994 µs | 0 | 14,041 | 49 MiB | 72 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | 213,970 µs | 0 | 23 | 602 MiB | 1008 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 80 MiB | 140 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | n/a | n/a | 1,023 (28,147 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | ✕ not measured (rig-limited) | 6,988 (openai → openai-responses) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | n/a | n/a | 501 (13,468 fps) | 22,984 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | n/a | n/a | ✕ not measured (rig-limited) | 14,146 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | n/a | n/a | 483 (13,129 fps) | 11,350 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | n/a | n/a | ✕ 0 - MEASURED: sustained no stall-free stream | 22,942 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | n/a | n/a | 1,922 (77,009 fps) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | n/a | n/a | ✕ not measured (rig-limited) | 4,799 (openai → gemini) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | n/a | n/a | 897 (34,590 fps) | 2,062 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | n/a | n/a | 150 (5,667 fps) | 902 (openai → anthropic) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | n/a | n/a | 32 (1,257 fps) | 192 (openai → anthropic) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | n/a | n/a | 2,048 (10,036 fps) | 0 (openai → anthropic) |
| [Plano](https://github.com/katanemo/plano) | n/a | n/a | 1 (43 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607280851)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607280851)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607280851)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607280851)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607280851)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607280851)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607280851)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607280851)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607280851)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607280851)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607280851)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607280851)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607280851)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-28 08:51 UTC** from the raw `results/*.json`.</sub>
