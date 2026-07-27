# All gateways - full field

**Ran on:** unknown  ·  2026-07-27T00:56:20Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [Kong](https://github.com/Kong/kong) | 363 µs | 25,607 | 26,795 | 412 MiB | 619 MiB | `` |
| [APISIX](https://github.com/apache/apisix) | 456 µs | 18,496 | 18,754 | 178 MiB | 209 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,991 µs | 0 | 0 | 49 MiB | 70 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 87 MiB | 139 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Bifrost](https://github.com/maximhq/bifrost) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Helicone](https://github.com/Helicone/ai-gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Portkey](https://github.com/Portkey-AI/gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): agentgateway, AISIX (api7), Bifrost, Busbar, GoModel, Helicone, LiteLLM · Python, LiteLLM · Rust, Portkey. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [Kong](https://github.com/Kong/kong) | n/a | n/a | ✕ not measured (rig-limited) | 26,631 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | n/a | n/a | ✕ not measured (rig-limited) | n/a |
| [TensorZero](https://github.com/tensorzero/tensorzero) | n/a | n/a | ✕ not measured (rig-limited) | ✕ not measured (rig-limited) (openai → bedrock) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607270116)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607270116)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607270116)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607270116)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607270116)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607270116)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607270116)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607270116)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607270116)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607270116)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607270116)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607270116)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607270116)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/gateway.sh`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-27 01:16 UTC** from the raw `results/*.json`.</sub>
