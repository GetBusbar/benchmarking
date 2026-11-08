import test from 'node:test';
import assert from 'node:assert/strict';
import {trendIndicator,metricDirection} from './history.mjs';
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
 const response=await get('/api/v1/history?family=bifrost&limit=2');assert.equal(response.status,200);
 const first=await response.json();assert.equal(first.returned,2);assert.ok(first.next.includes(first.catalog_id));
 const second=await (await get(first.next)).json();assert.equal(second.offset,2);
 assert.ok(second.points.every(p=>!first.points.some(old=>old.run_id===p.run_id)));
 const ajv=new Ajv({strict:false});const schema={$ref:'#/components/schemas/HistoryPage',components:{schemas:schemas()}};
 const validate=ajv.compile(schema);assert.ok(validate(first),JSON.stringify(validate.errors));
 assert.equal((await get('/api/v1/history?family=bifrost&catalog=catalog-'+ '0'.repeat(64))).status,409);
 assert.equal((await get('/api/v1/history?family=bifrost&metric=made_up')).status,400);
 const html=await (await get('/gateways/bifrost/history')).text();assert.ok(html.includes('within ±5%'));assert.ok(html.includes('Run evidence'));
});
test('saved catalog stays traversable after a newer deployment',{skip:!artifact},async()=>{
 const catalog=JSON.parse(readFileSync(artifact+'/_publication/history/catalog.json'));
 const old={...catalog,catalog_id:'catalog-'+'a'.repeat(64)};
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
