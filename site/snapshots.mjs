// Scoped re-run layering. Pure functions, in their own module so they can be tested directly:
// gen-data.mjs is a script, and this logic decides whether 32 measured cells survive a 4-cell re-run.
/* ---- SCOPED RE-RUNS ------------------------------------------------------------------------------

   A re-run narrowed with OTB_DIALECTS walks a SUBSET of the 6x6 grid, so its snapshot carries only the
   upstreams it was told to walk. Recency alone would let those 4 cells replace a 36-cell run and delete
   32 measured cells from the board - the same failure the degraded-mode guard above exists to stop, and
   for the same reason: the producer was TOLD NOT TO MEASURE the rest, so its silence about them is not
   a finding.

   This is NOT the "a re-run that finds less IS the new truth" case, and the difference is visible in the
   data rather than asserted. A gateway that genuinely stops serving a pairing still emits that cell,
   with `served: "not_configurable"` and a reason - all 36 are present on every full run. A cell that is
   ABSENT ENTIRELY was never probed. So the scope is derivable from the snapshot itself and needs no
   env-var provenance to be trusted.

   The rule: when a newer snapshot's cell set is a STRICT SUBSET of an older one's, it does not replace
   that run - it layers over it. Cells it measured win outright (they are newer and were really taken);
   cells it never looked at keep the older run's numbers. Every layered cell is stamped with the run
   that actually produced it, so the board never claims a spliced cell was measured when its neighbours
   were. */
export function snapshotCellCoords(m) {
  const out = new Set();
  if (!m || typeof m !== "object") return out;
  for (const [egress, up] of Object.entries(m.upstreams || {}))
    for (const ingress of Object.keys((up && up.cells) || {})) out.add(`${egress}|${ingress}`);
  /* THE v1 TOP-LEVEL ROW COUNTS TOO. A v1-shaped matrix carries its cells under a bare `cells` map
     with no `upstreams`, and this returned an EMPTY set for it - and an empty set is a strict subset
     of everything, so such a snapshot could never be the base and layered nothing either. The newest
     run would be silently deleted from the board with no warning. `normalizeMatrix` and `app.js`
     both still treat that row as real measured cells, so counting zero of them here was this
     module disagreeing with the rest of the pipeline about what a cell is.

     The egress is unknown in that shape, hence the `v1` sentinel: it makes the coords comparable
     within v1 and keeps a v1 snapshot from ever looking like a subset of a v2 one, which is the
     honest answer when the two shapes cannot be aligned cell-for-cell. */
  if (!out.size) for (const ingress of Object.keys(m.cells || {})) out.add(`v1|${ingress}`);
  return out;
}
export function isStrictSubset(a, b) {
  if (a.size >= b.size) return false;
  for (const k of a) if (!b.has(k)) return false;
  return true;
}
/* Layer `scoped` over `base`, returning a new matrix. Both are snapshot matrices. */
export function layerScopedMatrix(base, scoped, scopedSnap) {
  const merged = structuredClone(base);
  const stamp = { build: scopedSnap.build ?? null, measured_at: scopedSnap.measured_at ?? null,
                  file: scopedSnap.__file ?? null };
  const layered = [];
  for (const [egress, up] of Object.entries(scoped.upstreams || {})) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      const target = merged.upstreams?.[egress]?.cells;
      if (!target) continue;                       // an upstream the base never had: nothing to layer onto
      const c = structuredClone(cell);
      // THE CELL CARRIES THE RUN THAT PRODUCED IT. Without this the board dates a spliced cell by its
      // neighbours' run, which is exactly the provenance lie the snapshot format exists to prevent.
      c.__run = stamp;
      target[ingress] = c;
      layered.push(`${ingress}>${egress}`);
    }
  }
  // v1-compat top-level `cells` row shares refs with upstreams on a full run; rebuild it from the
  // merged grid so the two shapes cannot disagree after a layering.
  if (merged.cells && merged.upstreams) {
    for (const [ingress] of Object.entries(merged.cells)) {
      for (const up of Object.values(merged.upstreams)) {
        if (up && up.cells && up.cells[ingress]) { merged.cells[ingress] = up.cells[ingress]; break; }
      }
    }
  }
  /* APPEND, NEVER OVERWRITE. `resolvedSnapshot` layers each scoped run in turn onto the result of
     the last, and this rebuilt the record from scratch every pass - so with two scoped re-runs the
     published provenance disclosed only the most recent one, and the cells from the earlier run
     read as if they had come from the base. The per-cell `__run` stamps survived, which is what
     bounded the damage, but `__layered` is the summary a reader actually looks at. */
  const prior = Array.isArray(base.__layered?.runs) ? base.__layered.runs : [];
  merged.__layered = {
    from: stamp,
    cells: layered.sort(),
    runs: [...prior, { from: stamp, cells: layered.sort() }],
  };
  return merged;
}
