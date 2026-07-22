# Top 5 gateways (lowest added latency)

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-22T05:41:27Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 138 µs | 34,286 | 42,773 | 263 MiB | 650 MiB | `litellm_rust_gateway_v1_messages_route@6980723` |
| [Busbar](https://github.com/GetBusbar/busbar) | 155 µs | 30,612 | 43,025 | 9 MiB | 314 MiB | `busbar 1.4.1` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 207 µs | 11,615 | 28,610 | 28 MiB | 716 MiB | `ghcr.io/agentgateway/agentgateway:v1.3.1 (@sha` |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 374 µs | 10,477 | 13,199 | 54 MiB | 5128 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606151f909b` |
| [APISIX](https://github.com/apache/apisix) | 475 µs | 16,905 | 18,620 | 178 MiB | 776 MiB | `apache/apisix:3.17.0-debian (@sha256:6cbf65f30` |

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
## Streaming, translation and governance

Same box, same mock, one gateway at a time. Streaming figures are the overhead the gateway adds on top of the mock's paced SSE stream; translation is an Anthropic client against an OpenAI-shape upstream (the conversion is the work being measured); governed is sustained throughput with key auth, rate limits and budgets enforced, next to the same gateway running plain.

| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms | Governed RPS @20ms | Governed vs plain |
|---|--:|--:|--:|--:|--:|--:|
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | 40.8 ms | 0 µs | 512 (22,474 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [Busbar](https://github.com/GetBusbar/busbar) | 326 µs | 8 µs | 512 (24,329 fps) | 27,122 | ✕ no native key governance | n/a |
| [agentgateway](https://github.com/agentgateway/agentgateway) | 288 µs | 11 µs | 128 (6,130 fps) | ✕ cannot translate | ✕ no native key governance | n/a |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | 540 µs | 4 µs | 512 (24,199 fps) | 10,548 | ✕ no native key governance | n/a |
| [APISIX](https://github.com/apache/apisix) | 11.2 ms | 9.1 ms | 512 (24,279 fps) | ✕ cannot translate | ✕ no native key governance | n/a |

**✕** cells are measured refusals, not gaps: the gateway was offered the load and could not do the thing (buffered instead of streaming, rejected the Anthropic shape, or has no native key/limit governance). **n/a** = that suite hasn't been run for this gateway yet.

![added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_added_latency.png?v=202607221613)

![rps_max_proxy](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_max_proxy.png?v=202607221613)

![rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_sustained_20ms.png?v=202607221613)

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607221613)

![rps_per_dollar](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_rps_per_dollar.png?v=202607221613)

![cost_per_million](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_cost_per_million.png?v=202607221613)

![stream_added_ttft](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_ttft.png?v=202607221613)

![stream_added_gap](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_added_gap.png?v=202607221613)

![stream_sustained](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_stream_sustained.png?v=202607221613)

![xlate_rps_sustained_20ms](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_rps_sustained_20ms.png?v=202607221613)

![xlate_added_latency](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_xlate_added_latency.png?v=202607221613)

![governed_throughput](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_governed_throughput.png?v=202607221613)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-22 16:13 UTC** from the raw `results/*.json`.</sub>
