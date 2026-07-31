# The full board is at [onthebench.ai](https://onthebench.ai)

This page used to render every gateway as a stack of static charts. The live board outgrew it.

A picture cannot answer a question asked after it was drawn. The site switches the tail-latency bound
and re-ranks the field in front of you, chooses which cell each row reads, says *why* a number is
absent instead of leaving a gap, and marks the rows the rig cannot actually tell apart.

It was also a second surface publishing the same numbers, which is a way to be wrong twice: on
2026-07-31 the chart toolchain silently failed to regenerate and these pages carried one run's images
beside another run's data. One board, generated from the committed snapshots, cannot drift from
itself.

The raw results are still here and remain the source of everything published:
[`results/snapshots/`](../../snapshots/) — one JSON per gateway per run, carrying every rung, every
sample, and the engine commit that measured it.
