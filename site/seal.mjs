// SPDX-License-Identifier: Apache-2.0
// seal.mjs: the data-honesty ENVELOPE — the single point where "is this number honest to show?"
// is decided, at PROJECTION time (gen-data.mjs), never at read time. Every metric the board
// consumes is sealed here into EITHER a certified envelope OR an explicit suppressed envelope; the
// raw scalar AND its `_mock_bound` flag are CONSUMED at seal time and are NEVER re-emitted into the
// projected bundle. There is therefore no ungated raw field for any render site to leak (Design E,
// invariant P1). app.js reads envelopes through metric(); charts.py mirrors metric() + SWEEP_CAPTION.
//
// Envelope shapes (Design E §2.1):
//   CERTIFIED   { value: N,    certified: true,  suppressed: false, source, ...extras }
//   MEASURED-0  { value: 0,    certified: true,  suppressed: false, source, note: "no_qualifying_ceiling" }
//   SUPPRESSED  { value: null, certified: false, suppressed: true,  reason: "mock_bound"|"unverifiable", source }
//   NOT-MEASURED{ value: null, certified: false, suppressed: false, reason: "not_measured", source }
//
// GATED metrics (throughput; the mock-bound honesty flag applies): rps_sustained_20ms, rps_max_proxy,
// streams_sustained, cpu_fps, and the translation sustained RPS. A gated metric is CERTIFIED only when
// value is present AND (value === 0 [measured-zero, honest] OR (value > 0 AND flag === false)). A
// positive value whose flag !== false is SUPPRESSED (reason mock_bound when flag === true, unverifiable
// when flag == null). UNGATED metrics (latency, ttft, gap, rss, …) are always certified when present.

// ---- provenance stamp + caption vocabulary ---------------------------------
// The `sweep` token names WHICH projection of the run the datum is; every caption renders FROM it via
// SWEEP_CAPTION (Design E §3.2). No caption string literal may hard-code a source token — the lint in
// check-consistency enforces that (invariant C3). The live deferred fallbacks (perf/xlate/stream suite)
// are sealed honestly with their OWN sweep token until the field run folds them into the matrix.
export const SWEEP = {
  DIAGONAL: "6x6-diagonal",
  TRANSLATION: "6x6-translation",
  MEMORY: "6x6-memory-window",
  STREAM_DIAGONAL: "6x6-stream-diagonal",
  STREAM_TRANSLATION: "6x6-stream-translation",
  PERF_SUITE: "perf-suite",
  XLATE_SUITE: "xlate-suite",
  STREAM_SUITE: "stream-suite",
};

// ---- the ONE metric-field vocabulary ---------------------------------------
// gen-data seals these, check-consistency asserts these. Both IMPORT from here, so a producer field
// added on one side can never ship unsealed because the other side's whitelist lagged (audit #11).
// GATED: the throughput-shaped metrics the mock-bound honesty rule applies to.
export const GATED_FIELDS = ["rps_sustained_20ms", "rps_max_proxy", "streams_sustained", "cpu_fps"];
// UNGATED, latency-shaped, on a perf cell.
export const UNGATED_LAT_FIELDS = ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"];
// UNGATED, latency/rate-shaped, on a stream record.
export const UNGATED_STREAM_FIELDS = ["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us"];
// RSS metrics are sealed BY DISCOVERY, not by a whitelist: ANY RSS-in-MiB field the producer emits is a
// metric and must be sealed — idle/peak/recovered today, plus the qualified variants the producer has
// already added (peak_rss_HWM_mib, post_load_rss_mib). The pattern deliberately allows a qualifier
// between `rss` and `mib`, because a whitelist (and an over-narrow `_rss_mib$`) is exactly how
// peak_rss_hwm_mib came to ship as a BARE unsealed scalar (audit #11).
export const RSS_FIELD_RE = /_rss_(?:[a-z0-9]+_)*mib$/;
// isMetricField(k): is this key a sealed-envelope metric field? The single predicate both gen-data
// (what to seal) and check-consistency (what must BE an envelope) use.
export function isMetricField(k) {
  return GATED_FIELDS.includes(k) || UNGATED_LAT_FIELDS.includes(k) ||
    UNGATED_STREAM_FIELDS.includes(k) || k === "streams_sustained_fps" || RSS_FIELD_RE.test(k);
}

// makeSource: the provenance stamp carried by every cell + every envelope. `kind` is the coarse
// origin ("matrix" for the single end-state path, or a "*-fallback" for a live deferred suite);
// `sweep` is the caption key. build + measured_at come from the run.
export function makeSource(kind, sweep, build, measuredAt) {
  return { kind, sweep, build: build ?? null, measured_at: measuredAt ?? null };
}

// ---- the seal ---------------------------------------------------------------
// sealMetric: raw scalar + its mock-bound flag (for gated metrics) -> a sealed envelope. The flag and
// the raw scalar do not survive onto the returned object except as `value` (present only when honest).
//   value      : the raw number (or null/undefined when not measured)
//   opts.gated : true for a throughput metric (apply the mock-bound honesty rule)
//   opts.flag  : the raw *_mock_bound sibling (false = certified, true = rig-bound, null/undefined = unverifiable)
//   opts.source: the provenance stamp (makeSource)
//   opts.extras: extra CERTIFIED-only fields to carry (concurrency, sweep array) — dropped when suppressed
// The envelope is LEAN: it does NOT repeat the provenance stamp (the CELL carries `source`, which is
// authoritative and drives every caption). Keeping the stamp off each envelope avoids ~10x bundle bloat
// across the 36-cell matrix while preserving invariant P1 (no raw scalar / no _mock_bound survives).
// opts.zeroNote: for a GATED metric, WHAT a measured 0 MEANS. A 0 is ALWAYS an honest MEASURED value
// (the harness ran and the answer was zero) and is ALWAYS certified — it is NEVER folded into
// "not measured", which is exclusively `value == null` (audit #3). The note names the meaning so each
// surface can render the two apart:
//   ZERO_NO_CEILING  (RPS ceilings)      — served, but no tested load held the qualifying gates.
//   ZERO_MEASURED_FAIL (streaming counts) — the gateway was offered stream load and sustained NONE.
// Publishing a measured stream-sustain FAILURE as "not measured (rig-limited)" flattered the gateway;
// null (absent field) is the ONLY not-measured state.
export const ZERO_NO_CEILING = "no_qualifying_ceiling";
export const ZERO_MEASURED_FAIL = "measured_failure";
export function sealMetric(value, opts = {}) {
  const { gated = false, flag, extras = null, zeroNote = ZERO_NO_CEILING } = opts;
  if (value == null) {
    return { value: null, certified: false, suppressed: false, reason: "not_measured" };
  }
  const num = Number(value);
  if (gated) {
    // measured-zero: an honest, certified 0 whose NOTE names what the zero means (see zeroNote above).
    if (num === 0) return { value: 0, certified: true, suppressed: false, note: zeroNote };
    // positive but not certified as gateway-vs-ceiling -> SUPPRESSED, raw number consumed (gone).
    if (!(num > 0 && flag === false)) {
      const reason = flag === true ? "mock_bound" : "unverifiable";
      return { value: null, certified: false, suppressed: true, reason };
    }
  }
  const env = { value: num, certified: true, suppressed: false };
  if (extras) for (const [k, v] of Object.entries(extras)) if (v != null) env[k] = v;
  return env;
}

// isEnvelope: a sealed metric is an object carrying a `certified` boolean (never a bare scalar).
export function isEnvelope(x) {
  return x != null && typeof x === "object" && typeof x.certified === "boolean";
}
