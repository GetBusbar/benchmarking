# All gateways — full field

**Ran on:** AWS m7g.2xlarge (Graviton3, 8 cores / 32 GB). Gateway pinned to 4 cores (m7g.xlarge class), mock+loadgen on the other 4, Ubuntu 24.04. One dedicated box per gateway.  ·  2026-07-21T06:16:34Z

Every number below is regenerated from the raw `results/*.json` — re-run `run-all.sh` and this page updates. Green in the charts = measured best.

| Gateway | Added latency (p99) | Max proxy RPS | Sustained RPS @20ms | Idle RSS | Peak RSS | Built |
|---|--:|--:|--:|--:|--:|---|
| LiteLLM · Rust | 142 µs | 41,293 | 33,344 | 263 MiB | 627 MiB | `litellm_rust_gateway_v1_messages_route` |
| Busbar | 154 µs | 44,030 | 30,163 | 9 MiB | 316 MiB | `busbar 1.4.1` |
| Kong | 1164 µs | 13,859 | 13,095 | 516 MiB | 724 MiB | `kong:3.8` |
| GoModel | 390 µs | 12,751 | 10,094 | 25 MiB | 5061 MiB | `enterpilot/gomodel:0.1.55 (@sha256:606` |
| Bifrost | 951 µs | 5,420 | 5,381 | 68 MiB | 15309 MiB | `maximhq/bifrost:v1.6.4 (@sha256:5f1fed` |
| Portkey | 6241 µs | 451 | 0 | 246 MiB | 666 MiB | `@portkey-ai/gateway@1.15.2` |
| LiteLLM · Python | 6475 µs | 174 | 0 | 290 MiB | 489 MiB | `litellm==?` |
| Helicone | -82 µs † | ✕ | ✕ | 0 MiB | 0 MiB | `helicone/ai-gateway:latest` |
| One-API | ⏳ *pending* | — | — | — | — | *pending measurement* |
| GPTRouter | ⏳ *pending* | — | — | — | — | *pending measurement* |
| Arch | ⏳ *pending* | — | — | — | — | *pending measurement* |
| Envoy AI Gateway | ⏳ *pending* | — | — | — | — | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): One-API, GPTRouter, Arch, Envoy AI Gateway. These land here as their runs complete — nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity under realistic LLM latency).
**✕** = did not serve under load (0 successful req/s). &nbsp; **0** = came up, but no tested concurrency held p99 < 1 s with zero errors. &nbsp; **†** = a concurrency-1 latency exists, but the gateway failed under load — not a clean result. &nbsp; **⏳** = a manifest exists but it hasn't been run on the rig yet.

![added_latency](../../added_latency.png)

![rps_max_proxy](../../rps_max_proxy.png)

![rps_sustained_20ms](../../rps_sustained_20ms.png)

![memory_rss](../../memory_rss.png)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and zero errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.
