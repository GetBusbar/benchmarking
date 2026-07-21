# Top 5 gateways (by throughput ceiling)

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-21T15:43:56Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Green in the charts = measured best.

| Gateway | Added latency (p99) | Max proxy RPS | Sustained RPS @20ms | Idle RSS | Peak RSS | Built |
|---|--:|--:|--:|--:|--:|---|
| LiteLLM · Rust | 148 µs | 39,720 | 31,916 | 263 MiB | 612 MiB | `litellm_rust_gateway_v1_messages_route` |
| Busbar | 154 µs | 44,030 | 30,163 | 9 MiB | 316 MiB | `busbar 1.4.1` |
| Kong | 1167 µs | 14,281 | 13,505 | 518 MiB | 846 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d` |
| GoModel | 390 µs | 12,751 | 10,094 | 25 MiB | 5061 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606` |
| Helicone | 622 µs | 10,363 | 9,532 | 42 MiB | 1138 MiB | `Helicone/ai-gateway@9649b27 (source bu` |

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
![added_latency](../../top5_added_latency.png)

![rps_max_proxy](../../top5_rps_max_proxy.png)

![rps_sustained_20ms](../../top5_rps_sustained_20ms.png)

![memory_rss](../../top5_memory_rss.png)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and zero errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-21 16:09 UTC** from the raw `results/*.json`.</sub>
