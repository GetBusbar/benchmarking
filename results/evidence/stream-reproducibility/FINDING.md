# `streams_sustained` is not reproducible

**Status: open. This metric should not be trusted at the precision the board prints it.**

Measured 2026-07-31 by re-running four gateways (agentgateway, aisix, busbar, one-api) on a
dialect-scoped grid, and by reading the repeated windows already present in every run.

## What was measured

Two independent checks, one within a run and one across runs.

### Within a run: a rung disagrees with itself

The bisection takes `WINDOWS_PER_RUNG` windows at the same concurrency and takes a majority. Those
repeats are a free repeatability experiment, and they fail it:

| engine | repeated rungs | disagreed with themselves | signature `[pass, fail, fail]` |
|---|---|---|---|
| `afe51ee1` (the published board, all 14) | 44 | 28 (**64%**) | 14 |
| `96d56db` (after the settle fix, 4) | 23 | 13 (**57%**) | 11 |

The disagreement is not random. In 25 of the 41 cases the FIRST window at a concurrency passes and
every repeat of it fails. A coin would not do that. Something degrades with repetition and does not
recover between windows.

### Across runs: the published number moves by 2x

Same gateway, same cell, same rig specification, two runs:

| metric | median ratio | worst | n |
|---|---|---|---|
| `added_ttft_p99_us` | 1.11x | 1.37x | 13 |
| `streams_sustained` | **1.86x** | **2.12x** | 6 |

Concretely: agentgateway `anthropic>anthropic` 4,356 -> 2,056. aisix `anthropic>anthropic`
4,376 -> 2,152. aisix `openai>anthropic` 6,148 -> could not be measured at all. one-api
`openai>openai` proved 341 in one run and 170 in the next.

## What this means

- **`added_ttft_*` is fine.** It moves by ~11% and the ranking is stable across runs (busbar ~250us,
  agentgateway ~350-500us, aisix ~500us, one-api ~700us+). It is a different measurement taken at low
  concurrency and it does not share this defect.
- **`streams_sustained` (and the `_fps`, `_headroom`, `_mock_ceiling` figures derived from the same
  bisection) is not reproducible** and a single published value overstates what the rig knows. Two
  honest runs disagree by up to 2.1x.

## What was already fixed, and why it was not enough

`96d56db` added a settle between the ladder's own windows and stopped attributing a rung that fails
below a proven-clean one to the gateway. That was a real defect - the engine settled only AFTER the
ladder, protecting the reference measurement and not the re-measurements that get published - and it
did convert 5 of 6 previously unmeasurable cells into numbers.

But the settle is proportional and capped at 10s, free below 512 concurrent streams, and the
`[pass, fail, fail]` signature survives it at 57%. So the residue is either larger than 10s, or it is
not (only) socket drain. Candidates not yet ruled out: the gateway's own connection table, the mock's
accept backlog, or the loadgen holding an `SseReader` per stream.

## The rejected shortcut

A "proven floor" rule was written and reverted: when the bisection failed to converge, publish the
highest rung the ascending sweep passed, as a lower bound. It is unsound for the same reason the rest
of this document describes - the ascending sweep's rungs are SINGLE windows, so its "clean prefix"
inherits exactly the first-window-passes bias measured above.

It was also wrong in both directions on real data. one-api `openai>openai`: floor would have published
>= 256 where the re-measured answer is 170 (**overstates a competitor**). busbar `openai>openai`: floor
would have published >= 2,048 where the re-measured answer is 2,761 (understates). A bound that is
neither an upper nor a lower bound is not a bound.

## Artifacts

The four scoped re-run snapshots in this directory are the evidence. They are kept OUT of
`results/snapshots/` deliberately: they were measured on `96d56db` while the published board was
measured on `afe51ee1`, and invariant C8 refuses to mix two instruments on one board. They are not
published numbers; they are the record of an experiment.
