// SPDX-License-Identifier: Apache-2.0
import { createHash } from 'node:crypto';
export const entryHash = entry => createHash('sha256').update(Buffer.from(entry.value, entry.base64 ? 'base64' : 'utf8')).digest('hex');

// Plan from remote state, never from a local success marker. Failed or partial
// uploads can be retried without forgetting evidence or repeating successful writes.
export async function pendingEntries(entries, { list, readText }) {
  const existing = await list();
  const pending = [], legacy = [];
  for (const entry of entries) {
    const remote = existing.get(entry.key), digest = entryHash(entry);
    const prepared = {...entry, metadata:{...entry.metadata,sha256:digest}};
    if (!remote) pending.push(prepared);
    else if (/^(datasets|definitions)\/dataset-[a-f0-9]{64}$/.test(entry.key)) continue; // Preserve the first publication of this evidence identity.
    else if (remote.metadata?.sha256 === digest) continue;
    // Raw evidence names contain the SHA-256 of the uncompressed source and is
    // immutable. Legacy raw keys predate upload metadata; preserve them in place.
    else if (entry.base64 && /^raw\/run-.+-[a-f0-9]{64}$/.test(entry.key)) continue;
    else if (entry.base64) pending.push(prepared);
    else legacy.push(prepared);
  }
  for (let i=0; i<legacy.length;) {
    const batch=[]; let bytes=0;
    while (i<legacy.length && batch.length<100 && (!batch.length || bytes+Buffer.byteLength(legacy[i].value)<4*1024*1024)) {
      bytes+=Buffer.byteLength(legacy[i].value);batch.push(legacy[i++]);
    }
    const values=await readText(batch.map(e=>e.key));
    for (const entry of batch) if (values[entry.key] !== entry.value) pending.push(entry);
  }
  return pending;
}

export function cloudflareRetention({account, namespace, token, fetcher=fetch}) {
  if (!token) throw new Error('CLOUDFLARE_API_TOKEN is required for incremental publication');
  const base=`https://api.cloudflare.com/client/v4/accounts/${account}/storage/kv/namespaces/${namespace}`;
  async function request(path, body) {
    const response=await fetcher(base+path,{method:body?'POST':'GET',headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json'},...(body?{body:JSON.stringify(body)}:{})});
    const data=await response.json();
    if (!response.ok || !data.success) throw new Error(`KV inventory failed (${response.status}): ${JSON.stringify(data.errors || [])}`);
    return data;
  }
  return {
    async list() {
      const keys=new Map();let cursor='';
      do {
        const data=await request('/keys?limit=1000'+(cursor?'&cursor='+encodeURIComponent(cursor):''));
        for (const key of data.result) keys.set(key.name,key);
        const next=data.result_info?.cursor || '';
        if (next && next===cursor) throw new Error('KV inventory cursor did not advance');
        cursor=next;
      } while(cursor);
      return keys;
    },
    async readText(keys) {return (await request('/bulk/get',{keys,type:'text'})).result.values;},
  };
}
