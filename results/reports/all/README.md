# All gateways — full field

**Ran on:** AWS m7g.4xlarge (Graviton3, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-23T22:11:23Z

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 146 µs | 33,682 | 41,550 | 263 MiB | 681 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 155 µs | 31,888 | 45,050 | 9 MiB | 284 MiB | `busbar 1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 198 µs | 11,678 | 29,878 | 28 MiB | 869 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 405 µs | 10,562 | 12,878 | 56 MiB | 4946 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 456 µs | 17,740 | 20,071 | 180 MiB | 770 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 590 µs | 9,629 | 10,141 | 42 MiB | 1145 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,035 µs | 5,419 | 5,418 | 133 MiB | 15106 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,304 µs | 12,821 | 13,662 | 1365 MiB | 1480 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 5,363 µs | 454 | 477 | 116 MiB | 491 MiB | `@portkey-ai/gateway@1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 6,582 µs | 611 | 640 | 1339 MiB | 2285 MiB | `litellm==1.93.0` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,501 µs | 0 | 0 | 85 MiB | 21463 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,944 µs | 4,165 | 12,123 | 49 MiB | 755 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [Arch](https://github.com/katanemo/archgw) | 241,883 µs | 18 | 0 | 563 MiB | 1680 MiB | `katanemo/archgw:0.3.22 (archgw CLI)` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is the gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, the gateway's measured egress out; direction named per row). A gateway with no matrix translation cell falls back to the legacy xlate suite (Anthropic in, OpenAI out), marked as such. The conversion is the work being measured.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 512 (89,201 fps) | ✕ cannot translate |
| [Busbar](https://github.com/GetBusbar/busbar) | 440 µs | 0 µs | 512 (89,059 fps) | 28,775 (openai → cohere) |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 283 µs | 0 µs | 256 (12,277 fps) | 11,612 (openai → anthropic) |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 406 µs | 3 µs | 512 (92,528 fps) | 10,814 (openai → anthropic) |
| [APISIX](https://github.com/apache/apisix) | 11.0 ms | 9.1 ms | 512 (89,326 fps) | 16,483 (anthropic → openai) |
| [Helicone](https://github.com/Helicone/ai-gateway) | 675 µs | 19.2 ms | 0 (48,396 fps) | 9,650 (openai → anthropic) |
| [Bifrost](https://github.com/maximhq/bifrost) | 1.1 ms | 17 µs | 128 (62,264 fps) | 5,332 (anthropic → openai) |
| [Kong](https://github.com/Kong/kong) | 106.2 ms | 168.7 ms | 0 (48,005 fps) | 11,854 (openai → gemini) |
| [Portkey](https://github.com/Portkey-AI/gateway) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | 400 (openai → bedrock) |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 10.4 ms | 2.7 ms | 1 (5,047 fps) | 640 (openai → gemini) |
| [One-API](https://github.com/songquanpeng/one-api) | 34.6 ms | 8 µs | 32 (1,335 fps) | 0 (openai → gemini) |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41.0 ms | 5 µs | 1 (12,267 fps) | 4,165 (openai → openai-responses) |
| [Arch](https://github.com/katanemo/archgw) | 243.5 ms | 215 µs | 1 (1,037 fps) | 18 (openai → bedrock) |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607232308)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607232308)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607232308)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607232308)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607232308)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607232308)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607232308)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607232308)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607232308)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607232308)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607232308)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607232308)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-23 23:08 UTC** from the raw `results/*.json`.</sub>
