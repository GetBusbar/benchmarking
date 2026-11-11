# One fresh box per cell

Status: built as an OPTION (`run-field.py --placement per-cell`); the default field is still one box
per gateway. Not yet validated by the A/B run below, so a per-cell field is not mixed with
per-gateway fields in one comparison.

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

## As built

```
# schedule only: no AWS call with both assumptions given, otherwise read-only quota/price calls
python3 run-field.py --placement per-cell --dry-run --quota-vcpus 256 --hour-usd 0.48
# a field (all gateways, or name some)
python3 run-field.py --placement per-cell --state-dir <dir> --rig-dir <rig> [gw ...] \
    [--qualify-band-pct 5] [--quota-margin-vcpus 32] [--max-boxes N]
# candidate check: one gateway, named cells, overridden image, same pinned instrument
python3 run-field.py --placement per-cell --state-dir <dir> --rig-dir <rig> busbar \
    --cells 'openai>openai' --candidate-image getbusbar/busbar:1.6.0-rc1   # or docker-archive:<path>
```

- Units (`lib/per_cell.py`): one per probed cell. Declared-`0` and `untestable` cells are never probed
  by the engine, so they ride on the gateway's first box (`OTB_CELLS` lists them beside its measured
  cell) and come back as the engine itself wrote them.
- Batches: concurrent boxes = (Service Quotas L-1216C47A - running on-demand vCPUs - margin) / 16,
  re-read before every batch. Gateways are packed first-fit-decreasing, a gateway larger than a batch
  is cut into batch-sized chunks. A batch is one `run-on-ec2.sh` invocation (arguments
  `<gw>@<cells>`); its first failed box job aborts it, and a failed batch aborts the field.
- Each box still qualifies against the instrument-matched calibration; `box.json` beside its snapshot
  records instance id, region, attempt and cells. Pieces land in `results/cells/<run>/<gw>/<cell>/`
  (or `results/candidates/...`), never `results/snapshots/`; `harvest` honours the box's `cell` tag.
- `--qualify-band-pct X` additionally fails a box whose recorded drift is below -X% (`lib/box_band.py`).
  The engine's own 20% band is part of the instrument and is unchanged.

### Merge (`lib/merge_cells.py`)

One snapshot per gateway in `results/snapshots/`, a full 36-cell grid, so the site reads it as a full
run (the base), with no site change.

| Field | Rule |
|---|---|
| matrix cells | verbatim from the box that measured them; a cell from two boxes is refused |
| `schema_version`, `definitions`, `gateway`, `build`, `arch`, `hardware`, `config`, `gateway_provenance`, every matrix flag, `upstreams.<e>.configurable/egress_config/serve_error`, `rig.arch/engine/mock/ugen/release_url/somaxconn/tcp_max_syn_backlog` | gateway-level: must agree across boxes, else refused. Any field not named here is treated the same way |
| `rig.box_qualify` | `outcome`, `band_pct`, `concurrency`, `baseline_rps`, `baseline_samples` must agree (same calibration); each box's `observed_rps`, `drift_pct`, `attempts` are per-cell, in `cell_runs[].rig` |
| `matrix.served`, `upstreams.<e>.served` | OR over cells, the engine's own rule (`record_upstream_cell`) |
| `measured_at` | newest piece (the grid was complete then) |
| `started_at`, `finished_at`, `duration_s`, `phase_s`, `memory`, `streaming`, `matrix.memory` | per-cell: kept in `cell_runs[]`; on the root only if every box agrees, else null and listed in `placement.per_cell_fields` |
| `placement`, `cell_runs` | added: method label and per-box provenance (file, sha256, box, timings, rig) |

Site changes not made, needed only to *show* per-cell provenance: `public-data.mjs` reads
`qualification` and `measured_at` from the root rig/snapshot, so per-cell qualification and dates
are in the raw JSON (`cell_runs`) but not in the API records; and no page labels the per-cell method.

### Candidate check

Writes `results/candidates/<gw>/<run>/`: the pieces, a merged snapshot stamped `candidate: true` with
`candidate_check` (image ref, id and repo digests as the box saw them, calibration, engine pin), and
`report.md`/`report.json` (`lib/candidate_report.py`) against the newest published snapshot carrying
those cells. The verdict is mechanical: same engine commit, mock sha256, calibration file, protocol
pair and metric-definition section, else it names which differs. `site/gen-data.mjs` reads only
`results/snapshots/` and also ignores any snapshot stamped `candidate: true`.
