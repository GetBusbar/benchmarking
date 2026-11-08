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
