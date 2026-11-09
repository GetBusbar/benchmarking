// Publication corrections are scoped to exact raw-file hashes. Raw evidence is never edited.
export function applyRunReview(snapshot, review) {
  if (!review) return snapshot;
  if (review.gateway !== snapshot.gateway || !review.reason || !Array.isArray(review.excluded_ingress))
    throw new Error('Invalid run review');
  const result = structuredClone(snapshot);
  const withhold = (cells) => {
    for (const [ingress, cell] of Object.entries(cells || {})) {
      if (!review.excluded_ingress.includes(ingress)) continue;
      cells[ingress] = { ...cell, served: 'untestable', reason: 'harness_error',
        probe_note: review.reason, verdict_note: review.reason,
        publication_review: { original_state: cell.served, reviewed_at: review.reviewed_at, evidence: review.evidence },
        perf: null, stream: null, memory: null };
    }
  };
  for (const upstream of Object.values(result.matrix?.upstreams || {})) withhold(upstream.cells);
  withhold(result.matrix?.cells);
  // Clarify only an already-recorded absence. A review cannot invent a value,
  // erase a failure, or discard the rest of a valid cell through this path.
  for (const note of review.absence_notes || []) {
    const cell = result.matrix?.upstreams?.[note.egress]?.cells?.[note.ingress]
      || result.matrix?.cells?.[note.ingress];
    if (!cell || !note.detail || !Array.isArray(note.paths) || !note.paths.length)
      throw new Error('Invalid absence review');
    for (const path of note.paths) {
      if (!cell.absences?.[path]) throw new Error(`Review targets no recorded absence: ${path}`);
      cell.absences[path] = { ...cell.absences[path], detail: note.detail };
    }
    cell.publication_review = { original_state: cell.served,
      reviewed_at: review.reviewed_at, evidence: review.evidence };
  }
  return result;
}
