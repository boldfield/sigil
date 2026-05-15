# Plan E2 Phase 3 GC-time follow-up

**Status:** scaffolded — measurement run pending. Data fields in
this doc are placeholders (`TBD …`) until Task 4's workflow run
lands. The scaffolding ships ahead of the measurement so the
runtime mechanisms (`SIGIL_MAX_HEAP_SIZE_KB` env var +
`SIGIL_COUNTER_PRECISE_WALKER_NS` counter) + the workflow input
(`heap_budget_kb`) can be reviewed independently from the
verdict.

**Spec:** [`/repos/designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md`](../../../designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md).

**Original throughput report:** [`compiler/docs/plan-e2-phase-3-throughput.md`](plan-e2-phase-3-throughput.md). That doc closes the wall-clock-overhead question for Phase 3; this doc closes the mark-phase-time question that the original report flagged as unfalsifiable.

## TL;DR

**TBD — pending measurement run.** Three possible verdicts the
filled doc will land:

- **Confirmed**: "Phase 3 reduces mark-phase time by X ms / Y%
  on workloads that triggered full GCs under the forced budget.
  The conservative-scan savings exceed the precise-walker cost."
- **Disproven**: "Phase 3 has no measurable mark-phase savings
  even under forced budget. The conservative scan was not the
  bottleneck Plan E2 hypothesised. The precise walker is still
  load-bearing for the false-retention correctness fixes (Phase
  2 closure), but its mark-phase-time motivation didn't hold up."
- **Inconclusive**: "Boehm refused to fire full GCs at any
  budget level we could test without OOM-abort. The hypothesis
  remains structurally unfalsifiable in Sigil's current
  Boehm-integration shape."

## Methodology

### Checkpoints

Same as Phase 3's throughput report — no checkpoint drift:

| Checkpoint | SHA | Notes |
|---|---|---|
| Pre-Phase-3 | `ca29d2061f2897cb824d8328c92a8d945da313cc` | Phase 2 closeout; Tasks 10–12 absent. |
| Post-Phase-3 follow-up | _TBD — captured at workflow trigger time_ | This branch's HEAD after the measurement-fill commit. |

### Workloads

Reused as-is from Phase 3's throughput suite (7 workloads):

- `fib_perf`
- `fib_cps_perf`
- `tree`
- `tree_stress_repeat`
- `tree_stress_repeat_large`
- `descriptor_cache_stress`
- `deep_sync_call_chain`

### New measurement mechanisms

**(a) `SIGIL_MAX_HEAP_SIZE_KB` env var (Task 1).** Read once at
`sigil_gc_init` time, BEFORE `GC_init()`. Calls
`GC_set_max_heap_size(N * 1024)` when the value parses to a
positive integer; otherwise logs a warning to stderr and proceeds
with Boehm's default heap-growth heuristic. Unset / empty / invalid
→ no budget.

**(c) `SIGIL_COUNTER_PRECISE_WALKER_NS` counter (Task 2).**
Always-on `AtomicU64` accumulator. Snapshots `Instant` at the
start of `push_sigil_thread_precise_roots`'s body (AFTER the
chained prior-proc call, since that's Boehm's internal hook
cost not Phase 3's), reads `Instant::elapsed()` at every exit
path (gate short-circuits + walked body), adds the nanosecond
count to the counter. Reported by `sigil_counter_print_all` at
process exit in `key=value` form alongside the existing counters.

Per-call cost: two `Instant::now()` reads + a relaxed atomic add
(~50 ns). Steady-state cost acceptable for an always-on counter.

### Budget value

**_TBD — pinned at Task 4 run time._** Initial guess: `512` KB.
If `512` causes Boehm OOM-abort on any workload, doubled until
all workloads complete. Final value recorded here + in the
deltas-summary artifacts.

### libgc versions

**_TBD — captured by `capture libgc version` step._**

| OS | libgc version |
|---|---|
| ubuntu-24.04 | _TBD_ |
| macos-14 | _TBD_ |

## Per-workload deltas

**_TBD — filled from `throughput-data-${os}` artifacts after the
Task 4 run._**

For each workload, two tables (one per OS) with columns:
`wall_clock_ms`, `boehm_gc_time_ms`, `precise_walker_ns`,
`alloc_count`. The `precise_walker_ns` column is post-only
(pre-Phase-3 binaries do not have the counter); the asymmetry is
documented under "Methodology caveats" below.

<!--
Per-workload data table — placeholder shape:

### `fib_perf`

| Host | wall_clock_ms (pre → post) | boehm_gc_time_ms (pre → post) | precise_walker_ns (post) | alloc_count (cross-check) |
|---|---|---|---|---|
| ubuntu-24.04 | TBD → TBD | TBD → TBD | TBD | TBD ≈ TBD |
| macos-14 | TBD → TBD | TBD → TBD | TBD | TBD ≈ TBD |
-->

## Decomposition

**_TBD — computed per workload after the data lands._**

For each workload that fired a GC under budget:

| Metric | Formula |
|---|---|
| Savings | pre-`boehm_gc_time_ms` − post-`boehm_gc_time_ms` |
| Cost | post-`SIGIL_COUNTER_PRECISE_WALKER_NS` / 1_000_000 (ns → ms) |
| **Net** | savings − cost |

A cross-platform table will summarise net per workload + the
sign of the verdict.

## Hypothesis-check resolution

Phase 3's original report named this follow-up as the
hypothesis-check escalation path:

> "Boehm's default allocator pacing escalates to a full
> collection only at heap sizes much larger than any workload in
> this suite reaches; the precise-walker-vs-conservative-scan
> distinction is therefore invisible to wall-clock measurement
> at this scale. See Discussion → 'Hypothesis check' for the
> escalation-case follow-up plan this finding triggers."
> — `plan-e2-phase-3-throughput.md` TL;DR

This doc closes that thread. The verdict (above) is the
escalation-case answer; the original report stays as a frozen
snapshot of the pre-budget unfalsifiable state.

## CI implications

No perf-gate change. The follow-up doc is a snapshot of a
specific workflow run, same shape as Phase 2's report and Phase
3's report. Per-PR CI's existing perf-floor tests remain the
gate for runtime regressions; this doc is decomposition-and-
verdict, not a steady-state guard.

## Methodology caveats

- **Budget is artificial pressure.** The forced collection
  cadence is not what natural workload scaling produces — it's
  a probe that reveals Phase 3's mark-phase behaviour at
  collection frequencies that don't otherwise occur. Real-world
  Sigil programs running without the env var see Boehm's default
  pacing; this doc's verdict reflects the artificially-paced
  case.
- **Counter excludes chained-prior-proc.** Boehm's internal
  push_other_roots proc handles TLS roots + dynamic-library
  roots; its cost is not Phase 3's overhead. The counter
  intentionally starts AFTER that call.
- **Pre-checkpoint `precise_walker_ns` is absent.** The counter
  was introduced by this follow-up plan; pre-Phase-3 binaries
  don't have it. The `diff-throughput.py` tool renders the
  pre value as `n/a` and the decomposition treats the missing
  pre value as 0 (the counter literally didn't exist on the
  pre side).
- **Budget value chosen empirically.** A different value might
  produce different decomposition shapes — too-tight a budget
  triggers OOM-abort, too-loose a budget reverts to Boehm's
  default pacing. The final number is what the workflow
  iteration converged on.
- **GitHub Actions runner variability.** Same as Phase 2 /
  Phase 3 reports — wall-clock measurements on shared runners
  are noisy. The script reports IQR; deltas inside 1.5× IQR
  are treated as noise.

## Related work

- [`compiler/docs/plan-e2-phase-3-throughput.md`](plan-e2-phase-3-throughput.md) — the original throughput report; frozen snapshot of the unfalsifiable state.
- [`compiler/docs/plan-e2-phase-2-throughput.md`](plan-e2-phase-2-throughput.md) — Phase 2 precise-typed-malloc report. Phase 2's costs were alloc-path, not mark-phase, so the budget-forcing mechanism added here doesn't change Phase 2's answer.
- `PLAN_E2_PROGRESS.md` — Phase 3 / Task 12 entry now links to both reports.
