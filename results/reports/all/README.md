# All gateways - full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-29T13:54:15Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 106 µs | 44,276 | 46,852 | - | - | `litellm-ai-gateway` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 284 µs | 15,170 | 15,328 | 43 MiB | 55 MiB | `target/release/ai-gateway` |
| [Kong](https://github.com/Kong/kong) | 402 µs | 23,527 | 23,446 | 382 MiB | 596 MiB | `kong:3.9.3` |
| [APISIX](https://github.com/apache/apisix) | 451 µs | 18,995 | 19,974 | 180 MiB | 209 MiB | `apache/apisix:3.17.0-debian` |
| [Bifrost](https://github.com/maximhq/bifrost) | 934 µs | 5,204 | 5,248 | 159 MiB | 818 MiB | `maximhq/bifrost:v1.6.6` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,223 µs | 173 | 176 | 1080 MiB | 1105 MiB | `ghcr.io/berriai/litellm:v1.94.0` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,993 µs | 0 | 13,002 | 47 MiB | 69 MiB | `tensorzero/gateway:2026.6.0` |
| [Plano](https://github.com/katanemo/plano) | 220,911 µs | 0 | 21 | 607 MiB | 1013 MiB | `katanemo/plano:0.4.29` |
| [One-API](https://github.com/songquanpeng/one-api) | 983,572 µs | 0 | 33 | 89 MiB | 143 MiB | `justsong/one-api:v0.6.10` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Portkey](https://github.com/Portkey-AI/gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): agentgateway, AISIX (api7), Busbar, GoModel, Portkey. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 241 µs | ≤ rig resolution | ✕ not measured (rig-limited) | n/a |
| [Helicone](https://github.com/Helicone/ai-gateway) | 463 µs | ≤ rig resolution | ✕ not measured (rig-limited) | 15,529 (openai → anthropic) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 653 (17,532 fps) | 20,178 (openai → gemini) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 9.0 ms | ✕ not measured (rig-limited) | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 849 µs | 5 µs | ✕ not measured (rig-limited) | 5,195 (openai → cohere) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 9.7 ms | ≤ rig resolution | 62 (1,138 fps) | 206 (openai → cohere) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 782 µs | 85 µs | 675 (9,779 fps) | 0 (openai → anthropic) |
| [Plano](https://github.com/katanemo/plano) | 177.1 ms | 64 µs | 43 (760 fps) | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 848 µs | ≤ rig resolution | 213 (6,144 fps) | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607291454)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607291454)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607291454)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607291454)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607291454)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607291454)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607291454)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607291454)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607291454)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607291454)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607291454)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607291454)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607291454)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-29 14:54 UTC** from the raw `results/*.json`.</sub>
