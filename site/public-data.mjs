// SPDX-License-Identifier: Apache-2.0
// Public records are a projection of the checked board, never a new measurement.
import { canonicalCellState } from "./states.mjs";
export const ORIGIN = "https://onthebench.ai";
export const DISCLOSURE = { operator: "Busbar team", operator_is_entrant: true };
export const PROTOCOLS = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"];
export const POLICY = {
  id: "matched-cell-v1",
  description: "Ratios require an identical protocol pair, hardware, architecture, metric definition, clean harness commit, recorded mock digest, and metric-specific workload. Missing evidence never matches missing evidence. These are observed gateway measurements against a local mock, not a claim about model latency or statistical significance.",
};
const value = x => x && typeof x === "object" && "value" in x ? x.value : x;
export function metricRecord(envelope) {
  if (!envelope || typeof envelope !== "object" || !("value" in envelope)) return null;
  const reason = envelope.reason ?? null;
  const available = envelope.certified === true && envelope.suppressed !== true && typeof envelope.value === "number" && Number.isFinite(envelope.value);
  return {
    ...envelope,
    value: reason === "below_resolution" ? null : available ? envelope.value : null,
    state: reason === "below_resolution" ? "below_resolution" : available ? "valid" : reason || "not_measured",
    reason,
  };
}
export function cellStatus(cell) {
  return canonicalCellState(cell);
}
function groupDefinition(definitions, section, key) {
  const prefix = section === "perf" ? (key.includes("latency") || key.includes("c1_p99") ? "perf.added_latency" : key.includes("rps_") || key.startsWith("cpu_") || key.startsWith("cost_") ? "perf.cost" : "perf.frontier")
    : section === "stream" ? (key.startsWith("streams_") ? "stream.streams_sustained" : "stream.added_ttft_and_gap") : "memory";
  return definitions?.[prefix] ?? null;
}
export function projectMeasurements(gateway, archivesByFile) {
  const measurements = [];
  const matrix = gateway.matrix;
  if (!matrix) return measurements;
  for (const [egress, upstream] of Object.entries(matrix.upstreams || {})) {
    for (const [ingress, cell] of Object.entries(upstream.cells || {})) {
      const archive = archivesByFile.get(cell.__run?.file || gateway.snapshot_file);
      const raw = archive?.snapshot;
      const rawCell = raw?.matrix?.upstreams?.[egress]?.cells?.[ingress];
      const rig = raw?.rig ?? null;
      const measuredAt = cell.__run?.measured_at ?? matrix.measured_at ?? null;
      const metrics = {};
      for (const section of ["perf", "stream", "memory"]) {
        for (const [key, envelope] of Object.entries(cell[section] || {})) {
          const m = metricRecord(envelope);
          if (!m) continue;
          metrics[key] = { ...m, section, definition: groupDefinition(raw?.definitions, section, key) };
        }
      }
      const frontier = (cell.perf?.frontier || []).map(reading => ({ ...reading, rps: metricRecord(reading.rps) }));
      measurements.push({
        schema_version: "1.0",
        measurement_id: archive ? `${archive.id}:${ingress}:${egress}` : null,
        gateway_id: gateway.key,
        gateway_build: cell.__run?.build ?? matrix.build ?? null,
        run_id: archive?.id ?? null, measured_at: measuredAt,
        status: { state: cellStatus(cell), raw_state: cell.served ?? null, reason: cell.reason ?? null, detail: cell.probe_note || cell.verdict_note || null, evidence_scope: cell.served === "not_configurable" ? "Historical capability research, not a fresh probe of the tested build. Referenced versions may differ from gateway_build; do not infer current capability." : "Recorded run evidence", probe_http_status: cell.status || null, response_excerpt: cell.body_snippet || null },
        test: {
          ingress_protocol: ingress, upstream_protocol: egress,
          hardware: raw?.hardware ?? matrix.hardware ?? null,
          architecture: raw?.arch ?? rig?.arch ?? matrix.arch ?? null,
          harness_commit: rig?.engine?.commit ?? null,
          harness_dirty: rig?.engine?.dirty ?? null,
          mock_sha256: rig?.mock?.sha256 ?? null,
          load_generator_sha256: rig?.ugen?.sha256 ?? null,
          qualification: rig?.box_qualify ?? null,
          // Definitions are recorded by the engine; do not invent absent knobs.
          definitions: raw?.definitions ?? {},
          sweep_duration_s: raw?.matrix?.sweep_dur ?? null,
          sweep_ttft_ms: raw?.matrix?.sweep_ttft_ms ?? null,
          memory_protocol: rawCell?.memory?.protocol ?? null,
          memory_concurrency: value(rawCell?.memory?.concurrency) ?? null,
          memory_load_s: value(rawCell?.memory?.load_s) ?? null,
          cost_concurrency: value(rawCell?.perf?.cost_window_conc) ?? null,
        },
        metrics, frontier,
        provenance: {
          source_url: `${ORIGIN}/gateways/${gateway.key}`,
          raw_result_url: archive ? `${ORIGIN}/api/v1/runs/${archive.id}/raw.json` : null,
          run_url: archive ? `${ORIGIN}/gateways/runs/${archive.id}` : null,
          methodology_url: `${ORIGIN}/gateways/method`,
          raw_sha256: archive?.sha256 ?? null,
        },
        disclosure: DISCLOSURE,
      });
    }
  }
  return measurements;
}

// No inference from headline cells, no cross-protocol division, no division by
// a display zero standing for below-resolution. A ratio is descriptive only.
export function compareMeasurements(measurements, query) {
  const { a, b, ingress = "openai", egress = "openai", metric = "added_latency_p99_us" } = query;
  const pick = gateway => measurements.find(m => m.gateway_id === gateway && m.test.ingress_protocol === ingress && m.test.upstream_protocol === egress);
  const left = pick(a), right = pick(b);
  const result = {
    comparison: { gateway_a: a, gateway_b: b, ingress_protocol: ingress, upstream_protocol: egress, metric, ...(metric === "throughput_rps" ? { bound_ms: query.bound === "none" ? null : Number(query.bound || 10) } : {}) },
    comparability: { status: "insufficient_evidence", policy: POLICY.id, conditions_matched: [], reasons: [], warnings: [] },
    results: { gateway_a: left ?? null, gateway_b: right ?? null, gateway_b_over_a: null },
    interpretation: { scope: "Gateway measurements against a local mock", generalizable_to_real_model_latency: false, statistical_significance: "not_established" },
    disclosure: DISCLOSURE,
  };
  const c = result.comparability;
  if (!left || !right) { c.reasons.push("requested_cell_not_measured"); return result; }
  const bound = query.bound === "none" ? null : Number(query.bound || 10);
  const frontier = m => m.frontier?.find(f => f.bound_ms === bound);
  const throughput = m => ({ ...frontier(m)?.rps, definition: m.test.definitions?.["perf.frontier"], section: "perf" });
  const x = metric === "throughput_rps" ? throughput(left) : left.metrics[metric];
  const y = metric === "throughput_rps" ? throughput(right) : right.metrics[metric];
  if (left.status.state !== "served" || right.status.state !== "served") c.reasons.push("requested_cell_not_served");
  if (x?.state !== "valid" || y?.state !== "valid") c.reasons.push("metric_not_valid_on_both_cells");
  const compare = (key, l, r) => {
    if (l == null || r == null || l === "" || r === "") c.reasons.push(`missing_${key}`);
    else if (JSON.stringify(l) !== JSON.stringify(r)) c.reasons.push(`different_${key}`);
    else c.conditions_matched.push(key);
  };
  for (const key of ["ingress_protocol", "upstream_protocol", "hardware", "architecture", "harness_commit", "mock_sha256"]) compare(key, left.test[key], right.test[key]);
  compare("metric_definition", x?.definition, y?.definition);
  if (left.test.harness_dirty !== false || right.test.harness_dirty !== false) c.reasons.push("clean_harness_not_established");
  if (!left.provenance.raw_result_url || !right.provenance.raw_result_url) c.reasons.push("missing_raw_evidence");
  if (left.test.qualification?.outcome !== "pass" || right.test.qualification?.outcome !== "pass") c.reasons.push("rig_qualification_not_established");
  if (left.test.load_generator_sha256 || right.test.load_generator_sha256) compare("load_generator_digest", left.test.load_generator_sha256, right.test.load_generator_sha256);
  else c.warnings.push("Load-generator binary digests were not recorded; instrument equivalence is based on the identical clean harness source commit and mock digest.");
  if (x?.section === "memory") for (const k of ["memory_protocol", "memory_concurrency", "memory_load_s"]) compare(k, left.test[k], right.test[k]);
  if (x?.section === "perf" && /^(cpu_|cost_|rps_per_cpu)/.test(metric)) compare("cost_concurrency", left.test.cost_concurrency, right.test.cost_concurrency);
  if (metric === "throughput_rps") {
    compare("operating_concurrency", frontier(left)?.concurrency, frontier(right)?.concurrency);
    if (frontier(left)?.lower_bound || frontier(right)?.lower_bound) c.reasons.push("throughput_is_only_a_lower_bound");
  }
  // Streaming has no single universal concurrency. Until the public record
  // establishes its metric-specific workload, leave the raw readings available.
  if (x?.section === "stream") c.reasons.push("stream_workload_equivalence_not_established");
  if (left.measured_at !== right.measured_at) c.warnings.push("Measurements were made at different times on independently provisioned machines.");
  c.status = c.reasons.some(r => r.startsWith("different_") || r.endsWith("not_served")) ? "incomparable" : c.reasons.length ? "insufficient_evidence" : "comparable";
  if (c.status === "comparable" && x.value > 0 && y.value >= 0) result.results.gateway_b_over_a = y.value / x.value;
  else if (c.status === "comparable") c.warnings.push("No ratio is defined when the denominator is zero.");
  return result;
}

export function measurementsCsv(measurements) {
  const columns = ["measurement_id", "gateway_id", "gateway_build", "run_id", "measured_at", "ingress_protocol", "upstream_protocol", "metric", "value", "state", "reason", "definition", "raw_result_url"];
  // Quoting alone does not stop spreadsheet formulas in project-controlled text.
  const quote = v => '"' + String(v ?? "").replace(/^[=+@\-]/, "'$&").replace(/"/g, '""') + '"';
  const rows = [columns];
  for (const m of measurements) for (const [key, metric] of Object.entries(m.metrics)) rows.push([
    m.measurement_id, m.gateway_id, m.gateway_build, m.run_id, m.measured_at,
    m.test.ingress_protocol, m.test.upstream_protocol, key, metric.value, metric.state, metric.reason, metric.definition, m.provenance.raw_result_url,
  ]);
  for (const m of measurements) for (const f of m.frontier || []) rows.push([
    m.measurement_id, m.gateway_id, m.gateway_build, m.run_id, m.measured_at,
    m.test.ingress_protocol, m.test.upstream_protocol, `throughput_rps@${f.bound_ms == null ? "unbounded" : f.bound_ms + "ms"}`, f.rps?.value, f.lower_bound ? "lower_bound" : f.rps?.state, f.rps?.reason,
    `${m.test.definitions?.["perf.frontier"] || "Definition not recorded"}; concurrency=${f.concurrency}; observed_p99_us=${f.p99_us}`, m.provenance.raw_result_url,
  ]);
  return rows.map(r => r.map(quote).join(",")).join("\r\n") + "\r\n";
}

// Publication time is unknown until deployed; never relabel a build as a new run.
export function freshness(measuredAt, generatedAt, publishedAt = null) {
  const elapsed = Date.parse(generatedAt) - Date.parse(measuredAt);
  const days = Number.isFinite(elapsed) && elapsed >= 0 ? Math.floor(elapsed / 86400000) : null;
  return { measured_at: measuredAt ?? null, published_at: publishedAt, generated_at: generatedAt,
    age_days: days, age_as_of: generatedAt,
    age: days == null ? "Measurement age unknown" : `${days} day${days === 1 ? "" : "s"} old at collection generation` };
}

export function compactSummary(publication, query = {}) {
  const ingress = query.ingress || "openai", egress = query.egress || "openai";
  const keys = ["added_latency_p50_us", "added_latency_p99_us", "added_ttft_p99_us"];
  const compact = compactRecords(publication.measurements.map(m => ({ ...m, test: { ...m.test, definitions: {} }, metrics: Object.fromEntries(Object.entries(m.metrics).filter(([key]) => keys.includes(key))) })));
  return {
    schema_version: "1.1", definitions: compact.definitions, dataset_id: publication.dataset_id, generated_at: publication.generated_at,
    published_at: publication.published_at, license: publication.license, selected_protocol_pair: { ingress_protocol: ingress, upstream_protocol: egress },
    results: publication.gateways.filter(g => !query.gateway || g.id === query.gateway).map(g => {
      const m = compact.measurements.find(m => m.gateway_id === g.id && m.test.ingress_protocol === ingress && m.test.upstream_protocol === egress);
      return { gateway_id: g.id, name: g.name, gateway_build: m?.gateway_build ?? null,
        ...freshness(m?.measured_at, publication.generated_at, publication.published_at),
        current_metadata: g.current_metadata,
        status: m?.status ?? { state: "not_measured", reason: "requested_cell_not_measured", detail: null },
        metrics: Object.fromEntries(keys.map(k => [k, m?.metrics[k] ?? { value: null, state: "not_measured", reason: "metric_not_recorded", definition_id: null }])),
        provenance: m?.provenance ?? { source_url: g.url, raw_result_url: null, run_url: null, methodology_url: `${ORIGIN}/gateways/method`, raw_sha256: null },
        evidence_url: `${ORIGIN}/api/v1/measurements?gateway=${g.id}&ingress=${ingress}&egress=${egress}` };
    }),
    comparison_policy: POLICY, disclosure: DISCLOSURE,
    interpretation: "Selected protocol cells, not a ranking. Check /api/v1/compare before calculating ratios. Null and below-resolution values are not zero. Repository statistics are dated metadata, not measurements. published_at is the recorded release timestamp, not the first publication of historical runs; null means unrecorded. generated_at is collection build time. Resolve metric definition_id through definitions.",
  };
}

// Deduplicate exact definitions without treating text from different harnesses
// as equivalent. IDs are local to the response's dictionary.
export function compactRecords(records) {
  const definitions = {}, ids = new Map();
  const intern = text => {
    if (text == null) return null;
    if (!ids.has(text)) { const id = `definition-${ids.size + 1}`; ids.set(text, id); definitions[id] = text; }
    return ids.get(text);
  };
  const measurements = records.map(m => ({ ...m,
    test: { ...m.test, definitions: {}, definition_ids: Object.fromEntries(Object.entries(m.test.definitions || {}).map(([key, text]) => [key, intern(text)])) },
    metrics: Object.fromEntries(Object.entries(m.metrics).map(([key, metric]) => {
      const { definition, ...rest } = metric; return [key, { ...rest, definition_id: intern(definition) }];
    })),
  }));
  return { measurements, definitions };
}
