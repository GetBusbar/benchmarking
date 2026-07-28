# All gateways - full field

**Ran on:** unknown  ·  2026-07-28T05:09:47Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 103 µs | 42,089 | 46,484 | - | - | `` |
| [Busbar](https://github.com/GetBusbar/busbar) | 123 µs | 38,345 | 46,821 | 7 MiB | 280 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 214 µs | 24,732 | 24,974 | 23 MiB | 39 MiB | `` |
| [AISIX (api7)](https://github.com/api7/aisix) | 282 µs | 16,061 | 18,377 | 67 MiB | 413 MiB | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 290 µs | 14,902 | 15,181 | 43 MiB | 55 MiB | `` |
| [Kong](https://github.com/Kong/kong) | 369 µs | 26,098 | 20,871 | 387 MiB | 590 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 440 µs | 18,390 | 20,025 | 179 MiB | 210 MiB | `` |
| [Bifrost](https://github.com/maximhq/bifrost) | 897 µs | 5,278 | 5,477 | 150 MiB | 809 MiB | `` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 1,873 µs | 2,086 | 2,794 | 56 MiB | 90 MiB | `` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,658 µs | 862 | 1,002 | 153 MiB | 245 MiB | `` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,418 µs | 160 | 337 | 1035 MiB | 1084 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,993 µs | 0 | 14,063 | 49 MiB | 68 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 80 MiB | 140 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): Plano. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | n/a | n/a | 1,023 (28,147 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | n/a | n/a | 785 (21,376 fps) | 34,179 (openai → openai-responses) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | n/a | n/a | ✕ not measured (rig-limited) | 22,472 (openai → anthropic) |
| [AISIX (api7)](https://github.com/api7/aisix) | n/a | n/a | 494 (13,445 fps) | 17,023 (openai → anthropic) |
| [Helicone](https://github.com/Helicone/ai-gateway) | n/a | n/a | ✕ not measured (rig-limited) | 14,983 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | n/a | n/a | ✕ 0 - MEASURED: sustained no stall-free stream | 22,942 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | n/a | n/a | ✕ not measured (rig-limited) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | n/a | n/a | ✕ not measured (rig-limited) | 5,242 (openai → gemini) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | n/a | n/a | 751 (30,828 fps) | 2,105 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | n/a | n/a | 135 (5,128 fps) | 850 (openai → anthropic) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | n/a | n/a | 29 (1,155 fps) | 177 (openai → gemini) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | n/a | n/a | ✕ not measured (rig-limited) | 0 (openai → anthropic) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607280543)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607280543)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607280543)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607280543)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607280543)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607280543)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607280543)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607280543)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607280543)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607280543)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607280543)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607280543)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607280543)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-28 05:43 UTC** from the raw `results/*.json`.</sub>
