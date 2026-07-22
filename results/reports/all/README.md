# All gateways — full field

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-22T05:41:27Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 138 µs | 34,286 | 42,773 | 263 MiB | 650 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 155 µs | 30,612 | 43,025 | 9 MiB | 314 MiB | `busbar 1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 207 µs | 11,615 | 28,610 | 28 MiB | 716 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 374 µs | 10,477 | 13,199 | 54 MiB | 5128 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 475 µs | 16,905 | 18,620 | 178 MiB | 776 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |
| [Helicone](https://github.com/Helicone/ai-gateway) | 629 µs | 9,358 | 9,838 | 43 MiB | 1527 MiB | `Helicone/ai-gateway@9649b27 (source build)` |
| [Bifrost](https://github.com/maximhq/bifrost) | 910 µs | 5,616 | 5,579 | 135 MiB | 15257 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed63b5c2c7` |
| [Kong](https://github.com/Kong/kong) | 1,479 µs | 12,501 | 12,881 | 709 MiB | 924 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d245ccbee` |
| [Portkey](https://github.com/Portkey-AI/gateway) | 6,168 µs | 406 | 437 | 304 MiB | 550 MiB | `@portkey-ai/gateway@1.15.2` |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 7,859 µs | 577 | 607 | 1339 MiB | 2267 MiB | `litellm==1.93.0` |
| [One-API](https://github.com/songquanpeng/one-api) | 34,531 µs | 0 | 0 | 88 MiB | 21938 MiB | `justsong/one-api:v0.6.10 (@sha256:e667221a2e19` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 41,878 µs | 4,172 | 12,262 | 49 MiB | 718 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c939db4f2` |
| [Arch](https://github.com/katanemo/archgw) | 248,939 µs | 0 | 0 | 469 MiB | 1417 MiB | `katanemo/archgw:0.3.22 (archgw CLI)` |

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.

## Streaming, translation and governance

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is an Anthropic client against an OpenAI-shape upstream (the conversion is the work being measured); governed is sustained throughput with key auth, rate limits and budgets enforced, next to the same gateway running plain.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms | Governed RPS @20ms | Governed vs plain |
|---|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 512 (22,474 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 326 µs | 8 µs | 512 (24,329 fps) | 27,122 | ✕ no native key governance | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 288 µs | 11 µs | 128 (6,130 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 540 µs | 4 µs | 512 (24,199 fps) | 10,548 | ✕ no native key governance | n/a |
| [APISIX](https://github.com/apache/apisix) | 11.2 ms | 9.1 ms | 512 (24,279 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [Helicone](https://github.com/Helicone/ai-gateway) | 728 µs | 19.2 ms | 0 | ✕ cannot translate | ✕ no native key governance | n/a |
| [Bifrost](https://github.com/maximhq/bifrost) | 0 µs | 0 µs | 0 | ✕ cannot translate | ✕ no native key governance | n/a |
| [Kong](https://github.com/Kong/kong) | 106.2 ms | 168.7 ms | 0 | ✕ cannot translate | ✕ no native key governance | n/a |
| [Portkey](https://github.com/Portkey-AI/gateway) | ✕ no SSE streaming | ✕ no SSE streaming | ✕ no SSE streaming | ✕ cannot translate | ✕ no native key governance | n/a |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | 8.9 ms | 2.6 ms | 1 (47 fps) | 481 | ✕ no native key governance | n/a |
| [One-API](https://github.com/songquanpeng/one-api) | 34.7 ms | 22 µs | 32 (1,327 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [TensorZero](https://github.com/tensorzero/tensorzero) | 0 µs | 0 µs | 0 | ✕ cannot translate | ✕ no native key governance | n/a |
| [Arch](https://github.com/katanemo/archgw) | 251.1 ms | 204 µs | 1 (41 fps) | 17 | ✕ no native key governance | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/added_latency.png?v=202607221613)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_max_proxy.png?v=202607221613)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_sustained_20ms.png?v=202607221613)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607221613)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/rps_per_dollar.png?v=202607221613)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/cost_per_million.png?v=202607221613)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_ttft.png?v=202607221613)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_added_gap.png?v=202607221613)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/stream_sustained.png?v=202607221613)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_rps_sustained_20ms.png?v=202607221613)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/xlate_added_latency.png?v=202607221613)

![governed_throughput](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/governed_throughput.png?v=202607221613)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-22 16:13 UTC** from the raw `results/*.json`.</sub>
