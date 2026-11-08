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
  return result;
}
