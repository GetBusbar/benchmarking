# Gateways — drop-in benchmark targets

A gateway is a directory. Drop it in and it appears everywhere; delete it and it disappears. Nothing
else in the tree should learn its name. That is a review rule, not an automated check: the shell lint
that enforced it was deleted in 1127cf1 and nothing replaces it yet (see Rules below).

```
gateways/<name>/
  definition.json     what the harness needs to know           REQUIRED
  env                 environment the gateway process gets      if it needs one
  headers.json        headers that select its upstreams         if it routes by header
  commands            shell lines run once it is up             if it needs them
  build.sh            builds it from a pinned commit            if it is built from source
  why                 why each setting is there (for readers)   every entrant has one
  <its own config>    whatever file the gateway itself reads    if it reads one
```

Only `definition.json` is required by the loader. The count of the rest is decided by the gateway,
not by us. `why` is read by people, not by the harness: nothing parses or checks it.

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

The loader rejects unknown keys, and accepts these optional ones beyond the example (see
`engine/src/manifest.rs`'s `Manifest` for the authoritative list and each field's doc):

| | |
|---|---|
| `matrix` | the declared 6x6 capability grid, six rows (ingress) of six `0`/`1` (egress), axis order `openai openai-responses anthropic gemini cohere bedrock`. A `0` cell is never probed and is published `not_configurable`. Empty = undeclared: every cell is probed. |
| `matrix_note` | the cited reasoning for the `0` cells, published as their reason. Cite the version you researched; if it is not the pinned one, say so. |
| `untestable`, `untestable_note` | `"<ingress>/<egress>"` cells the rig cannot pose (the gateway hard-codes a real upstream host). Never probed, published `untestable` with the note, which must be about exactly these cells. |
| `egress_models` | the model name that selects each egress column; absent = `model`. |
| `cell_paths` | a per-cell ingress path, keyed `"<ingress>><egress>"`; absent = `path`. |
| `headers` | headers sent on every request. A header the rig already sends for a dialect (its credential) is dropped on that dialect, so a credential here only reaches dialects that authenticate differently. |

`runtime` is the gateway's identity, declared **once**. Every memory reader, the readiness check and
the stop path derive from it, so the thing that gets started, the thing that gets measured and the
thing that gets stopped cannot be three different processes. Three manifests once drifted here and
published a gateway's idle memory from one process tree beside its peak from another.

`launch` is `docker` (an image) or `native` (a binary built from source). Prefer, in order: the
project's **official image**, then a **binary the project publishes**, then a **build from a pinned
commit**. The first two are the project's own artifact; the third makes our build flags part of their
number, so it is the fallback and only three entrants need it.

Where the pin lives: a docker image is `launch.image`, pinned by **tag only** (no digest, and none is
recorded in the result, so a re-pushed tag would not be detected). A source build's commit is the
`COMMIT` default in that gateway's own `build.sh` (overridable as `<NAME>_COMMIT` in the build's
environment); `definition.json` carries only its `repo`.

`egress` lists the upstream dialects this gateway is configured for. It is **not** a capability
claim: every cell in the grid is probed regardless, and the board publishes what was observed.

## env

`KEY=value`, one per line. Parsed, never executed.

```
BENCH_MOCK_KEY=dummy
-EXAMPLE_REPO
```

**Do not set a runtime's thread-count variable here.** `GOMAXPROCS`, `UV_THREADPOOL_SIZE`,
`RAYON_NUM_THREADS` and `OMP_NUM_THREADS` are set centrally by the harness for every gateway, from
the size of the core range it pinned, and its values are appended after this file's so a gateway
cannot override them (`manifest.rs`'s `pinned_parallelism`). The harness decides how many cores a
gateway gets, so the harness states it, identically, everywhere. A per-gateway copy is at best
redundant and at worst an invitation to tune one entrant's scheduler against the others.

A gateway whose runtime has its OWN differently-named knob does declare it here with `{NCORE}` (a
vendor-prefixed nginx worker count, say), because naming one entrant's variable in shared code would
put per-gateway logic in the one place that must not have any. That is the only case.

A leading `-` **removes** a variable from the environment the gateway inherits (`native` launches
only: a container gets only the variables declared, and a `-` line on a `docker` launch is currently
ignored, not rejected). That is not tidiness:
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
Anything outside that needs to know about your gateway discovers it by reading this directory. This
is **not currently enforced by any check**: `lib/gateway_isolation_test.sh` was deleted with the
shell-era manifests in 1127cf1, and shared code (site comments in particular) does name entrants
today. Treat it as a review rule.

Its config must be the **bare minimum required to run**: a setting may appear only if it is needed to
boot, to point an upstream at the mock, to expose an ingress path the matrix drives, or to bind the
port and cores the rig requires. Settings declared in `config` must name one of those four
necessities, and `config_lint` checks them - but every entrant today declares `"config": []` and
keeps its real settings in its templates, `env` and `headers`, which no lint reads. The `why` file is
where each of those settings is justified, and keeping it true is a review responsibility.

## Checking it

```
otb validate gateways/<name>           # everything wrong with your setup, in one pass
otb validate                           # the whole field
otb run gateways/<name> <gw> <mock>    # launch it, measure it, stop it
```

`validate` reports ALL problems at once rather than the first, and every message says what to do
about it. It catches the mistakes that otherwise show up as a container which starts and immediately
exits — a declared template that is not there, a placeholder the harness cannot supply, a mount
pointing at a file nothing renders, a header keyed by a dialect this benchmark does not speak. Run
it before you run anything else.
