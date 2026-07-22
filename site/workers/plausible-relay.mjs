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
      let response = await cache.match(request);
      if (!response) {
        response = await fetch(UPSTREAM_SCRIPT);
        response = new Response(response.body, response);
        response.headers.set("Cache-Control", "public, max-age=21600");
        ctx.waitUntil(cache.put(request, response.clone()));
      }
      return response;
    }

    // Proxied event ingest.
    if (request.method === "POST" && url.pathname === EVENT_PATH) {
      const headers = new Headers(request.headers);
      headers.delete("cookie");
      const clientIP = request.headers.get("CF-Connecting-IP");
      if (clientIP) headers.set("X-Forwarded-For", clientIP);
      return fetch(UPSTREAM_EVENT, { method: "POST", headers, body: request.body });
    }

    return new Response("Not found", { status: 404 });
  },
};
