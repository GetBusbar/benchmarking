#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// The scheduled refresh and an explicit release use exactly this path.
import { build } from "./build.mjs";
import { pendingEntries, cloudflareRetention } from "./retention-upload.mjs";
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
const here = dirname(fileURLToPath(import.meta.url));
const scratch = mkdtempSync(join(process.env.OTB_BUILD_ROOT || tmpdir(), "onthebench-publish-"));
const root = resolve(here, "..");
const wrangler = join(here, "node_modules/wrangler/bin/wrangler.js");
try {
  const out = join(scratch, "site");
  const { publication } = build({ root, out, publishedAt: process.argv.includes("--dry-run") ? null : new Date().toISOString() });
  const retained = JSON.parse(readFileSync(join(out, '_publication/retention.json'), 'utf8'));
  rmSync(join(out, '_publication/retention.json'));
  execFileSync(process.execPath, ["--test", join(here, "publishing.test.mjs"), join(here, "history.test.mjs"), join(here, "retention-upload.test.mjs")], { stdio: "inherit", env: { ...process.env, OTB_TEST_ARTIFACT: out } });
  const config = JSON.parse(readFileSync(join(here, "wrangler.jsonc"), "utf8"));
  config.main = join(here, "worker.mjs"); config.assets.directory = out;
  const configPath = join(scratch, "wrangler.json"); writeFileSync(configPath, JSON.stringify(config));
  const run = (...args) => execFileSync(process.execPath, [wrangler, ...args, "--config", configPath], { stdio: "inherit", env: { ...process.env, WRANGLER_SEND_METRICS: "false" } });
  if (process.argv.includes("--dry-run")) run("deploy", "--dry-run", "--outdir", join(scratch, "bundle"));
  else {
    const entries = [
      {key:`datasets/${publication.dataset_id}`,value:readFileSync(join(out,`api/v1/benchmarks/${publication.dataset_id}.json`),'utf8')},
      {key:`definitions/${publication.dataset_id}`,value:readFileSync(join(out,`api/v1/definitions/${publication.dataset_id}.json`),'utf8')},
      ...retained,
    ];
    const pending = await pendingEntries(entries, cloudflareRetention({account:config.account_id,namespace:config.kv_namespaces.find(n=>n.binding==='PUBLICATIONS').id,token:process.env.CLOUDFLARE_API_TOKEN}));
    console.log(`KV retention: ${pending.length} writes; ${entries.length-pending.length} unchanged entries skipped`);
    for (let i=0; i<pending.length; i+=100) {
      const batch=join(scratch,`retention-${i}.json`);
      writeFileSync(batch,JSON.stringify(pending.slice(i,i+100)));
      run('kv','bulk','put',batch,'--binding','PUBLICATIONS','--remote');
    }
    run("deploy", "--outdir", join(scratch, "bundle"));
    execFileSync(process.execPath, [join(here, "check-live.mjs")], { stdio: "inherit" });
  }
} finally { rmSync(scratch, { recursive: true, force: true }); }
