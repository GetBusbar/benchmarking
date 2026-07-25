# Per-cell measurement: cold start every cell, measure everything, aggregate nothing

Agreed with Matthew 2026-07-25. This replaces the single post-6x6 memory window that ran on each
gateway's own "peak cell".

## What was wrong

Memory was the only metric on the board reduced to one scalar per gateway. Because it was one number,
the harness had to pick a cell to produce it, and it picked each gateway's highest-throughput served
cell. Three defects followed:

1. **The cell is the workload, so the column compared different work.** busbar was measured on
   `cohere>cohere` (identity, no translation); gomodel on `openai>gemini` (full protocol translation).
   Eight distinct cells across eleven gateways with data.
2. **Select on X, report Y.** The cell was chosen by throughput and the number reported was memory.
   The selection axis has nothing to do with the reported quantity, so the result is neither a maximum
   nor a minimum of anything - just a sample of a curve at an arbitrary point.
3. **The candidate set differed per gateway.** busbar chose from 26 served cells, litellm-rust from 1.
   Breadth of capability converted directly into a wider pool of favourable lanes.

A fourth defect was found while designing the fix, and it is in the *throughput* data we already
publish: `matrix/run.sh` loops egress outer / ingress inner and launches the gateway once per egress
**column**, so one process serves all six ingress cells in fixed order. Any gateway that accumulates
state is measured progressively more degraded across a column. Since the order is fixed this is a
systematic bias, not noise. busbar demonstrably accumulates (see below), so it is not hypothetical.

## The rule this follows

- The unit of measurement is (subject, condition). Never collapse across conditions that differ in work.
- A summary must select on the same axis it reports.
- A summary is comparable only if every subject had the same candidate set.
- Unmeasurable means absent, never substituted.
- When a constant is contested and measuring everything is affordable, measure everything.
- Ranking happens within a condition.
- Test that governs all of them: could this rule have been chosen after seeing the data, to produce a
  preferred result?

## The design

Every cell gets its own cold-started process, for each of two independent windows. Nothing carries
between cells. Nothing is aggregated across cells. No cell is ever selected.

```
per (ingress, egress) cell:

  cold start -> idle sample -> fixed memory load (to plateau) -> recovery sample    [memory window]
  cold start -> fixed warm-up (N requests) -> throughput sweep                      [perf window]
```

### Why two cold starts rather than one shared process

The two measurements want opposite conditions and must not contaminate each other:

- **Load shape.** Perf is an adaptive *search* over concurrency; memory needs a *fixed* load. Sampling
  RSS during the sweep would mean a faster gateway gets driven to higher concurrency, holds more
  per-connection state, and reports more memory - throughput leaking into the memory number, which is
  defect 2 re-entering through the load.
- **Process state.** Memory needs a cold process (idle must be sampled before the first request is
  served, or it is "recovered", not "idle"). Perf needs a warm one, or the early sweep points measure
  a cold gateway.
- **Ordering.** Whichever runs first contaminates the second. Running memory first and using it as
  perf's warm-up was considered and rejected: the memory load is plateau-terminated, so its duration
  is gateway-dependent (65s to the 300s cap). Throughput would then be coupled to memory behaviour -
  fix a leak, plateau time drops, measured throughput moves, with no change to the request path.

A restart is ~10s. 72 restarts per gateway is ~12 minutes, trivial against the measurement windows,
and it buys complete independence.

Perf's warm-up becomes an **explicit fixed** warm-up (same request count for every gateway and cell).
Today it is a single readiness probe, so the sweep's early points measure a cold gateway.

### Memory load termination: plateau, not a fixed duration

A fixed 120s load decides the answer for any gateway still climbing at 120s - the number describes
when we stopped, not the gateway. Instead run until the RSS is steady.

**Plateau requires BOTH**, evaluated over a trailing window of `MEM_PLATEAU_WINDOW_S = 60`:

- mean(second half) - mean(first half) < **1%** of mean - no upward trend
- (max - min) within the window < **2%** of mean - not merely oscillating around a flat mean

The trend test is the one that matters. A naive range test passes on an asymptoting slow leak: busbar
climbs 7.1 -> 119.7 MiB over 111s, and near the tail its per-sample delta is small enough that
"max-min < 1%" would declare a plateau while memory is still genuinely rising.

Cap: `MEM_PLATEAU_CAP_S = 300`. Sample interval tightens 5s -> 2s (30 samples per window).

These are still chosen constants. What makes them different from the 120s they replace is that they
define a standard every gateway is held to identically - reach stability by this test, or be reported
as not having reached it. The stopping point no longer decides the answer; the gateway's behaviour
does. And time-to-plateau is published, so "settled in 15s" and "settled in 250s" stay distinguishable.

Concurrency (64) and payload (4096B) stay fixed. A constant that *defines the condition* is legitimate;
the workload must be identical for the comparison to mean anything. A constant that decides *when to
stop looking* is not.

### Published per cell

- `steady_state_rss_mib` - the plateau value, null if never reached
- `idle_rss_mib` - cold, before the first request
- `recovered_rss_mib`
- `plateaued` - bool
- `time_to_plateau_s`
- `growth_rate_mib_per_min` - over the final window. ~0 when plateaued. **When the cap is hit this is
  the leak rate**, and it is the most useful number the memory metric can produce. It turns "did not
  plateau" from a missing value into the headline finding.
- `rss_series`

### Not served

A cell the gateway does not serve reports memory as absent, with the reason. No substitution, no
fallback cell. This is the one branch the harness is allowed: the gateway declared it does not do this
cell, so it is reported as such.

### Display

Storage is raw and per cell. What to show is a display concern.

**Memory joins the existing cell chooser (`PERF_VIEWS`) with modes `Min | Max | Same | Custom`.**
Today the memory lane is deliberately NOT chooser-driven (`app.js:771`); that asymmetry goes.

- **Min** - this gateway's lowest steady-state RSS across the cells it serves
- **Max** - its highest
- **Same** (default, on the widest-coverage dialect) - one identity dialect applied to every gateway
- **Custom** - explicit ingress->egress
- **NO Peak.** Peak reads `best_cell`, which is selected by *throughput*. For the perf lane that is
  coherent (select on throughput, report throughput). For memory it would select on throughput and
  report memory - the original defect, re-offered behind a UI control. A shared URL arriving with
  `view=peak` on the memory lane falls back to Same.

Min and Max are legitimate here where Peak is not, because they select on memory and report memory -
genuine minima and maxima. Their candidate sets still differ per gateway (min-of-26 vs min-of-1), and
the two biases run opposite ways (Min flatters breadth, Max penalises it), so both are offered and the
row shows the cell count alongside the `Tested on` cell: "openai>gemini, of 26 served".

**The governing principle, which is why Min/Max are acceptable and the old peak-cell memory was not:**
*a selection the reader makes and can see is not the same defect as a selection the harness makes and
hides, even when the arithmetic is identical.* The old design picked each gateway's peak cell silently
and presented the result as THE memory column, so a reader comparing two rows could not know they were
not comparing the same thing. A named mode the reader chose carries its own disclosure.

Only **Same** and **Custom** are like-for-like; sorting is meaningful in all four modes but only means
"ranked comparison" in those two.

`idle` stays outside the chooser entirely: sampled cold with no cell involved, so it is valid across
all gateways in every mode. Report the median of the per-cell cold samples plus the spread.

**Never aggregate across cells into a single ranked column.** Cross-gateway ranking is defined only
within a cell (Same/Custom) or as an explicitly-chosen per-gateway extremum (Min/Max).

### Every gateway always appears; unserved reads n/a

The data tables already do this and it is deliberate - `applyFilters` (`app.js:989`) returns true
unconditionally, with the rationale in-comment: filtering a competitor out reads as hiding it, and a
gateway that does not serve the chosen cell reads n/a with null metrics sorting last.

An n/a row does work that a missing row cannot: it provokes "why is litellm-rust not showing on
openai?", and the answer (it serves 1 of 36 cells) is the single most important fact about that
gateway. A missing row provokes nothing.

Two latent holes to close:

- `renderMatrix` (`app.js:1896`) filters to gateways that HAVE a matrix. Every gateway has one today,
  so nothing is hidden, but a gateway that failed hard enough to produce no matrix would vanish from
  the protocol grid - the case where disappearing is most misleading, since total failure would render
  as absence rather than a row of red. Render an all-n/a row with the failure reason instead.
- The streaming/translation capability toggles do drop rows, and that is fine: explicit user-chosen
  "only show gateways that do X" controls, disclosed by the control itself.

### The one residual, disclosed rather than engineered around

Under the one-static-config standard every gateway is configured with all six upstreams regardless of
the cell under test, so a broad gateway boots more upstream clients than a narrow one. That difference
is not something the narrow gateway chose - there is no configuration in which it holds six protocols.
Minimal-config measurement (one upstream, the cell under test) removes the config term and is the
smallest condition every gateway can hold. What remains after that is that a gateway supporting six
protocols carries more code whatever you configure, which is the thing itself and not an artifact.

Methodology text: *no memory comparison between gateways of different capability breadth is fully
like-for-like, because capability is not a setting. We minimise the difference by measuring under the
smallest config every gateway can hold, and we publish the configs so the remainder is visible rather
than hidden.*

## Cost

- 36 extra cold starts for perf, 36 for memory: ~12 min/gateway.
- Memory windows: ~30s idle + 45-300s load + 30s recovery per served cell. Most of the field settles
  fast, so typical is ~2 min/cell; only non-plateauing gateways pay the cap, which is the right way
  round since that is where the extra time buys real information.
- Broadest gateway (busbar, 26 served cells) goes from ~3h to ~4-5h. Boxes run in parallel, so field
  wall-clock is the slowest gateway.

## Evidence that motivated this (2026-07-25 field run)

Ramp = time from first sample above idle to first sample at 95% of peak, under the old fixed 120s load
starting ~t=65s:

```
busbar          idle    7.1 -> peak  119.7   ramp 111s   still rising when the load stopped
bifrost         idle  161.9 -> peak 1023.6   ramp 126s   still rising
portkey         idle  124.4 -> peak  267.4   ramp  81s
one-api         idle   88.2 -> peak  165.1   ramp  35s
apisix          idle  178.1 -> peak  208.3   ramp  26s
litellm-python  idle 1035.7 -> peak 1086.6   ramp  25s
gomodel         idle   53.3 -> peak  112.0   ramp  15s
agentgateway    idle   23.6 -> peak   45.0   ramp  15s
tensorzero      idle   39.9 -> peak   73.8   ramp   5s
litellm-rust    idle  256.3 -> peak  263.0   ramp   5s
```

Most of the field plateaus within 5-35s - a working set, bounded by concurrency, the correct shape.
busbar and bifrost never plateau within the window, so their published peaks were duration-dependent:
a longer load would have produced a larger number. Under this design they publish
`plateaued: false` plus a leak rate instead.

(portkey's series rises from t=0, i.e. during what the protocol calls cold idle, so its idle sample is
suspect - separate harness bug, tracked independently.)

## Open, tracked separately

- litellm-rust serves 1 of 36 cells (`anthropic>anthropic` only) and not `openai>openai`. Same smell
  as aisix and helicone, both of which turned out to be harness misconfigs. Investigate.
- portkey's idle window contamination.
- busbar's non-plateau is a busbar defect, under investigation in that repo, not a harness issue.
