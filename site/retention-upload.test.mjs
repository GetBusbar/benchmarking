import test from 'node:test';
import assert from 'node:assert/strict';
import {pendingEntries,entryHash,cloudflareRetention} from './retention-upload.mjs';
test('unchanged metadata and legacy values cause no writes; missing and changed values do',async()=>{
 const entries=[{key:'known',value:'same'},{key:'legacy',value:'old'},{key:'changed',value:'new'},{key:'missing',value:'new'}];
 const remote=new Map([['known',{metadata:{sha256:entryHash(entries[0])}}],['legacy',{}],['changed',{}]]);
 const pending=await pendingEntries(entries,{list:async()=>remote,readText:async keys=>{assert.deepEqual(keys,['legacy','changed']);return {legacy:'old',changed:'old'};}});
 assert.deepEqual(pending.map(e=>e.key),['missing','changed']);
 assert.ok(pending.every(e=>e.metadata.sha256===entryHash(e)));
});
test('partial successful uploads are skipped on retry',async()=>{
 const entries=[{key:'a',value:'1'},{key:'b',value:'2'}];
 const pending=await pendingEntries(entries,{list:async()=>new Map([['a',{metadata:{sha256:entryHash(entries[0])}}]]),readText:async()=>assert.fail('no legacy keys')});
 assert.deepEqual(pending.map(e=>e.key),['b']);
});
test('inventory failure aborts instead of treating all evidence as missing',async()=>{
 await assert.rejects(pendingEntries([],{list:async()=>{throw new Error('unavailable')}}),/unavailable/);
});
test('remote inventory follows cursors and parses bulk text values',async()=>{
 const calls=[];
 const remote=cloudflareRetention({account:'a',namespace:'n',token:'test',fetcher:async(url,init)=>{
  calls.push({url,init});
  if(url.includes('/bulk/get')) return Response.json({success:true,result:{values:{a:'text'}}});
  return Response.json({success:true,result:[{name:calls.length===1?'a':'b'}],result_info:{cursor:calls.length===1?'next':''}});
 }});
 assert.equal((await remote.list()).size,2);
 assert.match(calls[1].url,/cursor=next/);
 assert.deepEqual(await remote.readText(['a']),{a:'text'});
});

test('legacy content-addressed raw evidence is retained without rewriting binary data',async()=>{
 const entry={key:'raw/run-helicone-'+ 'a'.repeat(64),value:Buffer.from('raw evidence').toString('base64'),base64:true};
 assert.deepEqual(await pendingEntries([entry],{list:async()=>new Map([[entry.key,{}]]),readText:async()=>assert.fail('binary must not be decoded as text')}),[]);
});

test('metadata-only republication does not rewrite a retained dataset identity',async()=>{
 const key='datasets/dataset-'+ 'b'.repeat(64);
 assert.deepEqual(await pendingEntries([{key,value:'new generated timestamp'}],{list:async()=>new Map([[key,{}]]),readText:async()=>assert.fail('retain first publication')}),[]);
});
