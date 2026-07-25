# All gateways — full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-25T06:42:18Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 96 µs | 38,555 | 46,089 | 256 MiB | 263 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 116 µs | 36,130 | 46,497 | - | - | `getbusbar/busbar:1.4.1 (@sha256:a5ba83034be882` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 221 µs | 25,026 | 25,412 | 23 MiB | 44 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [APISIX](https://github.com/apache/apisix) | 412 µs | 19,115 | 19,496 | 180 MiB | 213 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Bifrost](https://github.com/maximhq/bifrost) | 927 µs | 4,967 | 6,088 | 162 MiB | 1024 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,298 µs | 15,400 | 16,056 | 411 MiB | 613 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 2,552 µs | 2,500 | 2,576 | 53 MiB | 112 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,448 µs | 835 | 853 | 124 MiB | 267 MiB | `portkeyai/gateway:1.15.2 (@sha256:97f094d9c8a7` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,835 µs | 182 | 179 | 1034 MiB | 1084 MiB | `ghcr.io/berriai/litellm:v1.93.0 (@sha256:a1745` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,347 µs | 0 | 0 | 88 MiB | 165 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,948 µs | 13,227 | 13,987 | - | - | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Helicone](https://github.com/Helicone/ai-gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): AISIX (api7), Helicone. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 512 (24,405 fps) (stream suite) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 273 µs | 1 µs | 512 (24,438 fps) (stream suite) | 34,665 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 332 µs | 1 µs | 512 (24,416 fps) (stream suite) | 25,694 (anthropic → openai) (translation suite) |
| [APISIX](https://github.com/apache/apisix) | 11.2 ms | 9.1 ms | 512 (24,386 fps) (stream suite) | 17,437 (anthropic → openai) (translation suite) |
| [Bifrost](https://github.com/maximhq/bifrost) | 1.2 ms | 28 µs | 128 (6,131 fps) (stream suite) | 2,811 (openai → cohere) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | ✕ 0 - MEASURED: sustained no stall-free stream (stream suite) | 14,941 (openai → anthropic) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 219 µs | 10 µs | 512 (24,434 fps) (stream suite) | 2,552 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 30.7 ms | 139 µs | 32 (1,531 fps) (stream suite) | 716 (openai → bedrock) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 9.4 ms | 2.7 ms | 1 (47 fps) (stream suite) | 159 (anthropic → openai-responses) |
| [One-API](https://github.com/songquanpeng/one-api) | 34.6 ms | 4 µs | 32 (1,316 fps) (stream suite) | 0 (openai → anthropic) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40.8 ms | 1 µs | 1 (48 fps) (stream suite) | 12,752 (openai → openai-responses) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607250750)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607250750)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607250750)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607250750)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607250750)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607250750)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607250750)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607250750)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607250750)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607250750)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607250750)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607250750)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607250750)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-25 07:50 UTC** from the raw `results/*.json`.</sub>
