# All gateways — full field

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-23T04:49:53Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 146 µs | 33,682 | 41,550 | 263 MiB | 662 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 155 µs | 31,888 | 45,050 | 9 MiB | 314 MiB | `busbar 1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 198 µs | 11,678 | 29,878 | 28 MiB | 740 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 405 µs | 10,562 | 12,878 | 56 MiB | 4543 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 456 µs | 17,740 | 20,071 | 179 MiB | 759 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 590 µs | 9,629 | 10,141 | 43 MiB | 1534 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,035 µs | 5,419 | 5,418 | 129 MiB | 15248 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,304 µs | 12,821 | 13,662 | 713 MiB | 999 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 5,363 µs | 454 | 477 | 141 MiB | 546 MiB | `@portkey-ai/gateway@1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 6,582 µs | 611 | 640 | 1339 MiB | 2799 MiB | `litellm==1.93.0` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,501 µs | 0 | 0 | 86 MiB | 18752 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,944 µs | 4,165 | 12,123 | 49 MiB | 759 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [Arch](https://github.com/katanemo/archgw) | 241,883 µs | 18 | 0 | 469 MiB | 1422 MiB | `katanemo/archgw:0.3.22 (archgw CLI)` |

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 1,024 (48,080 fps) | ✕ cannot translate |
| [Busbar](https://github.com/GetBusbar/busbar) | 273 µs | 5 µs | 512 (22,414 fps) | 28,775 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 397 µs | 2 µs | 128 (6,141 fps) | 11,612 (openai → anthropic) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 515 µs | 4 µs | 1,024 (48,017 fps) | 10,814 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.9 ms | 9.1 ms | 1,024 (48,208 fps) | 16,483 (anthropic → openai) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 686 µs | 19.1 ms | 1,024 (46,233 fps) | 9,650 (openai → anthropic) |
| [Bifrost](https://github.com/maximhq/bifrost) | 1.0 ms | 31 µs | 1,024 (44,137 fps) | 5,332 (anthropic → openai) |
| [Kong](https://github.com/Kong/kong) | 106.4 ms | 168.7 ms | 128 (6,137 fps) | 11,854 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | 400 (openai → bedrock) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 596.7 ms | 2.5 ms | 1,024 (4,533 fps) | 640 (openai → gemini) |
| [One-API](https://github.com/songquanpeng/one-api) | 34.8 ms | 8 µs | 32 (1,352 fps) | 0 (openai → gemini) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41.1 ms | 4 µs | 128 (6,123 fps) | 4,165 (openai → openai-responses) |
| [Arch](https://github.com/katanemo/archgw) | 231.8 ms | 210 µs | 128 (1,127 fps) | 18 (openai → bedrock) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607231422)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607231422)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607231422)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607231422)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607231422)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607231422)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607231422)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607231422)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607231422)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607231422)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607231422)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607231422)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-23 14:22 UTC** from the raw `results/*.json`.</sub>
