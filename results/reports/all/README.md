# All gateways - full field

**Ran on:** unknown  ·  

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [APISIX](https://github.com/apache/apisix) | - | 0 | 0 | 178 MiB | 208 MiB | `` |
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 88 MiB | 137 MiB | `` |
| [TensorZero](https://github.com/tensorzero/tensorzero) | - | 0 | 0 | 49 MiB | 72 MiB | `` |
| [agentgateway](https://github.com/agentgateway/agentgateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [AISIX (api7)](https://github.com/api7/aisix) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Bifrost](https://github.com/maximhq/bifrost) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Busbar](https://github.com/GetBusbar/busbar) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [GoModel](https://github.com/ENTERPILOT/GOModel) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Helicone](https://github.com/Helicone/ai-gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Kong](https://github.com/Kong/kong) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [LiteLLM · Python](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [LiteLLM · Rust](https://github.com/BerriAI/litellm) | ⏳ *pending* | - | - | - | - | *pending measurement* |
| [Portkey](https://github.com/Portkey-AI/gateway) | ⏳ *pending* | - | - | - | - | *pending measurement* |

⏳ **Pending measurement** (a manifest exists; not yet run on the rig): agentgateway, AISIX (api7), Bifrost, Busbar, GoModel, Helicone, Kong, LiteLLM · Python, LiteLLM · Rust, Portkey. These land here as their runs complete - nothing is hidden.

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
**⏳** = a manifest exists but it hasn't been run on the rig yet.

![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_rss.png?v=202607262110)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/memory_recovery.png?v=202607262110)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/gateway.sh`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-26 21:10 UTC** from the raw `results/*.json`.</sub>
