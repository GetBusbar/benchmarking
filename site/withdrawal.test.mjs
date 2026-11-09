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
test('retained datasets, fields and releases that name a withdrawn run are refused or dropped',async()=>{
 const sha='b'.repeat(64),run='run-example-'+sha,ds='dataset-'+'c'.repeat(64),fid='field-'+'d'.repeat(64),okFid='field-'+'e'.repeat(64);
 const store={[`datasets/${ds}`]:JSON.stringify({measurements:[{run_id:run}]}),[`fields/${fid}`]:JSON.stringify({runs:[run]}),[`field-evidence/${fid}`]:JSON.stringify({measurements:[{run_id:run}]}),
  [`fields/${okFid}`]:JSON.stringify({runs:['run-ok-'+'f'.repeat(64)]}),'releases/a':JSON.stringify({field_id:fid}),'releases/b':JSON.stringify({field_id:okFid}),
  [`field-index/${fid}`]:JSON.stringify({field_id:fid}),[`field-index/${okFid}`]:JSON.stringify({field_id:okFid})};
 const env={ASSETS:{fetch:async req=>{const u=new URL(req.url).pathname;
   if(u==='/_publication/withdrawn-runs.json')return Response.json({[sha]:{reason:'Discarded refresh'}});
   if(u==='/_publication/history/fields.json')return Response.json([]);
   return new Response(null,{status:404});}},
  PUBLICATIONS:{get:async k=>store[k]??null,list:async({prefix})=>({keys:Object.keys(store).filter(k=>k.startsWith(prefix)).map(name=>({name})),list_complete:true})}};
 const get=p=>worker.fetch(new Request('https://onthebench.ai'+p),env);
 assert.equal((await get(`/api/v1/benchmarks/${ds}`)).status,410);
 assert.equal((await get(`/api/v1/fields/${fid}`)).status,410);
 assert.equal((await get(`/api/v1/fields/${fid}/rankings`)).status,410);
 assert.deepEqual((await (await get('/api/v1/releases')).json()).releases.map(r=>r.field_id),[okFid]);
 assert.deepEqual((await (await get('/api/v1/fields')).json()).fields.map(f=>f.field_id),[okFid]);
});
