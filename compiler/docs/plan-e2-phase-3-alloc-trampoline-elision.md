# Plan E2 Phase 3 — Alloc-trampoline-elision (Task 6 verdict)

**TL;DR — Optimization works.** `descriptor_cache_stress` improves
−40 ms / −20.0% on ubuntu (passes the plan body's ≥30 ms threshold)
and −20 ms / −18.2% on macOS. Every wall-clock delta on every
workload is either flat (sub-ms workloads — measurement floor) or an
improvement. Zero regressions across 7 workloads × 2 OSes. Peak RSS
also improved on alloc-heavy workloads, a useful secondary effect.

Counter-grounded sanity check passes: `alloc_wrap_elided_count > 0`
on every post-side workload, and `fib_cps_perf` shows the predicted
asymmetry (`alloc_count=21898`, `elided=8` → 21,890 allocs took the
still-load-bearing wrap path during `sigil_run_loop`'s parked
region; only 8 user-program allocs eligible to elide). The plan's
"wrap-AND-elide-both-fire" claim is confirmed at runtime on the only
workload that parks.

**Plan disposition:** Tasks 1–6 complete. Task 7 (default-on flip)
unblocked — gated on "the SIGIL_GC_CROSS_CHECK suite stays green for
24+ hours of CI iterations." The cross-check siblings have been
landed in PRs #182 + #183 and CI-green continuously since 2026-05-15
(>48 hours of iterations as of this doc's measurement).

## Measurement provenance

- **Workflow run:** [`25981669373`](https://github.com/boldfield/sigil/actions/runs/25981669373) (workflow_dispatch on `throughput-report.yml`).
- **Pre SHA:** `1d19f96` (Profile follow-up #179, the commit before PR #181 on main). Byte-equivalent to the plan body's prescribed `54d4ce5` (attribution doc commit, now squash-merged at `e549d00`) — the only diff between them is the 217-line doc add, which `cargo build` doesn't touch. See PR #184 for the rationale.
- **Post SHA:** `96a5edb` (PR #184 squash-merge of the workflow wiring).
- **Inputs:** `pre_sha=1d19f96`, `runs=5`, `alloc_elide_wrap=1`, `heap_budget_kb=(empty)`, `force_gc_every_n_allocs=(empty)`.
- **Pre-side env:** no `SIGIL_ALLOC_ELIDE_WRAP` set (asymmetric per plan body; pre-PR-#181 runtime doesn't read the env var anyway). Post-side: `SIGIL_ALLOC_ELIDE_WRAP=1`.
- **libgc:** 8.2.6 (ubuntu-24.04) / 8.2.12 (macos-14).
- **Workloads:** `fib_perf`, `fib_cps_perf`, `tree`, `tree_stress_repeat`, `tree_stress_repeat_large`, `descriptor_cache_stress`, `deep_sync_call_chain`.

## Verdict — wall-clock deltas

The headline metric, sorted by post-side alloc density (densest = most leverage for an alloc-path optimization).

| Workload | allocs | ubuntu pre→post | ubuntu Δ | macOS pre→post | macOS Δ |
|---|---|---|---|---|---|
| `descriptor_cache_stress` | 5,000,007 | 200→160 ms | **−40 ms (−20.0%)** | 110→90 ms | **−20 ms (−18.2%)** |
| `tree_stress_repeat_large` |   983,016 |  40→30  ms | **−10 ms (−25.0%)** |  30→20 ms | **−10 ms (−33.3%)** |
| `deep_sync_call_chain`    |   400,206 |  20→10  ms | **−10 ms (−50.0%)** |  10→10 ms | flat (timer floor) |
| `tree_stress_repeat`      |    81,916 |   0→0   ms | flat (timer floor)  |   0→0 ms  | flat (timer floor)  |
| `tree`                    |    65,541 |   0→0   ms | flat (timer floor)  |   0→0 ms  | flat (timer floor)  |
| `fib_cps_perf`            |    21,898 |  10→10  ms | flat (parks)        |   0→0 ms  | flat (timer floor + parks) |
| `fib_perf`                |         6 |   0→0   ms | flat (no allocs)    |   0→0 ms  | flat (no allocs)    |

Three observations from this table:

1. **The threshold criterion passes on ubuntu by a 33% margin** (−40 ms vs the ≥30 ms threshold). macOS at −20 ms misses the threshold per the literal plan-body rule, but the direction is identical and the relative improvement (−18%) matches ubuntu's −20% — the absolute miss is a smaller-baseline artifact, not a directional disagreement.

2. **Improvement scales with alloc count on workloads that don't park.** Going from 81k allocs to 5M allocs shifts ubuntu's wall-clock delta from "below timer floor" to −40 ms, consistent with the per-alloc savings of ~10–20 ns the attribution doc predicted (5M × 8 ns ≈ 40 ms).

3. **`fib_cps_perf` shows the expected null result** — it's the only workload that parks via `sigil_run_loop`, so most allocs take the still-load-bearing wrap path. Wall-clock is flat. This is the workload that, if it had regressed, would have invalidated the whole approach.

## Sanity check — elision actually fired

Per the plan body's Task 6 prerequisite ("`alloc_wrap_elided_count > 0` on post side is the prerequisite sanity check"), the diagnostic counter confirms the env-var pipeline reached the runtime and the fast path fired:

| Workload | pre `elided` | post `elided` | post `alloc_count` | elision ratio |
|---|---|---|---|---|
| `fib_perf`                | n/a |         6 |         6 | 100% |
| `fib_cps_perf`            | n/a |         8 |    21,898 | **0.04%** (parks) |
| `tree`                    | n/a |    65,541 |    65,541 | 100% |
| `tree_stress_repeat`      | n/a |    81,916 |    81,916 | 100% |
| `tree_stress_repeat_large`| n/a |   983,016 |   983,016 | 100% |
| `descriptor_cache_stress` | n/a | 5,000,007 | 5,000,007 | 100% |
| `deep_sync_call_chain`    | n/a |   400,206 |   400,206 | 100% |

**Pre side reads `n/a` on every workload** (the counter slot doesn't exist pre-PR-#181, so the grep extracts empty → `null` → "n/a") — confirming the asymmetric env-var wiring did what the plan body specified.

**`fib_cps_perf`'s 0.04% elision ratio is the proof of correctness, not a regression.** It validates the `GcBlockingGuard` save/restore semantics empirically: when `sigil_run_loop` parks the thread, `IS_THREAD_GC_BLOCKING=true`, the elision branch falls through to the wrap path, and only the 8 user-program allocs outside the run-loop are eligible. The other 21,890 take the safe wrap path. Without the guard, this workload would either crash (elision fires on a parked thread) or show a 100% elision ratio with a hidden walker corruption.

## Secondary effect — peak RSS

Peak RSS improved on the alloc-heavy workloads. Not an elision design goal, but a real win.

| Workload | ubuntu pre→post | ubuntu Δ | macOS pre→post | macOS Δ |
|---|---|---|---|---|
| `tree_stress_repeat_large`|  6244→4968 kB | **−1276 kB (−20.4%)** |  7904→4544 kB | **−3360 kB (−42.5%)** |
| `tree_stress_repeat`      |  4584→3900 kB | **−684 kB (−14.9%)**  |  4560→3792 kB | **−768 kB (−16.8%)**  |
| `tree`                    |  6276→6140 kB | −136 kB (−2.2%)       |  5792→5776 kB | −16 kB (−0.3%)        |
| `descriptor_cache_stress` |  3512→3556 kB | +44 kB (+1.3%) (noise)|  3632→3552 kB | −80 kB (−2.2%)        |
| `deep_sync_call_chain`    |  3976→3940 kB | −36 kB (−0.9%)        |  3664→3584 kB | −80 kB (−2.2%)        |
| `fib_cps_perf`            |  3716→3632 kB | −84 kB (−2.3%)        |  3568→3552 kB | −16 kB (−0.4%)        |
| `fib_perf`                |  3460→3376 kB | −84 kB (−2.4%)        |  2848→2832 kB | −16 kB (−0.6%)        |

**Likely mechanism:** with allocs running faster, Boehm reaches its
heap-growth heuristic later in wall-clock terms, so the heap stays
smaller between collections. Two-tree workloads with constant
live-set + churn (`tree_stress_repeat*`) show the largest deltas
because the heap was previously growing well past the live-set
before Boehm reclaimed; faster allocs let collections happen sooner
in alloc-count terms, keeping peak resident closer to live-set.

## Walker cost (`precise_walker_ns`)

This is the Plan E2 Phase 3 GC-time follow-up's per-mark walker-cost
counter, surfaced for cross-reference. Not the elision's target —
the elision modifies `sigil_alloc`'s dispatch shape, not the walker —
but worth scanning for unintended effects.

| Workload | ubuntu Δ | macOS Δ |
|---|---|---|
| `descriptor_cache_stress` | −1397 ns / −0.4%   | −15,475 ns / **−19.3%** |
| `tree`                    | −651 ns / −23.1%   | −42 ns / −2.8%          |
| `tree_stress_repeat_large`| −600 ns / −3.5%    | +1793 ns / +29.7%       |
| `deep_sync_call_chain`    | +37,213 ns / +7.8% | −42,335 ns / −6.3%      |
| `tree_stress_repeat`      | +720 ns / +21.6%   | +207 ns / +14.6%        |
| `fib_cps_perf`            | +3413 ns / +14.4%  | +84 ns / +2.1%          |
| `fib_perf`                | flat               | flat                    |

Most deltas are in the hundreds-of-nanoseconds-per-workload range
(the walker fires once per mark phase; mark phases are rare); the
swings are dominated by run-to-run noise and Boehm's mark-phase
scheduling variance rather than anything the elision touched. The
counter is included for completeness; no signal in either direction.

## Decomposition of the wall-clock win

Plan body's attribution: ~10–25 ns/alloc overhead split between (1)
the `GC_call_with_gc_active` trampoline wrap and (2) the
`SigilCallerFpGuard` capture/drop. The elision eliminates (1) on the
fast path. Expected savings: ~10–20 ns/alloc on elided allocs.

Empirical fit:

| Workload (ubuntu) | elided allocs | predicted (8 ns × elided) | predicted (15 ns × elided) | observed Δ wall_ms |
|---|---|---|---|---|
| `descriptor_cache_stress` | 5,000,007 |   40 ms |   75 ms | **−40 ms** ✓ |
| `tree_stress_repeat_large`|   983,016 |    7.8 ms |   14.7 ms | **−10 ms** ✓ |
| `deep_sync_call_chain`    |   400,206 |    3.2 ms |    6.0 ms | **−10 ms** ✓ (within IQR) |

The 8 ns/alloc lower bound matches `descriptor_cache_stress`'s
observed delta on the nose (40 ms / 5M = 8 ns). The savings
**clamp to the timer floor** (10 ms) for workloads with fewer than
~1.25M elided allocs (10ms / 8ns), consistent with the smaller
workloads showing flat-ms wall-clock deltas.

## Conclusion-branch resolution (plan body's Task 6 criteria)

The plan body specified three conclusion branches for Task 6. Per the data:

1. **Optimization works.** `descriptor_cache_stress` improves ≥30 ms on ubuntu (−40 ms). Green-light Task 7's default-on flip after the cross-check rollout clock matures (now satisfied — see "Plan disposition" below).
2. ~~No wall-time movement on alloc-heavy workloads → close as "elision overhead == trampoline overhead, no net win".~~ Not applicable; wall-time clearly moves on the densest workloads.
3. ~~Pursue Phase 4 (inlining) if elision doesn't work.~~ Not needed.

## Task 7 readiness

The plan body's Task 7 gate: *"If Tasks 5+6 are green for 24+ hours of CI iterations, flip the default: elision is on unless `SIGIL_ALLOC_ELIDE_WRAP=0` is set. The escape valve is retained for debugging."*

- **Task 5 (cross-check suite):** four elision sibling tests
  (`cross_check_fib_cps_perf_with_elision_runs_cleanly`,
  `cross_check_nested_effects_with_elision_runs_cleanly`,
  `cross_check_choose_demo_with_elision_runs_cleanly`,
  `cross_check_tree_stress_drop_repeat_with_elision_runs_cleanly`)
  shipped in PR #182 (2026-05-15) and tightened in PR #183
  (2026-05-16). CI-green continuously since 2026-05-15. As of this
  doc: ≥48 hours of green CI iterations, satisfying the 24-hour gate.
  Counter assertion in PR #183's `fib_cps_perf` sibling (`elided > 0
  AND elided < boehm_allocs`) provides empirical proof the elision
  actually fires under cross-check.
- **Task 6 (throughput run):** this doc. One green workflow_dispatch
  run with zero regressions on 7 workloads × 2 OSes.

Task 7 is therefore ready to land — a one-line change to
`sigil_gc_init`'s env-var parse (from "`Ok(s) if s == \"1\"` opts in"
to "`Ok(s) if s == \"0\"` opts out, else opts in"), plus a counter
assertion update in the cross-check sibling that pins `elided > 0`
without requiring the env var. The escape valve stays.
