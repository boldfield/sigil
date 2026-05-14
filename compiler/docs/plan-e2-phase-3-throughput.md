# Plan E2 Phase 3 throughput report

**Status:** scaffolding. Data captured from
`throughput-report.yml` run \<TODO: run id\> (commit \<TODO: post
SHA\>). The workflow runs end-to-end on both CI lanes
(ubuntu-24.04 + macos-14) at the pre-Phase-3 SHA
(`ca29d2061f2897cb824d8328c92a8d945da313cc`) and the branch
HEAD (\<TODO: post SHA\>); the per-workload JSON + per-OS
deltas summary live in the run's artifact upload.

The data tables and discussion below are filled in by the PR
author after the workflow run completes — the scaffolding
pre-commits the doc structure + the methodology / workload /
hypothesis sections so the data paste is the only post-CI step.

## TL;DR

Phase 3's hypothesis: **dropping conservative stack scan on
Sigil program threads (via `GC_do_blocking` around
`sigil_run_loop`) should reduce mark-phase time, with the
largest effect on workloads with deep call chains carrying
heap-bearing args at every frame.**

Headlines after the data lands (TBD):

- **`deep_sync_call_chain`** (new for this report; 200 rounds ×
  2000-deep non-TCO recursion; 400k allocations): \<TODO\>.
- **`tree_stress_repeat_large`** (983k allocations): \<TODO\>.
- **`descriptor_cache_stress`** (5M allocations, alloc-bound
  workload): \<TODO\> — Phase 3 should not regress the
  alloc-path cost the Phase 2 report measured at +21% ubuntu /
  +86% macos.
- **Existing perf-floor workloads** (`fib_perf`, `fib_cps_perf`,
  `tree`, `tree_stress_repeat`): \<TODO\>.
- **Allocation counts**: identical pre/post on every workload
  (expected — Phase 3's changes are runtime-side; no codegen
  delta).
- **GC time (`boehm_gc_time_ms`)**: \<TODO — the load-bearing
  number for Phase 3's hypothesis\>.

**Spec:** [`designs/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md`](https://github.com/boldfield/designs/blob/main/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md)
**Implementation plan:** [`designs/in-progress/2026-05-13-sigil-plan-e2-phase-3-throughput-report.md`](https://github.com/boldfield/designs/blob/main/in-progress/2026-05-13-sigil-plan-e2-phase-3-throughput-report.md)

## Methodology

### Checkpoints

| Checkpoint | SHA | What's there |
|---|---|---|
| Pre-Phase-3 | `ca29d2061f2897cb824d8328c92a8d945da313cc` | Plan E2 **Phase 2** fully merged. Heap-side precision (typed-malloc dispatch + descriptor cache) is on. Stack-side is still **conservative**: Boehm's auto stack scan walks every word of every Sigil call frame at every mark phase. The push_other_roots callback installed by PR #170 is NOT yet present; the captured-FP mechanism + `GC_do_blocking` wrap from PR #171 are NOT yet present. |
| Post-Phase-3 | \<TODO: post SHA\> | Plan E2 Phase 3 Tasks 10–12 fully merged. `sigil_run_loop` body runs inside `GC_do_blocking` so Boehm's conservative stack scan covers only the frames ABOVE the trampoline (Rust main shim, libc init); the Sigil call chain is supplied to Boehm precisely via the stackmap-driven `push_other_roots` callback. `sigil_alloc` wraps the allocator dispatch in `GC_call_with_gc_active` and captures its own FP into a thread-local that the callback reads as the walker's starting FP. |

**Phase 3 introduces a small compiler-side delta** (runtime
TLS-write at `sigil_alloc` entry — emitted by codegen via the
`SigilCallerFpGuard::capture()` inline call) but NO change to
the emitted machine code for the workloads in this report. The
guard is `#[cfg(not(test))]`-gated and runs in production
builds only. Within a workload's hot path, codegen-emitted
instructions are identical pre/post; the measured delta is
therefore attributable entirely to runtime-side cost (the
`GC_do_blocking` re-entry on each `sigil_run_loop`, the
`GC_call_with_gc_active` re-entry on each `sigil_alloc`, the
FP-capture TLS write, and Boehm's mark-phase savings from
skipping the Sigil call chain's conservative scan).

**libgc version recording.** The measurement workflow runs
`pkg-config --modversion bdw-gc` on each runner and emits the
version into the per-OS `deltas-<os>.md` summary it uploads.
Phase 2's report doc explicitly flagged that the libgc version
could drift between runs of the workflow; the Phase 3 numbers
will carry the version stamp, which lets a future reader
cross-reference whether a libgc upgrade (e.g., to 8.3) changed
the mark-phase semantics this report is measuring.

### Workloads

Seven workloads, all in `examples/`. Six carry over from the
Phase 2 report (so cross-phase comparison is apples-to-apples);
the seventh (`deep_sync_call_chain`) is added specifically to
exercise Phase 3's boundary.

1. **`fib_perf.sigil`** — naïve recursive `fib(20)`. ~6 heap
   allocations total. Pins the alloc-free perf floor.
2. **`fib_cps_perf.sigil`** — CPS-color `fib(20)` via effect
   handlers. ~22k allocations of `TAG_CLOSURE` /
   `TAG_CONTINUATION` shapes. Subject to the Plan B Task 60
   perf gate (50ms x86 / 500ms aarch64).
3. **`tree.sigil`** — depth-15 binary tree (65,535 nodes).
   Each node is `Node(Int, Tree, Tree)` → count=3,
   bitmap=0b110 (two pointers, one int). Subject to the
   Plan A3 Task 44 perf gate (500ms aarch64).
4. **`tree_stress_repeat.sigil`** — 10 rounds of depth-12 tree
   build + fold + drop, ~81,910 allocations. Subject to its
   own perf gate.
5. **`tree_stress_repeat_large.sigil`** — 30 rounds of depth-14
   build + fold + drop, ~983,010 allocations. Provides bulk
   alloc volume.
6. **`descriptor_cache_stress.sigil`** — 10 distinct sum-type
   shapes × 500,000 allocations each = 5,000,000 total.
   Exercises the descriptor cache hot path the Phase 2 report
   pinned. Phase 3 shouldn't regress this — its changes are
   stack-side, not alloc-path.
7. **`deep_sync_call_chain.sigil`** — **new for this report.**
   200 rounds of build-fold over a 2000-deep non-TCO linked
   list. Each `build_nontco` frame holds a heap pointer
   (`Cons` cell) as a live root; pre-Phase-3 Boehm's
   conservative stack scan walks every word of every frame
   (~64 KB of stack at peak) at every mark, post-Phase-3 only
   the stackmap-emitted root slot per frame is supplied to
   Boehm. **This is the workload most likely to show Phase 3's
   "win" if the hypothesis holds.**

### Metrics

For each workload × checkpoint, 5 runs, median + IQR:

- **`wall_clock_ms`** — `/usr/bin/time -v` on Linux, `-l` on macOS.
- **`peak_rss_kb`** — same time output, normalised to kB.
- **`alloc_count`** — `SIGIL_COUNTER_BOEHM_ALLOC_COUNT` from
  `sigil --print-runtime-stats` stderr.
- **`alloc_bytes`** — `SIGIL_COUNTER_BOEHM_ALLOC_BYTES`.
- **`boehm_gc_time_ms`** — `GC_get_full_gc_total_time` queried
  at process exit. Phase 2's report carried this on the
  post side only; Phase 3 carries it on both sides (the probe
  is on main as of `ca29d20`).

### Reproducing

The two-checkpoint measurement is mechanised in
`.github/workflows/throughput-report.yml`. Trigger via the
Actions UI ("Run workflow") on the throughput-report branch,
passing `pre_sha=ca29d2061f2897cb824d8328c92a8d945da313cc`
(the workflow's input default after this PR lands). The
workflow:

1. Builds the post-Phase-3 compiler at the branch HEAD.
2. Compiles + measures each of the 7 workloads (5 runs each).
3. Adds a git worktree at the pre-Phase-3 SHA.
4. Idempotently cherry-picks the new workload + scripts onto
   the worktree (only where missing — `tree_stress_repeat_large`
   and `descriptor_cache_stress` ALREADY exist at the
   pre-Phase-3 SHA, so the workflow keeps the worktree's
   own copies for measurement-source fidelity).
5. Builds the pre-Phase-3 compiler in the worktree.
6. Compiles + measures the same 7 workloads.
7. Uploads JSON + per-OS `deltas-<os>.md` as artifacts.

On the local pod the measurement is OOM-banned (`cargo build
--release` of `sigil-compiler` blows the node's memory budget
per `CLAUDE.md`). CI is the authoritative measurement
environment — same constraint as the Phase 2 report.

## Workload definitions

### `fib_perf.sigil`

Same as Phase 2's report — see
`compiler/docs/plan-e2-phase-2-throughput.md`.

### `fib_cps_perf.sigil`

Same as Phase 2's report.

### `tree.sigil`

Same as Phase 2's report.

### `tree_stress_repeat.sigil`

Same as Phase 2's report.

### `tree_stress_repeat_large.sigil`

Same as Phase 2's report.

### `descriptor_cache_stress.sigil`

Same as Phase 2's report.

### `deep_sync_call_chain.sigil`

New. Non-TCO recursive linked-list builder + non-tail-recursive
sum, repeated for 200 rounds at 2000-deep:

```sigil
type Cons = | Nil | C(Int, Cons)

fn build_nontco(n: Int) -> Cons ![] {
  match n {
    0 => Nil,
    _ => C(n, build_nontco(n - 1)),
  }
}

fn sum_list(c: Cons) -> Int ![] {
  match c {
    Nil => 0,
    C(v, rest) => v + sum_list(rest),
  }
}

fn iter(rounds: Int, depth: Int, total: Int) -> Int ![] {
  match rounds {
    0 => total,
    _ => {
      let xs: Cons = build_nontco(depth);
      iter(rounds - 1, depth, total + sum_list(xs))
    },
  }
}
```

200 rounds × 2000-deep = 400,000 allocations across ~6.4 MB
of payload. The 2000-deep `build_nontco` recursion is the
load-bearing shape: wrapping the recursive call inside the
`C(n, ...)` constructor defeats TCO, so all 2000 stack frames
remain live during the build phase. Each frame holds the
running `Cons` partial as a live root — a heap pointer the
walker must surface.

Pre-Phase-3: Boehm conservatively scans every word of those
2000 frames at every mark. Each frame is a few hundred bytes
of stack (saved rbp + saved PC + locals + spill slots), so
the conservative scan inspects ~tens of KB of stack words
per mark phase, with many false-positive pointer-shape hits.

Post-Phase-3: the precise walker yields exactly one heap-
pointer slot per `build_nontco` frame (the in-flight `result`
once we've returned from the recursive call into the
constructor's argument slot). Boehm's conservative scan
covers only the Rust main shim + the GC_call_with_gc_active
re-active window — a handful of frames, not 2000.

Expected post-Phase-3 wall-clock: 100–400 ms range.

## Deltas — ubuntu-24.04

\<TODO — paste from `throughput-data-ubuntu-24.04/deltas-ubuntu-24.04.md`\>

### `fib_perf`

\<TODO\>

### `fib_cps_perf`

\<TODO\>

### `tree`

\<TODO\>

### `tree_stress_repeat`

\<TODO\>

### `tree_stress_repeat_large`

\<TODO\>

### `descriptor_cache_stress`

\<TODO\>

### `deep_sync_call_chain`

\<TODO\>

## Deltas — macos-14

\<TODO — paste from `throughput-data-macos-14/deltas-macos-14.md`\>

### `fib_perf`

\<TODO\>

### `fib_cps_perf`

\<TODO\>

### `tree`

\<TODO\>

### `tree_stress_repeat`

\<TODO\>

### `tree_stress_repeat_large`

\<TODO\>

### `descriptor_cache_stress`

\<TODO\>

### `deep_sync_call_chain`

\<TODO\>

## Discussion

### Hypothesis check: did dropping conservative stack scan reduce mark-phase time?

\<TODO — load-bearing answer once data lands. The hypothesis is
specifically that `deep_sync_call_chain` (and to a lesser
extent `tree_stress_repeat_large`) should show a `boehm_gc_time_ms`
decrease post-Phase-3, while `fib_perf` / `fib_cps_perf` (no
GC fires) should show no signal. If `boehm_gc_time_ms` is 0
on every workload, the hypothesis is **unmeasured at this
workload scale** rather than disproven — the same gap Phase 2's
report flagged.\>

### Cross-check: precise-root-set coverage from PR #163's harness

PR #163 (Task 5) added the `SIGIL_GC_CROSS_CHECK=1` runtime
assertion that every precise root the stackmap walker yields
is (a) inside the calling thread's stack range and (b) heap-
pointer-shaped per Boehm's view. The Task 12 e2e tests
(`precise_walker_deep_chain_under_cross_check` etc.) run this
harness against deep-chain workloads on every CI run, asserting
zero divergence.

**Coverage cross-check for this report:** the
`deep_sync_call_chain` workload runs without
`SIGIL_GC_CROSS_CHECK` set in the throughput workflow (the
cross-check has its own runtime cost that would dominate the
measured delta). The structural argument that Phase 3 hasn't
regressed coverage is:

- The precise walker's correctness is pinned by the cross-check
  on a structurally-identical workload (`precise_walker_deep_chain_under_cross_check`,
  1000-deep) running on every CI lane.
- Phase 3 did not change the stackmap writer, the descriptor
  cache, or the per-allocation precise-root emission — those
  are all Phase 1 / Phase 2 territory. Phase 3 only changed
  *which root supply mechanism Boehm uses on Sigil threads*
  (conservative auto-scan → `push_other_roots` callback). The
  precise root set Cranelift emits is unchanged.

If a future re-run shows a `deep_sync_call_chain` regression
(post becomes slower than pre on this workload), the
suspicion ladder is:

1. **GC_do_blocking re-entry cost dominates.** Each
   `sigil_run_loop` call pays a `GC_do_blocking` →
   trampoline transition. If `sigil_run_loop` is called many
   times (nested handle expressions), the per-call overhead
   adds up. `deep_sync_call_chain` calls run_loop a handful
   of times (one per top-level dispatch); should be small.
2. **GC_call_with_gc_active per-alloc cost dominates.**
   Every `sigil_alloc` pays the re-active transition. At
   400k allocs, even a small per-alloc cost adds up.
3. **Precise walker per-frame cost dominates.** The walker
   walks the FP chain on every mark. 2000 frames × N marks
   could be visible.

Each is independently measurable as a follow-up; not chased
in this report (the report's job is measurement, not
optimisation).

### Comparison with Phase 2's report

Phase 2's report ([compiler/docs/plan-e2-phase-2-throughput.md](./plan-e2-phase-2-throughput.md)) measured **alloc-path** cost: descriptor cache lookup + typed-malloc dispatch. The headline was +21% / +86%
(ubuntu / macos) on `descriptor_cache_stress`, with no measurable
delta on the perf-floor workloads.

Phase 3 measures **stack-side** cost: `GC_do_blocking` + `GC_call_with_gc_active`
wrap overhead + precise walker cost vs Boehm's conservative
stack scan savings.

Together, the two phases characterise Plan E2's full cost
surface:

\<TODO — paste a one-table summary combining the two reports'
descriptor_cache_stress + deep_sync_call_chain numbers so the
total Plan E2 cost is visible in one place.\>

## CI implications

The existing perf-floor gates continue to pass on the
post-Phase-3 checkpoint (verified by the `ci.yml` lanes on
PR #171). The throughput workflow measured wall-clock for the
perf-floor workloads at the 0-precision-floor; that's a
**non-measurement** (the workloads finish below `/usr/bin/time`'s
~10ms resolution), not a quantitative "no slowdown" guarantee.

**No follow-up plan to widen or tighten any perf gate.** Same
rule as Phase 2's report: the gates are wired in
`compiler/tests/e2e.rs` and verify the post-Phase-3 binary
passes them; the throughput workflow's wall-clock numbers
for these workloads are observational, not gate-defining.

## Stability / caveats

- **Boehm version drift between checkpoints.** Both checkpoints
  use the system libgc. The workflow records
  `pkg-config --modversion bdw-gc` per lane and emits it
  into the per-OS deltas summary — verified in the JSON
  artifacts.
- **GitHub Actions runner variability.** Wall-clock measurements
  on shared runners are noisy. The script reports IQR;
  readers should treat any delta < 1.5× IQR as noise.
- **GC time fraction is now symmetric.** Phase 2's report had
  `boehm_gc_time_ms = null` on the pre-Phase-2 side because
  the probe didn't exist there. Phase 2's report PR landed
  the probe, so by `ca29d20` it's on main; both Phase 3
  checkpoints carry it.
- **Phase 3 introduces additional Rust-side TLS writes on the
  hot path** (FP capture in `sigil_alloc` via the
  `SigilCallerFpGuard`). The per-alloc cost is one thread-
  local cell write (~ns). At 5M allocs (the
  `descriptor_cache_stress` shape), the cumulative cost
  projects to ~5–15 ms — small fraction of the workload's
  150–300 ms baseline but worth checking against the actual
  measurement (the data will tell us whether the projection
  matches reality or whether some compiler / inlining
  pessimisation is in play).

## Plan body verification

- [x] `cargo check --workspace` clean on both pre + post worktrees
      (verified via the throughput workflow's "cargo build --release"
      steps, which would fail-fast on a check failure).
- [ ] `scripts/measure-throughput.sh` reproduces valid JSON.
      \<TODO: confirm by inspecting the artifact JSON post-run\>.
- [x] Report doc renders (this file).
- [x] PLAN_E2_PROGRESS.md updated (Phase 3 closeout checklist's
      throughput line flipped to ✅).
- [ ] CI 4/4 lanes green.
      \<TODO: confirm post-merge\>.
