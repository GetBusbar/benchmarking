// Plausible analytics reverse-proxy for onthebench.ai.
// Fronts Plausible so requests never touch plausible.io from the browser
// (adblock-resistant) and are served from the site's own origin under /relay/.
//
//   GET  /relay/js/script.js  -> https://plausible.io/js/pa-duWXDrBvUrYVaSx6gqjuS.js  (edge-cached)
//   POST /relay/api/event     -> https://plausible.io/api/event  (cookies stripped, client IP forwarded)
//
// Mirrors the getbusbar.com "plausible-relay" Worker, minus the server-side
// install-counting routes (onthebench.ai has no install.sh / providers.yaml).
//
// Deploy:  cd site/workers && npx wrangler@4 deploy --config wrangler.plausible-relay.toml
// Routes are declared in that wrangler config and only take effect once the
// onthebench.ai nameservers point at Cloudflare (see the cutover runbook).

const SCRIPT_PATH = "/relay/js/script.js";
const EVENT_PATH = "/relay/api/event";
const UPSTREAM_SCRIPT = "https://plausible.io/js/pa-duWXDrBvUrYVaSx6gqjuS.js";
const UPSTREAM_EVENT = "https://plausible.io/api/event";

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);

    // Proxied Plausible script (edge-cached).
    if (request.method === "GET" && url.pathname === SCRIPT_PATH) {
      const cache = caches.default;
      // ONE cache key for the script, whatever query string the request carried: keying on the full URL
      // let every distinct ?x= bypass the cache and mint a new entry plus an upstream fetch.
      const key = new Request(url.origin + SCRIPT_PATH);
      let response = await cache.match(key);
      if (!response) {
        let upstream;
        try {
          upstream = await fetch(UPSTREAM_SCRIPT);
        } catch {
          return new Response("analytics script unavailable", { status: 502, headers: { "Cache-Control": "no-store" } });
        }
        response = new Response(upstream.body, upstream);
        // ONLY A GOOD SCRIPT IS CACHED. A 429/5xx/404 from plausible.io was stamped public for six hours
        // and served to every visitor as the analytics script.
        if (!upstream.ok) {
          response.headers.set("Cache-Control", "no-store");
          return response;
        }
        response.headers.set("Cache-Control", "public, max-age=21600");
        ctx.waitUntil(cache.put(key, response.clone()));
      }
      return response;
    }

    // Proxied event ingest.
    if (request.method === "POST" && url.pathname === EVENT_PATH) {
      const headers = new Headers(request.headers);
      headers.delete("cookie");
      // Only the edge's own view of the client IP is forwarded; a client-supplied X-Forwarded-For is
      // never passed through as if Cloudflare had vouched for it.
      headers.delete("X-Forwarded-For");
      const clientIP = request.headers.get("CF-Connecting-IP");
      if (clientIP) headers.set("X-Forwarded-For", clientIP);
      try {
        return await fetch(UPSTREAM_EVENT, { method: "POST", headers, body: request.body });
      } catch {
        return new Response("analytics relay unavailable", { status: 502, headers: { "Cache-Control": "no-store" } });
      }
    }

    return new Response("Not found", { status: 404 });
  },
};
