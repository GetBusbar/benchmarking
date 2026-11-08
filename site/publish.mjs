#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// The scheduled refresh and an explicit release use exactly this path.
import { build } from "./build.mjs";
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
  execFileSync(process.execPath, ["--test", join(here, "publishing.test.mjs"), join(here, "history.test.mjs")], { stdio: "inherit", env: { ...process.env, OTB_TEST_ARTIFACT: out } });
  const config = JSON.parse(readFileSync(join(here, "wrangler.jsonc"), "utf8"));
  config.main = join(here, "worker.mjs"); config.assets.directory = out;
  const configPath = join(scratch, "wrangler.json"); writeFileSync(configPath, JSON.stringify(config));
  const run = (...args) => execFileSync(process.execPath, [wrangler, ...args, "--config", configPath], { stdio: "inherit", env: { ...process.env, WRANGLER_SEND_METRICS: "false" } });
  if (process.argv.includes("--dry-run")) run("deploy", "--dry-run", "--outdir", join(scratch, "bundle"));
  else {
    // Retain the complete evidence collection across later deployments. Its key
    // is a hash of its content; star refreshes do not create another identity.
    run("kv", "key", "put", `datasets/${publication.dataset_id}`, "--binding", "PUBLICATIONS", "--path", join(out, `api/v1/benchmarks/${publication.dataset_id}.json`), "--remote");
    run("kv", "key", "put", `definitions/${publication.dataset_id}`, "--binding", "PUBLICATIONS", "--path", join(out, `api/v1/definitions/${publication.dataset_id}.json`), "--remote");
    // Content-addressed keys never overwrite a different observation. Batch to
    // keep the CLI request bounded; archive retention is independent of Git reachability.
    for (let i=0; i<retained.length; i+=100) {
      const batch=join(scratch,`retention-${i}.json`);
      writeFileSync(batch,JSON.stringify(retained.slice(i,i+100)));
      run('kv','bulk','put',batch,'--binding','PUBLICATIONS','--remote');
    }
    run("deploy", "--outdir", join(scratch, "bundle"));
    execFileSync(process.execPath, [join(here, "check-live.mjs")], { stdio: "inherit" });
  }
} finally { rmSync(scratch, { recursive: true, force: true }); }
