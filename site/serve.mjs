#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// The production handler with a filesystem ASSETS binding for local checks.
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { resolve, extname, sep } from "node:path";
import { createHash } from "node:crypto";
import worker from "./worker.mjs";
const root = resolve(process.argv[2] || new URL("./dist", import.meta.url).pathname);
const types = { ".html": "text/html", ".json": "application/json", ".js": "application/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".woff2": "font/woff2", ".xml": "application/xml", ".gz": "application/gzip", ".md": "text/markdown", ".csv": "text/csv", ".txt": "text/plain" };
export function fileAssets(directory) {
  return { async fetch(request) {
    const file = resolve(directory, "." + decodeURIComponent(new URL(request.url).pathname));
    if (!file.startsWith(directory + sep)) return new Response(null, { status: 404 });
    try {
      const [bytes, info] = await Promise.all([readFile(file), stat(file)]);
      return new Response(bytes, { headers: { "Content-Type": types[extname(file)] || "application/octet-stream", ETag: '"' + createHash("sha256").update(bytes).digest("hex") + '"', "Last-Modified": info.mtime.toUTCString() } });
    } catch { return new Response(null, { status: 404 }); }
  } };
}
if (process.argv[1] && resolve(process.argv[1]) === new URL(import.meta.url).pathname) {
  const env = { ASSETS: fileAssets(root) };
  const server = createServer(async (req, res) => {
    try {
      const response = await worker.fetch(new Request(`http://localhost:${server.address().port}${req.url}`, { method: req.method, headers: req.headers }), env);
      res.writeHead(response.status, Object.fromEntries(response.headers));
      res.end(Buffer.from(await response.arrayBuffer()));
    } catch (e) { console.error(e); res.writeHead(500); res.end("Server error"); }
  });
  server.listen(Number(process.env.PORT || 8899), "127.0.0.1", () => console.log(`On the Bench: http://127.0.0.1:${server.address().port}`));
}
