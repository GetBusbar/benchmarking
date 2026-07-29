# Audit ledger - verified but deferred - 2026-07-29

Findings from the six-agent fresh-eyes audit of 2026-07-29 that were verified against the
tree but deliberately not fixed that day. File:line references are as of 2026-07-29 on main.
Each entry: claim, concrete trigger, why deferred, and what promotes it to urgent.

## Engine - rig (launch / supervise / loadgen / http)

### RIG-01 minted-auth goes stale across restarts
- Claim: `resolve_minted_auth` (engine/src/bin/otb.rs:24, used at otb.rs:496) runs once at launch; `restart_to_rest` (engine/src/run.rs:444) has no path to re-read `.minted-auth`.
- Trigger: a gateway whose `commands` mints a fresh credential on every start; after the memory-phase restart, all requests carry the old token.
- Deferred: latent - no current gateway manifest writes `.minted-auth`.
- Promote when: any manifest adds a minting `commands` step.

### RIG-02 manifest commands run with inherited cwd
- Claim: `run_line` (engine/src/launch.rs:383) spawns with the engine's inherited cwd, not the gateway's `gw_dir`.
- Trigger: a manifest command that uses a relative path (writes a config, reads a key) lands in whatever directory otb was started from.
- Deferred: latent - current manifests use absolute paths or path-free commands.
- Promote when: a manifest command with a relative path is added, or otb is documented as runnable from arbitrary cwd.

### RIG-03 read timeouts misclassified in the load generator
- Claim: `read_response` (engine/src/gen.rs:345) maps a timeout with no bytes to `ClosedBeforeAnyBytes` (gen.rs:413); on a kept-alive connection that is retried silently (gen.rs:255), and on a fresh connection it counts as a plain fail, not `budget_exceeded` (gen.rs:262-266).
- Trigger: a gateway that accepts a connection and then hangs; the run reports generic fails instead of budget overruns, skewing failure attribution.
- Deferred: latent - qualifying gates still fail the rung either way; only the reason label is wrong.
- Promote when: anything starts branching on `budget_exceeded` vs `fail`, or a published verdict cites the wrong failure kind.

### RIG-04 post-stop drain inflates ok against fixed elapsed
- Claim: after the load window closes, in-flight responses that complete during the drain still count into `ok` while `elapsed_s` is frozen at `load_elapsed` (engine/src/gen.rs:514-525).
- Trigger: high concurrency with slow responses; the tail of drained completions raises rps beyond what the window sustained.
- Deferred: cosmetic - the inflation is bounded by one in-flight response per lane and is inside measurement noise at current settings.
- Promote when: window durations shrink or concurrency rises enough that the drain tail is a visible fraction of ok.

### RIG-05 connect timeout counted as gateway fail with no rig attribution
- Claim: the 5 s connect timeout (engine/src/gen.rs:191) lands in the gateway's fail count; nothing distinguishes rig-side ephemeral-port or fd exhaustion from a gateway refusing.
- Trigger: rig fd/port pressure during a big sweep charges the gateway with failures it did not cause.
- Deferred: latent - current rig sizing stays far from those limits.
- Promote when: sweeps grow past ~10k concurrent connections or a run shows connect fails uncorrelated with gateway load.

### RIG-06 substring process matching can hit the wrong process
- Claim: `pkill -f`/`pgrep -f` substring matching (engine/src/supervise.rs:127, :156; engine/src/rss.rs:213) can match unrelated processes including the engine; `matching_pid` picks the numerically smallest pid (rss.rs:221-227), and an empty `proc_match` matches everything (otb.rs:176 passes one in smoke).
- Trigger: a `proc_match` that is a substring of another running command line, or two runs on one box.
- Deferred: latent - current proc_match strings are distinctive and the box runs one engine at a time.
- Promote when: co-located runs are attempted or a manifest ships a short/generic proc_match.

### RIG-07 fixed docker container names collide across concurrent runs
- Claim: a docker container's `--name` comes solely from `Runtime::identity()` (engine/src/launch.rs:206-218), a fixed per-gateway string.
- Trigger: two engine runs on one host launch the same gateway; the second `docker run` fails or `rm -f`s the first.
- Deferred: latent - the EC2 workflow runs one engine per box.
- Promote when: concurrent or overlapping runs on a shared host become a supported mode.

### RIG-08 end-of-run stop result discarded
- Claim: the final `stop_and_wait` result is dropped with `let _ =` (engine/src/bin/otb.rs:522).
- Trigger: a gateway that survives stop escalation leaks into the next gateway's baseline (idle RSS, port conflicts).
- Deferred: latent - the next launch's port checks would fail loudly rather than measure through it.
- Promote when: a run shows cross-gateway contamination or the port-check assumption changes.

### RIG-09 run_line timeout kills the shell, not the process group
- Claim: on timeout, `run_line` (engine/src/launch.rs:383) kills the spawned shell; children of that shell survive.
- Trigger: a manifest command like `sh -c "slow-tool ..."` that hangs; the tool outlives the kill and holds ports or files.
- Deferred: latent - current manifest commands are short-lived single processes.
- Promote when: a manifest gains a long-running or forking command.

### RIG-10 finish() flushes an un-terminated SSE event as delivered
- Claim: `SseParser::finish` (engine/src/http.rs:848) promotes a partial event held at close/timeout into a counted frame, stamped at the close time.
- Trigger: a gateway that dies mid-event; the truncated fragment counts as a delivered frame with an artificial timestamp, polluting gap percentiles by one sample.
- Deferred: cosmetic - one frame per stream at worst, and only on failing streams.
- Promote when: frame-exact accounting is claimed anywhere, or truncation-heavy gateways appear.

### RIG-11 no per-dialect terminal-frame awareness in frame counts
- Claim: frame counting in the SSE path (engine/src/http.rs) treats every event as a content frame; dialect terminators (e.g. a `[DONE]`-style sentinel) are not distinguished per dialect.
- Trigger: comparing frame counts across dialects whose terminator conventions differ counts the sentinel on some and not others.
- Deferred: needs-design - requires a per-dialect frame taxonomy, and current comparisons are within-dialect.
- Promote when: cross-dialect frame-count comparisons are published.

### RIG-12 hostile manifest can duplicate credential headers or inject CRLF
- Claim: `headers_for` (engine/src/run.rs:141) can emit a manifest header alongside the credential header it duplicates, and `build_request` (engine/src/gen.rs:276) interpolates header values without CRLF rejection.
- Trigger: a malicious or malformed gateway manifest carrying `\r\n` in a header value smuggles extra headers or a second request.
- Deferred: latent - manifests are first-party and reviewed; no untrusted manifest path exists.
- Promote when: third-party or user-submitted manifests are accepted.

### RIG-13 mid-grid rescue triggers only on "no connection"
- Claim: the mid-grid restart rescue keys on `why.contains("no connection")` (engine/src/run.rs:1875); a gateway that accepts connections but never answers is not rescued.
- Trigger: a wedged-but-listening gateway turns the rest of the grid into slow timeouts instead of a restart-and-resume.
- Deferred: latent - observed wedge modes so far all drop the listener.
- Promote when: a run shows an accept-but-hang wedge, or grid time budgets tighten.

## Engine - search and suite

### SRCH-01 sustained ceiling at top of range published as Measured
- Claim: `refine_ceiling` (engine/src/run.rs:712) with no failing rung above the top holder (`hi` = None, run.rs:749-755) publishes the top rung as a Measured ceiling, while `bisect_ceiling` refuses the same situation as `SearchExhausted` (engine/src/search.rs:277, :304).
- Trigger: a gateway whose true ceiling exceeds `max_conc`; the board shows max_conc as its measured ceiling instead of a lower bound.
- Deferred: needs-design - wants a "lower bound" publication kind, not a one-line fix.
- Promote when: any gateway's sustained rung lands at the configured max in a real run.

### SRCH-02 failed rungs publish placeholder evidence points
- Claim: the `refine_ceiling` points loop (engine/src/run.rs:721-727) pushes every climb rung with `fail: 0` and rps from `gate_median.unwrap_or(median)`, so a failed rung's evidence row carries placeholder numbers.
- Trigger: reading `sweep_sustained_20ms` evidence for a failed rung suggests it ran clean at a rate it did not sustain.
- Deferred: cosmetic - the pass/fail flag on the point is correct; only the companion numbers mislead.
- Promote when: any consumer starts plotting or ranking from the evidence rps/fail columns.

### SRCH-03 single-window bracket bisection can re-open the sustained>max inversion
- Claim: bracket probes inside `refine_ceiling` are single windows by design (engine/src/run.rs:758-761); a lucky single window can push sustained above the separately measured max proxy with no noise correction.
- Trigger: a noisy gateway where one bisection window catches a transient high; C6 on the site then flags the inversion downstream.
- Deferred: latent - the confirmation pass bounds it, and C6 catches the gross case.
- Promote when: C6 warnings recur on the same cell across runs.

### SRCH-04 percentile convention split with comments claiming otherwise
- Claim: `ttft_pct` (engine/src/metric.rs:881) and `gap_percentile_us` (metric.rs:1221) use ceil nearest-rank, while `stats::percentile` (engine/src/stats.rs:197) and gen.rs use floor, and the stats.rs:187-189 / gen.rs:101 comments claim the conventions match.
- Trigger: small samples (n under ~20) where floor vs ceil differ by one rank; cross-metric comparisons shift by one sample.
- Deferred: needs-design - unifying changes published numbers, so it needs a deliberate cut-over run.
- Promote when: any metric pairs a stats.rs percentile against a metric.rs percentile in one published figure.

### SRCH-05 confirmation test pins nothing
- Claim: `every_bisected_ceiling_is_confirmed_before_it_is_published` (engine/src/run.rs:3404) asserts against strings taken from its own source, so it passes vacuously.
- Trigger: the confirm step could be deleted and this test would still pass.
- Deferred: cosmetic - the behavior it meant to pin is currently correct and covered indirectly.
- Promote when: anyone touches `refine_ceiling`'s confirmation logic.

### SRCH-06 c=0 probed literally when min_conc=0
- Claim: `bisect_ceiling` (engine/src/search.rs:219) samples `min_conc` as-is (search.rs:227); a configured floor of 0 probes zero concurrency, a meaningless window.
- Trigger: a suite config with `min_conc: 0`.
- Deferred: latent - no config sets 0, and config_lint could catch it.
- Promote when: config surface is exposed to users; add a lint before then.

### SRCH-07 demoted winner can land outside the plateau band
- Claim: `published_winner` (engine/src/search.rs:703, applied at :839) can demote the peak to a rung whose value sits outside the plateau band or whose knee sits above the peak rung.
- Trigger: a rung profile with a sharp spike followed by a plateau; the published winner is defensible but the band annotation misdescribes it.
- Deferred: cosmetic - the published number is a real measured rung either way.
- Promote when: the plateau band is surfaced on the site or used for ranking.

### SRCH-08 metric.rs test-local model encodes the retired clamp rule
- Claim: a test-local expected-value model clamps negative diffs to 0 via `.max(0.0)` (engine/src/metric.rs:1928), the rule production retired in favor of BelowResolution (metric.rs:1015, :1535).
- Trigger: the test agrees with production only while fixtures avoid negative diffs; a fixture change would make the test enforce the wrong rule.
- Deferred: cosmetic - current fixtures do not exercise the divergence.
- Promote when: fixtures gain sub-noise negative diffs or the test starts failing "mysteriously".

## Engine - artifact

### ART-01 CellMemory load_s/plateaued can be null with no absences entry
- Claim: suite.rs builds `plateaued` and `load_s` via `take(...).copied()` (engine/src/suite.rs:475, :492), collapsing why-absent; these Option fields are not coverable by `absences_of!` (engine/src/record.rs:369), which only walks Measurement fields.
- Trigger: a served cell whose memory leg lacked those readings publishes bare nulls, the exact "bare hole" bench-audit polices for Measurement fields.
- Deferred: needs-design - wants Measurement-typed fields or an Option-aware absences path.
- Promote when: bench-audit's field list grows to cover them, or a consumer reads load_s/plateaued.

### ART-02 qualify baseline harvests observed_rps from failed runs
- Claim: `Outcome::qualifies_as_baseline` (engine/src/qualify.rs:46) exists but `qualify_history_on_disk` (engine/src/suite.rs:912, called at :898) harvests every on-disk record unconditionally.
- Trigger: a failed qualify run's rps lands in the baseline history and drags the qualification band.
- Deferred: latent - current history files on the rig contain only passing runs.
- Promote when: a failing qualify run is recorded on a production rig.

### ART-03 perf_dropped documented but never set
- Claim: `perf_dropped` (engine/src/record.rs:292) is serialized but no writer sets it; a refuted reverify still publishes perf/stream under the cell's name at flush (engine/src/suite.rs:1234).
- Trigger: reverify refutes a cell mid-run; the snapshot carries the refuted numbers with no drop marker.
- Deferred: needs-design - what a refutation should do to already-measured blocks is a policy call.
- Promote when: reverify refutations occur in a published run.

### ART-04 CellStream.reason carries prose, null when reasonless
- Claim: `cell_stream` (engine/src/suite.rs:537-548) puts detail prose in `reason` rather than a stable token, and null when there is none.
- Trigger: any consumer trying to branch on reason gets free text.
- Deferred: cosmetic - the site displays it verbatim and nothing branches on it.
- Promote when: reason-driven UI or audit rules are added.

### ART-05 stream_served vocabulary wider than the doc
- Claim: `stream_served` statuses produced at suite.rs:533-567 exceed what the record.rs:415 doc lists, and can misdescribe gap-measured cells when only the TTFT legs failed.
- Trigger: a cell with good gap data but failed TTFT legs reads as less served than it was.
- Deferred: cosmetic - display-only today.
- Promote when: stream_served becomes machine-consumed or the doc is cited externally.

### ART-06 mirrored twin absences drop detail by design
- Claim: companion fields sharing one absence (e.g. a value and its `_conc` twin) get the reason token via `absences_of!` (engine/src/record.rs:369) but the twin's entry drops the detail string.
- Trigger: reading a twin's absence gives less context than its primary.
- Deferred: cosmetic and by design - detail lives once, on the primary.
- Promote when: a consumer reads twins in isolation.

### ART-07 matrix.hardware/arch/build never filled while root's are
- Claim: flush (engine/src/suite.rs:1234) fills the snapshot root's hardware (suite.rs:1267) but the matrix-level hardware/arch/build fields stay null.
- Trigger: a reader of the matrix block alone cannot state the hardware; the 2026-07-28 "hardware: null" episode at root level (suite.rs:2103) repeats one level down.
- Deferred: cosmetic - all readers today go through the root.
- Promote when: the matrix block is exported or diffed standalone.

### ART-08 snapshot current/historical pair not atomic as a pair
- Claim: `atomic_write` runs per-file for current then historical (engine/src/snapshot.rs:248-249); each is atomic, the pair is not.
- Trigger: a crash between the two writes leaves a current file with no matching historical copy.
- Deferred: latent - a re-run rewrites both; nothing reconciles the pair today.
- Promote when: history completeness is asserted or audited.

## Site

### SITE-01 oracle compares only .v
- Claim: the consistency oracle compares sealed values by `.v` only (site/check-consistency.mjs:678, :696); flattening or loss of reason/note/detail is invisible to it.
- Trigger: a seal change that preserves values but mangles reasons passes the oracle clean.
- Deferred: needs-design - comparing envelopes wholesale needs a canonical envelope equality.
- Promote when: a reason/detail regression ships undetected once.

### SITE-02 oracle verifies claimed coordinates, never re-derives selection
- Claim: the oracle re-derives values at the coordinates the bundle claims (check-consistency.mjs:624-627) but never re-derives which cell should have been selected.
- Trigger: a selection bug (wrong best cell) with correct values at the wrong coordinates passes.
- Deferred: needs-design - re-deriving selection duplicates gen-data's ranking logic.
- Promote when: best-cell selection logic changes again.

### SITE-03 whole-block skip still counts the gateway as oracled
- Claim: `oracledKeys.add(g.key)` (check-consistency.mjs:657) fires per gateway even when whole blocks inside it were skipped.
- Trigger: a gateway with one comparable block and several skipped ones reports as covered.
- Deferred: cosmetic - the per-gateway gate was the audit fix; per-block is the next resolution step.
- Promote when: a skipped-block regression is missed in practice.

### SITE-04 v1-shape raw artifact yields zero comparisons
- Claim: the v1-shape walk (check-consistency.mjs:89) produces no oracle comparisons, and the per-gateway coverage gate then blocks an honest legacy publish.
- Trigger: republishing a board that includes a v1-era artifact.
- Deferred: latent - no v1 artifacts remain in results/ on main.
- Promote when: a legacy artifact needs republishing.

### SITE-05 C3 exemption is too broad
- Claim: the C3 caption lint skips any line matching `\bsource\b` or `sweep:` (check-consistency.mjs:387) and only inspects double-quoted tokens.
- Trigger: a caption literal that also mentions "source", or a single-quoted sweep token, escapes the lint.
- Deferred: cosmetic - lint precision, not correctness of the site.
- Promote when: a caption regression slips through C3.

### SITE-06 C5 misses several raw-read spellings
- Claim: `lintAccessorRouting` (check-consistency.mjs:404, emits at :437-442) misses `env["value"]`, bracket-then-.value chains, and destructuring reads.
- Trigger: a raw envelope read written in any of those forms passes C5.
- Deferred: cosmetic - lint precision; metric()/mval() routing is currently clean.
- Promote when: a raw read ships despite C5 passing.

### SITE-07 paced-flag comment promises a signal the envelope never carries
- Claim: the sealMetric paced-path comment says "the flag stays on the envelope as the signal it always was" (site/seal.mjs:155-157), but the returned envelope carries no flag field.
- Trigger: a reader trusting the comment looks for a flag that is not there.
- Deferred: cosmetic - comment/code drift only.
- Promote when: anything is built to consume that flag.

### SITE-08 gated measured-zero drops extras
- Claim: the measured-zero early return (site/seal.mjs:149) skips the extras attachment (seal.mjs:170), so concurrency and sweep evidence vanish on honest zeros.
- Trigger: a certified 0 on a gated metric publishes without its sweep evidence.
- Deferred: cosmetic - zeros have little evidence worth showing today.
- Promote when: zero-cell evidence is surfaced in the UI.

### SITE-09 legacy m.memory reseal covers only RSS keys, passes no absent option
- Claim: the top-level memory reseal (site/gen-data.mjs:530-531) seals only keys matching `RSS_FIELD_RE` and passes `{}` with no absent metadata.
- Trigger: a legacy memory block with a non-RSS metric or an absence reason loses that information on reseal.
- Deferred: latent - legacy blocks on disk carry only RSS keys.
- Promote when: legacy artifacts with richer memory blocks appear.

### SITE-10 in-place stream reseal drops CellStream.reason and stream_c1_note
- Claim: `sealMatrixCellsInPlace` (site/gen-data.mjs:487) replaces served cells' stream metrics but does not carry `reason` or `stream_c1_note` through.
- Trigger: the sealed bundle loses the stream explanation prose the artifact carried.
- Deferred: cosmetic - the drawer reads the raw artifact for prose today.
- Promote when: the sealed bundle becomes the only data source for the drawer.

### SITE-11 degraded-mode guard fires only when a disk artifact is shadowed
- Claim: the smoke-run guard (site/gen-data.mjs:202-208) only errors when a degraded snapshot is newer than a real results/matrix artifact; a lone local smoke snapshot publishes silently.
- Trigger: a fresh gateway whose only snapshot is a phases-off smoke run appears on the board as measured.
- Deferred: latent - production snapshots always follow a full-phase run.
- Promote when: partial-phase runs become a normal workflow.

### SITE-12 stream-fallback stamps cpu_fps with g.stream's provenance
- Claim: the stream-fallback projection (site/gen-data.mjs:294-310) includes `cpu_fps` from `g.streamcpu` (gen-data.mjs:307) but stamps the whole source with `g.stream.build`/`measured_at` (gen-data.mjs:310).
- Trigger: on a fallback row, cpu_fps displays a build/date from a different suite run than produced it.
- Deferred: latent - the legacy per-suite path is retired for new runs; fallback rows are legacy-only.
- Promote when: a legacy fallback row is published with divergent stream/streamcpu dates.

### SITE-13 compare-table naText flattens not-served vs never-ran
- Claim: `naText` (site/app.js:235) renders one label family that does not distinguish a lane the suite refused from a lane that never ran.
- Trigger: two visually identical "n/a" cells with different honesty stories.
- Deferred: cosmetic - the drawer carries the full story.
- Promote when: the compare table is screenshot-shared as a standalone claim.

### SITE-14 roster desc sort breaks name-tiebreak stability
- Claim: the roster sort's string branch (site/app.js:1828-1831) reverses comparison direction wholesale on desc, so equal-value rows also reverse name order instead of keeping a stable name tiebreak.
- Trigger: toggling desc on a column with ties reorders tied rows.
- Deferred: cosmetic - visual stability only.
- Promote when: users report jumpy rows or the roster gains dense ties.

### SITE-15 sanitizeState seeds sameDialect globally
- Claim: `sanitizeState` (site/app.js:2998) seeds `sameDialect` for the whole state rather than only for the memory tab that needs it.
- Trigger: entering via a deep link to a non-memory tab still mutates dialect state used elsewhere.
- Deferred: cosmetic - the seeded value is a valid default everywhere.
- Promote when: per-tab dialect defaults diverge.

### SITE-16 below-resolution ranks first-tie only in compare bestIndex
- Claim: `bestIndex` (site/app.js:2205, used at :2262) marks only the first of several tied below-resolution cells as best.
- Trigger: two gateways both below resolution on a metric; one gets the highlight, the other does not, implying a difference the data cannot support.
- Deferred: cosmetic - highlight only, values shown are identical.
- Promote when: the highlight feeds any ranking or export.

## Tooling

### TOOL-01 test.mjs gated skips print ok
- Claim: `testWithData` (site/test.mjs:196-200) returns early on an empty board and the harness prints `ok - name` (test.mjs:44), indistinguishable per-test from a pass; only a file-level warn (test.mjs:191-192) says skipping happened.
- Trigger: reading a green test list off an empty board as evidence the checks ran.
- Deferred: cosmetic - the warn banner exists and CI boards are populated.
- Promote when: per-test results are machine-parsed.

### TOOL-02 C6_GROSS_PCT duplicated in two languages
- Claim: the 5% gross-noise ceiling exists as `C6_GROSS_PCT = 5.0` (bench-audit.py:47) and `C6_GROSS_PCT = 5` (site/check-consistency.mjs:124) with no cross-check.
- Trigger: tuning one side silently forks the invariant.
- Deferred: cosmetic - both are 5 today.
- Promote when: either constant is next edited.

### TOOL-03 bench-audit globs are cwd-relative
- Claim: `glob.glob("results/snapshots/result_*.json")` (bench-audit.py:60, :83, :322) resolves against the invoker's cwd.
- Trigger: running bench-audit from outside the repo root silently audits nothing (the :322 empty-board path).
- Deferred: latent - CI and docs both run it from the root; the empty-board skip now errors loudly per the same-day HIGH fix.
- Promote when: bench-audit is invoked from another directory in any workflow.

### TOOL-04 audit cannot see an omitted field
- Claim: the bare-hole check (bench-audit.py:247-252) requires the field to be present-and-null (`f in blk and blk[f] is None`); a field omitted from the block entirely is invisible.
- Trigger: a serializer change that drops a field instead of nulling it passes the audit.
- Deferred: latent - the engine serializes all listed fields today.
- Promote when: record.rs serialization gains any skip_serializing behavior.

### TOOL-05 charts clamp_negatives vestigial on stream charts
- Claim: `clamp_negatives` (charts.py:286, applied at :1028) is still set on stream charts (charts.py:542, :559, :640) although the engine now publishes BelowResolution instead of negative diffs, so the clamp has nothing to clamp.
- Trigger: none today; it is dead configuration that misleads a reader about where clamping happens.
- Deferred: cosmetic - removing it changes no pixels.
- Promote when: charts.py is next refactored; delete it then.

## Process note

These entries came out of six parallel fresh-eyes audits run on 2026-07-29 (rig, search/suite,
artifact, site, tooling, cross-cutting). Everything rated HIGH that day was fixed same-day and is
not in this ledger: chooser min/max leak, popup failure conflation, unescaped repo hrefs,
drawer/compare metric() routing, memcurve all-or-nothing, recordShowsValues side-door,
confirm_ceiling zero, streams step-down seed vote, cpu_fps interrupted zero, memory-restart
continue, grid restart poisoning, wrong-model metric bodies, timings_s serialization, conc-twin
reasons, rigbound verdict details, workflow paths, newest_engine ordering, audit field list,
dashboard test in CI, charts below-res label, bench-audit empty-board skip, translation/bestCell
below-res ranking, and failed-cell leg attribution. This file records only what was verified but
deliberately deferred, so the next audit does not re-derive it from scratch.
