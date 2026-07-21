# Busbar core-scaling analysis (historical, separate rig)

> **Scope + provenance.** This is a **Busbar-specific** analysis of how one gateway's throughput scales
> with core count, from a **2026-07-19 run on a different rig** (`c7g.8xlarge`, 16-core pin, a Go mock +
> `oha`) than the neutral cross-gateway field benchmark (`m7g.2xlarge`, 4-core, the Rust mock). The
> latency figure here is Busbar's **self-reported** `Server-Timing: busbar;dur` — its own internal
> compute time, **not** an externally-measured added-latency, and **not comparable** to another
> gateway's externally-clocked number. For the neutral, externally-measured, apples-to-apples
> comparison across all gateways, use [`../results/reports/`](../results/reports/) — not this page.

## Method

- **Hardware: one `c7g.8xlarge` (32 vCPU Graviton3).** Graviton has no hyperthreading, so 1 vCPU = 1
  physical core — per-core scaling is real.
- **The gateway under test is pinned to N cores** (`taskset -c 0..N-1`); the load generator and mock get
  their own cores, so the gateway is never starved and the mock is never the bottleneck.
- **Unique request bodies** (`ugen.go`) so no gateway can cache-and-skip the proxy work.
- **Sweep 2 → 16 cores**, recording req/s at 100% success and peak RSS. Busbar additionally records its
  own `busbar;dur` (self-reported internal compute) at concurrency 1.

## Files

| File | Role |
|---|---|
| `ugen.go` | Unique-body load generator (Go). Reports rps / success / p50 / p99. |
| `latency.py` | Concurrency-1 client that reads the `Server-Timing: busbar;dur` header (Busbar's self-report). |
| `bb_grav.sh` / `bf_grav.sh` | Per-core sweeps for Busbar and Bifrost (both record client-measured rps + p50/p99). |
| `bb_grav.csv` / `bf_grav.csv` | Raw per-core results from the 2026-07-19 run (both gateways' externally-measured latency is in the CSVs). |

## Throughput scaling (2026-07-19, c7g.8xlarge, unique traffic, 100% success)

Externally-measured req/s per core (both gateways, same box, same method):

| cores | Busbar req/s | Bifrost req/s |
|--:|--:|--:|
| 2 | 15,692 | 2,761 |
| 4 | 30,920 | 5,597 |
| 8 | 63,453 | 10,854 |
| 12 | 93,876 | 15,904 |
| 16 | 122,650 | 20,682 |

Both scale roughly linearly with cores on this rig. Client-measured p50/p99 latency for **both**
gateways is in the CSVs; Busbar's `busbar;dur` (its self-reported internal compute) held ~37–40 µs p99
across the sweep — a Busbar-only internal number, listed here for transparency, not as a cross-gateway
latency comparison.

## Reproduce

Launch a `c7g.8xlarge` (AL2023 arm64), install `git golang docker python3`, build the Go mock and
`ugen`, fetch `oha` + the released Busbar arm64 binary, then run `bb_grav.sh` and `bf_grav.sh`. Tear the
box down when done.
