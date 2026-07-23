# OVERNIGHT GOAL: onthebench.ai SOLID. DONE. NO DOUBT.

Unattended run authorized 2026-07-23 ~06:10Z. Wake up to a board with ZERO doubt.
No check-ins. Judgment calls made + noted. Full report in the morning. NO AskUserQuestion.

## Definition of DONE (every box must be checked)

1. **All 13 gateways field-re-run, validated, published.**
   - Every gateway's re-run data committed + pushed (benchmarking repo push is authorized anytime).
   - NO false reds. Every non-green cell is either: one of the 2 sanctioned real reds
     (portkey stream = upstream bug Portkey#1389; agentgateway xlate = claimed-but-untranslated,
     local-repro'd), OR a cited not_verified / untestable / not_declared with evidence.
   - A failure is OUR bug until proven the gateway's. Prove every red before publishing it.

2. **bifrost resolved.** Its anthropic->openai-responses cell was a transient 502
   (provider_connection_failed). The harness now retries transient (5xx/000) probes 3x/120s
   before recording and records not_verified if still failing. Re-run bifrost is in flight;
   confirm the cell is green OR honestly not_verified, NEVER a bogus wrong_answer red.

3. **SINGLE SOURCE OF TRUTH for every number.** THE trust-critical item.
   - The per-cell matrix sweep is the ONE canonical perf source everywhere (table, drawer,
     hover popup, compare modal, charts). perf-suite = fallback ONLY (no matrix).
   - Apply the fix from the number-correctness audit (agent a890892c99c977394) covering EVERY
     divergence it maps, not just Max proxy / Sustained. Fix ALL in one pass.
   - drawer reads the SAME field as the table. charts.py reads the unified data.json bundle
     (not raw results/perf) so it cannot diverge.

4. **BUILD-TIME CONSISTENCY GUARD.** A gen-data/test check that FAILS the build if any
   (gateway, metric) is sourced two ways or the numbers disagree beyond rounding. Makes the
   two-sources-of-truth class of bug impossible to ship again. This is what restores faith.

5. **Independently spot-verify** >=3 gateways: open their results/{perf,matrix}/*.json and
   confirm the site shows exactly the canonical number, matching across table + drawer + charts.

6. **Charts regenerated** from the unified source. CF deploy GREEN. Live site verified
   (fetch onthebench.ai/data.json, assert table==drawer==charts consistency for a sample).

7. **All site tests pass** (node site/test.mjs). Zero em dashes. Committed + pushed.

8. **All EC2 boxes TERMINATED** (no cost bleed). Verify none left running.

9. **Outreach (approved):** file the 11 maintainer issues as MattJackson (task #94 template)
   ONLY after the board is verified SOLID (done-1 through done-7). If ANY doubt remains about a
   number, do NOT file - leave outreach as the one morning item. Bar = zero doubt.

10. **Morning report:** what was done, judgment calls + assumptions, and anything (if anything)
    that genuinely needs the user. If everything checks, say plainly: otb is SOLID, no doubt.

## Order of operations
- Wait for field boxes (busbar, bifrost) + the number-correctness audit to land (auto-notify).
- Validate + push each gateway as it completes.
- When the audit lands: apply the full SSOT fix + the guard, in one pass, against SETTLED data.
- Regenerate/verify charts, deploy, spot-verify live, terminate boxes.
- Then, only if zero doubt: file outreach. Report.

## 1.5.0 (secondary, no deadline, NEVER tag/push)
- P4-P9 build agent (aa2c39c37bf9effc8) chugs on local main. Verify each phase as it lands,
  keep main green (2386 tests baseline). Do NOT push, do NOT tag - held for explicit go.
- Changelog #92 / docs #93 remain queued.

## Standing constraints
- benchmarking push: authorized anytime. busbar 1.5.0: NOT pushed, NOT tagged.
- No Co-Authored-By. Use `git -C <path>`, never `cd &&`. No em dashes.
- EC2 boxes terminate when done. onthebench stays NEUTRAL.

## ============ OVERNIGHT RESULT (2026-07-23 ~14:30Z) ============
NUMBERS/BOARD: SOLID, DONE, VERIFIED. NO DOUBT ON ANY NUMBER.
- 13/13 gateways validated + published. Zero wrong-answer matrix reds (every former
  red was OUR config bug, all fixed). Only 2 reds remain, both genuine + locally
  reproduced + cited lane failures (agentgateway xlate untranslated; portkey stream
  upstream bug #1389). Every non-green honestly classified (grey/not_verified/untestable).
- SSOT: one canonical source (matrix per-cell sweep) everywhere; table==drawer==compare
  ==charts, verified against raw JSON (busbar/kong/litellm-rust) AND on the LIVE data.json;
  build-time guard proven to trip on divergence (33 errors on a forced regression), gates
  the deploy. bifrost transient cell now green; all green-cell statuses now 2xx.
- 25 site tests pass. Deploy green (bedd2c09). All EC2 boxes terminated. No em dashes.

ONE OPEN ITEM (pre-existing, NOT a numbers issue, does NOT block "numbers solid"):
- Deep-link 404 status (task #96): /gateways/* returns 404 status though it RENDERS
  (404.html shadows the CF _redirects 200-rewrite). Fix needs a careful preview-deploy
  on the LIVE site or a dashboard SPA toggle - NOT done blindly unattended.
- OUTREACH (#94) HELD: the 11 issue links use /gateways?gw=<key> which 404s. File after
  the deep-link fix, OR switch outreach to root links. USER DECISION.

1.5.0: P4-P9 config redesign COMPLETE + independently verified green on local main.
Held for tag (never pushed). See task #81.
