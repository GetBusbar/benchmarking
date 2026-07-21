# Gateways — drop-in benchmark targets

Every gateway the benchmark can measure is a directory here. **Adding a gateway = adding a
directory.** The runners (`memory/run.sh`, and friends) are gateway-agnostic: they `source`
`gateways/<name>/gateway.sh` and call a fixed contract. No runner edits, no branching.

## The contract

`gateways/<name>/gateway.sh` sets four variables and defines four functions:

```sh
GW_KIND=native|docker      # informational
GW_PORT=8080               # port the gateway listens on
GW_PATH=/v1/chat/completions   # request path used to probe + load it
GW_MODEL=gpt-4o-mini       # model string put in the request body
GW_AUTH=bench-token        # bearer token the gateway accepts

gw_build()  { :; }         # build/pull/install — idempotent; may be empty
gw_launch() { :; }         # start it, pinned to $CORES, upstream = mock at 127.0.0.1:$MOCK_PORT
gw_rss()    { :; }         # echo current resident memory in MiB
gw_stop()   { :; }         # stop + clean up
```

The runner exports for you: `$MOCK_PORT` (deterministic mock upstream), `$CORES` (cpu pin),
`$GW_DIR` (this gateway's directory, for config files).

The load body is `{"model","messages":[…],"max_tokens":16}` — valid for both OpenAI
`/v1/chat/completions` and Anthropic `/v1/messages`, so a gateway picks its `GW_PATH`/`GW_MODEL`
and it just works. The mock answers both shapes (OpenAI by default, Anthropic for `/messages`).

## Shipped gateways

**In the default run** (serve the mock as a single-box drop-in):

| dir | what | notes |
|---|---|---|
| `busbar/` | Busbar single binary | needs `BUSBAR_BIN`; governance-memory + minted vkey |
| `bifrost/` | maximhq/bifrost (docker), documented pool config | needs Docker |
| `litellm-rust/` | BerriAI compiled AI-gateway beta | **only serves `/v1/messages` via `azure_ai` + the `python-config` reader** — see its `gateway.sh` header (verified against their source) |
| `litellm-python/` | LiteLLM `[proxy]` CLI | pip-installed on first run |
| `portkey/` | Portkey OSS gateway (npx) | routes via `x-portkey-*` headers |
| `kong/` | Kong Gateway + `ai-proxy` (docker, DB-less) | `upstream_url` → mock; config verified locally, mock-hop needs the Linux box (Docker-Desktop host-net) |
| `helicone/` | Helicone AI Gateway (Rust) — **built from source, run native** | no arm64 image is published, so we compile it (pinned commit in `versions.env`) like litellm-rust; `openai` provider `base-url` → mock |
| `gomodel/` | GoModel (ENTERPILOT/GOModel, Go, docker) | `OPENAI_BASE_URL` → mock; discovers routable models from the mock's `/v1/models`; unprotected for pure proxy-overhead |
| `one-api/` | One-API (songquanpeng/one-api, docker) | pinned to `v0.6.10` (the tag that ships an arm64 image); its channel + token are bootstrapped over the admin API automatically in `gw_launch` |

**Documented but opt-in by name** (need multi-container / k8s bring-up — run `run-all.sh <name>` explicitly; each `gateway.sh` header explains what's required):

| dir | what | blocker |
|---|---|---|
| `gptrouter/` | GPTRouter (Writesonic) | docker-compose stack (router + Postgres + queue) + runtime provider registration |
| `arch/` | Arch (Katanemo) | Envoy + Arch services via the `archgw` CLI |
| `envoy-ai/` | Envoy AI Gateway | Kubernetes-native (Envoy Gateway + CRDs) |

## Fairness

Same box, same mock, same load profile, same cpu pin for every gateway. Each is launched the only
way it actually serves the endpoint — no strawmen, no idle-only snapshots. If a gateway can't serve
the endpoint, that's recorded (`served:false`) rather than hidden.
