# Diagnostic runs

Snapshots here were measured to answer a specific question, not to be published.
They are kept because they are real measurements and the reasoning that used them
should stay checkable — and they are kept OUT of `results/snapshots/` because
`gen-data.mjs` builds the board from the newest snapshot per gateway, and a board
assembled from two different engine commits is not a comparison.

That is not a policy invented here: `check-consistency.mjs`'s C8 invariant refuses
a mixed-instrument board outright, on the grounds that a defect fixed between two
commits applies to only part of the field, so the columns are not comparable.

## result_bifrost_2026-07-29T18-38-18Z.json

Engine `10c6a69a`, while the other twelve gateways on the board were measured by
`5cf833c1`. Run alone to test four specific repairs with predictions stated in
advance. Two held: the config reached the artifact (`bfdata/config.json`), and
snapshots were written incrementally during the run rather than only at the end.
Two failed, and the failures are why this file is worth keeping:

- The direct-to-mock reference still read ~50% of theory (19,312 fps at c=767
  where the gateway leg measured 33,013 and a clean local window measures
  32,496), so the median-of-three and the settle did not address the real cause.
- `cpu_fps` moved from an overshoot suppression to `search_exhausted` at c=4096,
  which traced to a stream-concurrency cap that had no business being a
  measurement bound. Its sweep is the evidence that a ladder ending on failing
  rungs had found the ceiling rather than run out of range.
