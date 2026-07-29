# Unproven work from codeaudit round 1 — DO NOT MERGE AS-IS

Four defects did not produce a proven fix. Their partial work is saved here as
patches rather than on `codeaudit/round1-fixes`, because none of them passed
red-before-green plus the project gate, and a branch you merge should contain
only fixes that did.

Base commit for every patch: e543d9dc9ae3494ad45e47352fedf7f0dbc33355

- **fix-0.patch** — HIGH, `bench-audit.py` declared-field gate is scoped
  per-gateway so it cannot fail for the whole-gateway case. The agent reported
  BLOCKED while its own notes say the design was prototyped and verified green;
  the patch contains only the reproduction TEST, not the fix. The defect stands.
  Re-run this one alone.

- **fix-3.patch** — MEDIUM, hardcoded dialect list with no cross-check against
  the engine's `Dialect::ALL`. **DESIGN REFUTED**, and correctly: the adversarial
  reviewer found the proposed test's fixture contained no engine source at all,
  so it passed because the file was missing rather than because the axis order
  was checked — a test that would pass for a broken parser. The defect is real;
  the fix was not. Needs a different design.

- **fix-4.patch** — MEDIUM, `json.load` on `definition.json` with no exception
  handling crashes the whole audit on one malformed file. The agent died without
  returning a structured result, so nothing verified it. This patch touches
  `bench-audit.py` as well as its test, so it is the closest to complete.

- **fix-5.patch** — MEDIUM, `remote_tail()` collapses SSH failure into "",
  displaying a box hours into a run as still booting. Test only, no fix.

Apply with `git apply <patch>` from the repo root, then read it before trusting
it and run the gate yourself.
