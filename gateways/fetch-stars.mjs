#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// fetch-stars.mjs: refresh the committed GitHub star snapshot (gateways/stars.json).
//
// The site's Gateways overview shows a star count per gateway. That count comes from
// THIS committed snapshot, never from a live API call: the site build stays
// reproducible and needs no network in CI. Re-run this script whenever the snapshot
// should be refreshed, then commit the updated stars.json:
//
//   node gateways/fetch-stars.mjs && git add gateways/stars.json
//
// Reads each gateways/*/gateway.sh manifest's GW_REPO, hits the GitHub API
// (unauthenticated is fine for this many public repos; set GITHUB_TOKEN if you are
// rate-limited), and writes { "<gateway-key>": { "stars": N, "as_of": "YYYY-MM-DD" } }.
// Two gateways sharing one repo (litellm-python / litellm-rust) each get the repo's
// count under their own key, so gen-data stays a plain key lookup.

import { readdirSync, readFileSync, statSync, existsSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "stars.json");

const keys = readdirSync(HERE).filter((d) => {
  try {
    return statSync(join(HERE, d)).isDirectory() && existsSync(join(HERE, d, "gateway.sh"));
  } catch { return false; }
}).sort();

const headers = { "user-agent": "onthebench-stars-snapshot", accept: "application/vnd.github+json" };
if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;

const asOf = new Date().toISOString().slice(0, 10);
const out = {};
const cache = new Map(); // owner/repo -> stars (shared repos fetched once)
for (const key of keys) {
  const text = readFileSync(join(HERE, key, "gateway.sh"), "utf8");
  const m = text.match(/^GW_REPO=(?:"([^"]*)"|(\S+))/m);
  const repo = m ? (m[1] ?? m[2]) : null;
  const slug = repo && repo.match(/github\.com\/([^/]+\/[^/\s#?]+)/)?.[1];
  if (!slug) {
    console.warn(`skip ${key}: no parsable github GW_REPO (${repo})`);
    continue;
  }
  if (!cache.has(slug)) {
    const r = await fetch(`https://api.github.com/repos/${slug}`, { headers });
    if (!r.ok) throw new Error(`GitHub API ${r.status} for ${slug} (rate-limited? set GITHUB_TOKEN or use: gh api repos/${slug} --jq .stargazers_count)`);
    cache.set(slug, (await r.json()).stargazers_count);
  }
  out[key] = { stars: cache.get(slug), as_of: asOf };
  console.log(`${key}: ${out[key].stars} (${slug})`);
}

writeFileSync(OUT, JSON.stringify(out, null, 1) + "\n");
console.log(`wrote ${Object.keys(out).length} entries -> ${OUT}`);
