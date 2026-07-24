# All gateways — full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-24T01:24:58Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 119 µs | 36,688 | 44,151 | 263 MiB | 658 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 130 µs | 29,394 | 44,430 | 9 MiB | 304 MiB | `getbusbar/busbar:1.4.1 (@sha256:a5ba83034be882` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 179 µs | 11,694 | 30,240 | 28 MiB | 867 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 313 µs | 11,021 | 15,855 | 55 MiB | 5638 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 446 µs | 18,208 | 20,105 | 178 MiB | 791 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 511 µs | 9,840 | 10,231 | 42 MiB | 1135 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 943 µs | 5,577 | 5,647 | 134 MiB | 15261 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,451 µs | 13,619 | 14,430 | 1369 MiB | 1499 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 5,311 µs | 471 | 486 | 177 MiB | 422 MiB | `portkeyai/gateway:1.15.2 (@sha256:97f094d9c8a7` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,756 µs | 598 | 604 | 3584 MiB | 5120 MiB | `ghcr.io/berriai/litellm:v1.93.0 (@sha256:a1745` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,377 µs | 0 | 0 | 88 MiB | 20512 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,954 µs | 4,157 | 12,237 | 49 MiB | 766 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [Arch](https://github.com/katanemo/archgw) | 236,942 µs | 0 | 0 | 563 MiB | 1653 MiB | `katanemo/archgw:0.3.22 (archgw CLI)` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.7 ms | 0 µs | 512 (88,399 fps) | ✕ cannot translate |
| [Busbar](https://github.com/GetBusbar/busbar) | 309 µs | 3 µs | 256 (12,265 fps) | 26,147 (openai → openai-responses) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 345 µs | 3 µs | 256 (12,274 fps) | 11,680 (openai → anthropic) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 506 µs | 8 µs | 512 (92,665 fps) | 12,467 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 12.0 ms | 9.1 ms | 512 (90,064 fps) | 16,723 (anthropic → openai) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 735 µs | 19.3 ms | 0 (48,217 fps) | 9,531 (openai → anthropic) |
| [Bifrost](https://github.com/maximhq/bifrost) | 844 µs | 16 µs | 32 (59,690 fps) | 5,661 (openai → cohere) |
| [Kong](https://github.com/Kong/kong) | 106.2 ms | 168.6 ms | 0 (93,085 fps) | 12,578 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | 30.7 ms | 127 µs | 32 (9,504 fps) | 458 (openai → gemini) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8.9 ms | 2.8 ms | 1 (5,261 fps) | 764 (openai → cohere) |
| [One-API](https://github.com/songquanpeng/one-api) | 34.6 ms | 9 µs | 32 (1,306 fps) | 0 (openai → anthropic) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41.0 ms | 0 µs | 1 (12,259 fps) | 4,189 (openai → bedrock) |
| [Arch](https://github.com/katanemo/archgw) | 253.6 ms | 208 µs | 1 (1,015 fps) | 19 (openai → bedrock) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607240346)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607240346)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607240346)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607240346)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607240346)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607240346)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607240346)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607240346)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607240346)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607240346)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607240346)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607240346)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-24 03:46 UTC** from the raw `results/*.json`.</sub>
