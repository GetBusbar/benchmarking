# DESIGN — Sharded field run (one gateway, N boxes, merged snapshot)

Status: DRAFT for review. Target: land before the next busbar bump (1.6.0). Do NOT reshard an
in-flight run. All file:line anchors verified against the tree at the time of writing.

## Problem

One gateway's 36-cell grid runs sequentially on a single box. busbar is the board's critical path
(~8.85 box-h; this 1.5.5 run is ~13–18h). The grid is embarrassingly parallel: cells are independent
measurements. Splitting it across boxes gives ~same total cost at ~1/N wall-clock.

## Goal / non-goals

- GOAL: measure one gateway's grid across N boxes in parallel, then merge into ONE snapshot that is
  byte-for-byte the shape the board already consumes, with per-shard provenance recorded.
- GOAL: measurement-neutral — a sharded number must equal a single-box number for the same cell.
- NON-GOAL: change any metric, sweep, probe, or the rendered gateway config.
- NON-GOAL: shard across *gateways* (run-on-ec2 already does one box per gateway — that stays).

## The comparability model (why this is fair)

The board's rule is one frozen instrument (ENGINE_PIN) on one box class (m7g.4xlarge, gateway pinned
to 4 cores), each box `box_qualify`-gated to a 500k±20% baseline band. Sharding measures a gateway's
cells on *several* boxes instead of one. Comparability then rests on three things, all already in the
system:
1. Every shard box builds the SAME gateway from the SAME pin and renders the SAME full 6-upstream
   config (the config render is independent of which cells are measured — see §Engine change).
2. Every shard box measures on the frozen ENGINE_PIN.
3. Every shard box independently passes `box_qualify`; we record ALL N qualifications in the merged
   snapshot so the audit can see each box was in-band.

This is marginally weaker than single-box (36 cells no longer share one machine), but the qualify band
bounds cross-box variance — the same guarantee that already lets the 14 gateways' boxes be compared to
each other. Shard axis = EGRESS upstream.

### Correction to an earlier assumption
Memory is measured PER CELL (the gateway is restarted every cell to read idle RSS —
`engine/src/run.rs:75-82,2913-2914`; results land per-cell at `record.rs:313-316`). It is NOT a
per-upstream shared cost. So memory does not favor any shard axis; sharding is memory-neutral at any
granularity. Egress is chosen for STRUCTURE, not cost:
- egress is the OUTER loop (`run.rs:2825` `for eg in &cfg.dialects { for ing in ... }`),
- the snapshot is checkpoint-flushed per egress column (`suite.rs:1066-1101`, `last_egress`),
- each egress column is a self-contained `Upstream` block keyed by egress
  (`matrix.upstreams[egress]`, `record.rs:212,248`), so shards own DISJOINT keys and merge by union.

## Component 1 — Engine: `OTB_EGRESS` selector (decouple egress from ingress)

Today `OTB_DIALECTS` (`bin/otb.rs:384` → `ingress::dialects_from` `ingress.rs:41`) sets one
`cfg.dialects` used for BOTH axes: `run.rs:2825-2826` iterates `for eg in &cfg.dialects { for ing in
&cfg.dialects`. So `OTB_DIALECTS=openai` yields a 1×1 grid, not a 1-egress column. We add an
independent egress restriction that keeps the full ingress set.

Pseudo-diff:
- `engine/src/run.rs` (`RunConfig`, near line 32): add
  `pub egress_dialects: Option<Vec<Dialect>>,` (None ⇒ use `dialects` for egress, current behaviour).
- `engine/src/run.rs` `run_grid_streaming` (line 2818, 2825):
  ```
  let egresses = cfg.egress_dialects.as_deref().unwrap_or(&cfg.dialects);
  let total_cells = egresses.len() * cfg.dialects.len();      // was dialects.len()^2
  for eg in egresses {                                        // was &cfg.dialects
      for ing in &cfg.dialects {                              // UNCHANGED — full ingress column
  ```
- `engine/src/suite.rs` (`SuiteConfig`, line ~25): add `pub egress_dialects: Option<Vec<Dialect>>,`;
  set `rc.egress_dialects = cfg.egress_dialects.clone();` where `rc` is built (~`suite.rs:931`).
- `engine/src/bin/otb.rs` (~line 384, beside the OTB_DIALECTS read): parse
  `OTB_EGRESS` with the SAME `ingress::dialects_from` helper into `egress_dialects`.

Measurement-neutral because `manifest.render_configs(...)` (`otb.rs:404`) takes only cores/port/dir —
NOT dialects — so the gateway still boots with all 6 upstreams and identical routing; we merely walk a
subset of egress columns, and everything downstream keys off `id.egress`
(`run.rs:123-191`, `suite.rs:998-1006`).

Blast radius: `run.rs`, `suite.rs`, `bin/otb.rs`. No metric/probe/config/snapshot-shape change.

## Component 2 — Snapshot merge

New pure function `merge_snapshots(shards: &[ResultSnapshot]) -> Result<ResultSnapshot, MergeError>`,
home: `engine/src/snapshot.rs` (beside `write_snapshot`). Plus an `otb merge <scratch_dir> <out_dir>`
subcommand in `bin/otb.rs`.

ResultSnapshot = `engine/src/record.rs:53-104`. Merge rules:
- INVARIANT (must be identical across all shards, else refuse): `schema_version`, `definitions`,
  `gateway`, `build`, `arch`, `config.files`, `rig.engine`, `rig.mock`, `rig.release_url`.
- PAYLOAD: `matrix.upstreams` = key-union; assert shard keys are DISJOINT (each shard owns its egress
  columns). `matrix.served` / top `served` = OR across shards.
- PER-SHARD, combined: `started_at`=min, `finished_at`=max, `duration_s`/`phase_s` summed,
  `measured_at` = canonical (earliest, since it names the historical file at `snapshot.rs:298`).
- PER-SHARD, recorded (schema addition, §3): `hardware` and `rig.box_qualify` differ per box.
- RECOMPUTE: `streaming` best-diagonal projection after the union.
- Publish ONLY the merged snapshot through `write_snapshot` (`snapshot.rs:265`); the promote guard
  (`snapshot.rs:271-293`, keys via `served_cell_keys` `snapshot.rs:119`) passes because the merged
  (union) snapshot strictly dominates any prior. CAUTION: never `write_snapshot` an individual shard
  into the canonical dir — a single-column snapshot would trip the guard against a fuller prior.
  Shards write to per-shard SCRATCH dirs; only the merge reaches the canonical dir.

## Component 3 — Schema: per-egress provenance (record.rs)

Today there is exactly one `rig.box_qualify` and one `hardware` per snapshot (`record.rs:143,86`);
`Upstream` (`record.rs:248-260`) has neither. To keep all N qualifications (the fairness evidence),
add to `Upstream`:
```
pub box_qualify: Option<serde_json::Value>,  // the shard box that measured THIS egress column
pub hardware: Option<String>,                // that box's hardware string
```
`schema_version` bump (`record.rs:54`). Site/audit read: box shows top-level `rig.box_qualify` for a
single-box run and per-`Upstream` for a sharded run (both present is fine — top-level = the merge box's
own qualify or the canonical one). Auditors that assert one engine pin are unaffected (engine is an
INVARIANT across shards).

## Component 4 — Orchestration (run-on-ec2.sh): shard mode

New: `SHARD_BY=egress run-on-ec2.sh <gateway>` (or `--shard egress`). For that gateway:
- Launch N boxes (N = |egress upstreams| = 6), each tagged `run=$RUN_ID`, `purpose=gateway-bench`,
  plus `shard=<egress>` and `gateway=<gw>`. Each box: full clone at BENCH_COMMIT, full config,
  `OTB_EGRESS=<egress>`, same ENGINE_PIN, its own `box_qualify`.
- Harvest each box's partial snapshot to `results/shards/<gw>/<egress>.json` (scratch, NOT canonical).
- Once all N are DONE: run `otb merge results/shards/<gw> results/snapshots` → one merged snapshot →
  normal publish (commit+push just that gateway's merged snapshot).
- Failure handling: if a shard box fails/qualify-fails, the merge is INCOMPLETE — refuse to publish a
  partial board row (loudly), keep the good shards for a targeted re-run of only the missing egress
  column(s). (Mirrors the promote-guard philosophy: never publish a thinner result as if complete.)
- Reuses the now-robust shared-key lifetime (teardown keeps the key while sibling shard boxes live).

## Cost / wall-clock

Per-cell compute is unchanged, so total box-hours ≈ single-box + N× fixed overhead
(~5–10 min boot+build+qualify per box). Egress shard = 6 boxes:
- wall-clock ≈ slowest egress column. Streaming is only on openai+anthropic INGRESS, so every egress
  column carries exactly 2 streaming cells (~1700s) + 4 non-streaming (~800s) ≈ ~1.8h + overhead ≈ ~2h,
  vs ~13–18h single-box. ~7–9× faster.
- cost ≈ 6 × ~2h = ~12 box-h + ~6×8min overhead ≈ ~13 box-h — within noise of the single-box run.
- Finer (per-cell, 36 boxes) would need a per-cell selector and 36 qualifications for ~40-min wall-clock;
  deferred as a variant. Egress (6) is the balance: clean merge unit, modest overhead.

## Risks

1. Cross-box variance (the real one): 36 cells no longer share a machine. Mitigation: per-shard
   `box_qualify` in-band + recorded; optionally tighten the qualify band for sharded runs.
2. Merge correctness: disjoint-key assertion + invariant checks fail LOUD; unit-tested (below).
3. Incomplete shard set publishing a partial row: refuse-to-publish guard (Component 4).
4. Schema bump: coordinate site/audit read of per-`Upstream` provenance.

## Verification (RED → GREEN)

- `run.rs` egress decoupling: unit test — `RunConfig{ dialects: ALL, egress_dialects: Some([openai]) }`
  yields a cell list of 6 (all ingress × openai) NOT 1 and NOT 36. RED before the loop-source swap.
- `merge_snapshots`: unit tests — (a) two disjoint single-egress shards merge to a 2-column snapshot;
  (b) overlapping egress keys ⇒ Err; (c) differing `gateway`/`build`/`arch`/`config`/`rig.engine` ⇒ Err;
  (d) merged snapshot's `served_cell_keys` = union. RED before the merger exists.
- End-to-end (cheap): `OTB_DIALECTS` small grid + `OTB_EGRESS` subset on the mock, assert the partial
  snapshot has only the selected egress upstream(s) with full ingress cells.
- Field parity: one gateway measured single-box vs egress-sharded on the same pin; assert per-cell
  frontier RPS within run-to-run noise (±3%). This is the acceptance gate before adopting for the board.

## Rollout

1. Land Components 1–3 + unit tests (engine PR; ENGINE_PIN bump — additive/measurement-neutral, so the
   owner may treat existing single-box rows as comparable, but the parity test above is the proof).
2. Land Component 4 behind `SHARD_BY` (default off ⇒ current single-box path untouched).
3. Parity-test on one gateway; if within ±3%, adopt sharding for busbar (and any slow gateway) on 1.6.0.
