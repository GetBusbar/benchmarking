# Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)

**Ran on:** unknown  ·  

Every number below is regenerated from the raw `results/*.json` - re-run `run-all.sh` and this page updates. Passthrough and translation figures are the canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) from `site/data.json`, the same values the site table ranks. Chart bars are **colored by implementation language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency (p99), lowest first.**

| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |
|---|--:|--:|--:|--:|--:|---|
| [One-API](https://github.com/songquanpeng/one-api) | - | 0 | 0 | 87 MiB | 139 MiB | `` |

Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity under realistic LLM latency).
![memory_rss](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_rss.png?v=202607270103)

![memory_recovery](https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results/top5_memory_recovery.png?v=202607270103)

---
Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after first 200, peak = under sustained load. Same box, same mock, same load, one gateway at a time. Each gateway's source ref is pinned in its own `gateways/<name>/gateway.sh`; the built commit is in each row.

<sub>Page + charts regenerated **2026-07-27 01:03 UTC** from the raw `results/*.json`.</sub>
