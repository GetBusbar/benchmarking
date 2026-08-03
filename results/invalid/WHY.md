# Snapshots here were stamped with an engine they were not measured on.

`result_litellm-rust_2026-08-03T20-29-28Z.json` carries `rig.engine.commit =
0ce7a9078f45`. It was not measured on that engine.

The `otb` binary is NOT built from `BENCH_COMMIT`. It is downloaded from the
GitHub release tagged `rig` (lib/rig.sh:19, run-on-ec2.sh:1369), which is built
by .github/workflows/bench-rig.yml - and that workflow triggers only on a push to
`main` touching `engine/**` or `mock/**`. The engine fixes for this run were
pushed to a BRANCH, so the release was never rebuilt and every box fetched the
previous binary.

`BENCH_ENGINE_COMMIT` is an env var the orchestrator sets, so the stamp recorded
the intended commit while the binary was the old one. Proven directly on the box:

    strings ~/benchmarking/otb | grep -c "confirming the floor"   -> 0
    strings ~/benchmarking/otb | grep -c "has not drained to tw"  -> 0

Both strings exist in 0ce7a907. Neither is in what ran.

Kept rather than deleted: it is the evidence for the fix that must follow, which
is that the engine stamp has to be DERIVED FROM THE BINARY rather than asserted
beside it. A run whose stamp cannot be checked against the artifact can claim any
engine at all, and this one did.
