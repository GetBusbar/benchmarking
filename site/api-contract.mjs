// SPDX-License-Identifier: Apache-2.0
import { ORIGIN, POLICY, PROTOCOLS } from "./public-data.mjs";
const str = { type: "string" }, num = { type: "number" }, bool = { type: "boolean" };
const nullable = schema => ({ anyOf: [schema, { type: "null" }] });
const ns = nullable(str), nn = nullable(num);
const ref = name => ({ $ref: `#/components/schemas/${name}` });
const array = items => ({ type: "array", items });
const object = (properties, required = Object.keys(properties)) => ({ type: "object", properties, required });
const map = value => ({ type: "object", additionalProperties: value });
// Raw instrument records and legacy dashboard projections are extensible. Their
// stable identity is typed; arbitrary historical payloads remain explicitly JSON.
const jsonValue = { description: "Version-specific JSON from the recorded instrument; consult its pinned source.", anyOf: [{ type: "object", additionalProperties: true }, { type: "array", items: {} }, str, num, bool, { type: "null" }] };
const freshness = {
  measured_at: ns, generated_at: str,
  published_at: { ...ns, description: "Actual deployment timestamp, null when not recorded. Never inferred from measurement or build time." },
  age_days: nullable({ type: "integer", minimum: 0 }), age_as_of: str,
  age: { type: "string", description: "Human-readable measurement age relative to age_as_of, not request time." },
};
export function schemas() {
  return {
    JsonValue: jsonValue,
    Disclosure: object({ operator: str, operator_is_entrant: bool }),
    Policy: object({ id: str, description: str }),
    CurrentMetadata: object({ stars: nullable({ type: "integer", minimum: 0 }), stars_as_of: ns, first_commit: ns }),
    Status: object({ state: str, reason: ns, detail: ns }),
    Metric: object({ value: nn, state: { type: "string", description: "valid, below_resolution, or the recorded absence/failure reason. Only valid values are quantitative evidence." }, reason: ns, definition: ns, section: str, certified: bool, suppressed: bool, note: str }, ["value", "state", "reason"]),
    Provenance: object({ source_url: str, raw_result_url: ns, run_url: ns, methodology_url: str, raw_sha256: ns }),
    ProtocolPair: object({ ingress_protocol: { type: "string", enum: PROTOCOLS }, upstream_protocol: { type: "string", enum: PROTOCOLS } }),
    TestConfiguration: object({ ingress_protocol: str, upstream_protocol: str, hardware: ns, architecture: ns, harness_commit: ns, harness_dirty: nullable(bool), mock_sha256: ns, load_generator_sha256: ns, qualification: nullable(ref("JsonValue")), definitions: map(str), sweep_duration_s: nn, sweep_ttft_ms: nn, memory_protocol: ns, memory_concurrency: nn, memory_load_s: nn, cost_concurrency: nn }),
    Frontier: object({ bound_ms: nn, concurrency: nn, lower_bound: bool, p99_us: nn, rps: nullable(ref("Metric")) }, ["rps"]),
    Measurement: object({ schema_version: { const: "1.0" }, measurement_id: ns, gateway_id: str, gateway_build: ns, run_id: ns, ...freshness, status: ref("Status"), test: ref("TestConfiguration"), metrics: map(ref("Metric")), frontier: array(ref("Frontier")), provenance: ref("Provenance"), disclosure: ref("Disclosure") }, ["schema_version", "measurement_id", "gateway_id", "gateway_build", "run_id", "measured_at", "status", "test", "metrics", "frontier", "provenance", "disclosure"]),
    GatewayLink: object({ id: str, name: str, url: str, json_url: str, current_metadata: ref("CurrentMetadata") }),
    Catalog: object({ schema_version: str, gateways: array(ref("GatewayLink")), disclosure: ref("Disclosure") }),
    Discovery: object({ schema_version: str, links: map(str), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    Run: object({ id: str, sha256: str, gateway_id: str, measured_at: ns, build: ns }),
    RunResource: { allOf: [ref("Run"), object({ schema_version: str, raw_result_url: str, source_url: str, disclosure: ref("Disclosure") })] },
    RawRun: object({ gateway: str, build: ns, measured_at: ns, matrix: map(ref("JsonValue")), rig: ref("JsonValue"), definitions: map(str) }, ["gateway", "matrix"]),
    Gateway: object({ key: str, display: str, lang: str, repo: ns, version: ref("JsonValue"), stars: { ...nn, deprecated: true, description: "Legacy alias for current_metadata.stars, not stars at measurement time." }, stars_as_of: { ...ns, deprecated: true }, matrix: ref("JsonValue"), current: bool }, ["key"]),
    GatewayEvidence: object({ schema_version: str, gateway: ref("Gateway"), current_metadata: ref("CurrentMetadata"), metadata_scope: str, measurements: array(ref("Measurement")), history: array(ref("Run")), generated_at: str, disclosure: ref("Disclosure") }, ["schema_version", "gateway", "measurements", "history", "disclosure"]),
    EvidenceCollection: object({ schema_version: str, collection_kind: { const: "independent_gateway_runs" }, dataset_id: str, measurements: array(ref("Measurement")), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    LatestCollection: { allOf: [ref("EvidenceCollection"), object({ generated_at: str, published_at: ns, measured_range: object({ earliest: ns, latest: ns }), metadata_as_of: ns, license: ns, archive_history_included: bool, source_url: str, immutable_url: str, gateways: array(ref("GatewayLink")), metrics: array(str) })] },
    BenchmarkResource: { anyOf: [ref("EvidenceCollection"), ref("RunResource")] },
    MeasurementPage: object({ schema_version: str, dataset_id: str, total: { type: "integer", minimum: 0 }, offset: { type: "integer", minimum: 0 }, limit: { type: "integer", minimum: 1, maximum: 100 }, next: ns, measurements: array(ref("Measurement")) }),
    CompactResult: object({ gateway_id: str, name: str, gateway_build: ns, ...freshness, current_metadata: ref("CurrentMetadata"), status: ref("Status"), metrics: map(ref("Metric")), provenance: ref("Provenance"), evidence_url: str }),
    Summary: object({ schema_version: str, dataset_id: str, generated_at: str, published_at: ns, selected_protocol_pair: ref("ProtocolPair"), results: array(ref("CompactResult")), comparison_policy: ref("Policy"), disclosure: ref("Disclosure"), interpretation: str }),
    Comparison: object({
      comparison: object({ gateway_a: str, gateway_b: str, ingress_protocol: str, upstream_protocol: str, metric: str, bound_ms: nn }, ["gateway_a", "gateway_b", "ingress_protocol", "upstream_protocol", "metric"]),
      comparability: object({ status: { enum: ["comparable", "incomparable", "insufficient_evidence"] }, policy: str, conditions_matched: array(str), reasons: array(str), warnings: array(str) }),
      results: object({ gateway_a: nullable(ref("Measurement")), gateway_b: nullable(ref("Measurement")), gateway_b_over_a: nn }),
      interpretation: object({ scope: str, generalizable_to_real_model_latency: { const: false }, statistical_significance: { const: "not_established" } }), disclosure: ref("Disclosure"),
    }),
    Methodology: object({ schema_version: str, url: str, markdown: str, definitions: ref("JsonValue"), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    SchemaIndex: object({ schema_version: str, schemas: map(str), states: array(str), comparison_policy: ref("Policy") }),
    Error: object({ error: str, status: { type: "integer", enum: [400, 404, 500] } }),
  };
}
export function measurementSchema() {
  const components = schemas();
  // Standalone schema uses $defs rather than OpenAPI components.
  return JSON.parse(JSON.stringify({ $schema: "https://json-schema.org/draft/2020-12/schema", $id: `${ORIGIN}/api/v1/schema/measurement.json`, ...ref("Measurement"), $defs: components }).replaceAll("#/components/schemas/", "#/$defs/"));
}
export function openapi() {
  const query = (name, description, schema = str, example, required = false) => ({ name, in: "query", required, description, schema, ...(example === undefined ? {} : { example }) });
  const protocol = name => query(name, `${name === "egress" ? "Upstream" : "Client ingress"} protocol. Summary and comparison default to openai; measurements omit this filter by default.`, { type: "string", enum: PROTOCOLS }, "openai");
  const gateway = name => query(name, "Published gateway ID from /api/v1/gateways. Comparison a and b must differ.", { type: "string", minLength: 1, pattern: "^[a-z0-9-]+$" }, name === "b" ? "litellm-python" : "busbar", name === "a" || name === "b");
  const metric = query("metric", "Recorded metric key from latest.metrics, or throughput_rps. Comparison defaults to added_latency_p99_us; measurements return all metrics by default.", { type: "string", minLength: 1 }, "added_latency_p99_us");
  const path = { name: "id", in: "path", required: true, description: "Exact identifier from the catalog or run evidence; unknown IDs return 404.", schema: { type: "string", minLength: 1 }, example: "busbar" };
  const get = (operationId, description, schema, parameters = []) => ({ get: { operationId, description, parameters, responses: {
    "200": { description: "Published evidence. Null means unknown or unavailable; never coerce to zero.", content: { "application/json": { schema: ref(schema) } } },
    "400": { description: "Invalid, empty, repeated or unsupported query parameter, invalid bound, or identical comparison IDs.", content: { "application/json": { schema: ref("Error"), example: { error: "Unknown protocol: invalid", status: 400 } } } },
    "404": { description: "Unknown gateway, run or collection.", content: { "application/json": { schema: ref("Error") } } },
    "500": { description: "Publication artifact unavailable.", content: { "application/json": { schema: ref("Error") } } },
  } } });
  return { openapi: "3.1.0", info: { title: "On the Bench", version: "1.1.0", description: `${POLICY.description} Start with /api/v1/summary for bounded retrieval. Full evidence can exceed 4 MB. Actual publication time is unknown when published_at is null; generated_at is build time. Dynamic filters reject unknown and repeated parameters.` }, servers: [{ url: ORIGIN }], components: { schemas: schemas() }, paths: {
    "/api/v1": get("discovery", "API links and interpretation policy", "Discovery"),
    "/api/v1/summary": get("summary", "Compact core latency measurements for one selected protocol pair, with freshness and provenance; not a ranking.", "Summary", [gateway("gateway"), protocol("ingress"), protocol("egress")]),
    "/api/v1/gateways": get("gateways", "Gateway catalog with dated repository metadata", "Catalog"),
    "/api/v1/gateways/{id}": get("gateway", "Gateway measurements, history and legacy dashboard projection", "GatewayEvidence", [path]),
    "/api/v1/benchmarks/latest": get("latest", "Large collection of independent runs; use summary or measurements for bounded retrieval", "LatestCollection"),
    "/api/v1/benchmarks/{id}": get("benchmark", "Immutable collection (dataset-<sha256>) or run metadata (run-<gateway>-<sha256>)", "BenchmarkResource", [{ ...path, example: "dataset-" + "0".repeat(64) }]),
    "/api/v1/measurements": get("measurements", "Paginated evidence. Follow next until null. Missing requested metrics remain absent.", "MeasurementPage", [gateway("gateway"), protocol("ingress"), protocol("egress"), metric, query("limit", "Maximum records per page", { type: "integer", minimum: 1, maximum: 100, default: 50 }, 10), query("offset", "Zero-based result offset", { type: "integer", minimum: 0, maximum: Number.MAX_SAFE_INTEGER, default: 0 }, 0)]),
    "/api/v1/compare": get("compare", "Inspect comparability.status before citing a ratio. Full source measurements and provenance are preserved.", "Comparison", [gateway("a"), gateway("b"), protocol("ingress"), protocol("egress"), metric, query("bound", "Only valid with metric=throughput_rps; none means unbounded. Matched operating concurrency is required.", { type: "string", enum: ["1", "5", "10", "50", "100", "none"], default: "10" }, "10")]),
    "/api/v1/methodology": get("methodology", "Method text plus definitions recorded by the instrument", "Methodology"),
    "/api/v1/schema": get("schemas", "Schema discovery", "SchemaIndex"),
    "/api/v1/runs/{id}/raw.json": get("rawRun", "Exact historical instrument JSON, schema may vary by harness version. Verify decoded bytes against raw_sha256.", "RawRun", [{ ...path, example: "run-busbar-" + "0".repeat(64) }]),
  } };
}
