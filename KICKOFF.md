# Kickoff: finish the board

Written 2026-07-29 at the end of a long session. State, findings, and what to do next.
Delete this file once the board is published.

> **SUPERSEDED IN PART, 2026-07-30.** Read the metric names below as HISTORY, not as the current
> artifact. Everything this file says about `rps_sustained_20ms`, `rps_max_proxy` and `cpu_fps` was
> true when written; all three are now deleted.
>
> - The two throughput scalars are replaced by `CellPerf.frontier`: six readings off ONE sweep, at
>   tail-latency bounds of 1/5/10/50/100 ms plus one unbounded. A scalar could not express the
>   tradeoff, and the two of them could invert against each other because two algorithms read one set
>   of windows. See `engine/src/frontier.rs`.
> - `cpu_fps` is retired outright. Of the 16 cells that published both it and `streams_sustained_fps`,
>   4 had it INVERTED below the proven delivery boundary, 5 were redundant within 1%, and 7 were
>   measured at a concurrency where the delivery gate did not hold - a frame rate recorded while
>   dropping frames.
> - The "p99 under 1 s" bar quoted on the old surfaces was never enforced: the retired gate was 20 ms.
>
> The acceptance bar in the next section - every cell needs data, `n/a` is a major issue - still
> stands, and the frontier serves it better: nothing is suppressed now, so a measured number always
> reaches the board even when what it MEANS is open.

## THE REQUIREMENT: every cell needs data. `n/a` is a major issue.

This is the acceptance bar, set by the owner while looking at the live site. A board where most
cells read `n/a` is not a benchmark, it is a table of holes. Screenshots taken 2026-07-28 evening
are the specification; what they showed:

**Performance, Same / OpenAI diagonal**

```
APISIX     OpenAI   p50 199    p99 433    sustained 18,796 @170   max 19,202 @64   <- complete
Helicone   OpenAI   p50 277    p99 301    sustained 14,567 @153   max 14,675 @64   <- complete
One-API    OpenAI   p50 n/a    p99 n/a    sustained 0             max 40 @16
Plano      OpenAI   p50 n/a    p99 n/a    sustained 0             max 85 @512
agentgateway, AISIX, Bifrost, Busbar, GoModel, Kong, LiteLLM-Python, Portkey, TensorZero: all n/a
```

**Memory**

```
One-API       OpenAI    idle 85.4   steady 142.5  growth 0.0  recovered 142.5   <- complete
APISIX        OpenAI    idle 178.3  steady 210.9  growth 0.0  recovered 210.9   <- complete
LiteLLM-Rust  (no pill) idle 251.8  steady n/a    growth n/a  recovered n/a
Plano         OpenAI     everything n/a
everyone else: all n/a
```

Two rows out of fourteen are complete. That is the problem to solve.

### The `n/a`s are THREE different bugs. Do not treat them as one.

**Class 1 - never measured.** The nine gateways reading all-`n/a` had not landed when the run was
killed. Legitimately absent; resolves when a full 14-gateway run completes. Not a defect.

**Class 2 - measured, but the metric refused to produce.** THIS IS THE REAL BUG CLASS and it is
where the effort goes.
  - `added_latency_p50/p99 = n/a` for One-API and Plano, which are the two SLOW gateways
    (34ms and 219ms p99 at c=1). Their throughput measured fine. Suspect a reliability gate on the
    c=1 window that always trips on a slow gateway - see `not_measured_text="added latency not
    measured"` and the TTFT charts' "unreliable c1 window" wording in `charts.py`.
  - `streams_sustained` / `cpu_fps` absent on nearly every cell - the streaming gate calibration
    documented in the next section. `cpu_fps` present on 1 of 16 served cells.
  - The pattern in both: a gate strict enough that real gateways fail at every rung, so the search
    returns absence instead of a number. A gateway that cannot pass at ANY concurrency must publish
    a measured result with a REASON, never a bare `n/a`.

**Class 2b - the `n/a` MEANS "better than we can measure", and it is the best possible result.**
This is the most damaging of the four, because it turns a win into a hole. Evidence from the
Streaming page, Same / OpenAI:

```
Helicone   added TTFT p50 413     p99 476       added gap p50 0      p99 n/a
APISIX     added TTFT p50 10,578  p99 11,162    added gap p50 n/a    p99 9,022
```

APISIX has a p99 and NO p50. That is impossible for two percentiles of one distribution, and it
gives the game away: `added_gap_*` is a DIFFERENCE of percentiles (gateway leg minus the direct-to-
mock leg). The engine was changed this session so that a negative raw difference publishes as absent
rather than being clamped to zero - honest, because the gateway's gap was not measurably above the
mock's. But "the gateway adds no detectable inter-frame gap" is the BEST outcome the test can
express, and it currently renders exactly like "we never measured this".

Fix: absence caused by a difference at or below measurement resolution must render as its own thing
- "<1us", "none measurable", "0" with a note - never as `n/a`. `Measurement` already carries a
reason; the table is discarding it. Check `added_gap_p50_us`, `added_gap_p99_us`,
`added_ttft_p50_us`, `added_ttft_p99_us`, `added_latency_p50_us`, `added_latency_p99_us` - all six
are differences and all six have this failure mode.

**Class 3 - the number exists and the UI drops it.** Pure rendering, verifiable against the JSON.
  - LiteLLM-Rust memory: `251.8` published with `Tested on: n/a`, and steady/growth/recovered
    blank while the artifact carries `steady_state_rss_mib 255.0`, `recovered_rss_mib 255.0`,
    `growth_rate_mib_per_min 0.0`, `plateaued true`. The row IS measured. The table shows one field.
  - Plano memory: an `OpenAI` pill with every value `n/a` - a row advertising a measurement it does
    not have.
  Both live in `colTested` / `LANE_RECORD` in `site/app.js`. `colTested`'s own comment states the
  contract being broken: "NO record -> NO pill. A row whose every column reads n/a must not
  advertise a measurement."

### Definition of done

For every gateway x every declared cell, each published metric is either a real number, or an
explicit measured-and-failed value carrying the reason it failed. No bare `n/a` on a cell the
gateway serves. `bench-audit.py` is the place to encode that as a gate so it cannot regress.

## Where things stand

- **No boxes running.** All gateway-bench instances terminated, 0 volumes, 0 elastic IPs.
- **No board.** Every snapshot deleted from `results/snapshots/`. Recoverable from git history
  (`636e678d` has the last dc7a53c board; `HEAD~1` at time of writing has the partial 939896b run).
- **Engine is at `939896b`** plus the site fixes after it. Nothing measured is published.
- **The site deploys green** and shows all 14 gateways with their pinned version and
  `last benchmarked: n/a`.

## The one thing to fix before rerunning: streaming gate calibration

This is the "blatant missing cell data" problem, and it is NOT a regression. It was equally true
on the old engine.

The c=1 streaming leg works everywhere: every served cell reports `64 frame(s) through the gateway
and 64 direct to the mock, out of a 64 frame budget`. Perfect delivery. `added_ttft_*` is populated.

What produces nothing is the two SEARCH-based stream metrics:

```
apisix   openai>openai   stream_served=True  ttft=10,578us   streams_sustained=None  cpu_fps=None
helicone openai>openai   stream_served=True  ttft=413us      streams_sustained=None  cpu_fps=None
kong     openai>openai   stream_served=True  ttft=105,934us  streams_sustained=0     cpu_fps=None
```

Across the whole board: `cpu_fps` present on 1 of 16 served cells, `streams_sustained` on 6 of 16.

Cause: `streams_gate_passes` demands
- `STREAM_MIN_DELIVERY_RATIO = 1.0` (every frame, no exceptions), and
- no gap past `STREAM_STALL_MULTIPLIER = 2` times the mock's pace.

The mock paces deltas at `MOCK_STREAM_INTERVAL_MS = 20`, so the stall bound is 40ms. Under
concurrency any gateway whose inter-frame gap crosses 40ms fails EVERY rung, so the search returns
absence instead of a ceiling. apisix adds 10.6ms at c=1 before concurrency is applied at all; kong
adds 106ms.

The 1.0 delivery ratio is a deliberate product decision ("no frames should be lost") and should
stay. The **stall bound** is the part to look at: 2x a 20ms pace is a tight budget that mostly
measures whether the gateway can keep to the mock's clock, not whether it drops anything. Decide
whether the metric is "keeps pace" or "delivers everything", and calibrate to that. Whatever is
chosen, a gateway failing at every rung must publish a REASON, not a bare absence.

## Corrections to things I said earlier in the session (do not re-derive from my claims)

1. **There is no `cpu_fps` regression.** I reported 28% -> 6% and killed the run over it. That was a
   composition artifact: I compared a 13-gateway board to a 6-gateway partial board. Matched
   per-gateway, yields are identical (apisix 0->0, helicone 0->0, kong 0->0, litellm-rust 1->1,
   one-api 0->0; only plano 1->0, and plano dies mid-run). The run did not need to be killed.
2. **kong's top-rung peak is pre-existing.** The old board had the same failure on agentgateway,
   aisix, litellm-python and plano. The union stopping rule reduced it; I over-claimed that it would
   eliminate it. Still worth fixing, still not a regression.
3. **one-api and plano really are that slow.** plano's old run on the OLD engine shows the identical
   `c=1 = [4, 4, 4]`. Not the instrument.

## Open UI bugs (all verified against artifacts, none fixed)

1. **`rps_sustained_20ms = 0` renders as a bare number.** It means "no concurrency held the 20ms
   ceiling", and it reads as "this gateway does nothing" next to a real max. `ZERO_NO_CEILING` /
   `zeroNote` machinery exists in `seal.mjs` and is not reaching the table. This is the
   worst-looking item on the live site.
2. **Memory: a value with no provenance pill.** litellm-rust shows Idle `251.8` with `Tested on:
   n/a`. `colTested`'s own contract says a published number must carry its cell.
3. **Memory: a pill with no values.** plano shows an `OpenAI` pill with every column `n/a`. Same
   invariant, opposite direction. Both are in `colTested` / `LANE_RECORD` in `site/app.js`.

## Already fixed and pushed this session

- `fix(engine): answer both throughput questions from one sweep` - one climb per cell produces both
  `rps_max_proxy` and `rps_sustained_20ms`, so they can no longer describe two different states of
  the gateway. Deleted the second search.
- `fix(engine): do not re-measure a ceiling the climb already measured` - `confirm_ceiling` was
  re-probing when the bisection never moved, spending windows inside `Throughput` that starved the
  streaming legs.
- `fix(site): the seal and its oracle are one rule, not two copies` - `displayedValue` is now shared;
  the oracle had never learned about paced metrics. This is what had blocked EVERY deploy since
  2026-07-27, 25 mismatches.
- `fix(site): a guard with no input has not gone inert` - coverage and RED self-tests now scale to
  board completeness, so a run publishes as each gateway lands instead of freezing behind the
  slowest box. Completeness is counted against the manifests, not the bundle.
- `fix(site): added latency ranks the peak cell, it does not qualify it` - `bestCell` required
  `added_latency_p99_us` as a precondition, silently deleting whole Peak rows (one-api, plano).
- `fix(charts): an absence may not name a cause the chart cannot establish`.
- `bench-audit.py` + `bench-audit_test.py`, wired into CI - the cross-metric invariants as a program
  that exits non-zero, with each check proven able to fail.
- Version bumps: agentgateway v1.4.0, bifrost v1.6.6, gomodel 0.1.63, kong 3.9.3, litellm-python
  v1.94.0. All publish linux/arm64; kong was booted for real against this repo's declarative config.
- one-api no longer declares `openai/anthropic` and `openai/gemini`, which it had declared AND
  marked untestable.
- `cargo fmt --all --check` now gates CI.

## The gap that caused a bad sign-off

I certified the engine on `cargo test` + `fmt` + `clippy`. None of those measure whether a metric is
still PRODUCED on real cells. Add a yield gate: assert that a search which used to return values
still returns them, so "a search quietly stopped producing" fails CI instead of passing it. Without
it the next sign-off is worth no more than mine was.

## Suggested order

Everything above class 1 is about ONE thing: no cell the gateway serves may read a bare `n/a`.

1. **Class 2b, the difference-metrics.** Six fields (`added_latency_p50/p99`, `added_ttft_p50/p99`,
   `added_gap_p50/p99`) render "below measurement resolution" as `n/a`, which hides the best result
   the test can produce. Highest value, smallest change, and it is pure rendering plus a reason
   that already exists on the `Measurement`.
2. **Class 3, the two memory UI bugs.** litellm-rust publishes one field of four it measured;
   plano advertises a pill with nothing behind it.
3. **`rps_sustained_20ms = 0`** must read as "no concurrency held the 20ms ceiling", not as a
   throughput of zero beside a real maximum.
4. **Class 2, the gates that never pass.** Streaming gate calibration, and whatever suppresses
   `added_latency` on slow gateways. A gateway failing at every rung must publish a measured result
   with a reason.
5. **Yield gate in CI**, so a metric that stops being produced fails a test.
6. **kong top-rung peak.**
7. **Relaunch 14.** `./run-on-ec2.sh` with no args. Results publish per gateway as each box
   finishes; the site deploys green on partial boards now.

Encode the definition of done in `bench-audit.py` BEFORE the rerun, so the next board is judged by a
program rather than by screenshots.

## Standing constraints

- Never `Co-Authored-By` or any AI attribution in commits, PRs, or amends.
- `git -C <path>` rather than `cd <path> && git`.
- Never `git add results/`.
- No em dashes in prose, code, or commits.
- Rule 0: no gateway product name outside its own `gateways/<name>/` directory.
- Never run `charts.py` while a box is live.
