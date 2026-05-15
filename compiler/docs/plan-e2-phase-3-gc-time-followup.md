# Plan E2 Phase 3 GC-time follow-up

**Status:** measured. Data captured from `throughput-report.yml` run
[`25899135194`](https://github.com/boldfield/sigil/actions/runs/25899135194)
on commit `d8182348eff30fea4e0e1ee7fd790951eb3b3c27`. The workflow
ran end-to-end on both CI lanes (ubuntu-24.04 + macos-14) at the
pre-Phase-3 SHA (`ca29d2061f2897cb824d8328c92a8d945da313cc`) and
this branch HEAD (`d818234`); the per-workload JSON + per-OS
deltas summary live in the run's artifact upload.

**Heap budget for the run:** `16384` KB. Smaller budgets
(`512`, `4096`) OOM-aborted on increasingly large workloads;
`16384` was the smallest budget every workload completed under.

**Spec:** [`/repos/designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md`](../../../designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md).

**Original throughput report:** [`compiler/docs/plan-e2-phase-3-throughput.md`](plan-e2-phase-3-throughput.md).

## TL;DR

**Inconclusive — the hypothesis remains structurally unfalsifiable
even under forced budget.** `boehm_gc_time_ms = 0` on every workload
× every checkpoint × every OS in this run, including under a
`SIGIL_MAX_HEAP_SIZE_KB=16384` pin. Two readings are consistent
with that observation: either no stop-the-world full GC fired, or
each collection completed in under 1 ms (Boehm's
`GC_get_full_gc_total_time` reports integer milliseconds, so any
sub-ms aggregate rounds to 0). The unit test
`sigil_max_heap_size_kb_pin_forces_full_gc` confirms that at a
much tighter 1 MiB budget the mechanism DOES fire collections
(`GC_get_gc_no` advances), so the budget plumbing is verified
end-to-end; what we don't have at the 16 MiB workload budget is a
millisecond-resolved signal. The smallest budget that lets every
workload complete (`descriptor_cache_stress` allocates 192 MB
of total churn at 5M alloc sites; smaller budgets OOM-abort with
"Heap size: N MiB. Returning NULL!" before that workload can
finish) sits in the regime where Boehm collects fast enough to
read 0 ms but apparently not often enough to register the
hypothesised savings.

The walker-cost side IS measurable, and per-Phase-3 design that's
half the decomposition that was wanted:

| Workload | ubuntu walker_ns | macos walker_ns |
|---|--:|--:|
| `fib_perf` | 0 | 0 |
| `fib_cps_perf` | 18,114 | 3,543 |
| `tree` | 2,044 | 1,291 |
| `tree_stress_repeat` | 2,986 | 1,626 |
| `tree_stress_repeat_large` | 14,847 | 5,543 |
| `descriptor_cache_stress` | 351,576 | 66,494 |
| `deep_sync_call_chain` | 419,927 | 625,002 |

Walker cost spans 0 µs (`fib_perf`, which never enters Cps
machinery and never invokes a Sigil-thread mark callback) to
~625 µs cumulative (`deep_sync_call_chain` on macos, 400k Cps
allocations over a 20 ms run = ~3% relative cost). The largest
absolute walker cost (`deep_sync_call_chain`) is consistent with
its CPS-heavy call chain producing the most precise-root push
volume per mark cycle.

**What this report does NOT say.** Phase 3's correctness gains —
the false-retention closure under Plan E2 Phase 2's precise typed-
malloc + Phase 3's per-thread precise stack roots — are
unaffected by this measurement. Those are load-bearing for
soundness regardless of the mark-phase-time outcome. This report
addresses only Phase 3's throughput-side load-bearing claim, and
its verdict is "we can't measure the savings side, but we now
know the cost side."

## Spec link

Design: [`/repos/designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md`](../../../designs/docs/plans/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup-design.md).
Plan body: [`/repos/designs/done/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup.md`](../../../designs/done/2026-05-14-sigil-plan-e2-phase-3-gc-time-followup.md) (post-merge).

## Methodology

### Checkpoints

| Checkpoint | SHA | Notes |
|---|---|---|
| Pre-Phase-3 | `ca29d2061f2897cb824d8328c92a8d945da313cc` | Phase 2 closeout; Tasks 10–12 absent. |
| Post-Phase-3 follow-up | `d8182348eff30fea4e0e1ee7fd790951eb3b3c27` | This branch after workflow `sed`-strip + budget plumbing. |

### Workloads

Reused as-is from Phase 3's throughput suite (7 workloads):
`fib_perf`, `fib_cps_perf`, `tree`, `tree_stress_repeat`,
`tree_stress_repeat_large`, `descriptor_cache_stress`,
`deep_sync_call_chain`.

### New measurement mechanisms

**(a) `SIGIL_MAX_HEAP_SIZE_KB` env var.** Read once at
`sigil_gc_init` time, BEFORE `GC_init()`. Calls
`GC_set_max_heap_size(N * 1024)` when the value parses to a
positive integer; otherwise (empty / unset / invalid) silently
proceeds with Boehm's default heap-growth heuristic. Non-empty
non-numeric values log a warning to stderr.

**(b) `SIGIL_COUNTER_PRECISE_WALKER_NS` counter.** Always-on
`AtomicU64` accumulator. Snapshots `Instant` at the start of
`push_sigil_thread_precise_roots`'s body (AFTER the chained
prior-proc call), reads `Instant::elapsed()` at every exit
path (gate short-circuits + walked body), adds the nanosecond
count to the counter. Reported by `sigil_counter_print_all` at
process exit in `key=value` form.

### Budget value chosen

| Budget (KB) | Outcome |
|--:|---|
| 512 | OOM-abort on `tree` (Heap size: 0 MiB). |
| 4096 | OOM-abort on `tree_stress_repeat` on macos (Heap size: 3 MiB). |
| **16384** | **All 7 workloads complete; doc captures this data.** |

The doubling pattern matches the plan body's mitigation
guidance ("If 512 causes Boehm OOM-abort, bump to 1024, 2048,
4096 — pick the smallest budget every workload completes
under.") `16384` is the smallest budget where every workload
completes on both lanes.

### libgc versions

| OS | libgc version |
|---|---|
| ubuntu-24.04 | 8.2.6 |
| macos-14 | 8.2.12 |

## Per-workload deltas

Numbers below are pre-Phase-3 vs. post-Phase-3-follow-up
medians ± IQR (5 runs per workload per side, same as Phase 3's
report).

### ubuntu-24.04

| Workload | wall_ms (pre → post) | boehm_gc_time_ms (pre → post) | precise_walker_ns (post) | alloc_count |
|---|---|---|--:|--:|
| `fib_perf` | 0 → 0 | 0 → 0 | 0 | 6 |
| `fib_cps_perf` | 0 → 10 | 0 → 0 | 18,114 | 21,898 |
| `tree` | 0 → 0 | 0 → 0 | 2,044 | 65,541 |
| `tree_stress_repeat` | 0 → 0 | 0 → 0 | 2,986 | 81,916 |
| `tree_stress_repeat_large` | 30 → 40 | 0 → 0 | 14,847 | 983,016 |
| `descriptor_cache_stress` | 160 → 210 | 0 → 0 | 351,576 | 5,000,007 |
| `deep_sync_call_chain` | 10 → 20 | 0 → 0 | 419,927 | 400,206 |

### macos-14

| Workload | wall_ms (pre → post) | boehm_gc_time_ms (pre → post) | precise_walker_ns (post) | alloc_count |
|---|---|---|--:|--:|
| `fib_perf` | 0 → 0 | 0 → 0 | 0 | 6 |
| `fib_cps_perf` | 10 → 0 | 0 → 0 | 3,543 | 21,898 |
| `tree` | 10 → 0 | 0 → 0 | 1,291 | 65,541 |
| `tree_stress_repeat` | 0 → 0 | 0 → 0 | 1,626 | 81,916 |
| `tree_stress_repeat_large` | 30 → 30 | 0 → 0 | 5,543 | 983,016 |
| `descriptor_cache_stress` | 130 → 140 | 0 → 0 | 66,494 | 5,000,007 |
| `deep_sync_call_chain` | 20 → 20 | 0 → 0 | 625,002 | 400,206 |

Notes on the tables:

- **`precise_walker_ns` is post-only.** The counter was
  introduced by this follow-up plan; pre-Phase-3 binaries
  don't have it. `diff-throughput.py` renders the pre value
  as `n/a`. The decomposition below treats the missing pre
  value as 0 (the counter literally did not exist on the pre
  side).
- **`boehm_gc_time_ms = 0` everywhere.** Plotted side-by-side
  with the original throughput report's finding (also all
  zeros), under a 16384 KB budget pin this run was designed
  to break, the timer still reads 0. See "Verdict" below.
- **Alloc-count cross-check.** Every workload's `alloc_count`
  matches exactly pre-vs-post on both lanes. Confirms the
  workloads ran the same computation on both sides — the
  measurement is comparing apples to apples.

## Decomposition

Phase 3's net effect was supposed to decompose into:

| Metric | Formula | This run |
|---|---|---|
| Savings | pre-`boehm_gc_time_ms` − post-`boehm_gc_time_ms` | **0** (every workload) |
| Cost | post-`SIGIL_COUNTER_PRECISE_WALKER_NS` / 1,000,000 (ns → ms) | 0–0.6 ms cumulative per workload |
| Net | savings − cost | 0 ms − cost = small cost only |

The savings column is zero by measurement, not zero by
inference. The cost column is non-zero across every Cps-
bearing workload. The net is therefore "Phase 3 added a
small mark-phase overhead, with no measurable mark-phase
savings to offset it" — but the savings column is
unmeasurable, not necessarily zero. See "Hypothesis-check
resolution" below.

## Hypothesis-check resolution

Phase 3's original report named this follow-up as the
hypothesis-check escalation path:

> "Boehm's default allocator pacing escalates to a full
> collection only at heap sizes much larger than any workload
> in this suite reaches; the precise-walker-vs-conservative-
> scan distinction is therefore invisible to wall-clock
> measurement at this scale. See Discussion → 'Hypothesis
> check' for the escalation-case follow-up plan this finding
> triggers."
> — `plan-e2-phase-3-throughput.md` TL;DR

This follow-up closes that thread. The escalation-case
verdict: **the hypothesis is unfalsifiable at Sigil's current
Boehm-integration shape**, even under forced pressure. Specifically:

- Boehm's behaviour under `GC_set_max_heap_size` is "grow
  toward the limit, OOM-abort at the limit, do NOT fire
  extra full GCs to stay under it." This is the documented
  behaviour per `gc.h`; the env var is a hard ceiling, not
  a GC-aggression knob.
- The design body's option (b) rejection was specifically
  of a **Sigil-language-surface `force_gc()` intrinsic** —
  a user-callable builtin that becomes part of the
  language permanently. The argument was "adds permanent
  language surface for a one-shot measurement aid", which
  remains correct for a language-level intrinsic.
- A **runtime-internal debug-build-only `GC_gcollect()`
  injection** is a different mechanism. No language-surface
  change; just an env-var-gated runtime knob (e.g., fire
  `GC_gcollect()` every N allocations under a debug build).
  The design doc didn't explicitly consider this variant,
  so it remains designable as a separate plan without
  re-litigating option (b)'s rejection. Future work that
  wants to measure mark-phase savings would follow this
  path.

> **Update (force-injection follow-up landed):** the runtime-
> internal `GC_gcollect()` injection alluded to above shipped
> as the `SIGIL_FORCE_GC_EVERY_N_ALLOCS` env var (PR introducing
> this section). See the "Force-injection follow-up" section
> below for the conclusive verdict.

What this follow-up DOES provide:

- **Mechanism plumbing.** `SIGIL_MAX_HEAP_SIZE_KB` + the
  `SIGIL_COUNTER_PRECISE_WALKER_NS` counter remain available
  for future debug runs.
- **Walker-cost numbers.** Across the standard Sigil
  workload suite, the precise walker costs 0–625 µs
  cumulative per workload run. On the largest Cps workload
  (`deep_sync_call_chain` on macos at 625 µs over a 20 ms
  run) that's ~3.1% relative overhead — small but non-zero.
- **A frozen architectural finding.** The original Phase 3
  throughput report stays as a snapshot of the unfalsifiable
  state at the default-pacing-only level; this report
  extends it to "still unfalsifiable under explicit budget
  pressure short of `GC_gcollect()` injection."

## Comparison with Phase 3 report's TBD

Phase 3's original report's "boehm_gc_time_ms = 0 on every
workload" remains the headline. The escalation-case data
this report adds confirms the finding holds under stronger
pressure than Phase 3's report tested.

The walker-cost column is genuinely new — Phase 3's report
had no measurement for "what does the precise walker actually
cost?" beyond the per-alloc FP-capture cost. This report
separates the walker callback's cumulative wall-clock from
the per-alloc FP-capture cost, and finds the walker callback
is a small fraction of the total wall-clock on every
workload.

## CI implications

No perf-gate change. The follow-up doc is a snapshot of a
specific workflow run, same shape as Phase 2's report and
Phase 3's report. Per-PR CI's existing perf-floor tests
remain the gate for runtime regressions; this doc is
decomposition-and-verdict, not a steady-state guard.

## Methodology caveats

- **Budget is artificial pressure.** The 16384 KB ceiling is
  not what natural workload scaling produces — it's a probe
  designed to force collection cadence higher than Boehm's
  default heuristic provides. Real-world Sigil programs
  running without the env var see Boehm's default pacing.
- **Pre/post budget asymmetry.** The `SIGIL_MAX_HEAP_SIZE_KB`
  env var is honored only by the post-checkpoint binary —
  the read site landed in this PR. The pre-checkpoint
  binary (`ca29d20`) runs unconstrained regardless of the
  workflow input. The verdict ("`boehm_gc_time_ms = 0` even
  under the budget pin") is therefore verified on the post
  side only; the pre side's behaviour under budget is not
  measured. Future re-runs against a post-merge `pre_sha`
  honor the budget symmetrically on both sides. The verdict
  doesn't change either way — if the post side under budget
  produces no ms-resolved GC time, the pre side under budget
  wouldn't either (Boehm's heap-pin semantics are version-
  stable across `8.2.6` ↔ `8.2.12`).
- **Counter excludes chained-prior-proc.** Boehm's internal
  push_other_roots proc handles TLS roots + dynamic-library
  roots; its cost is not Phase 3's overhead. The counter
  intentionally starts AFTER that call.
- **Pre-checkpoint `precise_walker_ns` is absent.** The
  counter was introduced by this follow-up plan; pre-Phase-3
  binaries don't have it. `diff-throughput.py` renders the
  pre value as `n/a`; the decomposition treats the missing
  pre value as 0.
- **`use std.*;` strip on cherry-pick.** PR #173's
  qualified-only-imports surface added `use` statements to
  every workload that the pre-checkpoint parser (ca29d20,
  pre-PR-#173) rejects. The workflow `sed`-strips the `use`
  lines from the cherry-picked workloads on the pre side
  only — semantically equivalent because pre-PR-#173
  `import std.foo` already brought every name into the bare
  namespace.
- **GitHub Actions runner variability.** Same as Phase 2 /
  Phase 3 reports — wall-clock measurements on shared
  runners are noisy. The script reports IQR; deltas inside
  1.5× IQR are treated as noise. Several wall-clock entries
  in the per-OS tables are ±0 with noisy ±10 ms swings (e.g.,
  `tree_stress_repeat_large` ubuntu 30 → 40 ms with IQR=0);
  treat them as noise-floor rather than signal.
- **The walker-cost numbers vary across runners.** Same
  workload's walker_ns differs ~5× between ubuntu and macos
  for `descriptor_cache_stress` (351k vs 66k), with
  `deep_sync_call_chain` reversing the direction (420k vs
  625k). The ratio reflects a combination of factors none of
  which the verdict is sensitive to:
  1. **libgc mark-callback firing frequency.** libgc 8.2.6
     (ubuntu) vs 8.2.12 (macos) may differ in how often
     `push_other_roots` fires for a given allocation volume;
     more fires → more wall-clock accumulated even at
     identical per-fire cost.
  2. **Per-fire work shape.** The closure passed to
     `walk_for_gc_with_callback_from` does FP-chain walking
     + `GC_push_all_eager` calls; CPU branch predictors,
     cache shape, and TLS access cost differ between
     runner architectures (linux x86_64 vs apple-silicon
     macos).
  3. **`Instant::now()` resolution.** Linux's
     `clock_gettime(CLOCK_MONOTONIC)` and macos's
     `mach_absolute_time` are both fast but not identical;
     for small per-call costs the timer overhead itself is
     a measurable fraction.
  The verdict ("walker has real but small cost") survives
  any of these explanations. Future readers should treat the
  walker-cost numbers as absolute-per-workload, not as
  relative-per-callback — the ratio could shift under a
  different workload mix or libgc version.

## Force-injection follow-up (Status: scaffolded, awaiting measurement)

This section is scaffolded with the infrastructure landing in this
PR; the **verdict body is operator-filled** after triggering the
`throughput-report.yml` workflow under the new
`force_gc_every_n_allocs` input. Until that run lands, every
`TODO(operator)` marker below is unresolved.

### Mechanism

`SIGIL_FORCE_GC_EVERY_N_ALLOCS=N` (positive integer) makes
`sigil_alloc` invoke `GC_gcollect()` after every Nth allocation
completes. Read once at `sigil_gc_init` time and cached in a
`OnceLock<Option<NonZeroU64>>`; the per-alloc cost is a single
Relaxed `OnceLock::get` + branch when the env var is unset.
Diagnostic counter `SIGIL_COUNTER_FORCED_GC_COUNT` records how
many forced collections fired so the operator can sanity-check
the injection actually ran (`alloc_count ÷ N` should match
± 1).

The mechanism is symmetric across pre/post via
[`scripts/pre-checkpoint-cadence-patch.diff`](../../scripts/pre-checkpoint-cadence-patch.diff),
applied to `/tmp/pre-checkpoint/runtime/src/gc.rs` by the workflow's
`pre — patch in cadence injection` step. Without that patch the
pre side would run unconstrained and the comparison would not be
apples-to-apples. The patch verified clean against the pre-Phase-3
SHA `ca29d2061f2897cb824d8328c92a8d945da313cc` at authoring time.

### How the operator runs the measurement

1. **Trigger** `throughput-report.yml` from the Actions UI on this
   branch with:
   - `pre_sha=ca29d2061f2897cb824d8328c92a8d945da313cc`
   - `runs=5`
   - `heap_budget_kb=""` (leave empty — injection alone is enough;
     combining with budget muddies the signal).
   - `force_gc_every_n_allocs=1000` (starting cadence; iterate if
     wall-clock explodes >60 s/workload or if injection fails to
     register `boehm_gc_time_ms`).
2. **Sanity-check artifacts:**
   `SIGIL_COUNTER_FORCED_GC_COUNT` per workload ≈ `alloc_count / N`.
   If the counter is 0 on a workload that allocated ≥ N times,
   the injection didn't run — investigate before iterating N.
3. **Iterate N if needed:**
   - Wall-clock > 60 s/workload → N=10_000 or N=100_000.
   - `boehm_gc_time_ms` still zero with counter advancing → root
     cause something other than cadence; see "Failure modes" in
     the implementation plan body.
4. **Capture** the final workflow run ID + chosen N for the
   verdict subsection below.

### Verdict — TODO(operator)

TODO(operator): **Replace this paragraph with one of the three
verdict shapes (Confirmed / Disproven / Pathological) per the
plan body's "Verdict outcomes" section.** Lead with the verdict in
the first sentence, then the headline number. Then update this
doc's top-of-file TL;DR to reflect the resolved state — the
"Inconclusive" framing from PR #176 is no longer the latest word.

### Run metadata — TODO(operator)

- **Workflow run ID:** TODO(operator)
- **Pre SHA:** `ca29d2061f2897cb824d8328c92a8d945da313cc`
- **Post SHA:** TODO(operator) — commit on this branch the run measured
- **N (final):** TODO(operator)
- **Iteration history:** TODO(operator) — if N was adjusted, list prior values + why

### Per-OS measurement tables — TODO(operator)

Fill in from `throughput-data-{ubuntu-24.04,macos-14}` artifacts.
Recommend one table per OS with columns:
`workload | wall_clock_ms (pre/post) | boehm_gc_time_ms (pre/post) | SIGIL_COUNTER_FORCED_GC_COUNT (pre/post) | SIGIL_COUNTER_PRECISE_WALKER_NS (post) | alloc_count`.

### Decomposition — TODO(operator)

For each alloc-heavy workload:
- `savings = pre boehm_gc_time_ms - post boehm_gc_time_ms`
- `cost = post SIGIL_COUNTER_PRECISE_WALKER_NS / 1_000_000` (ns → ms)
- `net = savings - cost`

### Methodology caveats

- **Pre-side patch is cherry-picked** by the workflow at run time —
  the pre-checkpoint binary does not natively honor
  `SIGIL_FORCE_GC_EVERY_N_ALLOCS`. The patch's stability rests on
  `ca29d20`'s `sigil_alloc` shape; future re-pinning to a
  different pre SHA may need a regenerated patch.
- **No `ForcedGcCount` counter on the pre side.** The pre-side
  patch does NOT backport the `CounterId::ForcedGcCount = 29`
  slot — that would require a sibling patch to `counters.rs`.
  The sanity-check column is post-side only. Pre-side firing is
  inferred from `boehm_gc_time_ms` advancement.
- **N selection trades wall-clock for signal.** Smaller N → more
  GC cycles → more mark-phase wall-clock signal but slower runs.
  Larger N → faster but mark-phase aggregate shrinks. Operator
  should iterate empirically.

## Related work

- [`compiler/docs/plan-e2-phase-3-throughput.md`](plan-e2-phase-3-throughput.md) — the original throughput report; frozen snapshot of the unfalsifiable state.
- [`compiler/docs/plan-e2-phase-2-throughput.md`](plan-e2-phase-2-throughput.md) — Phase 2 precise-typed-malloc report.
- `PLAN_E2_PROGRESS.md` — Phase 3 / Task 12 entry now links to both reports.
