// SPDX-License-Identifier: Apache-2.0
import { esc, ORIGIN, mdSpan, pairHtml, metricLabelHtml, stateLabel, formatNumber, numberHtml, timeHtml } from "./render.mjs";
import { DISCLOSURE, POLICY } from "./public-data.mjs";

import { CELL_STATES, METRIC_STATES } from "./states.mjs";

export const disclosureHtml = `<p class="muted">Operated by the ${esc(DISCLOSURE.operator)}; Busbar is also an entrant. Gateway overhead against a local mock is not end-to-end model latency. <a href="/gateways/method">Methodology and limitations</a>.</p>`;
// Phones show each row as a card; data-label names the column beside each value (style.css).
const table = (headers, rows) => `<div class="table-scroll"><table class="cards"><thead><tr>${headers.map(h => `<th>${esc(h)}</th>`).join("")}</tr></thead><tbody>${rows.map(r => `<tr>${r.map((c, i) => `<td data-label="${esc(headers[i])}">${c}</td>`).join("")}</tr>`).join("")}</tbody></table></div>`;
// The exact text each cell carried before presentation; the Markdown twin keeps it through data-md.
const rawMetricText = m => m?.state === "valid" ? `${m.value}${m.note ? ` (${m.note})` : ""}` : (m?.state || "not_measured");
// What a measured-zero note means, as defined where the note is assigned (seal.mjs).
const ZERO_NOTES = {
  measured_failure: "measured failure: the gateway was offered stream load and sustained none of it",
  no_qualifying_ceiling: "no qualifying ceiling: served, but no tested load held the qualifying gates",
};
const valueHtml = m => m?.state === "valid"
  ? (typeof m.value === "number" ? mdSpan(`<span title="${esc(m.value)}">${esc(formatNumber(m.value))}</span>`, rawMetricText(m)) : mdSpan(esc(m.value), rawMetricText(m)))
  : mdSpan("—", rawMetricText(m));
const metricStateText = m => m?.state === "valid"
  ? `Valid${m.note ? ` — ${ZERO_NOTES[m.note] || stateLabel(m.note).toLowerCase()}` : ""}`
  : stateLabel(m?.reason || m?.state || "not_measured");
// Long recorded notes are listed once per page and referenced by number from each row that carries them.
export function noteIndex() {
  const list = [];
  return { list, ref(text) {
    let i = list.indexOf(text);
    if (i < 0) { list.push(text); i = list.length - 1; }
    return `<a href="#note-${i + 1}">recorded note ${i + 1}</a>`;
  } };
}
export const notesHtml = notes => notes?.list.length ? `<h2 id="recorded-notes">Recorded notes</h2><p>Text recorded with the cells above. Each note is shown once; rows refer to it by number.</p><ol class="recorded-notes">${notes.list.map((text, i) => `<li id="note-${i + 1}">${esc(text)}</li>`).join("")}</ol>` : "";
const LONG_NOTE = 140;
const noteHtml = (text, notes) => notes && String(text).length > LONG_NOTE ? notes.ref(String(text)) : esc(text);
// WHICH RECORDED SAMPLE COUNT EACH PERCENTILE WAS TAKEN OVER, as the engine documents it (record.rs
// CellPerf/CellStream): a c=1 leg's p99 over that leg's count, an added-latency/TTFT/gap difference over
// both legs' populations. Shown only when the run recorded the count (engine v1.1 `sample_counts`).
const SAMPLE_KEYS = {
  gateway_c1_p99_us: [["gateway_c1_samples"]], direct_c1_p99_us: [["direct_c1_samples"]],
  added_latency_p50_us: [["gateway_c1_samples", "gateway"], ["direct_c1_samples", "direct"]],
  added_latency_p99_us: [["gateway_c1_samples", "gateway"], ["direct_c1_samples", "direct"]],
  added_ttft_p50_us: [["ttft_gw_samples", "gateway"], ["ttft_direct_samples", "direct"]],
  added_ttft_p99_us: [["ttft_gw_samples", "gateway"], ["ttft_direct_samples", "direct"]],
  added_gap_p50_us: [["gap_gw_samples", "gateway"], ["gap_direct_samples", "direct"]],
  added_gap_p99_us: [["gap_gw_samples", "gateway"], ["gap_direct_samples", "direct"]],
};
export function samplesFor(counts, key) {
  const parts = (SAMPLE_KEYS[key] || []).filter(([k]) => Number.isFinite(counts?.[k]));
  if (!parts.length) return null;
  return { text: `n=${parts.map(([k, leg]) => `${formatNumber(counts[k])}${leg ? ` ${leg}` : ""}`).join(" / ")}`,
    md: parts.map(([k]) => `${k}=${counts[k]}`).join("; ") };
}
export function measurementTable(measurements, notes = null) {
  return table(["Ingress → upstream", "Metric", "Value", "State / reason", "p99 bound (ms)"], measurements.flatMap(m => {
    const path = pairHtml(m.test.ingress_protocol, m.test.upstream_protocol);
    if (!Object.keys(m.metrics).length) return [[path, "—", "—",
      mdSpan(`${esc(stateLabel(m.status.state))}${m.status.detail ? `: ${noteHtml(m.status.detail, notes)}` : ""}`, m.status.state + (m.status.detail ? `: ${m.status.detail}` : "")), "—"]];
    const rows = Object.entries(m.metrics).map(([key, v]) => {
      const n = samplesFor(m.sample_counts, key);
      return [path, metricLabelHtml(key), valueHtml(v), mdSpan(esc(metricStateText(v) + (n ? ` · ${n.text}` : "")), (v.reason || v.state) + (n ? `; ${n.md}` : "")), "—"];
    });
    for (const f of m.frontier) {
      const state = [f.rps?.state && f.rps.state !== "valid" ? stateLabel(f.rps.reason || f.rps.state) : f.rps?.note ? metricStateText(f.rps) : null, f.lower_bound ? "lower bound" : null, `concurrency ${f.concurrency == null ? "not recorded" : formatNumber(f.concurrency)}`, f.samples != null ? `n=${formatNumber(f.samples)}` : null].filter(Boolean).join("; ");
      // A v1.1 bound_not_met reading carries the engine's own sentence for why no rung met the bound.
      const why = f.rps?.state === "bound_not_met" && f.rps.detail ? f.rps.detail : null;
      rows.push([path, metricLabelHtml("throughput_rps"),
        f.rps?.state === "valid" && typeof f.rps.value === "number" ? mdSpan(`<span title="${esc(f.rps.value)}">${esc(formatNumber(f.rps.value))}</span>`, `${rawMetricText(f.rps)} req/s`) : mdSpan("—", `${rawMetricText(f.rps)} req/s`),
        mdSpan(esc(state) + (why ? `: ${noteHtml(why, notes)}` : ""), `${f.lower_bound ? "lower bound; " : ""}concurrency ${f.concurrency ?? "not recorded"}${f.samples != null ? `; samples ${f.samples}` : ""}${why ? `; ${why}` : ""}`),
        mdSpan(f.bound_ms == null ? "Unbounded" : `${esc(formatNumber(f.bound_ms))} ms`, String(f.bound_ms ?? "unbounded"))]);
    }
    return rows;
  }));
}
// The image digest is evidence, not a label: show the tag and keep the full reference on hover. A build the
// gateway's definition compiles from source records the binary path it ran; say so instead of a bare path.
export function buildHtml(build, { fromSource = false } = {}) {
  if (!build) return "Not recorded";
  if (build.includes("@sha256:")) return mdSpan(`<span class="build-ref" title="${esc(build)}">${esc(build.split("@")[0])}</span>`, build);
  if (fromSource && !build.includes(":")) return mdSpan(`built from source; recorded binary <code>${esc(build)}</code>`, build);
  return esc(build);
}
const ageHtml = m => mdSpan(m.age_days == null ? esc(m.age || "Measurement age unknown") : `${esc(formatNumber(m.age_days))} day${m.age_days === 1 ? "" : "s"} when this collection was generated`, m.age);
export function gatewayArticle(g, records, histories, gateways, { full = false, fromSource = false } = {}) {
  if (!full) {
    const selected = records.filter(m => m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai");
    const core = selected.map(m => ({ ...m, metrics: Object.fromEntries(Object.entries(m.metrics).filter(([key]) => ["added_latency_p50_us", "added_latency_p99_us", "added_ttft_p99_us"].includes(key))), frontier: [] }));
    return `<h1>${esc(g.display)} benchmark summary</h1><nav aria-label="Gateway"><strong>Overview</strong> · <a href="/gateways/${esc(g.key)}/history">History</a> · <a href="/gateways/${esc(g.key)}/evidence">Evidence</a></nav>${disclosureHtml}<p>Selected pair: <strong>OpenAI → OpenAI</strong>. Other protocol pairs are available through paginated measurements. This summary is not a universal product ranking.</p>` +
      `<nav aria-label="Gateway data"><a href="/api/v1/summary?gateway=${esc(g.key)}">Compact JSON</a> · <a href="/api/v1/measurements?gateway=${esc(g.key)}&amp;limit=10">Paginated measurements</a> · <a href="/gateways/${esc(g.key)}.md">Markdown</a> · <a href="/gateways/data.csv?gateway=${esc(g.key)}">Gateway CSV</a></nav>` +
      (core.length ? measurementTable(core) : (g.awaiting_engine ? "<p>n/a — not measured in the current refresh. Results appear after validation; earlier runs remain in History.</p>" : "<p>n/a — this protocol pair is not measured.</p>")) +
      selected.map(m => `<dl class="record-facts"><dt>Build</dt><dd>${buildHtml(m.gateway_build, { fromSource })}</dd><dt>Measured</dt><dd>${timeHtml(m.measured_at)}</dd><dt>Published</dt><dd>${timeHtml(m.published_at)}</dd><dt>Age</dt><dd>${ageHtml(m)}</dd></dl><p><a href="${esc(m.provenance.run_url)}">Immutable run and definitions</a> · <a href="${esc(m.provenance.raw_result_url)}">Raw evidence (large download)</a></p>`).join("") +
      `<p><a href="/gateways/${esc(g.key)}/evidence">Complete evidence: all protocol pairs, configuration and history (large page)</a> · <a href="/api/v1/runs?family=${esc(g.key.replace(/-\d+$/, ""))}">Paginated historical runs</a> · <a href="/gateways/data">Data &amp; API guidance</a></p>`;
  }
  const dates = [...new Set(records.map(r => r.measured_at).filter(Boolean))].sort();
  const runs = [...new Set(records.map(r => r.run_id).filter(Boolean))];
  const notes = noteIndex();
  // Instrument identity is usually one set for every cell of a run: list each distinct set once.
  const instruments = [...new Map(records.map(m => {
    const i = { harness_commit: m.test.harness_commit, mock_sha256: m.test.mock_sha256, load_generator_sha256: m.test.load_generator_sha256 };
    // Engine v1.1 rig and gateway identity, only when the run recorded it (older runs keep their old set).
    for (const k of ["somaxconn", "tcp_max_syn_backlog", "gateway_provenance"]) if (k in m.test) i[k] = m.test[k];
    if (m.test.qualification && "attempts" in m.test.qualification) i.qualification_attempts = m.test.qualification.attempts;
    return [JSON.stringify(i), i];
  })).values()];
  const rigExtra = i => [["somaxconn", "net.core.somaxconn"], ["tcp_max_syn_backlog", "net.ipv4.tcp_max_syn_backlog"],
    ["qualification_attempts", "Box qualification attempts"], ["gateway_provenance", "Gateway build receipt"]]
    .filter(([k]) => k in i).map(([k, label]) => `<dt>${mdSpan(esc(label), k)}</dt><dd>${factsHtml(i[k])}</dd>`).join("");
  const probeRows = records.map(m => [pairHtml(m.test.ingress_protocol, m.test.upstream_protocol),
    mdSpan(esc(stateLabel(m.status.state)), m.status.state),
    m.status.probe_http_status ? esc(m.status.probe_http_status) : "—",
    m.status.reason ? noteHtml(m.status.reason, notes) : "—",
    m.status.detail ? noteHtml(m.status.detail, notes) : "—",
    m.status.evidence_scope ? noteHtml(m.status.evidence_scope, notes) : "—",
    m.status.response_excerpt ? `<code>${esc(m.status.response_excerpt)}</code>` : "—",
    buildHtml(m.gateway_build, { fromSource })]);
  return `<h1>${esc(g.display)} benchmark measurements</h1>${disclosureHtml}` +
    `<p>${esc(g.lang)} · recorded build: ${buildHtml(g.matrix?.build || g.version?.ref, { fromSource })}. Measured: ${dates.map(d => timeHtml(d)).join(", ") || "not measured"}.</p>` +
    `<p><a href="/api/v1/gateways/${esc(g.key)}">JSON</a> · <a href="/gateways/${esc(g.key)}.md">Markdown</a> · <a href="/gateways/data.csv?gateway=${esc(g.key)}&amp;compact=1">CSV</a> · <a href="${esc(g.repo)}">Project source</a> · <a href="/gateways">Interactive dashboard</a></p>` +
    `<p>Each row below names its protocol pair. A representative dashboard cell may differ between gateways; compare an identical pair before interpreting a ranking. Unavailable metrics retain their reason; below-resolution readings are not exact zeroes.</p>` +
    `<h2>Published measurements</h2>${measurementTable(records, notes)}` +
    `<h2>Probe evidence and instrument provenance</h2><p>Historical capability notes may reference an earlier product version. They are not a new probe or a current support claim. Full RSS samples remain in the immutable raw JSON.</p>` +
    table(["Ingress → upstream", "Cell state", "Probe HTTP status", "Recorded reason", "Recorded detail", "Evidence scope", "Response excerpt", "Tested build"], probeRows) +
    `<dl class="record-facts">${instruments.map(i => `<dt>Harness commit</dt><dd><code>${esc(i.harness_commit || "not recorded")}</code></dd><dt>Mock SHA-256</dt><dd><code>${esc(i.mock_sha256 || "not recorded")}</code></dd><dt>Load generator SHA-256</dt><dd><code>${esc(i.load_generator_sha256 || "not recorded")}</code></dd>${rigExtra(i)}`).join("")}</dl>` +
    streamRungTable(records) +
    notesHtml(notes) +
    `<h2>Evidence for the current measurements</h2><ul>${runs.map(id => `<li><a href="/gateways/runs/${id}">${esc(id)}</a> · <a href="/api/v1/runs/${id}/raw.json">complete raw record including RSS series</a></li>`).join("")}</ul>` +
    `<h2>Configuration recorded by the run</h2><pre>${esc(g.ootb_config || "Not recorded.")}</pre>` +
    `<h2>Compare a shared protocol cell</h2><ul>${gateways.filter(other => other.key !== g.key).map(other => `<li><a href="/gateways/compare/${esc(g.key)}-vs-${esc(other.key)}">${esc(g.display)} vs ${esc(other.display)}</a></li>`).join("")}</ul>` +
    `<h2>Historical measurements</h2><ul>${histories.map(r => `<li><a href="/gateways/runs/${r.id}">${timeHtml(r.measured_at, "Date not recorded")} — ${buildHtml(r.build, { fromSource })}</a></li>`).join("")}</ul>`;
}
// Engine v1.1 per stream-sweep rung: lanes per HTTP status and lanes that got no response, as recorded.
// Rendered only when some record carries them, so pages built from older runs are unchanged.
export function streamRungTable(records) {
  const rows = records.flatMap(m => (m.stream_rung_diagnostics || []).map(r => [pairHtml(m.test.ingress_protocol, m.test.upstream_protocol),
    r.conc == null ? "—" : numberHtml(r.conc),
    r.http_status_counts && Object.keys(r.http_status_counts).length
      ? mdSpan(Object.entries(r.http_status_counts).map(([s, n]) => `HTTP ${esc(s)}: ${esc(formatNumber(n))}`).join(", "), JSON.stringify(r.http_status_counts))
      : "—",
    r.stream_errors_no_response == null ? "—" : numberHtml(r.stream_errors_no_response)]));
  return rows.length ? `<h2>Stream sweep diagnostics</h2><p>Per streaming-sweep concurrency, as recorded by the engine: lanes by final HTTP status, and lanes that received no response.</p>` +
    table(["Ingress → upstream", "Concurrency", "Lanes by HTTP status", "Lanes with no response"], rows) : "";
}
const REASON_LABELS = {
  requested_cell_not_measured: "requested cell not measured", requested_cell_not_served: "requested cell not served",
  metric_not_valid_on_both_cells: "metric not valid on both cells", clean_harness_not_established: "clean harness not established",
  missing_raw_evidence: "missing raw evidence", rig_qualification_not_established: "rig qualification not established",
  throughput_is_only_a_lower_bound: "throughput is only a lower bound", stream_workload_equivalence_not_established: "stream workload equivalence not established",
};
const reasonHtml = r => mdSpan(esc(REASON_LABELS[r] || r.replaceAll("_", " ")), r);
export function comparisonArticle(comparison, names = {}) {
  const { comparison: q, comparability: c, results } = comparison;
  const name = id => names[id] || id;
  const params = new URLSearchParams({ a: q.gateway_a, b: q.gateway_b, ingress: q.ingress_protocol, egress: q.upstream_protocol, metric: q.metric });
  if (q.metric === "throughput_rps") params.set("bound", q.bound_ms == null ? "none" : String(q.bound_ms));
  const ratio = results.gateway_b_over_a;
  return `<h1>${mdSpan(`${esc(name(q.gateway_a))} vs ${esc(name(q.gateway_b))}`, `${q.gateway_a} vs ${q.gateway_b}`)}</h1>${disclosureHtml}` +
    `<p>Requested cell: ${pairHtml(q.ingress_protocol, q.upstream_protocol)}. Metric: ${metricLabelHtml(q.metric)}.${q.metric === "throughput_rps" ? ` Tail bound: ${q.bound_ms == null ? "unbounded" : `${esc(q.bound_ms)} ms`}.` : ""}</p>` +
    `<p><strong>Comparability: ${mdSpan(esc(stateLabel(c.status)), c.status)}</strong>. ${esc(POLICY.description)}</p>` +
    (c.reasons.length ? `<p>Reasons: ${c.reasons.map(reasonHtml).join(", ")}.</p>` : "") +
    `<ul>${c.warnings.map(w => `<li>${esc(w)}</li>`).join("")}</ul>` +
    `<p>Ratio (${mdSpan(`${esc(name(q.gateway_b))} ÷ ${esc(name(q.gateway_a))}`, "B ÷ A")}): ${ratio == null ? "not reported" : typeof ratio === "number" ? mdSpan(`<span title="${esc(ratio)}">${esc(formatNumber(ratio))}</span>`, ratio) : esc(ratio)}. No statistical significance is asserted.</p>` +
    `<p><a href="/api/v1/compare?${esc(params.toString())}">Comparison JSON</a> · <a href="/gateways/${esc(q.gateway_a)}">All ${esc(name(q.gateway_a))} evidence</a> · <a href="/gateways/${esc(q.gateway_b)}">All ${esc(name(q.gateway_b))} evidence</a></p>` +
    [results.gateway_a, results.gateway_b].map(m => m ? `<h2>${mdSpan(esc(name(m.gateway_id)), m.gateway_id)}</h2><p>Build ${buildHtml(m.gateway_build)}. Measured ${timeHtml(m.measured_at)}. ${m.provenance.run_url ? `<a href="${esc(m.provenance.run_url)}">Run evidence</a>` : "Raw provenance unavailable."}</p>${measurementTable([m])}` : "<p>Requested cell was not measured.</p>").join("");
}
export function dataArticle(publication) {
  return `<h1>Benchmark data and API</h1>${disclosureHtml}
<p>Published collection: ${esc(publication.dataset_id)}. Generated ${timeHtml(publication.generated_at)}; measurements ${timeHtml(publication.measured_range.earliest)} through ${timeHtml(publication.measured_range.latest)}. Metadata refresh is separate from measurement time. This is a collection of independent runs, not a claim of one complete simultaneous field run.</p>
<p><a href="/api/v1">API discovery</a> · <a href="/api/v1/benchmarks/latest">Latest JSON</a> · <a href="/gateways/index.md">Markdown</a> · <a href="/gateways/data.csv?compact=1">Compact CSV</a> · <a href="/openapi.json">OpenAPI</a> · <a href="/api/v1/schema">JSON schemas</a> · <a href="/gateways/runs">Historical runs</a></p>
<p><a href="/llms.txt">Agent discovery (llms.txt)</a> · <a href="/llms-full.txt">Full agent guide</a> · <a href="/api/v1/schema/measurement.json">Version 1 measurement schema</a></p>
<h2>Per-gateway exports</h2><ul>${publication.gateways.map(g => `<li>${esc(g.name)}: <a href="${g.url}.md">Markdown</a> · <a href="/gateways/data.csv?gateway=${esc(g.id)}&amp;compact=1">Compact CSV</a></li>`).join("")}</ul><h2>Compact results and freshness</h2><p>Start with <a href="/api/v1/summary">the compact summary</a> (one protocol pair, core metrics and provenance). Filter with gateway, ingress and egress. Full collections may exceed retrieval-tool limits; follow paginated measurement links for details. Each compact result and comparison measurement includes measured_at, published_at, generated_at, age, age_days and age_as_of. Age is measured at collection generation, not at request time. published_at is recorded for new releases when publishing begins; it remains null for local builds and older releases without that record.</p><p>current_metadata contains repository stars and their stars_as_of date. Legacy gateway.stars fields refer to that same metadata snapshot, not the benchmark date. Stars at measurement time were not recorded and are not inferred.</p><h2>Context-efficient retrieval</h2><p>Use <code>/api/v1/measurements?compact=1&amp;limit=10</code> to receive definitions once, keyed by definition_id. Compact summary uses the same dictionary pattern. Prefer each gateway’s Markdown and <code>/gateways/data.csv?gateway=ID&amp;compact=1</code> links; the CSV references <a href="/api/v1/definitions">the definition catalog</a>. Full CSV and full JSON are bulk exports. <a href="/api/v1/runs">The paginated run catalog</a> supports exact gateway IDs and families such as <code>family=busbar</code>.</p><h2>Retrieval</h2><p>All resources are public and read-only. HTML contains measurements without JavaScript. Explicit Markdown and JSON links avoid content negotiation. Query endpoints return 400 for invalid filters and 404 for unknown entities.</p>
<pre>GET /api/v1/summary?ingress=openai&amp;egress=openai
GET /api/v1/gateways
GET /api/v1/gateways/busbar
GET /api/v1/measurements?gateway=busbar&amp;ingress=openai&amp;egress=openai&amp;limit=50
GET /api/v1/compare?a=busbar&amp;b=bifrost&amp;ingress=openai&amp;egress=openai&amp;metric=added_latency_p99_us</pre>
<p>Measurements support gateway, ingress, egress, metric, limit (1–100), and offset (0 or greater). Responses are capped at 1 MiB of uncompressed UTF-8 JSON, so a page may contain fewer than limit rows. Follow the returned next URL until null; do not infer completion from row count. An individual oversized record returns 413 with narrowing guidance. Comparisons require two distinct published gateway IDs and a recorded metric. The default protocol pair is OpenAI → OpenAI and the default metric is added_latency_p99_us. Available gateway IDs and metrics are listed by the API.</p>
<h2>Compare and cite using HTTP only</h2>
<ol><li>Discover gateway IDs with <a href="/api/v1/gateways">the catalog</a> and choose an explicit ingress/upstream pair. The API calls the upstream protocol <code>egress</code>.</li>
<li>Retrieve <a href="/api/v1/measurements?ingress=openai&amp;egress=anthropic&amp;limit=100">OpenAI → Anthropic measurements</a>; follow <code>next</code> until it is null. Unavailable values are not zero.</li>
<li>Request <a href="/api/v1/compare?a=busbar&amp;b=litellm-python&amp;ingress=openai&amp;egress=anthropic&amp;metric=added_latency_p99_us">Busbar vs LiteLLM on that pair</a> and inspect <code>comparability.status</code>. Only <code>comparable</code> supports a ratio; <code>incomparable</code> and <code>insufficient_evidence</code> include reasons. Matching protocol names alone is not enough.</li>
<li>Cite each record’s <code>provenance.run_url</code>, tested <code>gateway_build</code>, <code>measured_at</code>, protocol pair and metric definition. <code>provenance.raw_result_url</code> and <code>raw_sha256</code> identify the exact source bytes. Include <a href="/gateways/method">the methodology</a> and operator disclosure.</li></ol>
<p><strong>The default dashboard is not a like-for-like ranking.</strong> Its representative protocol cell can differ between products. The API returns per-cell evidence and conservative pairwise comparisons; it does not publish an unconditional fastest-gateway ranking. These are independent gateway runs against a local mock, not end-to-end model latency.</p>
<h2>Interpretation</h2><p>${esc(POLICY.description)}</p><p>Every metric carries its original definition, state, units in its field name, and provenance when recorded. Missing evidence remains null. A measured zero is retained; a below-resolution reading has a null value and an explicit state. Throughput readings carry their tail-latency bound, operating concurrency, and lower-bound status. Compare conditions before ranking.</p>
<h2>Immutable evidence</h2><p>Run IDs include the SHA-256 of their exact raw JSON bytes. Raw files contain the complete samples; dashboard drawing series are thinned. Corrected raw content receives a different ID. The latest collection points to individual runs and retains their actual dates. A collection ID identifies its generated evidence representation; it is distinct from a per-gateway run ID.</p>
<h2>Dates and licensing</h2><p>published_at is the release timestamp recorded when publishing begins, not the original publication date of historical measurements; generated_at is collection build time; measured_at belongs to the instrument; stars_as_of belongs to metadata. An original first-publication timestamp is not inferred. Original measurements and dataset compilation are licensed under <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>. Credit On the Bench — Busbar team, link the source and license, and indicate modifications. <a href="/gateways/license">Dataset license and scope</a>; software and third-party materials retain their own terms.</p>`;
}

export function schemaIndex() {
  return { schema_version: "1.0", schemas: { measurement: ORIGIN + "/api/v1/schema/measurement.json" }, states: METRIC_STATES, metric_states: METRIC_STATES, cell_states: CELL_STATES, comparison_policy: POLICY };
}
export { measurementSchema, openapi } from "./api-contract.mjs";

// A recorded object (the run's instrument provenance) as nested definition lists instead of a JSON dump.
// Keys keep their recorded names on hover and in the Markdown twin; the raw JSON stays one link away.
const FACT_LABELS = { arch: "Architecture", release_url: "Release URL", engine: "Engine (harness)", commit: "Commit", dirty: "Uncommitted changes", mock: "Mock upstream", origin: "Origin", sha256: "SHA-256", asset_updated_at: "Asset updated", ugen: "Load generator", box_qualify: "Box qualification", band_pct: "Band (%)", baseline_rps: "Baseline (req/s)", baseline_samples: "Baseline samples", concurrency: "Concurrency", drift_pct: "Drift (%)", observed_absent_detail: "Observation absence detail", observed_absent_reason: "Observation absence reason", observed_rps: "Observed (req/s)", outcome: "Outcome", somaxconn: "net.core.somaxconn", tcp_max_syn_backlog: "net.ipv4.tcp_max_syn_backlog", attempts: "Attempts", rps: "Rate (req/s)" };
export function factsHtml(value) {
  if (value == null) return "Not recorded";
  if (typeof value !== "object") {
    if (typeof value === "number") return numberHtml(value);
    if (typeof value === "boolean") return mdSpan(value ? "Yes" : "No", String(value));
    return /^[0-9a-f]{12,}$/i.test(value) || /^[a-z]+:\/\//.test(value) ? `<code>${esc(value)}</code>` : esc(value);
  }
  const entries = Array.isArray(value) ? value.map((v, i) => [String(i + 1), v]) : Object.entries(value);
  if (!entries.length) return "None recorded";
  return `<dl class="record-facts">${entries.map(([k, v]) => `<dt>${mdSpan(esc(FACT_LABELS[k] || stateLabel(k)), k)}</dt><dd>${factsHtml(v)}</dd>`).join("")}</dl>`;
}
