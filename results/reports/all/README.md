# All gateways - full field

**Ran on:** unknown  ·  2026-07-29T01:51:12Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 40,446 | 47,576 | - | - | `` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 301 µs | 14,567 | 14,675 | 43 MiB | 52 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 85 MiB | 143 MiB | `` |
| [Plano](https://github.com/katanemo/plano) | - | 0 | 0 | - | - | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [APISIX](https://github.com/apache/apisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Bifrost](https://github.com/maximhq/bifrost) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Kong](https://github.com/Kong/kong) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Portkey](https://github.com/Portkey-AI/gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [TensorZero](https://github.com/tensorzero/tensorzero) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): agentgateway, AISIX (api7), APISIX, Bifrost, Busbar, GoModel, Kong, LiteLLM · Python, Portkey, TensorZero. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 233 µs | 22 µs | 877 (16,635 fps) | n/a |
| [Helicone](https://github.com/Helicone/ai-gateway) | 476 µs | n/a | ✕ not measured (rig-limited) | 14,542 (openai → anthropic) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607290226)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607290226)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607290226)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607290226)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607290226)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607290226)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607290226)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607290226)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607290226)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607290226)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607290226)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607290226)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607290226)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-29 02:26 UTC** from the raw `results/*.json`.</sub>
