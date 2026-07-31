# The board is at [onthebench.ai](https://onthebench.ai)

This page used to show a top-five cut of the field as static charts. The live board outgrew it, and
this page in particular: "top five" was a choice baked in at render time, by one metric, before the
reader arrived. The site sorts by any column, at any tail-latency bound, over the whole field — so
the top five is whatever the reader's question makes it, not whatever this file was generated with.

The raw results are still here and remain the source of everything published:
[`results/snapshots/`](../../snapshots/) — one JSON per gateway per run, carrying every rung, every
sample, and the engine commit that measured it.
