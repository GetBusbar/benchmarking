# Gateways — drop-in benchmark targets

A gateway is a directory. Drop it in and it appears everywhere; delete it and it disappears. Nothing
else in the tree learns its name, and a lint enforces that on every push.

```
gateways/<name>/
  definition.json     what the harness needs to know           REQUIRED
  env                 environment the gateway process gets      if it needs one
  headers.json        headers that select its upstreams         if it routes by header
  <its own config>    whatever file the gateway itself reads    if it reads one
```

Only `definition.json` is required. One entrant is configured entirely through its image and has
nothing else; another needs four files. The count is decided by the gateway, not by us.

## definition.json

The same shape for every entrant, so two gateways can be compared by reading them side by side.

```json
{
  "name": "example",
  "display": "Example",
  "lang": "Rust",
  "class": "AI gateway",
  "repo": "https://github.com/example/example",
  "port": 8080,
  "path": "/v1/chat/completions",
  "model": "gpt-4o-mini",
  "auth": "dummy",
  "egress": ["openai", "anthropic"],
  "runtime": { "kind": "docker", "container": "example-bench" },
  "launch": { "kind": "docker", "image": "example/example:1.2.3", "args": [], "mounts": [] },
  "config_files": [],
  "constants": {},
  "config": []
}
```

`runtime` is the gateway's identity, declared **once**. Every memory reader, the readiness check and
the stop path derive from it, so the thing that gets started, the thing that gets measured and the
thing that gets stopped cannot be three different processes. Three manifests once drifted here and
published a gateway's idle memory from one process tree beside its peak from another.

`launch` is `docker` (an image) or `native` (a binary built from source). Prefer, in order: the
project's **official image**, then a **binary the project publishes**, then a **build from a pinned
commit**. The first two are the project's own artifact; the third makes our build flags part of their
number, so it is the fallback and only three entrants need it.

`egress` lists the upstream dialects this gateway is configured for. It is **not** a capability
claim: every cell in the grid is probed regardless, and the board publishes what was observed.

## env

`KEY=value`, one per line. Parsed, never executed.

```
BENCH_MOCK_KEY=dummy
GOMAXPROCS={NCORE}
-EXAMPLE_REPO
```

A leading `-` **removes** a variable from the environment the gateway inherits. That is not tidiness:
one entrant's config loader claims every variable sharing its prefix and rejects unknown fields, so
the harness's own override variables killed config load before the port bound. The process is
backgrounded, so the launch still reported success and the only symptom was a port that never
listened — thirty six cells lost to a variable name.

## headers.json

Only if the gateway selects its upstream from a request header. Keyed by **egress column**:

```json
{
  "anthropic": ["x-llm-provider: anthropic"],
  "gemini":    ["x-llm-provider: gemini"]
}
```

Authentication headers do **not** go here. What the client sends to authenticate is decided by the
**ingress dialect** and is identical for every gateway — anthropic ingress uses `x-api-key` and
`anthropic-version`, gemini uses `x-goog-api-key`, the rest use `authorization: Bearer`. That lives
in the engine, once, so thirteen copies of one table cannot drift.

## The gateway's own config

If the gateway reads a config file, write a **template** beside it, named after the file it produces
with `.tmpl` appended: `config.gen.yaml.tmpl` produces `config.gen.yaml`. Keep it in the gateway's
own format. It is the artifact the gateway actually boots on, and the fairness rule is about its
contents, so it has to stay readable and diffable.

Declare it in `definition.json`:

```json
"config_files": [{ "template": "config.gen.yaml.tmpl", "output": "config.gen.yaml" }]
```

and mount, or point at, the **output** in `launch`.

### Placeholders

The harness supplies these, resolved when the gateway starts:

| | |
|---|---|
| `{MOCK_PORT}` | the mock upstream's port |
| `{GW_PORT}` | the port this gateway is driven on |
| `{GW_MODEL}` | the model name the client sends |
| `{GW_AUTH}` | the credential the client sends |
| `{GW_DIR}` | this directory, absolute |
| `{CORES}` | the CPU list the gateway is pinned to |
| `{NCORE}` | how many cores that is |

Anything else goes in `constants` and is referred to by the same syntax. Declare a value **once**
there and refer to it from every template that needs it — a model name spelled in a route and again
in a probe path is how a previous version lost a whole egress column when the two drifted.

A `{NAME}` the harness cannot supply is a **hard error**, not passed through. A gateway booting with
a literal `{MOCK_PORT}` in an upstream URL fails in a way that looks like the gateway being broken.
For a literal brace — some config formats use them, and a comment may document a URL shape — write
`{{` and `}}`.

## Rules

Your gateway's name must not appear anywhere outside its own directory, **including in comments**.
`lib/gateway_isolation_test.sh` enforces this and self-tests that it can actually fail. Anything
outside that needs to know about your gateway discovers it by reading this directory.

Its config must be the **bare minimum required to run**. Every setting declared in `config` names
which of four necessities justifies it, and a setting that merely turns a feature on has no way to be
declared. That is the point.

## Checking it

```
cargo test --lib manifest::            # it parses, launches, and its templates render
bash lib/gateway_isolation_test.sh     # its name is not leaking outside this directory
otb run gateways/<name> <gw> <mock>    # launch it, measure it, stop it
```
