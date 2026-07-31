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
  merged.__layered = { from: stamp, cells: layered.sort() };
  return merged;
}
