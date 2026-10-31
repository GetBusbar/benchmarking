# Private busbar comparison on one host

This mode runs published 1.5.5, then one immutable candidate, sequentially on the same
m7g.4xlarge ARM64 host. Both calls use the existing engine at
`2d209e76ba336c3478d3754e2bcd245b663459a3`, one fetched mock binary, the existing workload,
and the existing gateway/load/mock CPU assignments. Qualification runs independently for each
leg against the same supplied history baseline. The orchestration has its own commit; it is
not the unchanged historical runner. Sequential order can affect caches and host state.

The baseline is fixed to the published image digest independently retained during the first
1.5.5 repeat:

```
getbusbar/busbar@sha256:1582c076d25483457c2c8d06e064f1171a0469337a41bf38ef36d29d4d05615d
```

Prepare the candidate in a separate gateway directory, retaining logical `name: busbar` and
the baseline auth, model, port, path and dialect definitions. Its Docker image must use an
immutable digest. Review configuration differences: for 1.6.0, include the required
`store: {module: memory}` without changing the empty-chain authentication workload. Commit and
push that definition before launch. Retain the candidate's assembled production-build evidence,
including its source/plugin pins, three-run PGO, applied BOLT and matching profile/symbol hashes.
The runner records the artifact identity; it does not certify that production-build evidence.

From a clean, pushed orchestration checkout, set the candidate's full commit and directory:

```sh
AWS_DEFAULT_REGION=us-west-2 \
BENCH_SAME_HOST_PAIR=1 \
BENCH_COMMIT=2d209e76ba336c3478d3754e2bcd245b663459a3 \
BENCH_ENGINE_COMMIT=2d209e76ba336c3478d3754e2bcd245b663459a3 \
BENCH_PAIR_CANDIDATE_COMMIT="$CANDIDATE_COMMIT" \
BENCH_PAIR_CANDIDATE_GATEWAY="$CANDIDATE_DIRECTORY" \
OTB_DIALECTS=openai PUBLISH=0 BENCH_NO_PUBLISH=1 \
RUN_ID="$UNIQUE_RUN_ID" \
/opt/homebrew/bin/bash ./run-on-ec2.sh busbar
```

Input validation and archive verification precede AWS access. The runner makes one allocation,
aborts the candidate after any baseline failure, incrementally harvests each leg, and retains
the existing teardown and self-termination timers. It bypasses history updates and publication.
A run ID cannot reuse an existing local result directory.

Results remain under `results/pairs/$RUN_ID/`, including the immutable request, both source
archives, source/configuration hashes, image IDs and RepoDigests, host/AMI identity, engine/mock
hashes, per-leg statuses/logs and separate snapshots. A final transport failure does not discard
already-complete, verifiable incremental evidence. Partial evidence is retained with a failed
outcome. `run-on-ec2.sh harvest busbar` can rescue an existing host and reports incomplete or
unverifiable pairs explicitly.

Successful collection establishes the shared host, pinned artifacts and a served OpenAI
performance frontier. It does not establish stable historical numbers, acceptable qualification,
complete memory/streaming measurements, no regression, 120,000 RPS, or the 50% latency-reduction
goal. Inspect the raw metrics and qualifications before drawing those conclusions. Final release
performance requires the finished, frozen core/plugin candidate; a rehearsal cannot satisfy it.

Implementation validation so far: Bash syntax and Python AST parsing only. Remote controlled
validation and execution on a benchmark host remain pending.
