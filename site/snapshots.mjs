// Scoped re-run layering. Pure functions, in their own module so they can be tested directly.
/* SCOPED RE-RUNS: a run narrowed with OTB_DIALECTS walks a SUBSET of the 6x6 grid. Recency alone would
   let those few cells replace a full run and delete the rest from the board, so instead: when a newer
   snapshot's cell set is a STRICT SUBSET of an older one's, it LAYERS over that run rather than
   replacing it - its cells win (newer, really taken), the rest keep the older run's numbers, and each
   layered cell is stamped with the run that actually produced it.
   Scope is derived from the snapshot itself, not an env var: a cell that genuinely stopped being served
   still appears with `served: "not_configurable"`; a cell ABSENT ENTIRELY was simply never probed. */
export function snapshotCellCoords(m) {
  const out = new Set();
  if (!m || typeof m !== "object") return out;
  for (const [egress, up] of Object.entries(m.upstreams || {}))
    for (const ingress of Object.keys((up && up.cells) || {})) out.add(`${egress}|${ingress}`);
  /* v1-shaped matrices carry cells under a bare `cells` map with no `upstreams`; without this fallback
     that reads as an empty set (a strict subset of everything), silently dropping the newest run. The
     `v1` sentinel keeps a v1 snapshot from ever looking like a subset of a v2 one, since the two shapes
     can't be aligned cell-for-cell (egress is unknown in v1). */
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
      // Stamp the cell with the run that actually produced it, so it's never dated by its neighbours'.
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
  // Append, never overwrite: `resolvedSnapshot` layers multiple scoped runs in turn, so rebuilding
  // `__layered` from scratch each pass would disclose only the most recent run's provenance.
  const prior = Array.isArray(base.__layered?.runs) ? base.__layered.runs : [];
  merged.__layered = {
    from: stamp,
    cells: layered.sort(),
    runs: [...prior, { from: stamp, cells: layered.sort() }],
  };
  return merged;
}
