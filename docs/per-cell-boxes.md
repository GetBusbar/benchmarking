# One fresh box per cell

Status: design. The engine side (`OTB_CELLS`) ships in the current instrument; the orchestrator mode
below is not built yet. Field runs still use one box per gateway.

## Why

A gateway box walks its 36 cells in sequence, so every cell after the first inherits whatever the
earlier cells left behind: heap growth, connection pools, caches, TIME_WAIT sockets, a restarted
process. A fresh box per cell removes that carry-over, and because each cell is independent the
field finishes in roughly the time of its slowest cell instead of its slowest gateway. Box-hours
stay about the same; elapsed time drops.

## What does not change

- The measurement. A scoped cell runs the same code path as in a full walk
  (`run::run_grid_cells`, tested against the full walk's verdict).
- The instrument. Every box runs the same frozen engine and mock bytes.
- Isolation on the box: gateway cores 0-3, load generator 4-9, mock 10-15.
- Box qualification. Every box qualifies itself against the same calibration before measuring.

## What does change, and must be disclosed

- Cells of one gateway are measured on different machines. Box-to-box variance (August: -3.3% to
  +2.7% against the calibration) now enters comparisons between cells of the same gateway, where
  before it only entered comparisons between gateways.
- Memory figures start from a fresh process for every cell. That is the cleaner reading, but it is not
  the August method, so a per-cell field is a new method label on the board and is not mixed with
  per-gateway fields in one comparison until an A/B run (below) shows the two agree.

## Design

1. Units. The orchestrator expands each gateway's manifest into units:
   - one unit per supported cell (`matrix` = 1), with `OTB_CELLS=<ingress>><egress>`;
   - one unit per gateway for all unsupported/untestable cells together (they are only probed, so they
     cost seconds).
2. Scheduling. `run-field.py --per-cell` packs units onto at most `quota_vcpus / 16` concurrent boxes
   (16 for ap-south-1 today), longest-expected cells first (expected time from the previous field's
   per-cell timings). Raising the regional vCPU quota is what turns this into a speed-up.
3. Box job. Unchanged `bench_gateway`, except it passes `OTB_CELLS` and names its snapshot
   `result_<gw>_<cell>_<ts>.json`. Field abort, the stall watchdog and teardown apply per box as today.
4. Merge. The site already layers scoped runs (`site/snapshots.mjs`). Per-cell mode needs one
   addition: when every snapshot of a gateway in the active field is scoped and together they cover
   its whole grid, assemble them into the base instead of requiring one full run. The assembled row
   keeps each cell's own run stamp (`__run`), so provenance stays per cell.
5. Failure. A failed unit fails the field (fail-fast, as now). A rerun repeats only the failed units.

## Validation before adoption

Run two or three gateways both ways on the same instrument and compare per-cell results. Adopt only if
throughput and latency agree within the box-qualification band and any memory difference is
explained by the removed carry-over.

## Cost

About (supported cells x per-cell time) + (boxes x ~10 min boot, image pull, qualification). For 192
supported cells that is roughly 30 extra box-hours of overhead (~$15 at $0.48/hour), traded for a
field that finishes in under an hour once the quota allows ~190 boxes at once.
