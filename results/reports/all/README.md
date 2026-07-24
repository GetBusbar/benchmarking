# All gateways — full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-24T08:59:11Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 107 µs | 35,172 | 42,560 | 263 MiB | 1587 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 116 µs | 36,130 | 46,497 | 9 MiB | 1031 MiB | `getbusbar/busbar:1.4.1 (@sha256:a5ba83034be882` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 186 µs | 28,923 | 29,719 | 28 MiB | 1087 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 282 µs | 13,678 | 16,871 | 54 MiB | 3825 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 436 µs | 19,469 | 20,057 | 179 MiB | 766 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 444 µs | 9,859 | 10,260 | 42 MiB | 1130 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 939 µs | 4,975 | 6,244 | 128 MiB | 15130 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,277 µs | 14,351 | 14,325 | 1506 MiB | 2541 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 3,262 µs | 862 | 875 | 178 MiB | 439 MiB | `portkeyai/gateway:1.15.2 (@sha256:97f094d9c8a7` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,996 µs | 608 | 588 | 3585 MiB | 5118 MiB | `ghcr.io/berriai/litellm:v1.93.0 (@sha256:a1745` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,286 µs | 0 | 0 | 88 MiB | 21520 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,948 µs | 13,227 | 13,987 | 47 MiB | 1682 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): AISIX (api7). These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 512 (24,405 fps) | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 273 µs | 1 µs | 512 (24,438 fps) | 34,665 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 332 µs | 1 µs | 512 (24,416 fps) | 26,635 (openai → bedrock) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 219 µs | 10 µs | 512 (24,434 fps) | 15,559 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.2 ms | 9.1 ms | 512 (24,386 fps) | 17,437 (anthropic → openai) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 817 µs | 19.1 ms | ✕ not measured (rig-limited) | 9,674 (openai → anthropic) |
| [Bifrost](https://github.com/maximhq/bifrost) | 1.2 ms | 28 µs | 128 (6,131 fps) | 4,173 (openai → gemini) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | ✕ not measured (rig-limited) | 13,444 (openai → anthropic) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 30.7 ms | 139 µs | 32 (1,531 fps) | 861 (openai → gemini) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 9.4 ms | 2.7 ms | 1 (47 fps) | 761 (openai → cohere) |
| [One-API](https://github.com/songquanpeng/one-api) | 34.6 ms | 4 µs | 32 (1,316 fps) | 0 (openai → anthropic) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40.8 ms | 1 µs | 1 (48 fps) | 12,752 (openai → openai-responses) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607242045)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607242045)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607242045)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607242045)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607242045)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607242045)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607242045)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607242045)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607242045)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607242045)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607242045)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607242045)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-24 20:45 UTC** from the raw `results/*.json`.</sub>
