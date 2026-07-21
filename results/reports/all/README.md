# All gateways — full field

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores, mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-21T17:19:43Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. The highlighted bar in each chart = measured best.

| Gateway | Added latency (p99) | Max proxy RPS | Sustained RPS @20ms | Idle RSS | Peak RSS | Built |
|---|--:|--:|--:|--:|--:|---|
| LiteLLM · Rust | 152 µs | 38,423 | 30,930 | 263 MiB | 624 MiB | `litellm_rust_gateway_v1_messages_route` |
| Busbar | 155 µs | 42,242 | 29,684 | 9 MiB | 323 MiB | `busbar 1.4.1` |
| APISIX | 486 µs | 19,117 | 17,326 | 181 MiB | 754 MiB | `apache/apisix:3.17.0-debian (@sha256:6` |
| Kong | 1,506 µs | 12,428 | 12,467 | 704 MiB | 802 MiB | `kong:3.8 (@sha256:dd6cd1d94a7aae8c5a4d` |
| GoModel | 375 µs | 13,157 | 10,361 | 52 MiB | 5523 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606` |
| Helicone | 595 µs | 10,699 | 9,929 | 42 MiB | 1087 MiB | `Helicone/ai-gateway@9649b27 (source bu` |
| Bifrost | 1,054 µs | 5,513 | 5,403 | 137 MiB | 15309 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed` |
| TensorZero | 40,952 µs | 12,244 | 4,178 | 47 MiB | 767 MiB | `tensorzero/gateway:2026.6.0 (@sha256:c` |
| LiteLLM · Python | 8,070 µs | 561 | 566 | 257 MiB | 257 MiB | `litellm==1.93.0` |
| Portkey | 6,510 µs | 414 | 394 | 230 MiB | 469 MiB | `@portkey-ai/gateway@1.15.2` |
| One-API | 34,637 µs | 0 | 0 | 84 MiB | 20124 MiB | `justsong/one-api:v0.6.10 (@sha256:e667` |
| Arch | ⏳ *pending* | — | — | — | — | *pending measurement* |
| Envoy AI Gateway | ⏳ *pending* | — | — | — | — | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): Arch, Envoy AI Gateway. These land here as their runs complete — nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

![added_latency](../../added_latency.png)

![rps_max_proxy](../../rps_max_proxy.png)

![rps_sustained_20ms](../../rps_sustained_20ms.png)

![memory_rss](../../memory_rss.png)

![rps_per_dollar](../../rps_per_dollar.png)

![cost_per_million](../../cost_per_million.png)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-21 17:30 UTC** from the raw `results/*.json`.</sub>
