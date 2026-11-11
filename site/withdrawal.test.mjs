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

// THE PUBLISHED HISTORY IS THE AUGUST 2026 FIELD AND THE CURRENT FIELD. Every other retained run (the July and
// early-August engine development runs and every November engine-development run) is withdrawn by hash, and
// the raw files stay in Git. The current-field runs are not listed anywhere: whatever lands on the active
// engine after the field started is published, so this check does not pin which of them exist yet.
import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {gunzipSync} from 'node:zlib';
import {join,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {fileAssets} from './serve.mjs';
const ROOT=join(dirname(fileURLToPath(import.meta.url)),'..');
const withdrawnRuns=JSON.parse(readFileSync(join(ROOT,'results/withdrawn-runs.json'),'utf8')).runs;
const active=JSON.parse(readFileSync(join(ROOT,'results/active-field.json'),'utf8'));
const rankBaseline=JSON.parse(readFileSync(join(ROOT,'results/rank-baseline.json'),'utf8'));
const august=JSON.parse(gunzipSync(readFileSync(join(ROOT,'results/baselines',rankBaseline.baseline,'collection.json.gz'))));
const augustShas=new Set(august.measurements.map(m=>m.run_id.slice(-64)));
const isCurrentField=s=>s.rig?.engine?.commit===active.engine_commit && Date.parse(s.measured_at)>=Date.parse(active.started_at);
test('withdrawal list keeps exactly the August 2026 field and the current field',()=>{
 assert.equal(augustShas.size,15);
 for(const sha of augustShas) assert.equal(withdrawnRuns[sha],undefined,`August run ${sha} must stay published`);
 const dir=join(ROOT,'results/snapshots');let intermediate=0;
 for(const f of readdirSync(dir).filter(f=>f.endsWith('.json'))){
  const bytes=readFileSync(join(dir,f)),sha=createHash('sha256').update(bytes).digest('hex'),snap=JSON.parse(bytes);
  if(augustShas.has(sha)||isCurrentField(snap)) { assert.equal(withdrawnRuns[sha],undefined,f); continue; }
  assert.ok(withdrawnRuns[sha],`${f} is neither the August field nor the current field and must be withdrawn`);
  assert.equal(withdrawnRuns[sha].gateway,snap.gateway,f);
  if(snap.measured_at>='2026-11') { intermediate++; assert.match(withdrawnRuns[sha].reason,/Engine development run \(November 2026\)|Halted provisional field|Discarded refresh/,f); }
 }
 assert.ok(intermediate>=40,`expected the November engine-development runs to be withdrawn, found ${intermediate}`);
});
const artifact=process.env.OTB_TEST_ARTIFACT;
test('built history lists only the August 2026 run and current-field runs, one row per run',{skip:!artifact},async()=>{
 const env={ASSETS:fileAssets(artifact)};
 const get=async p=>worker.fetch(new Request('https://onthebench.ai'+p),env);
 const runs=JSON.parse(readFileSync(join(artifact,'_publication/runs.json'),'utf8'));
 const byFamily=new Map();
 for(const r of runs){
  assert.ok(r.sha256 && !withdrawnRuns[r.sha256],`withdrawn run ${r.id} is still published`);
  assert.notEqual(r.disposition,'metadata_only',`metadata-only development record ${r.id} is still published`);
  const isAugust=augustShas.has(r.sha256), isCurrent=r.engine===active.engine_commit && Date.parse(r.measured_at)>=Date.parse(active.started_at);
  assert.ok(isAugust||isCurrent,`${r.id} measured ${r.measured_at} is neither August nor current field`);
  (byFamily.get(r.gateway_family)||byFamily.set(r.gateway_family,[]).get(r.gateway_family)).push(isAugust?'august':'current');
 }
 for(const [family,kinds] of byFamily){
  assert.equal(kinds.filter(k=>k==='august').length,1,family);
  assert.ok(kinds.filter(k=>k==='current').length<=1,family);
  const page=await (await get(`/api/v1/history?family=${family}`)).json();
  assert.equal(page.total,kinds.length,family);
  assert.equal(new Set(page.points.map(p=>p.run_id)).size,page.points.length,family);
  const html=await (await get(`/gateways/${family}/history`)).text();
  assert.equal((html.match(/>Run evidence</g)||[]).length,kinds.length,family);
  assert.doesNotMatch(html,/Catalog <code>catalog-/,family);
  if(kinds.length===2) { assert.ok(page.summary?.to && page.summary.engine_changed,family); assert.match(html,/Aug 2026 → Nov 2026/,family); assert.match(html,/engine changed between these runs/i,family); }
 }
 const fields=await (await get('/api/v1/fields')).json();
 assert.deepEqual(fields.fields.map(f=>f.name).sort(),[rankBaseline.baseline,'Current field'].sort());
 for(const page of ['/','/gateways/performance','/gateways/kong/history']){
  const html=await (await get(page)).text(), footer=html.indexOf('<footer'), res=html.indexOf('publication-resources');
  assert.ok(footer>0 && res>footer && res<html.indexOf('</footer>'),`${page}: data links belong in the site footer`);
 }
 // The board embeds the August rows the rank-movement marker reads.
 const data=JSON.parse(readFileSync(join(artifact,'data.json'),'utf8'));
 assert.equal(Object.keys(data.rank_baseline.gateways).length,15);
 assert.equal(data.rank_baseline.label,rankBaseline.label);
});
test('a retained catalog from before the withdrawal no longer lists withdrawn runs',async()=>{
 const sha='9'.repeat(64),okSha='8'.repeat(64),cat='catalog-'+'7'.repeat(64);
 const catalog={catalog_id:cat,projector:'p',shards:{},runs:[{id:`run-kong-${sha}`,gateway_id:'kong',gateway_family:'kong',measured_at:'2026-11-09T05:55:36Z',evidence_available:true},{id:`run-kong-${okSha}`,gateway_id:'kong',gateway_family:'kong',measured_at:'2026-08-03T22:17:28Z',evidence_available:true}]};
 const env={ASSETS:{fetch:async req=>{const u=new URL(req.url).pathname;
   if(u==='/_publication/withdrawn-runs.json')return Response.json({[sha]:{reason:'Engine development run'}});
   if(u==='/_publication/history/catalog.json')return Response.json({catalog_id:'catalog-'+'6'.repeat(64),runs:[{id:`run-kong-${okSha}`,gateway_family:'kong'}]});
   return new Response(null,{status:404});}},
  PUBLICATIONS:{get:async k=>k===`history/catalogs/${cat}`?JSON.stringify(catalog):null}};
 const page=await (await worker.fetch(new Request(`https://onthebench.ai/api/v1/history?family=kong&catalog=${cat}`),env)).json();
 assert.deepEqual(page.points.map(p=>p.run_id),[`run-kong-${okSha}`]);
});
