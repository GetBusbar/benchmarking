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
// Reads each gateways/*/definition.json manifest's repo, hits the GitHub API
// (unauthenticated is fine for this many public repos; set GITHUB_TOKEN if you are
// rate-limited), and writes
//   { "<gateway-key>": { "stars": N, "first_commit": "YYYY-MM-DD", "as_of": "YYYY-MM-DD" } }.
// Two gateways may share one upstream repo; each still gets that repo's count under its
// OWN key, so gen-data stays a plain key lookup.
//
// ALL OR NOTHING. The file is rewritten wholesale, so a gateway skipped here would silently lose
// its previously recorded entry. Any gateway that cannot be resolved, or any value GitHub did not
// actually return, fails the script before stars.json is written, with a nonzero exit.

import { readdirSync, readFileSync, statSync, existsSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, "stars.json");

const keys = readdirSync(HERE).filter((d) => {
  try {
    return statSync(join(HERE, d)).isDirectory() && existsSync(join(HERE, d, "definition.json"));
  } catch { return false; }
}).sort();

const headers = { "user-agent": "onthebench-stars-snapshot", accept: "application/vnd.github+json" };
if (process.env.GITHUB_TOKEN) headers.authorization = `Bearer ${process.env.GITHUB_TOKEN}`;

const asOf = new Date().toISOString().slice(0, 10);

// The date of a repo's FIRST commit (project age) — deliberately NOT `created_at`, which resets
// on renames/re-imports (one entrant in this field was renamed, and `created_at` would show it
// as newborn while its real history runs years earlier). GitHub's commits list is
// newest-first with no reverse order, so: page the list at per_page=1, read the `last` page
// number from the Link header, fetch that page — its single commit is the root.
async function firstCommitDate(slug) {
  const url = `https://api.github.com/repos/${slug}/commits?per_page=1`;
  const r = await fetch(url, { headers });
  if (!r.ok) throw new Error(`GitHub API ${r.status} for ${slug} commits`);
  const last = r.headers.get("link")?.match(/[?&]page=(\d+)>; rel="last"/)?.[1];
  const page = last ? await (async () => {
    const r2 = await fetch(`${url}&page=${last}`, { headers });
    if (!r2.ok) throw new Error(`GitHub API ${r2.status} for ${slug} root commit`);
    return r2.json();
  })() : await r.json();
  const date = page[0]?.commit?.committer?.date?.slice(0, 10);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(date ?? "")) throw new Error(`GitHub API returned no root-commit date for ${slug}`);
  return date;
}

const out = {};
const skipped = [];
const cache = new Map(); // owner/repo -> {stars, first_commit} (shared repos fetched once)
for (const key of keys) {
  // The manifest is JSON, and it is the same one the engine runs from. Regex-scraping a shell
  // variable out of it would match nothing and silently drop every gateway's star count.
  let repo = null;
  try {
    repo = JSON.parse(readFileSync(join(HERE, key, "definition.json"), "utf8")).repo ?? null;
  } catch {
    skipped.push(`${key}: definition.json is unreadable or not JSON`);
    continue;
  }
  const slug = repo && repo.match(/github\.com\/([^/]+\/[^/\s#?]+)/)?.[1];
  if (!slug) {
    skipped.push(`${key}: no parsable github repo (${repo})`);
    continue;
  }
  if (!cache.has(slug)) {
    const r = await fetch(`https://api.github.com/repos/${slug}`, { headers });
    if (!r.ok) throw new Error(`GitHub API ${r.status} for ${slug} (rate-limited? set GITHUB_TOKEN or use: gh api repos/${slug} --jq .stargazers_count)`);
    const stars = (await r.json()).stargazers_count;
    if (!Number.isInteger(stars) || stars < 0) throw new Error(`GitHub API returned no stargazers_count for ${slug}`);
    cache.set(slug, { stars, first_commit: await firstCommitDate(slug) });
  }
  out[key] = { stars: cache.get(slug).stars, first_commit: cache.get(slug).first_commit, as_of: asOf };
  console.log(`${key}: ${out[key].stars} stars, since ${out[key].first_commit} (${slug})`);
}

if (skipped.length) {
  for (const why of skipped) console.error(`cannot resolve ${why}`);
  console.error(`stars.json NOT written: ${skipped.length} gateway(s) would have lost their entry`);
  process.exit(1);
}
writeFileSync(OUT, JSON.stringify(out, null, 1) + "\n");
console.log(`wrote ${Object.keys(out).length} entries -> ${OUT}`);
