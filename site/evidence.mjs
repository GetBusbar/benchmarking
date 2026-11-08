// SPDX-License-Identifier: Apache-2.0
import { esc, ORIGIN } from "./render.mjs";
import { DISCLOSURE, POLICY } from "./public-data.mjs";

import { CELL_STATES, METRIC_STATES } from "./states.mjs";

export const disclosureHtml = `<p class="muted">Operated by the ${esc(DISCLOSURE.operator)}; Busbar is also an entrant. Gateway overhead against a local mock is not end-to-end model latency. <a href="/gateways/method">Methodology and limitations</a>.</p>`;
const table = (headers, rows) => `<div class="table-scroll"><table><thead><tr>${headers.map(h => `<th>${esc(h)}</th>`).join("")}</tr></thead><tbody>${rows.map(r => `<tr>${r.map(c => `<td>${c}</td>`).join("")}</tr>`).join("")}</tbody></table></div>`;
const metricText = m => m?.state === "valid" ? `${esc(m.value)}${m.note ? ` (${esc(m.note)})` : ""}` : esc(m?.state || "not_measured");
export function measurementTable(measurements) {
  return table(["Ingress → upstream", "Metric", "Value", "State / reason"], measurements.flatMap(m => {
    const path = `${m.test.ingress_protocol} → ${m.test.upstream_protocol}`;
    if (!Object.keys(m.metrics).length) return [[esc(path), "—", "—", esc(m.status.state + (m.status.detail ? `: ${m.status.detail}` : ""))]];
    const rows = Object.entries(m.metrics).map(([key, v]) => [esc(path), esc(key), metricText(v), esc(v.reason || v.state)]);
    for (const f of m.frontier) rows.push([esc(path), `Throughput at p99 ${f.bound_ms == null ? "unbounded" : `≤ ${esc(f.bound_ms)} ms`}`, metricText(f.rps) + " req/s", `${f.lower_bound ? "lower bound; " : ""}concurrency ${esc(f.concurrency ?? "not recorded")}`]);
    return rows;
  }));
}
export function gatewayArticle(g, records, histories, gateways) {
  const dates = [...new Set(records.map(r => r.measured_at).filter(Boolean))].sort();
  const runs = [...new Set(records.map(r => r.run_id).filter(Boolean))];
  return `<h1>${esc(g.display)} benchmark measurements</h1>${disclosureHtml}` +
    `<p>${esc(g.lang)} · recorded build: ${esc(g.matrix?.build || g.version?.ref || "not recorded")}. Measurement timestamps: ${esc(dates.join(", ") || "not measured")}.</p>` +
    `<p><a href="/api/v1/gateways/${esc(g.key)}">JSON</a> · <a href="/gateways/${esc(g.key)}.md">Markdown</a> · <a href="/gateways/data.csv?gateway=${esc(g.key)}&amp;compact=1">CSV</a> · <a href="${esc(g.repo)}">Project source</a> · <a href="/gateways">Interactive dashboard</a></p>` +
    `<p>Each row below names its protocol pair. A representative dashboard cell may differ between gateways; compare an identical pair before interpreting a ranking. Unavailable metrics retain their reason; below-resolution readings are not exact zeroes.</p>` +
    `<h2>Published measurements</h2>${measurementTable(records)}` +
    `<h2>Probe evidence and instrument provenance</h2><p>Historical capability notes may reference an earlier product version. They are not a new probe or a current support claim. Full RSS samples remain in the immutable raw JSON.</p>${records.map(m => `<details><summary>${esc(m.test.ingress_protocol)} → ${esc(m.test.upstream_protocol)}: ${esc(m.status.state)}</summary><p>Tested build: ${esc(m.gateway_build)}.</p><pre>${esc(JSON.stringify({ status: m.status, harness_commit: m.test.harness_commit, mock_sha256: m.test.mock_sha256, load_generator_sha256: m.test.load_generator_sha256 }, null, 2))}</pre><a href="${esc(m.provenance.raw_result_url)}">Raw evidence including RSS series</a></details>`).join("")}` +
    `<h2>Evidence for the current measurements</h2><ul>${runs.map(id => `<li><a href="/gateways/runs/${id}">${esc(id)}</a> · <a href="/api/v1/runs/${id}/raw.json">complete raw record</a></li>`).join("")}</ul>` +
    `<h2>Configuration recorded by the run</h2><pre>${esc(g.ootb_config || "Not recorded.")}</pre>` +
    `<h2>Compare a shared protocol cell</h2><ul>${gateways.filter(other => other.key !== g.key).map(other => `<li><a href="/gateways/compare/${esc(g.key)}-vs-${esc(other.key)}">${esc(g.display)} vs ${esc(other.display)}</a></li>`).join("")}</ul>` +
    `<h2>Historical measurements</h2><ul>${histories.map(r => `<li><a href="/gateways/runs/${r.id}">${esc(r.measured_at || "Date not recorded")} — ${esc(r.build || "Build not recorded")}</a></li>`).join("")}</ul>`;
}
export function comparisonArticle(comparison) {
  const { comparison: q, comparability: c, results } = comparison;
  const params = new URLSearchParams({ a: q.gateway_a, b: q.gateway_b, ingress: q.ingress_protocol, egress: q.upstream_protocol, metric: q.metric });
  if (q.metric === "throughput_rps") params.set("bound", q.bound_ms == null ? "none" : String(q.bound_ms));
  return `<h1>${esc(q.gateway_a)} vs ${esc(q.gateway_b)}</h1>${disclosureHtml}` +
    `<p>Requested cell: ${esc(q.ingress_protocol)} → ${esc(q.upstream_protocol)}. Metric: ${esc(q.metric)}.${q.metric === "throughput_rps" ? ` Tail bound: ${q.bound_ms == null ? "unbounded" : `${esc(q.bound_ms)} ms`}.` : ""}</p>` +
    `<p><strong>Comparability: ${esc(c.status)}</strong>. ${esc(POLICY.description)}</p>` +
    (c.reasons.length ? `<p>Reasons: ${c.reasons.map(esc).join(", ")}.</p>` : "") +
    `<ul>${c.warnings.map(w => `<li>${esc(w)}</li>`).join("")}</ul>` +
    `<p>Ratio (B ÷ A): ${results.gateway_b_over_a == null ? "not reported" : esc(results.gateway_b_over_a)}. No statistical significance is asserted.</p>` +
    `<p><a href="/api/v1/compare?${esc(params.toString())}">Comparison JSON</a> · <a href="/gateways/${esc(q.gateway_a)}">All ${esc(q.gateway_a)} evidence</a> · <a href="/gateways/${esc(q.gateway_b)}">All ${esc(q.gateway_b)} evidence</a></p>` +
    [results.gateway_a, results.gateway_b].map(m => m ? `<h2>${esc(m.gateway_id)}</h2><p>Build ${esc(m.gateway_build)}. Measured ${esc(m.measured_at)}. ${m.provenance.run_url ? `<a href="${esc(m.provenance.run_url)}">Run evidence</a>` : "Raw provenance unavailable."}</p>${measurementTable([m])}` : "<p>Requested cell was not measured.</p>").join("");
}
export function dataArticle(publication) {
  return `<h1>Benchmark data and API</h1>${disclosureHtml}
<p>Published collection: ${esc(publication.dataset_id)}. Generated ${esc(publication.generated_at)}; measurements ${esc(publication.measured_range.earliest)} through ${esc(publication.measured_range.latest)}. Metadata refresh is separate from measurement time. This is a collection of independent runs, not a claim of one complete simultaneous field run.</p>
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
