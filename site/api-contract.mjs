// SPDX-License-Identifier: Apache-2.0
import { ORIGIN, POLICY, PROTOCOLS } from "./public-data.mjs";
import { METRIC_STATES, CELL_STATES, PAGE_MAX_BYTES } from "./retrieval.mjs";
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
  published_at: { ...ns, description: "Release timestamp recorded when publishing begins; not the first publication of historical measurements. Null for local builds or older unrecorded releases." },
  age_days: nullable({ type: "integer", minimum: 0 }), age_as_of: str,
  age: { type: "string", description: "Human-readable measurement age relative to age_as_of, not request time." },
};
export function schemas() {
  return {
    ActiveField: object({id:str,engine_commit:str,started_at:str,gateways:array(str)}),
    Trend: object({ tolerance_percent: { const: 5 }, change_percent: nn, state: { enum: ['improved','regressed','flat','not_comparable'] }, symbol: str, label: str, reasons: array(str), previous_run_id: ns }, ['tolerance_percent','change_percent','state','symbol','label']),
    HistoryPoint: object({run_id:str,measurement_id:ns,gateway_id:str,gateway_build:ns,measured_at:ns,engine:ns,metric:str,value:nn,state:str,reason:ns,definition_id:ns,disposition:str,trend:ref('Trend'),provenance:object({run_url:ns},['run_url'])},['run_id','gateway_id','gateway_build','measured_at','engine','value','state','disposition','provenance']),
    HistoryComparison: object({metric:str,before:nn,after:nn,status:{enum:['comparable','incomparable','insufficient_evidence']},reasons:array(str),change_percent:nn,trend:ref('Trend')}),
    HistoryPage: object({schema_version:str,catalog_id:str,projector:str,total:num,limit:num,offset:num,returned:num,next:ns,metrics:array(str),definitions:map(str),points:array(ref('HistoryPoint')),comparisons:array(ref('HistoryComparison'))}),
    FieldLink: object({field_id:str,dataset_id:str,name:str,published_at:ns,url:str}),
    FieldPage: object({schema_version:str,total:nn,limit:num,offset:num,next:ns,fields:array(ref('FieldLink'))}),
    FieldManifest: object({schema_version:str,field_id:str,dataset_id:str,roster:array(str),selected_measurements:array(object({gateway_id:str,measurement_id:ns,run_id:ns,ingress:str,egress:str})),comparison_policy:ref('Policy'),measured_range:nullable(object({earliest:ns,latest:ns}))}),
    Releases: object({schema_version:str,limit:num,next:ns,releases:array(ref('FieldLink'))},['schema_version','releases']),
    FieldRankings: object({schema_version:str,field_id:str,metric:str,ingress:str,egress:str,direction:ns,rostered:num,excluded:array(object({gateway_id:str,reasons:array(str)})),groups:array(object({group:num,eligible:num,rankings:array(object({gateway_id:str,value:num,rank:num,measurement_id:str,run_id:str}))})),interpretation:str}),
    JsonValue: jsonValue,
    Freshness: object(freshness, ["measured_at"]),
    MetricState: { type: "string", enum: METRIC_STATES },
    Retrieval: object({ start_here: str, next_step: str, measurements_max_response_bytes: { const: PAGE_MAX_BYTES }, full_collection_bytes: { type: "integer", minimum: 0 }, full_collection_use: str, license: str }),
    Disclosure: object({ operator: str, operator_is_entrant: bool }),
    Policy: object({ id: str, description: str }),
    CurrentMetadata: object({ stars: nullable({ type: "integer", minimum: 0 }), stars_as_of: ns, first_commit: ns }),
    CellState: { type: "string", enum: CELL_STATES, description: "served: passed; not_configured: probe did not complete correctly; not_declared: historical capability research, not a probe; untestable: harness limitation; unprobed_auth: authentication not tested; not_served: recorded refusal/failure; not_verified: inconclusive harness evidence; not_measured: no measurement. raw_state preserves the instrument token." },
    Status: object({ state: ref("CellState"), reason: ns, detail: ns, raw_state: { anyOf: [str, bool, { type: "null" }] }, evidence_scope: str, probe_http_status: ns, response_excerpt: ns }, ["state", "reason", "detail"]),
    Metric: object({ value: nn, state: { ...ref("MetricState"), description: "Only valid values are quantitative evidence; retain the reason for every absence." }, reason: ns, definition: ns, definition_id: ns, section: str, certified: bool, suppressed: bool, note: str }, ["value", "state", "reason"]),
    Provenance: object({ source_url: str, raw_result_url: ns, run_url: ns, methodology_url: str, raw_sha256: ns }),
    ProtocolPair: object({ ingress_protocol: { type: "string", enum: PROTOCOLS }, upstream_protocol: { type: "string", enum: PROTOCOLS } }),
    TestConfiguration: object({ ingress_protocol: str, upstream_protocol: str, hardware: ns, architecture: ns, harness_commit: ns, harness_dirty: nullable(bool), mock_sha256: ns, load_generator_sha256: ns, qualification: nullable(ref("JsonValue")), definitions: map(str), definition_ids: map(ns), sweep_duration_s: nn, sweep_ttft_ms: nn, memory_protocol: ns, memory_concurrency: nn, memory_load_s: nn, cost_concurrency: nn, somaxconn: { ...nn, description: "Engine v1.1: the box's effective net.core.somaxconn, recorded only when readable." }, tcp_max_syn_backlog: { ...nn, description: "Engine v1.1: the box's effective net.ipv4.tcp_max_syn_backlog, recorded only when readable." }, gateway_provenance: { ...jsonValue, description: "Engine v1.1: the source-built gateway binary's build receipt, copied verbatim; absent for image gateways and older runs." } }, ["ingress_protocol", "upstream_protocol", "hardware", "architecture", "harness_commit", "harness_dirty", "mock_sha256", "load_generator_sha256", "qualification", "definitions", "sweep_duration_s", "sweep_ttft_ms", "memory_protocol", "memory_concurrency", "memory_load_s", "cost_concurrency"]),
    Frontier: object({ bound_ms: nn, concurrency: nn, lower_bound: bool, p99_us: nn, samples: { ...num, description: "Engine v1.1: successful requests in the winning window, the population p99_us was taken over. Absent on older runs." }, rps: nullable(ref("Metric")) }, ["rps"]),
    Measurement: { allOf: [ref("Freshness"), object({ schema_version: { const: "1.0" }, measurement_id: ns, gateway_id: str, gateway_build: ns, run_id: ns, ...freshness, status: ref("Status"), test: ref("TestConfiguration"), metrics: map(ref("Metric")), frontier: array(ref("Frontier")), sample_counts: { ...map(num), description: "Engine v1.1 sample counts behind the percentiles, keyed by the engine's field names (gateway_c1_samples, direct_c1_samples, ttft_gw_samples, ttft_direct_samples, gap_gw_samples, gap_direct_samples). Present only when recorded." }, stream_rung_diagnostics: { ...array(object({ conc: nn, http_status_counts: map(num), stream_errors_no_response: num }, ["conc"])), description: "Engine v1.1 per stream-sweep rung: lanes per HTTP status and lanes that received no response. Present only when recorded." }, provenance: ref("Provenance"), disclosure: ref("Disclosure") }, ["schema_version", "measurement_id", "gateway_id", "gateway_build", "run_id", "measured_at", "status", "test", "metrics", "frontier", "provenance", "disclosure"])] },
    GatewayLink: object({ id: str, name: str, url: str, json_url: str, current_metadata: ref("CurrentMetadata") }),
    Catalog: object({ schema_version: str, gateways: array(ref("GatewayLink")), disclosure: ref("Disclosure") }),
    Discovery: object({ schema_version: str, retrieval: ref("Retrieval"), links: map(str), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    RunReviews: map(ref("RunReview")),
    RouteProbeReport: object({ image: str, checked_at: str, responses: array(object({ path: str, status: {type:"integer",minimum:100,maximum:599}, content_type: ns, body_sha256: str, body_start: str })) }),
    RunReview: object({ gateway: str, reviewed_at: str, excluded_ingress: array({type:"string",enum:PROTOCOLS}), reason: str, evidence: str }),
    Run: object({ review: ref("RunReview"), id: str, sha256: ns, gateway_id: str, measured_at: ns, build: ns, gateway_family: str, evidence_available: bool, disposition: str, engine: ns, available_metrics: array(str), measurements_url: str }, ["id", "gateway_id", "measured_at", "build"]),
    RunResource: { allOf: [ref("Run"), object({ schema_version: str, raw_result_url: str, source_url: str, disclosure: ref("Disclosure") })] },
    RawRun: object({ gateway: str, build: ns, measured_at: ns, matrix: map(ref("JsonValue")), rig: ref("JsonValue"), definitions: map(str) }, ["gateway", "matrix"]),
    Gateway: object({ key: str, display: str, lang: str, repo: ns, version: ref("JsonValue"), stars: { ...nn, deprecated: true, description: "Legacy alias for current_metadata.stars, not stars at measurement time." }, stars_as_of: { ...ns, deprecated: true }, matrix: ref("JsonValue"), current: bool }, ["key"]),
    GatewayEvidence: object({ schema_version: str, gateway: ref("Gateway"), current_metadata: ref("CurrentMetadata"), metadata_scope: str, measurements: array(ref("Measurement")), history: array(ref("Run")), generated_at: str, disclosure: ref("Disclosure") }, ["schema_version", "gateway", "measurements", "history", "disclosure"]),
    EvidenceCollection: object({ schema_version: str, collection_kind: { const: "independent_gateway_runs" }, dataset_id: str, measurements: array(ref("Measurement")), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    LatestCollection: { allOf: [ref("EvidenceCollection"), object({ active_field: nullable(ref("ActiveField")), generated_at: str, published_at: ns, measured_range: object({ earliest: ns, latest: ns }), metadata_as_of: ns, license: ns, archive_history_included: bool, source_url: str, immutable_url: str, gateways: array(ref("GatewayLink")), metrics: array(str) })] },
    BenchmarkResource: { anyOf: [ref("EvidenceCollection"), ref("RunResource")] },
    Definitions: object({ schema_version: str, dataset_id: str, definitions: map(str) }),
    RunPage: object({ schema_version: str, total: { type: "integer", minimum: 0 }, limit: { type: "integer", minimum: 1, maximum: 100 }, offset: { type: "integer", minimum: 0 }, next: ns, runs: array({ allOf: [ref("Run"), object({ gateway_family: str, raw_result_url: ns, source_url: str })] }) }),
    MeasurementPage: object({ schema_version: str, dataset_id: str, generated_at: str, published_at: ns, total: { type: "integer", minimum: 0 }, offset: { type: "integer", minimum: 0 }, limit: { type: "integer", minimum: 1, maximum: 100 }, returned: { type: "integer", minimum: 0 }, max_response_bytes: { const: PAGE_MAX_BYTES }, request_url: str, applied_filters: map(str), next: ns, measurements: array(ref("Measurement")), definitions: map(str) }, ["schema_version", "dataset_id", "total", "offset", "limit", "returned", "max_response_bytes", "request_url", "applied_filters", "next", "measurements"]),
    CompactResult: { allOf: [ref("Freshness"), object({ gateway_id: str, name: str, gateway_build: ns, ...freshness, current_metadata: ref("CurrentMetadata"), status: ref("Status"), metrics: map(ref("Metric")), provenance: ref("Provenance"), evidence_url: str })] },
    Summary: object({ active_field: nullable(ref("ActiveField")), schema_version: str, definitions: map(str), dataset_id: str, generated_at: str, published_at: ns, selected_protocol_pair: ref("ProtocolPair"), results: array(ref("CompactResult")), comparison_policy: ref("Policy"), disclosure: ref("Disclosure"), interpretation: str }),
    Comparison: object({
      comparison: object({ gateway_a: str, gateway_b: str, ingress_protocol: str, upstream_protocol: str, metric: str, bound_ms: nn }, ["gateway_a", "gateway_b", "ingress_protocol", "upstream_protocol", "metric"]),
      comparability: object({ status: { enum: ["comparable", "incomparable", "insufficient_evidence"] }, policy: str, conditions_matched: array(str), reasons: array(str), warnings: array(str) }),
      results: object({ gateway_a: nullable(ref("Measurement")), gateway_b: nullable(ref("Measurement")), gateway_b_over_a: nn }),
      interpretation: object({ scope: str, generalizable_to_real_model_latency: { const: false }, statistical_significance: { const: "not_established" } }), disclosure: ref("Disclosure"),
    }),
    Methodology: object({ schema_version: str, url: str, markdown: str, definitions: ref("JsonValue"), comparison_policy: ref("Policy"), disclosure: ref("Disclosure") }),
    SchemaIndex: object({ schema_version: str, schemas: map(str), states: array(ref("MetricState")), metric_states: array(ref("MetricState")), cell_states: array(ref("CellState")), comparison_policy: ref("Policy") }),
    Error: object({ error: str, status: { type: "integer", enum: [400, 404, 413, 500] } }),
  };
}
export function measurementSchema() {
  const components = schemas();
  // Standalone schema uses $defs rather than OpenAPI components.
  return JSON.parse(JSON.stringify({ $schema: "https://json-schema.org/draft/2020-12/schema", $id: `${ORIGIN}/api/v1/schema/measurement.json`, ...ref("Measurement"), $defs: components }).replaceAll("#/components/schemas/", "#/$defs/"));
}
// Examples are real identifiers from this publication, so every documented example request succeeds.
export function openapi(ex = {}) {
  const query = (name, description, schema = str, example, required = false) => ({ name, in: "query", required, description, schema, ...(example === undefined ? {} : { example }) });
  const protocol = name => query(name, `${name === "egress" ? "Upstream" : "Client ingress"} protocol. Summary and comparison default to openai; measurements omit this filter by default.`, { type: "string", enum: PROTOCOLS }, "openai");
  const gateway = name => query(name, "Published gateway ID from /api/v1/gateways. Comparison a and b must differ.", { type: "string", minLength: 1, pattern: "^[a-z0-9-]+$" }, name === "b" ? "litellm-python" : "busbar", name === "a" || name === "b");
  const metric = query("metric", "Recorded metric key from latest.metrics, or throughput_rps. Comparison defaults to added_latency_p99_us; measurements return all metrics by default.", { type: "string", minLength: 1 }, "added_latency_p99_us");
  const path = { name: "id", in: "path", required: true, description: "Exact identifier from the catalog or run evidence; unknown IDs return 404.", schema: { type: "string", minLength: 1 }, example: "busbar" };
  const get = (operationId, description, schema, parameters = []) => ({ get: { operationId, description, parameters, responses: {
    "200": { description: "Published evidence. Null means unknown or unavailable; never coerce to zero.", content: { "application/json": { schema: ref(schema) } } },
    ...(parameters.some(p => p.in === "query") ? { "400": { description: "Invalid, empty, repeated or unsupported query parameter, invalid bound, or identical comparison IDs.", content: { "application/json": { schema: ref("Error"), example: { error: "Unknown protocol: invalid", status: 400 } } } } } : {}),
    "404": { description: "Unknown gateway, run or collection.", content: { "application/json": { schema: ref("Error") } } },
    ...(operationId === "measurements" ? { "413": { description: "One measurement exceeds the 1 MiB response budget. Narrow the metric filter or use summary and raw evidence links.", content: { "application/json": { schema: ref("Error") } } } } : {}),
    "500": { description: "Publication artifact unavailable.", content: { "application/json": { schema: ref("Error") } } },
  } } });
  return { openapi: "3.1.0", info: { title: "On the Bench", version: "1.3.0", description: `${POLICY.description} Start with /api/v1/summary for bounded retrieval. Full evidence can exceed 4 MB. Actual publication time is unknown when published_at is null; generated_at is build time. Dynamic filters reject unknown and repeated parameters.` }, servers: [{ url: ORIGIN }], components: { schemas: schemas() }, paths: {
    "/gateways/data.csv": { get: { operationId: "measurementCsv", description: "Compact CSV by default. Definitions are referenced by ID and immutable definitions_url. Prefer gateway-filtered exports; compact=0 explicitly requests verbose rows.", parameters: [gateway("gateway"), protocol("ingress"), protocol("egress"), metric, query("compact", "1 deduplicates definitions; 0 includes full text", { type: "string", enum: ["0", "1"], default: "1" })], responses: { "200": { description: "CSV measurements", content: { "text/csv": { schema: str } } } } } },
    "/gateways/{id}.md": { get: { operationId: "gatewayMarkdown", description: "Gateway evidence in Markdown. HTML is available without the suffix; use .txt for text/plain compatibility.", parameters: [path], responses: { "200": { description: "Gateway evidence", content: { "text/markdown": { schema: str } } }, "404": { description: "Unknown gateway" } } } },
    "/api/v1/schema/measurement.json": { get: { operationId: "measurementJsonSchema", description: "JSON Schema 2020-12 with concrete measurement properties and shared $defs.", responses: { "200": { description: "JSON Schema document", content: { "application/json": { schema: { type: "object", required: ["$schema", "$id", "$defs", "$ref"], properties: { "$schema": str, "$id": str, "$ref": str, "$defs": map({ type: "object", additionalProperties: true }) } } } } } } } },
    "/api/v1/definitions/{id}": get("immutableDefinitions", "Immutable definition catalog for a dataset ID referenced by compact CSV", "Definitions", [{ ...path, example: ex.dataset || "dataset-" + "0".repeat(64) }]),
    "/api/v1/definitions": get("definitions", "Shared exact definition strings for compact CSV exports", "Definitions"),
    "/api/v1/history": get("gatewayHistory", "Historical observations. Changes within inclusive ±5% are roughly unchanged; this is a display tolerance, not a statistical margin of error. Follow revision-pinned next links.", "HistoryPage", [query("family", "Gateway family", str, "bifrost"), ...["ingress","egress","metric","bound","from","to","catalog","a","b"].map(name=>query(name, "History filter; a and b select distinct run IDs",str,name==='metric'?'added_latency_p99_us':name==='ingress'||name==='egress'?'openai':undefined)),query("limit","Maximum runs",{type:"integer",minimum:1,maximum:100},20),query("offset","Zero-based offset",{type:"integer",minimum:0},0)]),
    "/api/v1/runs/{id}/measurements": get("historicalMeasurements", "Bounded normalized measurements from one immutable raw run", "MeasurementPage", [{ ...path, example: ex.run || path.example },query("limit","Maximum records",{type:"integer",minimum:1,maximum:100},20),query("offset","Zero-based offset",{type:"integer",minimum:0},0)]),
    "/api/v1/fields": get("fields","Immutable published field selections and rosters","FieldPage",[query("cursor","Opaque retained-field pagination cursor; follow next",str),query("limit","Maximum fields",{type:"integer",minimum:1,maximum:100},20)]),
    "/api/v1/fields/{id}/rankings": get("fieldRankings","Rank within the recorded compatible field, never across incompatible engines","FieldRankings",[{ ...path, example: ex.field || path.example },...['ingress','egress','metric','bound'].map(name=>query(name,'Select protocol pair, metric and throughput bound',str))]),
    "/api/v1/fields/{id}": get("field","Exact roster and selected per-cell measurement identities","FieldManifest",[{ ...path, example: ex.field || path.example }]),
    "/api/v1/releases": get("releases","Named retained publications; metadata refreshes are not new benchmark runs","Releases",[query("limit","Maximum releases",{type:"integer",minimum:1,maximum:100},20),query("cursor","Opaque next-page cursor",str)]),
    "/api/v1/run-reviews.json": get("runReviews", "Publication corrections keyed by SHA-256 of exact raw evidence. Corrections withhold invalid measurements without modifying retained raw bytes.", "RunReviews", []),
    "/api/v1/reviews/{name}.json": get("reviewEvidence", "Recorded direct HTTP probe evidence supporting a publication correction.", "RouteProbeReport", [{name:"name",in:"path",required:true,schema:str,example: ex.review || "one-api-dashboard-probe-2026-11-08"}]),
    "/api/v1/runs": get("runs", "Paginated immutable run catalog. family=busbar includes busbar, busbar-150 and busbar-151; gateway filters an exact historical ID.", "RunPage", [query("gateway", "Exact historical gateway ID", str, "busbar-151"), query("family", "Gateway family across versioned IDs", str, "busbar"), query("limit", "Maximum runs per page", { type: "integer", minimum: 1, maximum: 100, default: 20 }, 20), query("offset", "Zero-based offset", { type: "integer", minimum: 0, default: 0 }, 0)]),
    "/api/v1": get("discovery", "API links and interpretation policy", "Discovery"),
    "/api/v1/summary": get("summary", "Compact core latency measurements for one selected protocol pair, with freshness and provenance; not a ranking.", "Summary", [gateway("gateway"), protocol("ingress"), protocol("egress")]),
    "/api/v1/gateways": get("gateways", "Gateway catalog with dated repository metadata", "Catalog"),
    "/api/v1/gateways/{id}": get("gateway", "Gateway measurements, history and legacy dashboard projection", "GatewayEvidence", [path]),
    "/api/v1/benchmarks/latest": get("latest", "Large collection of independent runs; use summary or measurements for bounded retrieval", "LatestCollection"),
    "/api/v1/benchmarks/{id}": get("benchmark", "Immutable collection (dataset-<sha256>) or run metadata (run-<gateway>-<sha256>)", "BenchmarkResource", [{ ...path, example: ex.dataset || "dataset-" + "0".repeat(64) }]),
    "/api/v1/measurements": get("measurements", "Paginated evidence capped at 1 MiB UTF-8 JSON. returned can be less than limit; always follow next until null. Missing requested metrics remain absent.", "MeasurementPage", [gateway("gateway"), protocol("ingress"), protocol("egress"), metric, query("compact", "Set to 1 to return definition_id references plus a shared definitions dictionary; default 1 deduplicates definitions; 0 retains self-contained evidence.", { type: "string", enum: ["0", "1"], default: "1" }, "1"), query("limit", "Maximum records per page", { type: "integer", minimum: 1, maximum: 100, default: 50 }, 10), query("offset", "Zero-based result offset", { type: "integer", minimum: 0, maximum: Number.MAX_SAFE_INTEGER, default: 0 }, 0)]),
    "/api/v1/compare": get("compare", "Inspect comparability.status before citing a ratio. Full source measurements and provenance are preserved.", "Comparison", [gateway("a"), gateway("b"), protocol("ingress"), protocol("egress"), metric, query("bound", "Only valid with metric=throughput_rps; none means unbounded. Matched operating concurrency is required.", { type: "string", enum: ["1", "5", "10", "50", "100", "none"] })]),
    "/api/v1/methodology": get("methodology", "Method text plus definitions recorded by the instrument", "Methodology"),
    "/api/v1/schema": get("schemas", "Schema discovery", "SchemaIndex"),
    "/api/v1/runs/{id}/raw.json": get("rawRun", "Exact historical instrument JSON, schema may vary by harness version. Verify decoded bytes against raw_sha256.", "RawRun", [{ ...path, example: ex.run || "run-busbar-" + "0".repeat(64) }]),
  } };
}
