// SPDX-License-Identifier: Apache-2.0
import families from "./gateway-families.json" with { type: "json" };
import { ORIGIN, POLICY, compactRecords } from "./public-data.mjs";
export const PAGE_MAX_BYTES = 1024 * 1024;
export const DATA_LICENSE = "https://creativecommons.org/licenses/by/4.0/";
export { METRIC_STATES, CELL_STATES } from "./states.mjs";
const bytes = text => new TextEncoder().encode(text).length;
export function measurementPage(rows, { datasetId, generatedAt = null, publishedAt = null, limit, offset, url, compact = false }, maxBytes = PAGE_MAX_BYTES) {
  const page = { schema_version: "1.0", dataset_id: datasetId, generated_at: generatedAt, published_at: publishedAt, total: rows.length, offset, limit, returned: 0, max_response_bytes: maxBytes, request_url: String(url), applied_filters: Object.fromEntries([...new URL(url).searchParams].filter(([key]) => ["gateway", "ingress", "egress", "metric"].includes(key))), next: null, measurements: [] };
  const next = new URL(url);
  function advance(count) {
    next.searchParams.set("offset", String(offset + count)); next.searchParams.set("limit", String(limit));
    page.returned = count; page.next = offset + count < rows.length ? next.href : null;
  }
  // Serialize each record once. Reserve the actual envelope's maximum size, so
  // UTF-8 bytes (not JS characters or compressed bytes) determine the budget.
  advance(Math.min(limit, rows.length - offset));
  let used = bytes(JSON.stringify(page)) + 1;
  for (const row of rows.slice(offset, offset + limit)) {
    const size = bytes(JSON.stringify(row)) + (page.measurements.length ? 1 : 0);
    if (used + size > maxBytes) break;
    page.measurements.push(row); used += size;
  }
  if (!page.measurements.length && offset < rows.length) return null;
  advance(page.measurements.length);
  const response = () => compact ? { ...page, ...compactRecords(page.measurements) } : page;
  // The final next link may contain fewer digits; validate the exact body too.
  while (bytes(JSON.stringify(response()) + "\n") > maxBytes && page.measurements.length) { page.measurements.pop(); advance(page.measurements.length); }
  return !page.measurements.length && offset < rows.length ? null : response();
}
export function retrievalGuide(fullBytes) {
  return { start_here: `${ORIGIN}/api/v1/summary`, next_step: `${ORIGIN}/api/v1/measurements?compact=1&limit=10`,
    measurements_max_response_bytes: PAGE_MAX_BYTES, full_collection_bytes: fullBytes,
    full_collection_use: "Bulk download only; may exceed retrieval-tool limits. Start with summary; follow measurements.next until null. A page may contain fewer rows than limit. Never infer completion from row count.",
    license: DATA_LICENSE };
}
export function semanticGuide(publication) {
  const lines = ["# On the Bench — measurement guide", "", "Operated by the Busbar team; Busbar is also an entrant.", "", "## Retrieval", "", `Start with ${ORIGIN}/api/v1/summary. For per-cell evidence use ${ORIGIN}/api/v1/measurements?compact=1&limit=10 and follow next until null. Pages are limited to 1 MiB of uncompressed UTF-8 JSON. The full collection is a bulk download, not the preferred agent entry point.`, "", "## Interpretation", "", POLICY.description, "Null and below-resolution values are not zero. These are measurements against a local mock, not model inference latency. Do not rank different protocol pairs or configurations. Check the comparison API before calculating a ratio.", "", "## Dates and reuse", "", `Collection: ${publication.dataset_id}`, `Generated: ${publication.generated_at}`, `Release timestamp: ${publication.published_at || "not recorded"}. This is not the original publication date of historical measurements.`, `Dataset license: CC BY 4.0 (${DATA_LICENSE}). Credit On the Bench — Busbar team, link the source and license, and indicate modifications. Scope: ${ORIGIN}/gateways/license.`, "", "## Selected protocol results", "", "The following records select OpenAI ingress and OpenAI upstream explicitly. Other cells remain available through each evidence link."];
  const compact = compactRecords(publication.measurements.filter(m => m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai").map(m => ({ ...m, test: { ...m.test, definitions: {} }, metrics: Object.fromEntries(Object.entries(m.metrics).filter(([key]) => ["added_latency_p50_us", "added_latency_p99_us", "added_ttft_p99_us"].includes(key))) })));
  const summary = compact.measurements.filter(m => m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai");
  const field = (name, value) => lines.push(`- ${name}: ${JSON.stringify(value ?? null)}`);
  for (const g of publication.gateways) {
    const m = summary.find(m => m.gateway_id === g.id);
    lines.push("", `### ${g.id}`, "");
    field("Gateway", g.name); field("Ingress protocol", "openai"); field("Upstream protocol", "openai");
    field("Build", m?.gateway_build); field("Measured at", m?.measured_at); field("Age", m?.age); field("Age as of", m?.age_as_of);
    field("Cell state", m?.status.state || "not_measured");
    field("Current repository metadata", g.current_metadata);
    field("Evidence URL", `${ORIGIN}/api/v1/measurements?gateway=${g.id}&ingress=openai&egress=openai&limit=10`);
    field("Run URL", m?.provenance.run_url); field("Raw JSON URL", m?.provenance.raw_result_url); field("Raw SHA-256", m?.provenance.raw_sha256);
    for (const key of ["added_latency_p50_us", "added_latency_p99_us", "added_ttft_p99_us"]) {
      const metric = m?.metrics[key]; lines.push("", `#### ${g.id}: ${key}`, "");
      field("Metric", key); field("Unit", "microseconds"); field("Value", metric?.value); field("State", metric?.state || "not_measured"); field("Reason", metric?.reason); field("Definition ID", metric?.definition_id);
    }
  }
  lines.push("", "## Recorded metric definitions", "");
  for (const [id, text] of Object.entries(compact.definitions)) lines.push(`### ${id}`, "", text, "");
  lines.push("", "## Canonical methodology", "", `${ORIGIN}/gateways/method`, `${ORIGIN}/api/v1/methodology`, "Each record supplies its recorded definitions, configuration, harness commit and raw evidence. Do not substitute current harness behavior for historical run behavior.", "");
  return lines.join("\n");
}

export function compactCsv(records, allRecords, datasetId) {
  const catalog = compactRecords(allRecords);
  const ids = new Map(Object.entries(catalog.definitions).map(([id, text]) => [text, id]));
  const columns = ["gateway_id", "gateway_build", "measured_at", "ingress_protocol", "upstream_protocol", "metric", "value", "state", "reason", "definition_id", "bound_ms", "concurrency", "lower_bound", "run_url", "definitions_url"];
  const rows = [columns];
  for (const m of records) {
    const row = (key, metric, definition, bound = null, concurrency = null, lower = null) => rows.push([m.gateway_id, m.gateway_build, m.measured_at, m.test.ingress_protocol, m.test.upstream_protocol, key, metric?.value, metric?.state, metric?.reason, ids.get(definition), bound, concurrency, lower, m.provenance.run_url, `${ORIGIN}/api/v1/definitions/${datasetId}`]);
    for (const [key, metric] of Object.entries(m.metrics)) row(key, metric, metric.definition);
    for (const f of m.frontier || []) row("throughput_rps", f.rps, m.test.definitions?.["perf.frontier"], f.bound_ms, f.concurrency, f.lower_bound);
  }
  const quote = value => '"' + String(value ?? "").replace(/^[=+@\-]/, "'$&").replace(/"/g, '""') + '"';
  return rows.map(row => row.map(quote).join(",")).join("\r\n") + "\r\n";
}
export const gatewayFamily = id => Object.entries(families).find(([, aliases]) => aliases.includes(id))?.[0] || id;
