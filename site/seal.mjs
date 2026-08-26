// SPDX-License-Identifier: Apache-2.0
// seal.mjs: the data-honesty ENVELOPE - the single point where a measurement becomes a published datum,
// at PROJECTION time (gen-data.mjs), never at read time. The raw scalar is consumed here so no ungated
// raw field survives for a render site to leak (Design E, invariant P1). app.js reads envelopes through
// metric(); charts.py mirrors metric() + SWEEP_CAPTION.
//
// Envelope shapes:
//   CERTIFIED    { value: N,    certified: true,  suppressed: false, headroom?, rig_ceiling?, ...extras }
//   MEASURED-0   { value: 0,    certified: true,  suppressed: false, note: "no_qualifying_ceiling" }
//   NOT-MEASURED { value: null, certified: false, suppressed: false, reason: <engine absence token>, detail? }
//
// A PRESENT NUMBER IS ALWAYS PUBLISHED. There used to be a fourth SUPPRESSED shape, gated on the
// engine's `*_mock_bound` flag (was this number provably not our own rig's ceiling?) - which withheld
// correct measurements: on the 2026-07-28 board a gateway within 0.7% of the paced mock published
// nothing. The engine now publishes the ceiling and the fraction reached instead of a verdict, riding on
// the certified envelope as `rig_ceiling`/`headroom`; nothing in this file judges a threshold.
// `suppressed: false` still rides on every envelope - invariant C2 asserts no published envelope ever
// carries `suppressed: true`, i.e. that this retired machinery hasn't come back.
//
// NOT-MEASURED's `reason` is the engine's own absence token (measurement.rs `Absent`: not_measured,
// below_resolution, rig_limited, untestable, search_exhausted, harness_error, not_served), carried
// through rather than flattened to one literal - "below_resolution" (the best result the rig can
// express) must render differently from a hole that was never measured.

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
// gen-data seals these; check-consistency asserts these. Both import from here so a field added on one
// side can never ship unsealed because the other's whitelist lagged (audit #11).
// THROUGHPUT_FIELDS (formerly GATED_FIELDS - a vocabulary of which keys must be envelopes, not a gate on
// which values show): throughput-shaped metrics that carry `headroom` when a rig ceiling was available.
export const THROUGHPUT_FIELDS = ["streams_sustained", "streams_sustained_fps"];

// The frontier's declared tail-latency bounds (ms), mirroring the engine's `frontier::P99_BOUNDS_US`.
// Ascending; the unbounded reading is rendered separately. A mirror is a second source of truth, so
// check-consistency compares this list against what raw artifacts actually contain rather than trusting
// it - deriving columns from whatever the first gateway happens to publish would let a missing bound
// silently shrink the board for everyone.
export const FRONTIER_BOUNDS_MS = [1, 5, 10, 50, 100];

// Which bound the board shows before the reader picks one. 10ms sits where the field population
// actually separates (of 1632 recorded rungs: 16% hold 1ms, 47% hold 10ms, 88% hold 100ms, 96% hold 1s)
// - a looser default would put nearly every gateway on the same side of it. It's a VIEW, not a verdict
// (unlike the `SUSTAINED_P99_CEILING_US` constant it replaces): every bound is published on every cell,
// and switching it just re-ranks the board in front of the reader.
export const DEFAULT_BOUND_MS = 10;

// What `sealMetric` will publish for a raw value - the ONE place the display rule lives (separate from
// sealMetric's full envelope) so an independent oracle can answer "does it show, and as what" without
// duplicating this logic. One branch: a measured number always shows.
export function displayedValue(raw, { absentReason = null } = {}) {
  // A below-resolution absence DISPLAYS as 0: the comparison ran and the difference was too small
  // for the rig to weigh, which ranks equal-best and renders as "≈0", never as a hole. Every other
  // absence displays as nothing.
  if (raw == null) return absentReason === "below_resolution" ? 0 : null;
  return raw;
}
// UNGATED, latency-shaped, on a perf cell.
export const UNGATED_LAT_FIELDS = ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"];
// UNGATED, cost-shaped, on a perf record - what the cell COST, not what it delivered. Listed explicitly
// rather than discovered by pattern: these are six unrelated shapes with no common suffix a regex could
// key on safely, so a bare unsealed scalar (the peak_rss_hwm_mib bug again) is one omission away.
export const UNGATED_COST_FIELDS = [
  "cpu_us_per_request", "rps_per_cpu_second", "cost_window_conc", "cost_core_utilisation",
  "cost_window_ok", "cost_window_rps",
  "cost_threads", "cost_nonvol_ctxt_per_request", "cost_majflt",
];
// UNGATED, latency/rate-shaped, on a stream record.
export const UNGATED_STREAM_FIELDS = ["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us"];
// RSS metrics are sealed BY DISCOVERY, not by a whitelist: any *_rss_..._mib field the producer emits
// (idle/peak/recovered, plus qualified variants like peak_rss_hwm_mib) is a metric and must be sealed.
// A fixed whitelist let a differently-named RSS field ship as a bare unsealed scalar before.
export const RSS_FIELD_RE = /_rss_(?:[a-z0-9]+_)*mib$/;
// UNGATED, memory-shaped, on a per-cell memory window. Not RSS values (RSS_FIELD_RE can't discover
// them), but published numbers - growth_rate is the leak rate when a gateway never reached steady
// state - so they're listed explicitly rather than shipping as bare unsealed scalars.
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
// sealMetric: a raw scalar -> a sealed envelope. The raw scalar survives only as `value`.
//   value        : the raw number (or null/undefined when not measured)
//   opts.source  : the provenance stamp (makeSource)
//   opts.extras  : extra CERTIFIED-only fields to carry (concurrency, sweep array)
//   opts.headroom: fraction of the rig's own ceiling reached (engine's `*_headroom`). Omitted (not 0)
//                  when the engine had no usable ceiling.
//   opts.ceiling : the ceiling that fraction is of (`*_rig_ceiling`/`*_mock_ceiling`), so headroom is
//                  checkable rather than asserted. For stream metrics this is DERIVED from the mock's
//                  declared pacing (run::mock_frame_ceiling_fps), not measured.
// The envelope does NOT repeat the provenance stamp - the cell's `source` is authoritative - to avoid
// ~10x bundle bloat across the 36-cell matrix while still preserving invariant P1 (no raw scalar leaks).
//
// opts.zeroNote: what a measured 0 means. A 0 is always an honest MEASURED, CERTIFIED value, never
// folded into "not measured" (exclusively `value == null`, audit #3):
//   ZERO_NO_CEILING    (RPS ceilings)     - served, but no tested load held the qualifying gates.
//   ZERO_MEASURED_FAIL (streaming counts) - offered stream load and sustained NONE of it.
export const ZERO_NO_CEILING = "no_qualifying_ceiling";
export const ZERO_MEASURED_FAIL = "measured_failure";
// WHICH ZERO-NOTE A FIELD TAKES, as data (one list, imported by both gen-data and check-consistency)
// rather than a literal repeated per call site - a swapped note once published a measured streaming
// failure as a missing RPS ceiling and both sides verified it green. Streaming counts are the
// measured-failure family; every other throughput metric is an RPS ceiling. `cpu_fps` is gone from this
// list because the metric is retired - leaving it would make `zeroNoteFor` keep answering
// ZERO_MEASURED_FAIL for a future field that happened to reuse the name.
export const ZERO_FAIL_FIELDS = ["streams_sustained", "streams_sustained_fps"];
// Null for a field with no zero-note vocabulary (most of them) - the two tokens above are claims about
// a THROUGHPUT measurement and are not true sentences about anything else. This used to fall through to
// ZERO_NO_CEILING for every unrecognised field, which fabricated the RPS sentence onto memory-growth and
// gap zeros once the caller's `gated` flag (which had made it harmless) was removed.
export function zeroNoteFor(field) {
  if (ZERO_FAIL_FIELDS.includes(field)) return ZERO_MEASURED_FAIL;
  return THROUGHPUT_FIELDS.includes(field) ? ZERO_NO_CEILING : null;
}
// HEADROOM: how close to our own rig's ceiling a measurement came, and RIG_CEILING: the ceiling it is a
// fraction of. Replace `PACED_MATCH`, a boolean re-statement of the engine's retired `*_mock_bound` flag
// - 0.993 and 0.20 were both `paced_match: undefined` before; now the ratio itself is published.
export const HEADROOM = "headroom";
export const RIG_CEILING = "rig_ceiling";
//   opts.absent: the engine's `absences` entry for this field ({reason, detail}), when the caller has
//                one. An absent value then publishes the ENGINE'S reason and its prose detail instead
//                of the flattened "not_measured" - the reason was measured too, and discarding it here
//                was how "below rig resolution" (a win) rendered identically to "never ran" (a hole).
export function sealMetric(value, opts = {}) {
  const { extras = null, zeroNote = null, absent = null, headroom = null, ceiling = null } = opts;
  // Extras (concurrency, sweep array) and headroom/ceiling attach to EVERY certified envelope, including
  // a certified 0 - "0" beside a real maximum is the claim that most needs its evidence (the sweep
  // curve) attached.
  const withExtras = (env) => {
    if (extras) for (const [k, v] of Object.entries(extras)) if (v != null) env[k] = v;
    if (Number.isFinite(headroom)) env[HEADROOM] = headroom;
    if (Number.isFinite(ceiling)) env[RIG_CEILING] = ceiling;
    return env;
  };
  if (value == null) {
    const env = { value: null, certified: false, suppressed: false, reason: (absent && absent.reason) || "not_measured" };
    if (absent && absent.detail) env.detail = absent.detail;
    return env;
  }
  // A non-number must not wear the CERTIFIED badge: `Number("n/a")` is NaN but `JSON.stringify(NaN)` is
  // `null`, which would silently pass as `{value: null, certified: true}` (matches no documented shape,
  // slips past isEnvelope/C2). Coercion alone isn't enough either - `Number("")`/`Number([])` are 0 and
  // finite, which would certify an empty string as a measured zero. Only a number, or a string that
  // actually spells one, counts.
  const numeric =
    typeof value === "number" || (typeof value === "string" && value.trim() !== "");
  const num = numeric ? Number(value) : NaN;
  if (!Number.isFinite(num)) {
    return {
      value: null,
      certified: false,
      suppressed: false,
      reason: (absent && absent.reason) || "not_measured",
      detail: (absent && absent.detail)
        || `the producer supplied ${JSON.stringify(value)}, which is not a finite number`,
    };
  }
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

// sealFrontier: the engine's frontier -> one sealed reading per bound. The RATE is a sealed envelope
// (absent means absent, with the engine's own reason); the rest of the reading (concurrency, tail, the
// concurrency above it that stopped qualifying) is evidence and rides as plain fields, like `source`.
// `bound_ms` comes from the engine's own `p99_bound_us`, not from FRONTIER_BOUNDS_MS, so a reading
// always names the bound it was actually taken under. `lower_bound` marks a rate the sweep never found
// a ceiling for as a floor, not a ceiling.
//
// The absence key is BLOCK-PREFIXED (`CellPerf::absences()` keys it `perf.frontier.10ms.rps`); both the
// prefixed and bare forms are accepted here (matching gen-data.mjs::absentEntryFor) since a projected
// record carries the bare form. Missing this once flattened every `below_resolution` reading (a real
// result - e.g. one-api genuinely can't serve under a 10ms tail) to an indistinguishable `not_measured`,
// which flatters the gateway.
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
/* sealRungs: the rate every gateway carried at the SAME concurrency, so the board can compare
   apples-to-apples on a rung instead of each gateway's own peak (peak-vs-peak misleadingly implies the
   concurrency itself was an advantage, e.g. "77,248 @ 128 conc" vs "44,475 @ 32 conc").
   Median of the clean windows at that rung (the sweep drives each concurrency WINDOWS_PER_RUNG times;
   a single window is noise near saturation). A window that failed requests is excluded, not averaged
   in - a rung with no clean window publishes an absent envelope, never a rate. */
export function sealRungs(sweep) {
  if (!Array.isArray(sweep) || !sweep.length) return [];
  const byConc = new Map();
  for (const w of sweep) {
    const c = numOrNull(w && w.conc);
    if (c == null) continue;
    if (!byConc.has(c)) byConc.set(c, []);
    byConc.get(c).push(w);
  }
  return [...byConc.keys()].sort((a, b) => a - b).map((conc) => {
    const windows = byConc.get(conc);
    const clean = windows.filter((w) => Number.isFinite(w.rps) && (w.fail ?? 0) === 0);
    const rates = clean.map((w) => w.rps).sort((a, b) => a - b);
    const tails = clean.map((w) => w.p99_us).filter(Number.isFinite).sort((a, b) => a - b);
    const med = (xs) => (xs.length ? xs[Math.floor((xs.length - 1) / 2)] : null);
    return {
      conc,
      // A rung whose every window dropped requests has no rate to publish. The reason travels with it
      // rather than the cell simply reading n/a, because "it failed here" is the finding.
      rps: sealMetric(med(rates), {
        absent: clean.length
          ? null
          : { reason: "not_measured",
              detail: `every window at c=${conc} failed requests, so no clean rate was carried there` },
      }),
      p99_us: med(tails),
      windows: windows.length,
      clean_windows: clean.length,
    };
  });
}

// The engine writes an absent number as `null`; anything else is a number or is not publishable.
function numOrNull(v) {
  return Number.isFinite(v) ? v : null;
}

// The reading the board is currently showing, by bound (`null` selects the unbounded reading). One
// accessor so every surface - table, drawer, compare, charts - reads the same reading for the same bound.
export function frontierAt(frontier, boundMs) {
  if (!Array.isArray(frontier)) return null;
  return frontier.find((r) => (boundMs == null ? r.bound_ms == null : r.bound_ms === boundMs)) || null;
}

// isEnvelope: a sealed metric is an object carrying a `certified` boolean (never a bare scalar).
export function isEnvelope(x) {
  return x != null && typeof x === "object" && typeof x.certified === "boolean";
}
