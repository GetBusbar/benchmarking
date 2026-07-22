# All gateways — full field

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-22T21:14:19Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 139 µs | 33,401 | 41,111 | 263 MiB | 658 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 146 µs | 32,723 | 45,831 | 9 MiB | 312 MiB | `busbar 1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 230 µs | 11,602 | 26,950 | 27 MiB | 725 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 363 µs | 10,909 | 13,727 | 56 MiB | 5424 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 505 µs | 16,323 | 17,975 | 179 MiB | 755 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 606 µs | 9,555 | 10,058 | 42 MiB | 1556 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 1,055 µs | 5,249 | 5,269 | 132 MiB | 15328 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,371 µs | 12,545 | 13,450 | 704 MiB | 765 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 6,748 µs | 380 | 417 | 118 MiB | 646 MiB | `@portkey-ai/gateway@1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,686 µs | 584 | 607 | 1339 MiB | 2277 MiB | `litellm==1.93.0` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,556 µs | 0 | 0 | 84 MiB | 19236 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 40,940 µs | 4,150 | 12,083 | 49 MiB | 766 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [Arch](https://github.com/katanemo/archgw) | 243,919 µs | 0 | 0 | 470 MiB | 1335 MiB | `katanemo/archgw:0.3.22 (archgw CLI)` |

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.

## Streaming and translation

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is an Anthropic client against an OpenAI-shape upstream (the conversion is the work being measured).

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |
|---|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 5 µs | 512 (22,481 fps) | ✕ untranslated passthrough |
| [Busbar](https://github.com/GetBusbar/busbar) | 344 µs | 2 µs | 512 (22,585 fps) | 28,863 |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 372 µs | 5 µs | 128 (6,118 fps) | ✕ cannot translate |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 523 µs | 13 µs | 128 (6,135 fps) | ✕ untranslated passthrough |
| [APISIX](https://github.com/apache/apisix) | 12.2 ms | 9.0 ms | 512 (24,287 fps) | 15,107 |
| [Helicone](https://github.com/Helicone/ai-gateway) | 857 µs | 19.1 ms | 0 | ✕ cannot translate |
| [Bifrost](https://github.com/maximhq/bifrost) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | ✕ cannot translate |
| [Kong](https://github.com/Kong/kong) | 106.2 ms | 168.7 ms | 0 | ✕ cannot translate |
| [Portkey](https://github.com/Portkey-AI/gateway) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | ✕ untranslated passthrough |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8.8 ms | 2.6 ms | 1 (47 fps) | 490 |
| [One-API](https://github.com/songquanpeng/one-api) | 34.6 ms | 5 µs | 32 (1,340 fps) | ✕ cannot translate |
| [TensorZero](https://github.com/tensorzero/tensorzero) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | ✕ cannot translate |
| [Arch](https://github.com/katanemo/archgw) | 248.1 ms | 235 µs | 1 (42 fps) | 0 |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607222317)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607222317)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607222317)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607222317)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607222317)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607222317)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607222317)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607222317)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607222317)

![streamcpu_fps](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/streamcpu_fps.png?v=202607222317)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607222317)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607222317)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-22 23:17 UTC** from the raw `results/*.json`.</sub>
