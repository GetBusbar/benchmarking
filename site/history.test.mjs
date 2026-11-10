import test from 'node:test';
import assert from 'node:assert/strict';
import {trendIndicator,metricDirection,historicalComparison,historyPoints,currentTrends,rankChanges} from './history.mjs';
test('trend tolerance includes both five-percent boundaries',()=>{
 for(const after of [950,999,1000,1001,1050]) assert.equal(trendIndicator(1000,after,'higher').state,'flat');
 assert.equal(trendIndicator(1000,1051,'higher').state,'improved');
 assert.equal(trendIndicator(1000,949,'higher').state,'regressed');
});
test('lower latency improves while lower throughput regresses',()=>{
 assert.equal(trendIndicator(1000,900,metricDirection('added_latency_p99_us')).state,'improved');
 assert.equal(trendIndicator(1000,900,metricDirection('throughput_rps')).state,'regressed');
});
test('missing, incompatible and zero-denominator evidence stays neutral',()=>{
 for(const before of [null,undefined,NaN,Infinity]) assert.equal(trendIndicator(before,10,'higher').state,'not_comparable');
 assert.equal(trendIndicator(1,100,'higher',false).state,'not_comparable');
 assert.equal(trendIndicator(0,100,'higher').state,'not_comparable');
 assert.equal(trendIndicator(1,100,null).state,'not_comparable');
});

import {readFileSync} from 'node:fs';
import worker from './worker.mjs';
import {fileAssets} from './serve.mjs';
import {schemas} from './api-contract.mjs';
import Ajv from 'ajv/dist/2020.js';
const artifact=process.env.OTB_TEST_ARTIFACT;
test('history pagination, schema and snapshot revisions preserve identities',{skip:!artifact},async()=>{
 const env={ASSETS:fileAssets(artifact)};
 const get=async path=>worker.fetch(new Request(new URL(path,'https://onthebench.ai')),env);
 // kong carries both published runs (August 2026 and the current field), so one-run pages paginate.
 const response=await get('/api/v1/history?family=kong&limit=1');assert.equal(response.status,200);
 const first=await response.json();assert.equal(first.returned,1);assert.ok(first.next.includes(first.catalog_id));
 const second=await (await get(first.next)).json();assert.equal(second.offset,1);
 assert.ok(second.points.every(p=>!first.points.some(old=>old.run_id===p.run_id)));
 const ajv=new Ajv({strict:false});const schema={$ref:'#/components/schemas/HistoryPage',components:{schemas:schemas()}};
 const validate=ajv.compile(schema);assert.ok(validate(first),JSON.stringify(validate.errors));
 assert.equal((await get('/api/v1/history?family=bifrost&catalog=catalog-'+ '0'.repeat(64))).status,409);
 assert.equal((await get('/api/v1/history?family=bifrost&metric=made_up')).status,400);
 const html=await (await get('/gateways/kong/history')).text();assert.ok(html.includes('within ±5%'));assert.ok(html.includes('Run evidence'));
});
test('saved catalog stays traversable after a newer deployment',{skip:!artifact},async()=>{
 const catalog=JSON.parse(readFileSync(artifact+'/_publication/history/catalog.json'));
 const old={...catalog,catalog_id:'catalog-'+'a'.repeat(64)};
 delete old.shards; // Older deployed catalogs remain supported.
 const family=readFileSync(artifact+'/_publication/history/families/bifrost/openai--openai.json','utf8');
 const env={ASSETS:fileAssets(artifact),PUBLICATIONS:{get:async key=>key===`history/catalogs/${old.catalog_id}`?JSON.stringify(old):key===`history/catalogs/${old.catalog_id}/families/bifrost/openai--openai`?family:null}};
 const r=await worker.fetch(new Request('https://onthebench.ai/api/v1/history?family=bifrost&catalog='+old.catalog_id),env);
 assert.equal(r.status,200);assert.equal((await r.json()).catalog_id,old.catalog_id);
});
test('active refresh keeps the full roster pending without stale current values',{skip:!artifact},async()=>{
 const data=JSON.parse(readFileSync(artifact+'/data.json'));
 if(!data.active_field)return;
 const env={ASSETS:fileAssets(artifact)};
 const get=async path=>(await worker.fetch(new Request('https://onthebench.ai'+path),env));
 const summary=await (await get('/api/v1/summary')).json();
 assert.equal(summary.results.length,data.active_field.gateways.length);
 for(const g of data.gateways.filter(g=>g.awaiting_engine)){
   const row=summary.results.find(r=>r.gateway_id===g.key);
   assert.equal(row.measured_at,null);
   assert.equal(row.status.state,'not_measured');
   assert.ok(Object.values(row.metrics).every(m=>m.value===null));
   assert.equal(g.matrix ?? null,null);
   assert.ok(!String(g.version).includes('sha256:'));
 }
 const fields=await (await get('/api/v1/fields')).json();
 const current=fields.fields.find(f=>f.dataset_id===summary.dataset_id);
 const manifest=await (await get(new URL(current.url).pathname)).json();
 assert.deepEqual([...manifest.roster].sort(),[...data.active_field.gateways].sort());
 assert.ok((await (await get('/api/v1/runs?limit=1')).json()).total>0);
});

test('two-run comparisons preserve distinct throughput bounds and missing values',()=>{
 const row=value=>({gateway_id:'bifrost',status:{state:'served'},provenance:{},test:{ingress_protocol:'openai',upstream_protocol:'openai'},metrics:{},frontier:[
   {bound_ms:1,concurrency:32,rps:{value,state:value==null?'not_measured':'valid'}},
   {bound_ms:null,concurrency:32,rps:{value:2000,state:'valid'}}
 ]});
 const comparisons=historicalComparison(row(null),row(1001));
 assert.equal(comparisons.length,2);
 assert.equal(comparisons[0].before,null);
 assert.equal(comparisons[0].after,1001);
 assert.match(comparisons[0].metric,/≤ 1 ms/);
 assert.equal(comparisons[0].change_percent,null);
 assert.equal(comparisons[0].trend.state,'not_comparable');
 assert.match(comparisons[1].metric,/unbounded/);
});


test('history distinguishes first observations from changed engines', () => {
 const row=(id,date,engine)=>({run_id:id,measured_at:date,gateway_id:'helicone',status:{state:'served'},provenance:{raw_result_url:'https://example.test/run'},test:{ingress_protocol:'openai',upstream_protocol:'openai',hardware:'m7g.4xlarge',architecture:'arm64',harness_commit:engine,harness_dirty:false,mock_sha256:'mock',configuration_sha256:'config',qualification:{outcome:'pass'}},metrics:{added_latency_p99_us:{value:293,state:'valid',definition:'added latency',section:'perf'}},frontier:[]});
 const points=historyPoints([row('old','2026-08-03','old-engine'),row('new','2026-11-08','new-engine')],{ingress:'openai',egress:'openai',metric:'added_latency_p99_us'});
 assert.equal(points[0].trend.label,'First recorded run');
 assert.equal(points[1].trend.label,'Engine changed');
 assert.ok(points[1].trend.reasons.includes('different_harness_commit'));
 assert.equal(points[1].trend.change_percent,null);
});

test('current indicators require compatible evidence and preserve metric and protocol selection', () => {
 const row=(id,date,value,engine='same')=>({run_id:id,measured_at:date,gateway_id:'helicone',status:{state:'served'},provenance:{raw_result_url:'https://example.test/run'},test:{ingress_protocol:'openai',upstream_protocol:'openai',hardware:'m7g.4xlarge',architecture:'arm64',harness_commit:engine,harness_dirty:false,mock_sha256:'mock',configuration_sha256:'config',qualification:{outcome:'pass'}},metrics:{added_latency_p99_us:{value,state:'valid',definition:'added latency',section:'perf'}},frontier:[]});
 const old=row('old','2026-08-03',100), current=row('new','2026-11-08',90);
 const trends=currentTrends([old,current],[current]);
 assert.equal(trends['openai/openai/added_latency_p99_us'].state,'improved');
 assert.equal(trends['openai/openai/added_latency_p99_us'].previous_run_id,'old');
 assert.deepEqual(currentTrends([old],[{...current,test:{...current.test,harness_commit:'different'}}]),{});
 assert.deepEqual(currentTrends([],[current]),{});
});

test('rank movement rejects partial fields and engine changes', () => {
 const row=(id,value,engine='same')=>({run_id:id+value,gateway_id:id,status:{state:'served'},provenance:{raw_result_url:'https://example.test/run'},test:{ingress_protocol:'openai',upstream_protocol:'openai',hardware:'m7g.4xlarge',architecture:'arm64',harness_commit:engine,harness_dirty:false,mock_sha256:'mock',qualification:{outcome:'pass'}},metrics:{added_latency_p99_us:{value,state:'valid',definition:'latency',section:'perf'}},frontier:[]});
 const field=measurements=>({gateways:[{id:'a'},{id:'b'}],measurements});
 const before=field([row('a',100),row('b',200)]), after=field([row('a',100),row('b',90)]);
 const q={ingress:'openai',egress:'openai',metric:'added_latency_p99_us'};
 assert.equal(rankChanges(before,after,q).b.change,1);
 assert.equal(rankChanges(before,after,q).a.change,-1);
 assert.deepEqual(rankChanges(before,field([row('b',90)]),q),{});
 assert.deepEqual(rankChanges(before,field([row('a',100,'new'),row('b',90,'new')]),q),{});
});

test('shared archive shards preserve nonempty history and explicit empty pairs',{skip:!artifact},async()=>{
 const catalog=JSON.parse(readFileSync(artifact+'/_publication/history/catalog.json'));
 assert.ok(catalog.shards);
 const retained=new Map(JSON.parse(readFileSync(artifact+'/_publication/retention.json')).map(e=>[e.key,e.value]));
 const old={...catalog,catalog_id:'catalog-'+'b'.repeat(64)};
 const env={ASSETS:fileAssets(artifact),PUBLICATIONS:{get:async key=>key===`history/catalogs/${old.catalog_id}`?JSON.stringify(old):retained.get(key)??null}};
 const get=async pair=>worker.fetch(new Request('https://onthebench.ai/api/v1/history?family=bifrost&catalog='+old.catalog_id+pair),env);
 const response=await get('');assert.equal(response.status,200);assert.ok((await response.json()).points.length);
 delete old.shards['families/bifrost/bedrock--bedrock'];delete old.shards['placements/bifrost/bedrock--bedrock'];
 const empty=await get('&ingress=bedrock&egress=bedrock');assert.equal(empty.status,200);assert.ok((await empty.json()).points.every(p=>p.value===null));
});
