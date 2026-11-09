import test from 'node:test';
import assert from 'node:assert/strict';
import { applyRunReview } from './run-reviews.mjs';
import { historicalMeasurements } from './history.mjs';
const snapshot = { gateway:'one-api', measured_at:'2026-11-08T20:16:06Z', matrix:{ upstreams:{openai:{cells:{
  openai:{served:true,perf:{added_latency_p99_us:200}},
  cohere:{served:true,perf:{added_latency_p99_us:1,frontier:[{bound_ms:10,rps:99999}]},stream:{streams_sustained:1000},memory:{peak_rss_mib:1}}
}}}}};
const review = {gateway:'one-api',excluded_ingress:['cohere'],reason:'Dashboard HTML, not a protocol response',reviewed_at:'2026-11-08T23:00:00Z',evidence:'https://onthebench.ai/proof'};
test('review withholds all affected measurements without changing original bytes or valid cells',()=>{
 const original=JSON.stringify(snapshot), corrected=applyRunReview(snapshot,review);
 assert.equal(JSON.stringify(snapshot),original);
 assert.deepEqual(corrected.matrix.upstreams.openai.cells.openai,snapshot.matrix.upstreams.openai.cells.openai);
 const cell=corrected.matrix.upstreams.openai.cells.cohere;
 assert.equal(cell.served,'untestable');
 for(const section of ['perf','stream','memory']) assert.equal(cell[section],null);
 assert.equal(cell.publication_review.original_state,true);
 assert.equal(applyRunReview(snapshot,null),snapshot);
 assert.throws(()=>applyRunReview(snapshot,{...review,gateway:'another-gateway'}));
});
test('historical API projection cannot resurrect reviewed dashboard numbers',()=>{
 const records=historicalMeasurements(snapshot,{id:'run-one-api-test',sha256:'exact-raw-hash',review});
 const cohere=records.find(m=>m.test.ingress_protocol==='cohere');
 assert.equal(cohere.status.state,'untestable');
 assert.equal(cohere.status.raw_state,true);
 assert.match(cohere.status.evidence_scope,/Publication correction/);
 assert.equal(cohere.status.detail,review.reason);
 assert.deepEqual(cohere.frontier,[]);
 assert.ok(Object.values(cohere.metrics).every(m=>m.value===null));
 assert.equal(cohere.provenance.raw_sha256,'exact-raw-hash');
});
import { reviewedOracleSnapshot } from './check-consistency.mjs';
test('independent oracle excludes reviewed values but preserves every unreviewed cell',()=>{
 const oracle=reviewedOracleSnapshot(snapshot,review);
 assert.equal(oracle.matrix.upstreams.openai.cells.cohere.perf,null);
 assert.equal(oracle.matrix.upstreams.openai.cells.cohere.served,'untestable');
 assert.deepEqual(oracle.matrix.upstreams.openai.cells.openai,snapshot.matrix.upstreams.openai.cells.openai);
 assert.equal(snapshot.matrix.upstreams.openai.cells.cohere.perf.added_latency_p99_us,1);
});
test('absence clarification preserves valid measurements, failed cells and immutable raw evidence',()=>{
 const raw=structuredClone(snapshot);
 const cell=raw.matrix.upstreams.openai.cells.openai;
 cell.perf.cpu_us_per_request=null;
 cell.absences={'perf.cpu_us_per_request':{reason:'not_measured',detail:'failed cost window'}};
 raw.matrix.upstreams.openai.cells.anthropic={served:'failed',status:'501',body_snippet:'unsupported'};
 const original=JSON.stringify(raw);
 const annotation={...review,excluded_ingress:[],absence_notes:[{ingress:'openai',egress:'openai',paths:['perf.cpu_us_per_request'],detail:'Two requests failed; their individual responses were not retained. Other measurements remain valid.'}]};
 const result=applyRunReview(raw,annotation), revised=result.matrix.upstreams.openai.cells.openai;
 assert.equal(JSON.stringify(raw),original);
 assert.equal(revised.served,true);
 assert.equal(revised.perf.added_latency_p99_us,200);
 assert.equal(revised.perf.cpu_us_per_request,null);
 assert.equal(revised.absences['perf.cpu_us_per_request'].reason,'not_measured');
 assert.equal(revised.absences['perf.cpu_us_per_request'].detail,annotation.absence_notes[0].detail);
 assert.deepEqual(result.matrix.upstreams.openai.cells.anthropic,raw.matrix.upstreams.openai.cells.anthropic);
 assert.deepEqual(result.matrix.upstreams.openai.cells.cohere,raw.matrix.upstreams.openai.cells.cohere);
 const historical=historicalMeasurements(raw,{id:'run-one-api-notes',sha256:'raw-hash',review:annotation});
 const measurement=historical.find(m=>m.test.ingress_protocol==='openai');
 assert.equal(measurement.metrics.cpu_us_per_request.value,null);
 assert.equal(measurement.metrics.cpu_us_per_request.detail,annotation.absence_notes[0].detail);
 assert.equal(measurement.metrics.added_latency_p99_us.value,200);
 assert.equal(reviewedOracleSnapshot(raw,annotation).matrix.upstreams.openai.cells.openai.perf.added_latency_p99_us,200);
});
test('absence reviews cannot target a metric with no recorded absence',()=>{
 const invalid={...review,excluded_ingress:[],absence_notes:[{ingress:'openai',egress:'openai',paths:['perf.added_latency_p99_us'],detail:'not allowed'}]};
 assert.throws(()=>applyRunReview(snapshot,invalid),/no recorded absence/);
 assert.throws(()=>reviewedOracleSnapshot(snapshot,invalid),/no recorded absence/);
});
