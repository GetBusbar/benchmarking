// SPDX-License-Identifier: Apache-2.0
// seal.mjs: the data-honesty ENVELOPE - the single point where "is this number honest to show?"
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
// SWEEP_CAPTION (Design E §3.2). No caption string literal may hard-code a source token - the lint in
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

// THE GATED FIELDS WHOSE MOCK REFERENCE IS A PACED TARGET, NOT A CAPACITY.
//
// The mock paces SSE deltas at a fixed interval, so its frames/sec is `concurrency * (1000/interval)`
// - a number it was TOLD to produce, not a ceiling it ran into. A gateway that matches it has kept up
// with the pace, which is the gateway succeeding, and suppressing that as "mock-bound" would hide the
// best result the test can express. `sealMetric` has known this since paced metrics existed.
//
// It lives here, exported, because `check-consistency.mjs` re-derives what SHOULD be displayed as an
// independent oracle, and an oracle that restates the rule from memory drifts from it. That is not
// hypothetical: the oracle implemented only the non-paced branch, so every cell where the flag was
// true had the seal correctly publish and the oracle demand null. Twenty-five of those mismatches
// blocked every deploy for two days, on three field names. One list, imported by both.
export const PACED_FIELDS = ["streams_sustained", "streams_sustained_fps", "cpu_fps"];

// What `sealMetric` will publish for a raw value, as a pure function of the same inputs.
//
// THE ONE PLACE THE DISPLAY RULE LIVES. `sealMetric` builds a whole envelope (notes, provenance,
// zero-reasons); this answers only "does the number show, and as what", which is the part an
// independent oracle needs and the part that must never be written twice.
export function displayedValue(raw, flag, { gated = false, paced = false } = {}) {
  if (raw == null) return null;
  if (!gated) return raw;
  if (raw === 0) return 0;                      // a measured zero is honest and always shown
  // Paced: the flag existing at all means the comparison against the mock was made. Capacity: the
  // number only stands if it was PROVEN not to be the mock's own ceiling.
  if (paced) return flag == null ? null : raw;
  return (raw > 0 && flag === false) ? raw : null;
}
// UNGATED, latency-shaped, on a perf cell.
export const UNGATED_LAT_FIELDS = ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"];
// UNGATED, latency/rate-shaped, on a stream record.
export const UNGATED_STREAM_FIELDS = ["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us"];
// RSS metrics are sealed BY DISCOVERY, not by a whitelist: ANY RSS-in-MiB field the producer emits is a
// metric and must be sealed, idle/peak/recovered today, plus the qualified variants the producer has
// already added (peak_rss_HWM_mib, post_load_rss_mib). The pattern deliberately allows a qualifier
// between `rss` and `mib` (e.g. peak_rss_hwm_mib) so a narrower regex or a fixed whitelist can't again
// let a differently-named RSS field ship as a bare unsealed scalar.
export const RSS_FIELD_RE = /_rss_(?:[a-z0-9]+_)*mib$/;
// UNGATED, memory-shaped, on a per-cell memory window. These are NOT RSS values (so RSS_FIELD_RE cannot
// discover them) but they ARE published numbers: the growth rate is the leak rate when a gateway never
// reached a steady state, i.e. the most load-bearing number the memory metric produces. Leaving them out
// of the vocabulary would let them ship as bare unsealed scalars: the peak_rss_hwm_mib bug again.
export const UNGATED_MEM_FIELDS = ["growth_rate_mib_per_min", "time_to_plateau_s"];
// isMetricField(k): is this key a sealed-envelope metric field? The single predicate both gen-data
// (what to seal) and check-consistency (what must BE an envelope) use.
export function isMetricField(k) {
  return GATED_FIELDS.includes(k) || UNGATED_LAT_FIELDS.includes(k) ||
    UNGATED_STREAM_FIELDS.includes(k) || UNGATED_MEM_FIELDS.includes(k) ||
    k === "streams_sustained_fps" || RSS_FIELD_RE.test(k);
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
//   opts.paced : true when the mock's reference is a PACED TARGET rather than a capacity (the stream
//                metrics). Matching it is the gateway keeping up, so a `true` flag is carried as a
//                signal instead of suppressing the value. An absent flag still suppresses.
//   opts.flag  : the raw *_mock_bound sibling (false = certified, true = rig-bound, null/undefined = unverifiable)
//   opts.source: the provenance stamp (makeSource)
//   opts.extras: extra CERTIFIED-only fields to carry (concurrency, sweep array) - dropped when suppressed
// The envelope is LEAN: it does NOT repeat the provenance stamp (the CELL carries `source`, which is
// authoritative and drives every caption). Keeping the stamp off each envelope avoids ~10x bundle bloat
// across the 36-cell matrix while preserving invariant P1 (no raw scalar / no _mock_bound survives).
// opts.zeroNote: for a GATED metric, WHAT a measured 0 MEANS. A 0 is ALWAYS an honest MEASURED value
// (the harness ran and the answer was zero) and is ALWAYS certified - it is NEVER folded into
// "not measured", which is exclusively `value == null` (audit #3). The note names the meaning so each
// surface can render the two apart:
//   ZERO_NO_CEILING  (RPS ceilings) - served, but no tested load held the qualifying gates.
//   ZERO_MEASURED_FAIL (streaming counts) - the gateway was offered stream load and sustained NONE.
// Publishing a measured stream-sustain FAILURE as "not measured (rig-limited)" would flatter the
// gateway; null (absent field) is the ONLY not-measured state.
export const ZERO_NO_CEILING = "no_qualifying_ceiling";
export const ZERO_MEASURED_FAIL = "measured_failure";
export function sealMetric(value, opts = {}) {
  const { gated = false, paced = false, flag, extras = null, zeroNote = ZERO_NO_CEILING } = opts;
  if (value == null) {
    return { value: null, certified: false, suppressed: false, reason: "not_measured" };
  }
  const num = Number(value);
  if (gated) {
    // measured-zero: an honest, certified 0 whose NOTE names what the zero means (see zeroNote above).
    if (num === 0) return { value: 0, certified: true, suppressed: false, note: zeroNote };
    // A PACED metric's flag is not a suppression. The mock paces its stream deltas at a fixed
    // interval - "the pacing is the model generating tokens", in the mock's own words - so its
    // frames/sec is a TARGET rate, c streams x one frame per interval, and not a capacity it ran out
    // of. A gateway forwarding every frame as it arrives lands at ~99% of it, which is the best
    // possible outcome, and suppressing that threw the number away for the gateways doing best: 24
    // of 69 cells in the 2026-07-28 run. The flag stays on the envelope as the signal it always was
    // (this gateway matched the paced upstream), and the value is published beside it.
    //
    // A metric that is gated but NOT paced keeps the old rule exactly: for a saturating throughput
    // load the mock's capacity really can be the limit, and publishing then ranks the rig.
    // `unverifiable` still suppresses on both paths - an unmeasurable reference tells us nothing
    // either way, and certifying a number on no evidence is what the whole gate exists to prevent.
    // Routed through `displayedValue` rather than restating the condition, so this branch and the
    // oracle in check-consistency.mjs cannot disagree about what shows: they are now the same code.
    if (displayedValue(num, flag, { gated, paced }) == null) {
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
