// SPDX-License-Identifier: Apache-2.0
// Shared projection for current and historical raw evidence.
import { sealMetric, sealFrontier, sealRungs, makeSource, SWEEP, UNGATED_LAT_FIELDS, UNGATED_COST_FIELDS, UNGATED_STREAM_FIELDS, isMetricField, zeroNoteFor, sampleCounts, PERF_SAMPLE_FIELDS, STREAM_SAMPLE_FIELDS } from "./seal.mjs";
const UNGATED_LAT = UNGATED_LAT_FIELDS;
function absentEntryFor(absences, prefix, k) {
  if (!absences || typeof absences !== "object") return null;
  return absences[`${prefix}.${k}`] || absences[k] || null;
}
// sealPerfCellPerf: a raw perf object -> {<sealed metrics>} (no path/source; the caller stamps those).
// Used BOTH for the canonical best_cell/translation_cell AND to seal every matrix cell in-place, so the
// matrix popup reads envelopes, never raw scalars (invariant C1: no ungated field survives in the bundle).
// `absences` is the raw CELL's sibling reason map; an absent metric now emits an envelope CARRYING that
// reason instead of no key at all - a missing key was how "below rig resolution" rendered as a bare n/a.
function sealPerfCellPerf(perf, absences = null) {
  const rec = {};
  for (const k of UNGATED_LAT)
    rec[k] = sealMetric(perf[k], { absent: absentEntryFor(absences, "perf", k) });
  // WHAT THE CELL COST. Sealed exactly like the latency fields, and for the same reason: an absent
  // cost must arrive carrying WHY it is absent. Every snapshot taken before the capture existed has
  // none of these, and "not measured" is the only honest rendering - a 0 would make a gateway that
  // was never measured look infinitely efficient.
  for (const k of UNGATED_COST_FIELDS)
    rec[k] = sealMetric(perf[k], { absent: absentEntryFor(absences, "perf", k) });
  // THE THROUGHPUT ANSWER: one reading per declared tail-latency bound, off one sweep.
  //
  // This replaces `sealThroughput` and the two scalars it produced (`rps_sustained_20ms` and
  // `rps_max_proxy`), which were the same sweep collapsed twice by a chosen ceiling - and which could
  // invert against each other, because two different algorithms summarised one set of windows. See
  // seal.mjs and the engine's `frontier.rs`.
  rec.frontier = sealFrontier(perf.frontier, absences);
  // The rungs every reading was taken from, so a reader can re-derive the whole frontier rather than
  // taking it on trust. It rides as a plain field: it is evidence, not a metric.
  if (Array.isArray(perf.sweep_max_proxy) && perf.sweep_max_proxy.length) {
    rec.sweep = perf.sweep_max_proxy;
    // AND THE SAME RUNGS AS A SEALED READING, so the board can be compared at ONE concurrency rather
    // than at each gateway's own peak. `sweep` above is evidence; this is the metric, median of the
    // clean windows at each rung, and it is what the concurrency selector ranks on.
    rec.rungs = sealRungs(perf.sweep_max_proxy);
  }
  if (perf.egress_reverified != null) rec.egress_reverified = perf.egress_reverified;
  // The verdict without its evidence is an assertion. egress_reverified is the fairness guard's boolean
  // (did this gateway actually TRANSLATE to the egress dialect, or just proxy the ingress request
  // verbatim - which the mock, answering all six dialects by path, would otherwise score as a
  // translation capability it does not have). reverify_note is the reason string behind that boolean, and
  // it was dying here while the flag travelled on. A FALSE reverify is an accusation against a gateway;
  // publishing it with no stated basis is exactly what this codebase refuses to do elsewhere.
  // Neither is a sealed metric - they are provenance about a metric, so they ride as plain fields.
  if (perf.reverify_note != null) rec.reverify_note = perf.reverify_note;
  // THE SAMPLE WEIGHT BEHIND THE c=1 PERCENTILES (record.rs CellPerf.c1_note: "taken over 148 successful
  // gateway round trips and 254967 direct-to-mock round trips"). The stream rebuild already carries its
  // twin (stream_c1_note); dropping this one published a thin percentile indistinguishable from a solid one.
  if (perf.c1_note != null) rec.c1_note = perf.c1_note;
  // And the same weight as numbers, where the engine records them (v1.1 gateway_c1_samples /
  // direct_c1_samples). Absent on older snapshots, so their records keep exactly their old shape.
  Object.assign(rec, sampleCounts(perf, PERF_SAMPLE_FIELDS));
  return rec;
}
// sealPerfCell: a matrix/fallback perf object -> the canonical {path, source, <sealed metrics>} record.
function sealPerfCell(perf, path, source, absences = null) {
  return { path: { ...path }, source, ...sealPerfCellPerf(perf, absences) };
}
// sealStreamRecord: a raw stream record -> {<sealed metrics>} (no path/source). Used for the canonical g.streaming AND
// for sealing every matrix cell's own .stream in-place (so the popup reads envelopes).
// `cpuSource`: the provenance stamp for cpu_fps ALONE, when that number came from a different run than
// the rest of the record (the legacy stream-suite fallback below reads it from the SEPARATE streamcpu
// suite). Null on every matrix path, where one cell produced the whole record.
function sealStreamRecord(s, absences = null, cpuSource = null) {
  const rec = {};
  const abs = (k) => absentEntryFor(absences, "stream", k);
  for (const k of UNGATED_STREAM_FIELDS)
    rec[k] = sealMetric(s[k], { absent: abs(k) });
  // streams_sustained_fps and streams_sustained are the SAME bisect's rate and count, so they carry the
  // same comparison: one derived mock ceiling, one fraction of it. (They used to share a mock-bound FLAG
  // for the same reason - audit #11 was that sealing the rate ungated beside a gated count let the rate
  // publish while the count it came from was suppressed. Neither is suppressed now; what they share is
  // the fact, not a gate.)
  //
  // THE CEILING HERE IS DERIVED, NOT MEASURED - `run::mock_frame_ceiling_fps`, from the mock's own
  // declared frame count and pacing interval. So a gateway at ~1.0 of it forwarded every frame as it
  // arrived, which is the best outcome a proxy of a paced upstream can have. That case used to publish
  // nothing: 13 cells lost this metric on the 2026-07-28 board.
  //
  // The zero-note comes from seal.mjs's zeroNoteFor, the ONE place that mapping lives, so the note the
  // independent oracle expects and the note the seal writes cannot be different notes.
  const streamCeiling = s.streams_sustained_mock_ceiling ?? null;
  const streamHeadroom = s.streams_sustained_headroom ?? null;
  rec.streams_sustained_fps = sealMetric(s.streams_sustained_fps, {
    headroom: streamHeadroom, ceiling: streamCeiling,
    zeroNote: zeroNoteFor("streams_sustained_fps"),
    absent: abs("streams_sustained_fps") });
  // AUDIT #3: streaming counts - a 0 is a MEASURED FAILURE (offered stream load, sustained none), NOT
  // "not measured". Only a null (absent field) is not-measured. The note names which, and every surface
  // renders the two apart.
  rec.streams_sustained = sealMetric(s.streams_sustained, {
    headroom: streamHeadroom, ceiling: streamCeiling,
    zeroNote: zeroNoteFor("streams_sustained"),
    absent: abs("streams_sustained") });
  // NO cpu_fps. Retired: of the 16 cells that published both it and `streams_sustained_fps` (the rate
  // at the PROVEN delivery boundary), 4 had it INVERTED below that boundary, 5 were redundant within 1%,
  // and 7 were measured at a concurrency where the delivery gate did not hold - a frame rate recorded
  // while dropping frames. See the engine's `run.rs` for the per-gateway numbers.
  void cpuSource;
  // The sample counts behind the TTFT and inter-frame-gap percentiles (record.rs CellStream
  // ttft_*_samples / gap_*_samples), carried verbatim where the engine recorded a number.
  Object.assign(rec, sampleCounts(s, STREAM_SAMPLE_FIELDS));
  return rec;
}
function sealStreaming(s, dialect, source, absences = null, cpuSource = null) {
  return { path: { dialect }, source, stream_served: true, ...sealStreamRecord(s, absences, cpuSource) };
}
// cpuFpsSourceFor(g): the provenance stamp cpu_fps needs OF ITS OWN on the legacy stream-suite fallback
// row, or null when the record's own stamp already describes it. cpu_fps is measured by the streamcpu
// suite and the rest of the record by the stream suite; they are separate runs with separate build +
// measured_at, so the record's single stamp is the truth for one of them and a fabrication for the other
// whenever they differ. Hoisted, so the per-gateway pass above can call it.
function cpuFpsSourceFor(g) {
  if (!g.streamcpu || g.streamcpu.streamcpu_frames_per_sec == null) return null;
  const build = g.streamcpu.build ?? null, at = g.streamcpu.measured_at ?? null;
  const sameRun = build === (g.stream.build ?? null) && at === (g.stream.measured_at ?? null);
  return sameRun ? null : makeSource("stream-fallback", SWEEP.STREAM_SUITE, build, at);
}
// matrixHasCellMemory(m): does this matrix carry a per-cell memory window on any served cell? The memory
// lane's freshness stamp ages by the matrix that produced those windows, so the lane must know whether it
// has anything to age. Hoisted, so the per-gateway pass above can call it.
function matrixHasCellMemory(m) {
  if (!m || typeof m !== "object") return false;
  const cellGroups = [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)];
  for (const cells of cellGroups) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (cell && cell.served === true && cell.memory && typeof cell.memory === "object") return true;
    }
  }
  return false;
}
// sealMatrixCellsInPlace: replace every served cell's raw perf/stream AND the top-level memory block's raw
// RSS with SEALED envelopes, so the matrix popup + Protocol view + the embedded/snapshot matrix carry
// envelopes, never raw scalars - NO ungated metric field survives anywhere in the bundle (invariant C1).
// Non-metric fields (served/status/path/verdict_note/load_cell/rss_series/…) are untouched.
export function sealMatrixCellsInPlace(m) {
  const seen = new Set();   // v1 shares m.cells with upstreams[shape].cells (same refs) - seal once.
  const cellGroups = [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)];
  for (const cells of cellGroups) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (!cell || typeof cell !== "object" || seen.has(cell)) continue;
      seen.add(cell);
      if (cell.perf) cell.perf = sealPerfCellPerf(cell.perf, cell.absences);
      // THE CASE THIS GUARD MISSED: stream_served is `true`/`false`/a status string ("not_measured",
      // "untestable", "not_probed" - StreamServed's real shape, record.rs), and in real field data it
      // is almost NEVER the literal boolean true - a non-streaming or untestable cell still carries a
      // raw stream object with its own *_mock_bound siblings. Gating the seal on `=== true` left every
      // other case's raw engine object (and its unconsumed _mock_bound flags) in the bundle untouched
      // (invariant C1). Seal whenever a stream object exists at all; sealStreamRecord/sealMetric are
      // already null-safe, and downstream readers (app.js) still gate display on stream_served === true
      // themselves, so preserving the real value here changes nothing they show.
      if (cell.stream && typeof cell.stream === "object") {
        cell.stream = {
          stream_served: cell.stream.stream_served,
          stream_error: cell.stream.stream_error ?? null,
          // THE EVIDENCE FOR THE STATUS, WHICH THE REBUILD WAS DROPPING. `stream_served` is a token
          // (`true`, any Absent token as a string, "not_probed"; `false` is legacy parse-only),
          // `reason` is the MACHINE token for why it came out that way (the same Absent vocabulary the
          // envelopes carry) and `stream_error` is the PROSE behind it; `stream_c1_note` is the c=1
          // leg's own note. Rebuilding the object from a fixed key list silently deleted the reason and
          // the c1 note - seven reasons and five c1 notes in the recovered 2026-07-29 snapshots - so the
          // bundle published a refusal with its explanation removed. A cell whose gap figures measured
          // now publishes stream_served:true WITH a reason token for the TTFT leg, which makes carrying
          // the pair load-bearing rather than decorative. None of the three is a sealed metric (they are
          // prose/vocabulary about the cell, not numbers), so they ride as plain fields; app.js renders
          // the prose and maps the token through its own note vocabulary, never printing it verbatim.
          reason: cell.stream.reason ?? null,
          stream_c1_note: cell.stream.stream_c1_note ?? null,
          ...sealStreamRecord(cell.stream, cell.absences),
        };
      }
      // PER-CELL MEMORY: its own cold-started, plateau-terminated window per cell. The memory tab reads
      // these directly off the matrix cell (Min/Max/Same/Custom), so every published number on them must
      // be an envelope like every other metric, sealed BY DISCOVERY (any RSS field) plus the non-RSS
      // memory metrics the vocabulary names (growth rate, time to plateau).
      if (cell.memory && typeof cell.memory === "object") {
        for (const k of Object.keys(cell.memory))
          if (isMetricField(k))
            cell.memory[k] = sealMetric(cell.memory[k], { absent: absentEntryFor(cell.absences, "memory", k) });
      }
    }
  }
  // A LEGACY top-level memory block (pre-per-cell results, no longer read by anything) still travels in the bundle (embedded
  // in g.matrix + the snapshot); seal its metrics so no bare ungated field survives.
  // Sealed BY DISCOVERY (audit #11): every `*_rss_mib` key present, not a 3-key whitelist that the
  // producer already outgrew (peak_rss_hwm_mib / post_load_rss_mib were shipping as BARE scalars).
  // THE SAME VOCABULARY AND THE SAME ABSENCES AS A PER-CELL WINDOW, because it is the same kind of
  // block. Testing only RSS_FIELD_RE here re-created the narrower-whitelist bug one level up: the
  // non-RSS memory metrics (growth rate, time to plateau) shipped as bare scalars off a legacy block,
  // and passing `{}` for the options threw away the engine's absence reason, so a legacy
  // below-resolution or untestable hole flattened to "not_measured" on reseal.
  if (m.memory && typeof m.memory === "object") {
    for (const k of Object.keys(m.memory))
      if (isMetricField(k))
        m.memory[k] = sealMetric(m.memory[k], { absent: absentEntryFor(m.absences, "memory", k) });
  }
}


export { absentEntryFor, sealPerfCell, sealStreaming, cpuFpsSourceFor, matrixHasCellMemory };
