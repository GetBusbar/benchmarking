import test from 'node:test';
import assert from 'node:assert/strict';
import worker from './worker.mjs';
test('withdrawn raw and historical run paths cannot fall back to retained storage',async()=>{
 const sha='a'.repeat(64),id='run-example-'+sha;
 const env={ASSETS:{fetch:async req=>req.url.endsWith('/_publication/withdrawn-runs.json')?Response.json({[sha]:{reason:'Discarded refresh'}}):assert.fail('Withdrawn run reached an asset fallback')},PUBLICATIONS:{get:async()=>assert.fail('Withdrawn run reached retained KV')}};
 for(const path of [`/api/v1/runs/${id}/raw.json`,`/api/v1/runs/${id}/measurements`,`/api/v1/benchmarks/${id}`,`/gateways/runs/${id}`]) {
  const r=await worker.fetch(new Request('https://onthebench.ai'+path),env);assert.equal(r.status,410,path);
 }
});
