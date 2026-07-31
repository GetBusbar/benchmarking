// SPDX-License-Identifier: Apache-2.0
// seal.mjs: the data-honesty ENVELOPE - the single point where a measurement becomes a published
// datum, at PROJECTION time (gen-data.mjs), never at read time. Every metric the board consumes is
// sealed here, and the raw scalar is CONSUMED at seal time so there is no ungated raw field for any
// render site to leak (Design E, invariant P1). app.js reads envelopes through metric(); charts.py
// mirrors metric() + SWEEP_CAPTION.
//
// Envelope shapes:
//   CERTIFIED   { value: N,    certified: true,  suppressed: false, headroom?, rig_ceiling?, ...extras }
//   MEASURED-0  { value: 0,    certified: true,  suppressed: false, note: "no_qualifying_ceiling" }
//   NOT-MEASURED{ value: null, certified: false, suppressed: false, reason: <engine absence token>, detail? }
//
// A PRESENT NUMBER IS ALWAYS PUBLISHED. There used to be a fourth shape - SUPPRESSED, `{value: null,
// certified: false, suppressed: true, reason: "mock_bound"|"unverifiable"}` - and a whole GATED/PACED
// vocabulary feeding it. A "gated" metric was certified only when the engine's `*_mock_bound` flag
// proved the number was not our own rig's ceiling; a positive value whose flag was `true` (rig-bound)
// or `null` (unmeasurable reference) had its value replaced with null.
//
// That withheld correct measurements. The engine reached the flag by comparing the observation against
// a rig reference and applying a chosen fraction, and it fired hardest on the gateways that did best:
// on the 2026-07-28 board a gateway keeping pace with the paced mock to within 0.7% published nothing
// at all. The number was right; only its INTERPRETATION was open, and deleting the number is not a way
// to resolve that.
//
// So the engine no longer produces a verdict, and this no longer consumes one. It produces the two
// facts the verdict was derived from - the ceiling the measurement was taken against and the fraction
// of it reached - and they ride ON the certified envelope as `rig_ceiling` and `headroom`. A reader
// sees `43297 frames/sec, 83% of the mock's own 52013 ceiling` and draws their own conclusion. Nothing
// in this file chooses a threshold, because nothing in this file reaches a conclusion.
//
// `suppressed: false` is still emitted on every envelope. It is load-bearing for consumers and for
// invariant C2, which asserts no envelope in a published bundle carries `suppressed: true` - the guard
// that the retired machinery has not come back.
//
// The NOT-MEASURED reason is not a hardcoded literal: the engine publishes WHY a field is absent
// (measurement.rs `Absent` - not_measured, below_resolution, rig_limited, untestable, search_exhausted,
// harness_error, not_served) in the cell's sibling `absences` map, and the seal CARRIES that reason plus
// its prose detail instead of flattening every hole to one token. In particular "below_resolution" is a
// difference that came out at or below what the rig can resolve - the best result the comparison can
// express - and every surface renders it apart from a never-measured hole.

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
//
// THROUGHPUT_FIELDS was `GATED_FIELDS`, and the rename is the point: this is a VOCABULARY (which keys
// are metrics that must be envelopes), never again a gate (which values are allowed to show). It is the
// set of throughput-shaped metrics, and it is the set that carries `headroom` when a rig ceiling was
// available for the comparison.
export const THROUGHPUT_FIELDS = ["streams_sustained", "streams_sustained_fps"];

// THE FRONTIER'S DECLARED TAIL-LATENCY BOUNDS, in milliseconds, mirroring the engine's
// `frontier::P99_BOUNDS_US`. Ascending, with the unbounded reading rendered separately.
//
// A MIRROR IS A SECOND SOURCE OF TRUTH, so this one is CHECKED rather than trusted: the bundle carries
// each reading's own `p99_bound_us`, `check-consistency` compares this list against what the raw
// artifacts actually contain, and a divergence is a violation rather than a silently short table. The
// alternative - deriving the columns from whatever the first gateway happens to publish - would let a
// gateway missing a bound quietly shrink the board for everyone.
export const FRONTIER_BOUNDS_MS = [1, 5, 10, 50, 100];

// WHICH BOUND THE BOARD SHOWS BEFORE THE READER PICKS ONE.
//
// It has to default to something to render a table, and this is the honest way to choose: 10 ms sits at
// the middle of where the field population actually separates (of 1632 recorded rungs, 16% hold 1 ms,
// 47% hold 10 ms, 88% hold 100 ms, and 96% hold 1 s - so a looser default would put nearly every gateway
// on the same side of it and discriminate between almost nothing).
//
// IT IS A VIEW, NOT A VERDICT, and that is the whole difference from the constant it replaces.
// `SUSTAINED_P99_CEILING_US` decided which measurements existed; this decides which column opens first.
// Every bound is published on every cell, the UI names the one it is showing, and switching it re-ranks
// the board in front of the reader.
export const DEFAULT_BOUND_MS = 10;

// What `sealMetric` will publish for a raw value, as a pure function of the same inputs.
//
// THE ONE PLACE THE DISPLAY RULE LIVES. `sealMetric` builds a whole envelope (notes, headroom, absence
// reasons); this answers only "does the number show, and as what", which is the part an independent
// oracle needs and the part that must never be written twice.
//
// It used to take a `flag` and `{gated, paced}` and return null for a measured number whose flag was
// not `false`. It now has exactly one branch, because a measured number always shows.
export function displayedValue(raw, { absentReason = null } = {}) {
  // A below-resolution absence DISPLAYS as 0: the comparison ran and the difference was too small
  // for the rig to weigh, which ranks equal-best and renders as "≈0", never as a hole. Every other
  // absence displays as nothing.
  if (raw == null) return absentReason === "below_resolution" ? 0 : null;
  return raw;
}
// UNGATED, latency-shaped, on a perf cell.
export const UNGATED_LAT_FIELDS = ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"];
// UNGATED, cost-shaped, on a perf record. WHAT THE CELL COST rather than what it delivered - the
// answer that does not stop describing the gateway once it saturates its cores. Listed explicitly
// (rather than discovered by pattern) because these are six unrelated shapes - a duration, a rate, a
// concurrency, two counts and a fault total - with no common suffix a regex could key on safely.
// Leaving any of them out would let it ship as a bare unsealed scalar: the peak_rss_hwm_mib bug again.
export const UNGATED_COST_FIELDS = [
  "cpu_us_per_request", "rps_per_cpu_second", "cost_window_conc", "cost_core_utilisation",
  "cost_threads", "cost_nonvol_ctxt_per_request", "cost_majflt",
];
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
  return THROUGHPUT_FIELDS.includes(k) || UNGATED_LAT_FIELDS.includes(k) ||
    UNGATED_COST_FIELDS.includes(k) ||
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
// sealMetric: a raw scalar -> a sealed envelope. The raw scalar does not survive onto the returned
// object except as `value`.
//   value        : the raw number (or null/undefined when not measured)
//   opts.source  : the provenance stamp (makeSource)
//   opts.extras  : extra CERTIFIED-only fields to carry (concurrency, sweep array)
//   opts.headroom: the fraction of the rig's own ceiling this measurement reached, from the engine's
//                  `*_headroom`. A FACT ABOUT THE COMPARISON, carried so a reader can weigh a number
//                  that sat near our equipment's limit. Omitted when the engine had no usable ceiling -
//                  which costs the fraction and NOT the value. It used to cost both.
//   opts.ceiling : the ceiling that fraction is of, from `*_rig_ceiling` / `*_mock_ceiling`, so the
//                  headroom is checkable rather than asserted. For the stream metrics this is DERIVED
//                  from the mock's declared pacing (run::mock_frame_ceiling_fps), not measured.
// The envelope is LEAN: it does NOT repeat the provenance stamp (the CELL carries `source`, which is
// authoritative and drives every caption). Keeping the stamp off each envelope avoids ~10x bundle bloat
// across the 36-cell matrix while preserving invariant P1 (no raw scalar survives).
//
// opts.zeroNote: WHAT a measured 0 MEANS. A 0 is ALWAYS an honest MEASURED value (the harness ran and
// the answer was zero) and is ALWAYS certified - it is NEVER folded into "not measured", which is
// exclusively `value == null` (audit #3). The note names the meaning so each surface can render the two
// apart:
//   ZERO_NO_CEILING    (RPS ceilings)     - served, but no tested load held the qualifying gates.
//   ZERO_MEASURED_FAIL (streaming counts) - the gateway was offered stream load and sustained NONE.
// Publishing a measured stream-sustain FAILURE as "not measured" would flatter the gateway; null (an
// absent field) is the ONLY not-measured state.
export const ZERO_NO_CEILING = "no_qualifying_ceiling";
export const ZERO_MEASURED_FAIL = "measured_failure";
// WHICH ZERO-NOTE A FIELD TAKES, as data rather than as a literal repeated at each call site.
//
// The note is the whole difference between "served, but no offered load held the gates" and "offered
// stream load and sustained none of it", and gen-data used to pick it by hand per call while the
// independent oracle in check-consistency knew nothing about it at all - so a swapped note published a
// measured streaming failure as a missing RPS ceiling and verified green. One list, imported by both.
// The streaming counts are the measured-failure family; every other throughput metric is an RPS ceiling.
// `cpu_fps` is gone from this list because the metric is retired - see the engine's `run.rs`. A field
// name left in a vocabulary after the field stops existing is not harmless: `zeroNoteFor` would keep
// answering ZERO_MEASURED_FAIL for it, so a future field that happened to reuse the name would silently
// inherit a "the gateway was offered load and sustained none of it" annotation it never earned.
export const ZERO_FAIL_FIELDS = ["streams_sustained", "streams_sustained_fps"];
// NULL FOR A FIELD WITH NO ZERO-NOTE VOCABULARY, which is most of them. The two tokens above are claims
// about a THROUGHPUT measurement - "no tested load held the qualifying gates", "offered stream load and
// sustained none" - and neither is a true sentence about anything else.
//
// This used to fall through to ZERO_NO_CEILING for every field it did not recognise. That was harmless
// only because the caller applied it under a `gated` flag; when the gate was removed the fallthrough
// became live, and 37 `growth_rate_mib_per_min` zeros and 6 `added_gap_p50_us` zeros shipped annotated
// "served, but no tested load held p99 < 1 s at <0.1% errors" - a fabricated claim about a memory growth
// rate and an inter-frame gap. Neither surface could tell, and the independent oracle called
// `zeroNoteFor` too, so it expected the same wrong note and the bundle verified green.
export function zeroNoteFor(field) {
  if (ZERO_FAIL_FIELDS.includes(field)) return ZERO_MEASURED_FAIL;
  return THROUGHPUT_FIELDS.includes(field) ? ZERO_NO_CEILING : null;
}
// HEADROOM: the field the "how close to our own rig's ceiling did this come" fact rides on, and the
// ceiling it is a fraction of.
//
// These replace `PACED_MATCH`, which was a boolean re-statement of the engine's retired `*_mock_bound`
// flag ("this number matched the paced upstream"). A boolean was all that could be carried while the
// engine only published a verdict; now that it publishes the ceiling and the ratio, the same
// information is available as a number a reader can actually weigh. 0.993 and 0.20 were both
// `paced_match: undefined` before.
export const HEADROOM = "headroom";
export const RIG_CEILING = "rig_ceiling";
//   opts.absent: the engine's `absences` entry for this field ({reason, detail}), when the caller has
//                one. An absent value then publishes the ENGINE'S reason and its prose detail instead
//                of the flattened "not_measured" - the reason was measured too, and discarding it here
//                was how "below rig resolution" (a win) rendered identically to "never ran" (a hole).
export function sealMetric(value, opts = {}) {
  const { extras = null, zeroNote = null, absent = null, headroom = null, ceiling = null } = opts;
  // The extras (concurrency, the rung, the sweep array) attach to EVERY certified envelope, including a
  // certified 0. They used to attach only on the last line, which the measured-zero branch returned
  // before ever reaching - so a certified 0 published without the concurrency it was measured at and
  // without the sweep array that is the evidence FOR the zero: the one reading whose curve a reader
  // most needs, since "0" beside a real maximum is the claim that most demands its evidence.
  const withExtras = (env) => {
    if (extras) for (const [k, v] of Object.entries(extras)) if (v != null) env[k] = v;
    // The comparison's own facts, on every certified envelope that has them - including a certified 0,
    // for the same reason the extras are.
    if (Number.isFinite(headroom)) env[HEADROOM] = headroom;
    if (Number.isFinite(ceiling)) env[RIG_CEILING] = ceiling;
    return env;
  };
  if (value == null) {
    const env = { value: null, certified: false, suppressed: false, reason: (absent && absent.reason) || "not_measured" };
    if (absent && absent.detail) env.detail = absent.detail;
    return env;
  }
  const num = Number(value);
  // A measured zero is honest and certified, and its NOTE names what the zero means (see zeroNote) - but
  // ONLY for the fields that have such a meaning. An unannotated zero stays a bare 0 rather than
  // borrowing a throughput sentence.
  if (num === 0) {
    const env = { value: 0, certified: true, suppressed: false };
    if (zeroNote != null) env.note = zeroNote;
    return withExtras(env);
  }
  return withExtras({ value: num, certified: true, suppressed: false });
}

// sealFrontier: the engine's frontier -> one sealed reading per bound.
//
// Each reading's RATE is a sealed envelope for the same reason every other published number is: absent
// means absent, with the engine's own reason, and no surface can reach a raw scalar. The rest of the
// reading is EVIDENCE ABOUT that rate - the concurrency it was observed at, the tail it actually came
// with, the concurrency above it that stopped qualifying - and rides as plain fields, exactly as
// `source` and `reverify_note` do.
//
// `bound_ms` is derived from the engine's `p99_bound_us` rather than from `FRONTIER_BOUNDS_MS`, so a
// reading always reports the bound IT was taken under even if the mirror above ever drifts.
// `lower_bound` travels because a rate the sweep never found a ceiling for is a floor, and a surface
// that renders it as a ceiling is making a claim the data does not support.
//
// THE ABSENCE KEY IS BLOCK-PREFIXED, and getting that wrong silently destroyed the distinction this
// whole metric exists to preserve. `CellPerf::absences()` keys the frontier as
// `perf.frontier.10ms.rps`; this looked up `frontier.10ms.rps`, found nothing, and every absent reading
// fell back to a bare `not_measured` with no detail.
//
// The damage was not cosmetic. one-api genuinely cannot serve under a 10 ms tail - its own rungs never
// got below 34 ms - and the engine says exactly that, with `below_resolution` and the prose "every
// cleanly-served rung had a tail latency at or above 10ms". Flattened to `not_measured`, no surface
// could tell "measured, and it cannot do this" from "nothing was measured here", and the neutral
// reading FLATTERS the gateway. Both prefixed and bare keys are accepted, matching
// `gen-data.mjs::absentEntryFor`, because a projected record carries the bare form.
export function sealFrontier(readings, absences = null) {
  if (!Array.isArray(readings)) return [];
  return readings.map((r) => {
    const us = Number.isFinite(r.p99_bound_us) ? r.p99_bound_us : null;
    const at = us == null ? "unbounded" : `${Math.round(us / 1000)}ms`;
    const key = `frontier.${at}.rps`;
    const abs = absences ? absences[`perf.${key}`] || absences[key] || null : null;
    return {
      bound_ms: us == null ? null : Math.round(us / 1000),
      rps: sealMetric(r.rps, { absent: abs }),
      concurrency: numOrNull(r.concurrency),
      p99_us: numOrNull(r.p99_us),
      first_disqualified_conc: numOrNull(r.first_disqualified_conc),
      lower_bound: r.lower_bound === true,
    };
  });
}
// The engine writes an absent number as `null`; anything else is a number or is not publishable.
function numOrNull(v) {
  return Number.isFinite(v) ? v : null;
}

// THE READING THE BOARD IS CURRENTLY SHOWING, by bound. `null` selects the unbounded reading.
//
// One accessor, so every surface - table, drawer, compare, charts - reads the same reading for the same
// bound and cannot disagree about which column it is displaying.
export function frontierAt(frontier, boundMs) {
  if (!Array.isArray(frontier)) return null;
  return frontier.find((r) => (boundMs == null ? r.bound_ms == null : r.bound_ms === boundMs)) || null;
}

// isEnvelope: a sealed metric is an object carrying a `certified` boolean (never a bare scalar).
export function isEnvelope(x) {
  return x != null && typeof x === "object" && typeof x.certified === "boolean";
}
