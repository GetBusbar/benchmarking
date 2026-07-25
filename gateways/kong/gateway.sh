#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Kong Gateway + the ai-proxy plugin (DB-less, docker).
#
# Kong's ai-proxy plugin fronts an OpenAI-shaped /v1/chat/completions and forwards to an upstream
# LLM; `model.options.upstream_url` overrides that upstream, so we point it straight at the mock.
# DB-less declarative config, generated against the runner's mock port. KONG_IMAGE is pinned below
# in this file.
#
# ── ONE STATIC CONFIG (the standard) ──────────────────────────────────────────────────────────────
# ONE config, rendered IDENTICALLY for every lane and every egress column, published verbatim as the
# artifact: install this kong.yml on that box with that image and you reproduce the board. The config
# declares ALL FOUR upstream providers at once; the matrix changes only WHAT THE CLIENT ASKS FOR (one
# request header), never the config bytes.
#
# HOW ONE CONFIG SERVES FOUR PROVIDERS (Kong 3.8.0 source, verified locally — see the run log below):
#   * ai-proxy binds ONE provider per PLUGIN INSTANCE (`local ai_driver = require("kong.llm.drivers."
#     .. conf.model.provider)`, kong/llm/proxy/handler.lua:63/198/246/428; model.provider is required,
#     kong/llm/schemas/init.lua:191-195) — but a declarative config may hold ARBITRARILY MANY ai-proxy
#     instances: plugin uniqueness is the tuple (name, route, service, consumer)
#     (kong/db/schema/entities/plugins.lua:8 `cache_key = { "name", "route", "service", "consumer" }`),
#     so one instance per ROUTE is legal. There is no singleton constraint in ai-proxy/schema.lua.
#   * Routes match on a CLIENT REQUEST HEADER: `headers` is a first-class route field
#     (kong/db/schema/entities/routes.lua:190-199 -> typedefs.headers, typedefs.lua:619-629, a
#     map<header-name, string[]>), compiled to `any(lower(http.headers.x_llm_provider)) == "..."`
#     (kong/router/transform.lua:420-446) under the default traditional_compatible flavor
#     (kong.conf.default:1710). Multiple values on one header OR; multiple headers AND; matching is
#     case-insensitive.
#   * PRIORITY makes the header-less route the natural fallback: get_priority()
#     (kong/router/transform.lua:558-668) packs `match_weight` into the TOP 3 bits (`lshift_uint64(
#     match_weight, 61)`) and increments it once per populated matcher category — so paths+headers
#     (weight 2) STRICTLY outranks paths-only (weight 1). Docs: developer.konghq.com/gateway/
#     entities/route/ ("a Route that specifies both hosts and headers will have a higher priority
#     than one that only specifies hosts").
#   So: four routes on the SAME uniform /v1/chat/completions path — three selected by
#   `x-llm-provider: anthropic|gemini|bedrock`, one header-less fallback = openai. NOT ai-proxy-advanced:
#   that plugin does not exist in OSS at tag 3.8.0 (kong/plugins/ai-proxy-advanced/ is 404 on the OSS
#   repo and absent from constants.lua BUNDLED_PLUGINS; developer.konghq.com/plugins/ai-proxy-advanced/
#   is tier: ai_gateway_enterprise) — this is plain bundled ai-proxy only, in the kong:3.8 OSS image.
#
# Permitted deviations only: provider upstream_url → mock, dummy auth/AWS signing (the mock ignores
# it), the per-provider REQUIRED fields (anthropic_version, bedrock region+creds), and KONG_DATABASE=
# off, which Kong needs to boot without an external Postgres. The client always hits the uniform
# /v1/chat/completions route; no special passthrough route is added.
#
# FAIRNESS AUDIT (Kong 3.8.0 source):
#   * REMOVED KONG_ADMIN_LISTEN=off: that DISABLED a default-on feature. Kong's Admin API is ON by
#     default (kong.conf.default @3.8.0: admin_listen = 127.0.0.1:8001 ... + 127.0.0.1:8444 ssl),
#     bound to localhost, and DB-less only makes it read-only — it does not turn it off. Turning it
#     off was a feature-strip; restored to the default (the var is simply not set). Harmless on a
#     dedicated single-box bench (localhost-only, no port clash, proxy traffic unaffected).
#   * REMOVED KONG_ANONYMOUS_REPORTS=off: anonymous_reports defaults to `on` (kong.conf.default
#     @3.8.0), so Kong phones home usage/error data on a stock install. We measure defaults. Turning
#     a shipped-on feature off is a config change we do not make, in either direction, and the old
#     "isolated rig" justification for it was simply untrue - the bench boxes have full internet
#     access (cloud-init apt-gets and pulls images over it), so the report succeeds rather than
#     hanging. Consequence, disclosed rather than designed around: with per-cell cold starts Kong
#     boots ~36 times a run, so a field run sends Kong a few hundred install reports. That is Kong's
#     shipped behaviour, which is the thing this benchmark exists to measure.
GW_KIND=docker
# The docker container this manifest launches under. DECLARED here so that anything outside this
# directory which needs the name (the local verifier's teardown, for one) READS it from the
# manifest instead of hardcoding it. lib/gateway_isolation_test.sh checks it against the --name
# below, so the two cannot drift.
GW_CONTAINER=kong-bench
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Kong"                      # label in charts + report tables
GW_LANG=Other                            # implementation language → bar color bucket
GW_CLASS="API gateway"   # the project's OWN self-description (README: 'cloud-native API gateway'), not our editorial
GW_REPO=https://github.com/Kong/kong   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
# KONG_MODEL is a MANIFEST CONSTANT, never a per-column value: it is the one model name every route in
# the single config is bound to, and the one name the client sends in every lane. _kong_write_config
# reads THIS (not GW_MODEL) so the rendered bytes cannot depend on whatever a column left in GW_MODEL —
# the column-independence of the render is grep-provable: the render body contains no $GW_MODEL.
KONG_MODEL=gpt-4o-mini
GW_MODEL="$KONG_MODEL"
GW_AUTH=dummy

# ── CONFIG NECESSITY (lib/gateway_config_lint.sh) ─────────────────────────────────────────────────
# Every setting this manifest writes, and the ONE reason it is here. The lint fails on a setting with
# no claim AND on a claim with no setting, so this block cannot drift from the config in either
# direction. Reasons: boot (it will not run without this) | upstream (points an upstream at the test
# mock) | ingress (an ingress path the 6x6 matrix drives) | bind (the port or CPU pin the rig needs).
GW_CONFIG_WHY="
KONG_DATABASE               boot  # DB-less declarative; without it Kong demands an external Postgres
KONG_DECLARATIVE_CONFIG     boot  # where that declarative config is
KONG_PROXY_LISTEN           bind  # the port the harness drives
KONG_NGINX_WORKER_PROCESSES bind  # nginx \`auto\` reads the HOST cpu count, blind to --cpuset-cpus
_format_version boot     # the declarative schema will not load without it
services        boot
name            boot
url             boot     # a service REQUIRES a url; ai-proxy overrides the target per plugin instance
routes      ingress      # four routes on the one uniform OpenAI path
paths       ingress
strip_path  ingress
plugins     ingress
config      boot
headers        upstream  # the route matcher that selects the egress column
x-llm-provider upstream  # the one request header a column changes
route_type   boot        # ai-proxy schema: required
model        upstream    # ai-proxy 400s a body model that differs from a set model.name
provider     upstream    # binds this plugin instance to one upstream dialect
options      upstream
upstream_url upstream    # -> the mock, replacing scheme/host/port AND path
auth              boot   # ai-proxy schema: required
header_name       boot
header_value      boot
anthropic_version boot   # entity-check REQUIRED for the anthropic provider; without it Kong will not boot
bedrock           boot
aws_region        boot   # the bedrock signer fails without a region -> HTTP 500
aws_access_key_id     boot
aws_secret_access_key boot
"
# The client-facing egress SELECTOR header. Unset by default: a header-less request falls through to
# the openai route (see the priority note above), which is exactly the OOTB client experience.
GW_HEADERS=()

KONG_IMAGE="${KONG_IMAGE:-kong:3.8}"
gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$KONG_IMAGE" 2>/dev/null)
  echo "${KONG_IMAGE}${dg:+ (@${dg##*@})}"
}
gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=kong-bench --format '{{.Status}}' 2>/dev/null)"
  echo "logs:"; sudo docker logs --tail 25 kong-bench 2>&1
}

gw_build() {
  _kong_write_config
  sudo docker pull "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1 || true
}

# _kong_write_config: emit THE DB-less declarative config. NO ARGUMENTS — there is one config and the
# render is column-independent by construction: its only inputs are $MOCK_PORT (the rig's mock port,
# constant for a whole run) and the manifest constant $KONG_MODEL. Every egress column loads these
# same bytes; the column changes only the client's `x-llm-provider` header (gw_matrix_egress).
#
# Kong 3.8 ai-proxy always accepts the OpenAI-canonical ingress on /v1/chat/completions (route_type
# llm/v1/chat) and TRANSFORMS it into the configured provider's native upstream shape;
# model.options.upstream_url REPLACES the whole egress URL — scheme, host, port AND path
# (kong/llm/drivers/anthropic.lua:446-464: parse(upstream_url) -> set_path/set_scheme/set_target) —
# so each route points at the mock's own per-dialect endpoint. The parent service `url` is a
# placeholder for exactly that reason (the plugin calls kong.service.set_target()).
#
# Per-provider REQUIRED config (kong/llm/schemas/init.lua @3.8.0 - omitting these was OUR bug that
# published boot failures as Kong reds):
#   anthropic - model.options.anthropic_version is entity-check REQUIRED
#               (conditional_at_least_one_of: "must set %s for anthropic provider"); without it the
#               declarative config fails validation and Kong never boots.
#   bedrock   - configure_request SigV4-signs every request (drivers/bedrock.lua); with no
#               auth.aws_access_key_id/aws_secret_access_key and no ambient AWS credentials the
#               signer fails ("failed to sign AWS request") -> HTTP 500. Dummy keys +
#               model.options.bedrock.aws_region satisfy the signer; the mock ignores the signature.
#   gemini    - the default gemini/bedrock path templates EMBED the model name
#               (kong/llm/drivers/shared.lua:141 `/v1beta/models/%s:%s`, :153 `/model/%s/%s`), so the
#               upstream_url override spells that model out; it is $KONG_MODEL on every route, the
#               same name the client sends, so the URL and the request agree.
# model.name is set on every route: with it set, ai-proxy 400s a request whose body model differs
# ("cannot use own model - must be: ...", kong/llm/proxy/handler.lua:367-386). All four routes carry
# the SAME $KONG_MODEL, so one unchanging client body works on every column.
# cohere is NOT declared: Kong 3.8's cohere driver emits the Cohere *v1* /v1/chat shape, not the v2
# dialect this suite probes, so that egress column is a cited grey (GW_MATRIX_CAP) rather than a route.
_kong_write_config() {
  local url="http://127.0.0.1:$MOCK_PORT"
  cat > "$GW_DIR/kong.gen.yml" <<YAML
_format_version: "3.0"
# ONE static config, four upstream providers. The client picks the upstream with a request header on
# the SAME uniform OpenAI path: \`x-llm-provider: anthropic|gemini|bedrock\`; no header = openai.
# Route priority puts every header-matched route above the header-less openai fallback
# (kong/router/transform.lua get_priority: match_weight occupies the top 3 bits).
services:
  - name: llm
    # Placeholder: ai-proxy overrides host/port/scheme/path per plugin instance via
    # kong.service.set_target() + set_path() from model.options.upstream_url.
    url: http://127.0.0.1:1
    routes:
      - name: chat-anthropic
        paths: ["/v1/chat/completions"]
        headers:
          x-llm-provider: ["anthropic"]
        strip_path: false
        plugins:
          - name: ai-proxy
            config:
              route_type: llm/v1/chat
              auth:
                header_name: Authorization
                header_value: "Bearer dummy"
              model:
                provider: anthropic
                name: $KONG_MODEL
                options:
                  anthropic_version: "2023-06-01"
                  upstream_url: "$url/v1/messages"
      - name: chat-gemini
        paths: ["/v1/chat/completions"]
        headers:
          x-llm-provider: ["gemini"]
        strip_path: false
        plugins:
          - name: ai-proxy
            config:
              route_type: llm/v1/chat
              auth:
                header_name: Authorization
                header_value: "Bearer dummy"
              model:
                provider: gemini
                name: $KONG_MODEL
                options:
                  upstream_url: "$url/v1beta/models/${KONG_MODEL}:generateContent"
      - name: chat-bedrock
        paths: ["/v1/chat/completions"]
        headers:
          x-llm-provider: ["bedrock"]
        strip_path: false
        plugins:
          - name: ai-proxy
            config:
              route_type: llm/v1/chat
              auth:
                aws_access_key_id: "AKIAMOCKACCESSKEY"
                aws_secret_access_key: "mock-secret-access-key"
              model:
                provider: bedrock
                name: $KONG_MODEL
                options:
                  bedrock:
                    aws_region: "us-east-1"
                  upstream_url: "$url/model/$KONG_MODEL/converse"
      # Header-less fallback: the OOTB OpenAI client experience (lowest route priority).
      - name: chat
        paths: ["/v1/chat/completions"]
        strip_path: false
        plugins:
          - name: ai-proxy
            config:
              route_type: llm/v1/chat
              auth:
                header_name: Authorization
                header_value: "Bearer dummy"
              model:
                provider: openai
                name: $KONG_MODEL
                options:
                  upstream_url: "$url/v1/chat/completions"
YAML
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. Kong 3.8 ai-proxy accepts ONLY the OpenAI-canonical ingress (kong/llm/init.lua
# identify_request keys on body.messages[]/body.prompt; there is NO anthropic/gemini/bedrock/cohere
# ingress detector) and fans that one ingress out to the configured provider's native UPSTREAM shape
# via driver.to_format. So the only capable row is openai-ingress, into the egress providers whose
# native Converse/Messages/generateContent shape Kong 3.8 emits with an upstream_url override
# (kong/llm/drivers/shared.lua): anthropic (/v1/messages), gemini (:generateContent), bedrock
# (converse). NOT declared: openai-responses (no llm/v1/responses route_type in 3.8 — the enum is
# {llm/v1/chat, llm/v1/completions, preserve}) and cohere-v2 (Kong 3.8's cohere driver emits the
# Cohere *v1* /v1/chat shape, CHATBOT/chat_history, not the v2 dialect this suite probes) - both grey
# with the cited reason. cohere IS a supported 3.8 provider (schema enum), just at the v1 dialect.
# Evidence: kong/llm/init.lua (ingress detect + route_type enum), kong/llm/drivers/shared.lua
# (upstream_url override + per-provider paths), kong/llm/schemas/init.lua (provider enum), 3.8.0.
GW_MATRIX_CAP="
101101
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress; it emits no OpenAI-Responses route_type (enum: llm/v1/chat|completions|preserve) and its Cohere driver emits the Cohere v1 /v1/chat shape, not the v2 dialect this suite probes (kong/llm/init.lua, drivers/shared.lua, schemas/init.lua @3.8.0)"
GW_MATRIX_EGRESS="openai anthropic gemini bedrock"

# ── xlate lane: not declared (no anthropic-format ingress at 3.8) ────────────────────────────────
# Kong 3.8's ai-proxy ingress detector (kong/llm/init.lua identify_request) keys on the
# OpenAI-canonical body only (messages[]/prompt); there is no Anthropic-Messages ingress detector
# and no /v1/messages route in this manifest's declarative config, so the probe's 404 "no Route
# matched" was Kong's correct answer, not a failed translation.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress (llm/init.lua identify_request has no Anthropic-Messages detector), so anthropic-in -> openai-out translation is not a claimed capability"
# gw_matrix_egress <dialect>: change ONLY what the CLIENT asks for. All four upstream providers are
# already wired in the ONE config (_kong_write_config, rendered identically for every column); the
# column just sets the request header Kong routes on. The config is NOT re-rendered here — the same
# bytes gw_build wrote (and gw_config publishes) serve every column. openai is the header-less
# fallback route, so its column sends no selector header at all.
gw_matrix_egress() {
  case "$1" in
    openai)    GW_HEADERS=();;
    anthropic) GW_HEADERS=("x-llm-provider: anthropic");;
    gemini)    GW_HEADERS=("x-llm-provider: gemini");;
    bedrock)   GW_HEADERS=("x-llm-provider: bedrock");;
    *) return 1;;
  esac
  gw_launch
}

# _kong_env: the ONE definition of Kong's non-secret launch env — the single source of truth that
# gw_launch turns into docker -e flags (like gomodel's _gomodel_env) and gw_config publishes verbatim,
# so the benchmarked env and the website-published env cannot drift.
#   KONG_DATABASE=off        = DB-less declarative (no external Postgres - required to boot).
#   KONG_NGINX_WORKER_PROCESSES = pinned to the cpuset core count (0-3 → 4), NOT Kong's default `auto`.
#     Kong is nginx/OpenResty, and nginx's `worker_processes auto` reads the HOST cpu count via
#     sysconf(_SC_NPROCESSORS_ONLN) — it is BLIND to --cpuset-cpus, so on a 4-core-pinned container it
#     spawns 16 workers thrashing 4 cores, a scheduler-contention HANDICAP the Rust gateways (tokio
#     available_parallelism respects cpuset) never pay. Pinning to the cpuset count emulates the same
#     N-core box every gateway is measured on — the identical CPU-pinning run-mechanic the Go gateways
#     use with GOMAXPROCS (and exactly what nginx `auto` WOULD read on a real 4-core box). Run-mechanic
#     correcting nginx's cpuset-blindness, not a perf/concurrency tune.
#   The Admin API listener is left at its default (ON, localhost:8001/8444) — not disabled.
_kong_env() {
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  cat <<ENV
KONG_DATABASE=off
KONG_NGINX_WORKER_PROCESSES=$ncore
KONG_DECLARATIVE_CONFIG=/kong/kong.yml
KONG_PROXY_LISTEN=0.0.0.0:$GW_PORT
ENV
}

gw_launch() {
  sudo docker rm -f kong-bench >/dev/null 2>&1; sleep 1
  # SINGLE SOURCE: _kong_env() is the one env manifest; gw_launch turns each KEY=value into a docker
  # -e flag and gw_config prints the same bytes, so the run and the published config cannot drift.
  local args=() line
  while IFS= read -r line; do [ -n "$line" ] && args+=(-e "$line"); done < <(_kong_env)
  sudo docker run -d --name kong-bench --network host --cpuset-cpus="$CORES" \
    "${args[@]}" \
    -v "$GW_DIR/kong.gen.yml:/kong/kong.yml:ro" \
    "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config Kong launches with — and because there is now exactly ONE
# config, what is published IS what ran in every lane and every egress column, byte for byte. Kong is
# file-driven, so the artifact is the rendered DB-less declarative config (exactly what
# KONG_DECLARATIVE_CONFIG loads) PLUS the non-secret launch env (any auth/AWS values in the config are
# dummy on the isolated rig). Read from the file _kong_write_config rendered (re-rendering with the
# same no-argument function if absent), so it can never drift from what Kong loaded. The launch env is printed from
# the SAME _kong_env() gw_launch consumes, so the two cannot drift. OOTB posture: ai-proxy on the
# uniform /v1/chat/completions route, admin API left at its default (not disabled), telemetry left at
# its default (not suppressed); the only non-provider settings are KONG_DATABASE=off (Kong cannot boot
# DB-less without it) and KONG_NGINX_WORKER_PROCESSES pinned to the cpuset core count (nginx `auto`
# misreads the host's cores under --cpuset-cpus; the same CPU pin the Go gateways get from GOMAXPROCS).
gw_config() {
  local cfg="$GW_DIR/kong.gen.yml"
  echo "# ── kong.gen.yml (rendered DB-less declarative; loaded via KONG_DECLARATIVE_CONFIG) ──"
  [ -f "$cfg" ] || _kong_write_config
  cat "$cfg"
  echo
  echo "# ── launch env (non-secret) ──"
  _kong_env
}

gw_rss() { container_rss_mib kong-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib kong-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f kong-bench >/dev/null 2>&1; }
# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch).
#
# LOCAL VERIFICATION of the one-config standard (kong:3.8 + the pinned recording mock, --network host,
# THIS manifest's rendered kong.gen.yml, config bytes untouched between the four probes — same file
# sha256 before and after): one OpenAI-shaped POST /v1/chat/completions per column, selector header
# only, read back from the mock's /__mock/state recorder:
#   no header               -> HTTP 200  X-Kong-LLM-Model: openai/gpt-4o-mini
#                              upstream openai      body_ok=true  /v1/chat/completions
#   x-llm-provider: anthropic -> HTTP 200  X-Kong-LLM-Model: anthropic/gpt-4o-mini
#                              upstream anthropic   body_ok=true  /v1/messages
#   x-llm-provider: gemini    -> HTTP 200  X-Kong-LLM-Model: gemini/gpt-4o-mini
#                              upstream gemini      body_ok=true  /v1beta/models/gpt-4o-mini:generateContent
#   x-llm-provider: bedrock   -> HTTP 200  X-Kong-LLM-Model: bedrock/gpt-4o-mini
#                              upstream bedrock     body_ok=true  /model/gpt-4o-mini/converse
# Four native upstream dialects, ONE config, zero reconfiguration. The EC2 field run still turns each
# declared-1 CELL green or red under load; every grey cell is a cited capability limit.
