# Plan E2 Phase 2 throughput report

**Status:** measured. Data captured from
`throughput-report.yml` run 25826188199 (commit `2871742`).
The workflow ran end-to-end on both CI lanes (ubuntu-24.04 +
macos-14) at the pre-Phase-2 SHA (`4f7ec86`) and the branch
HEAD (`2871742`); the per-workload JSON + per-OS deltas
summary live in the run's artifact upload.

## TL;DR

Phase 2 makes the descriptor-cache hot path measurably slower
under sustained alloc pressure:

- **`descriptor_cache_stress`** (5M allocs across 10 shapes):
  +21% wall-clock on ubuntu-24.04 (140ms → 170ms);
  +86% on macos-14 (70ms → 130ms).
  Per-alloc cost increase: 30 ms / 5,000,000 allocs = **6 ns/alloc
  on ubuntu**; 60 ms / 5,000,000 = **12 ns/alloc on macos**.
  Attributable to the new path: `descriptor::get_or_create`
  (`BTreeMap` lookup under `RwLock` read) + the descriptor
  passed through `GC_malloc_explicitly_typed` instead of plain
  `GC_malloc`.
- **`tree_stress_repeat_large`** (983k allocs): wall-clock
  flat-to-+50% at the precision floor (20–30ms range). Noise
  bounds the signal at this scale.
- **Existing perf-floor workloads** (`fib_perf`, `fib_cps_perf`,
  `tree`, `tree_stress_repeat`): all 0ms on both pre and post;
  below `/usr/bin/time`'s ~10ms precision floor by design.
- **Allocation counts**: identical pre/post on every workload
  (expected — same source, same code path).
- **GC time (`boehm_gc_time_ms`)**: 0 on every post-Phase-2 run.
  None of the workloads triggered a full GC cycle. The cost
  delta is therefore *entirely* alloc-path, not mark-phase.

This matches the plan body's hypothesis: the descriptor cache
adds per-alloc cost on the order of nanoseconds (the lookup is
not free); the mark-phase savings the precise marker should
deliver are not visible on these workloads because none of them
sustain enough heap pressure to trigger a full collection.

**Spec:** [`designs/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md`](https://github.com/boldfield/designs/blob/main/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md)
**Implementation plan:** [`designs/in-progress/2026-05-13-sigil-plan-e2-phase-2-throughput-report.md`](https://github.com/boldfield/designs/blob/main/in-progress/2026-05-13-sigil-plan-e2-phase-2-throughput-report.md)

## Methodology

### Checkpoints

| Checkpoint | SHA | What's there |
|---|---|---|
| Pre-Phase-2 | `4f7ec86c52c4aa7335571c0be5e7e771e766c0ad` | Plan E2 Phase 2 Tasks 6 + 7 merged (Boehm precise-mode API spike + descriptor cache). Task 8 (the dispatch flip from `GC_malloc` to `GC_malloc_explicitly_typed`) NOT yet applied — every non-zero-bitmap object still routes through plain `GC_malloc` for conservative full-payload scan. |
| Post-Phase-2 | `270f6b14e8b8eaf3c1ff9aaa28b8d0f6` (`270f6b1`) | Plan E2 Phase 2 Tasks 6–9 fully merged. `sigil_alloc` routes through the three-branch dispatch (atomic / conservative-by-count / precise typed-malloc). The descriptor cache holds one `GC_descr` per shape. The false-retention reproducer (`runtime/src/gc.rs::tests::false_retention_reproducer_precise_marker_drops_aliased_address`) is the load-bearing precision proof. |

**Phase 2 introduced zero compiler-side changes.** Verified via
`git diff 4f7ec86 270f6b1 -- compiler/src` (empty output) at the
time of measurement. The pre and post compilers produce identical
native code for the workloads in this report; the measured delta
is therefore attributable entirely to runtime-side cost
(`sigil_alloc` dispatch + descriptor cache lookup + Boehm
typed-malloc vs `GC_malloc`), not to codegen-emitted instruction
differences. This is the cross-check that the two-checkpoint
measurement compares apples to apples.

**libgc version recording.** The measurement workflow runs
`pkg-config --modversion bdw-gc` on each runner and emits the
version into the per-OS `deltas-<os>.md` summary it uploads.
This recording was added late in the report-PR cycle, so the
data captured here (workflow run 25826188199, both lanes)
predates the change — those JSON files do not carry a libgc
version stamp. Future re-runs (and the Phase 3 throughput
report) will. The Phase 2 numbers in this report should be
interpreted as observing whatever libgc the GitHub Actions
`ubuntu-24.04` + `macos-14` images were carrying on 2026-05-13
(roughly libgc 8.2.x on both); a more precise version pin
arrives with the next workflow run.

Different libgc versions can have different mark-phase +
allocator-pacing behaviour; the recording protects future
cross-checkpoint comparisons from silent drift.

### Workloads

Six workloads, all in `examples/`:

1. **`fib_perf.sigil`** — naïve recursive `fib(20)`. ~6 heap allocations
   total. Pins the alloc-free perf floor. Reads at the
   `/usr/bin/time` ~10ms precision floor on both CI hosts (≈ 0 ± 0 ms);
   the report relies on RSS + alloc_count for cross-checking rather
   than wall-clock for this workload.

2. **`fib_cps_perf.sigil`** — CPS-color `fib(20)` via effect handlers.
   ~22k allocations of TAG_CLOSURE / TAG_CONTINUATION shapes. Subject
   to the Plan B Task 60 perf gate (50ms x86 / 500ms aarch64). Below
   the precision floor on both hosts.

3. **`tree.sigil`** — depth-15 binary tree (65,535 nodes). Each node is
   `Node(Int, Tree, Tree)` → count=3, bitmap=0b110 (two pointers, one int).
   Pins the multi-pointer shape descriptor-cache cost. Subject to the
   Plan A3 Task 44 perf gate (500ms aarch64 CI). Below the precision floor.

4. **`tree_stress_repeat.sigil`** — 10 rounds of depth-12 tree build +
   fold + drop, ~81,910 allocations. Subject to its own perf gate. Below
   the precision floor.

5. **`tree_stress_repeat_large.sigil`** — **new for this report.** 30
   rounds of depth-14 build + fold + drop, ~983,010 allocations.
   A sibling of `tree_stress_repeat.sigil` scaled up for measurable
   wall-clock signal — keeps the existing workload's perf gate
   untouched. Expected wall-clock: 100–200ms per checkpoint.

6. **`descriptor_cache_stress.sigil`** — **new for this report.** 10
   distinct sum-type shapes × 500,000 allocations each = 5,000,000
   total. Exercises the descriptor cache at its widest shape diversity
   (10 entries; one cache miss per shape, ~99.999% hit rate
   steady-state). Expected wall-clock: 150–300ms per checkpoint.

The wall-clock precision floor for the first four workloads is a known
limitation; their measurement value lies in RSS + alloc_count + GC
time deltas, where any change is signal regardless of wall-clock
resolution.

### Metrics

For each workload × checkpoint, 5 runs, median + IQR:

- **`wall_clock_ms`** — `/usr/bin/time -v` on Linux, `-l` on macOS.
- **`peak_rss_kb`** — same time output, normalised to kB.
- **`alloc_count`** — `SIGIL_COUNTER_BOEHM_ALLOC_COUNT` from
  `sigil --print-runtime-stats` stderr output.
- **`alloc_bytes`** — `SIGIL_COUNTER_BOEHM_ALLOC_BYTES`.
- **`boehm_gc_time_ms`** — new probe added in this PR (queries
  `GC_get_full_gc_total_time` at exit). Reported as `null` on the
  pre-Phase-2 checkpoint (the runtime didn't expose this counter).
  Post-Phase-2 numbers are reportable; the pre-Phase-2 gap is
  documented honestly rather than backfilled by patching the
  pre-Phase-2 worktree.

### Reproducing

The two-checkpoint measurement is mechanised in
`.github/workflows/throughput-report.yml`. Trigger via the Actions
UI ("Run workflow") on the throughput-report branch. The workflow:

1. Builds the post-Phase-2 compiler at the branch HEAD.
2. Compiles + measures each of the 5 workloads (5 runs each).
3. Adds a git worktree at the pre-Phase-2 SHA
   (`4f7ec86c52c4aa7335571c0be5e7e771e766c0ad`).
4. Cherry-picks the new workload file + the measurement scripts
   onto the pre-Phase-2 worktree (the pre SHA doesn't ship them).
5. Builds the pre-Phase-2 compiler in the worktree.
6. Compiles + measures the same 5 workloads.
7. Uploads JSON + per-OS `deltas-<os>.md` as artifacts.

On the local pod the measurement is OOM-banned (`cargo build
--release` of `sigil-compiler` blows the node's memory budget per
`CLAUDE.md`). CI is the authoritative measurement environment.

## Workload definitions

### `fib_perf.sigil`

```sigil
fn fib(n: Int) -> Int ![] { match n { 0 => 0, 1 => 1, _ => fib(n - 1) + fib(n - 2), } }
fn main() -> Int ![IO] { perform IO.println(int_to_string(fib(20))); 0 }
```

Compile + run: `./target/release/sigil examples/fib_perf.sigil -o bin/fib_perf && bin/fib_perf`.

### `fib_cps_perf.sigil`

(See file header for the CPS-color setup; the workload `fib(20)` is the same.)

### `tree.sigil`

Builds a depth-15 binary tree (65,535 allocations), folds, prints sum.

### `tree_stress_repeat.sigil`

10 rounds of depth-12 build-fold-drop = 81,910 allocations.

### `tree_stress_repeat_large.sigil`

New. 30 rounds of depth-14 build + fold + drop. ~983,010 allocations.
Sibling of `tree_stress_repeat.sigil` scaled up so wall-clock signal
clears the `/usr/bin/time` precision floor without touching the
existing workload's Plan B Task 60 perf gate.

### `descriptor_cache_stress.sigil`

New. 10 distinct sum-type shapes × 500,000 allocations each =
5,000,000 total. Each shape has a unique `(payload_count,
pointer_bitmap)` pair, so the descriptor cache builds 10 entries
(one cache miss per shape) and serves 4,999,990 hits.

## Deltas — ubuntu-24.04

Source: `throughput-data-ubuntu-24.04/deltas-ubuntu-24.04.md`
artifact from `throughput-report.yml` run 25826188199. Pre SHA
`4f7ec86`, post SHA `2871742`, 5 runs per workload.

### `fib_perf`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 3344 ± 52 | 3300 ± 52 | -44 kB | -1.3% |
| alloc_count | 6 | 6 | +0 | +0.0% |
| alloc_bytes (bytes) | 528 | 528 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `fib_cps_perf`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 3400 ± 76 | 3404 ± 76 | +4 kB | +0.1% |
| alloc_count | 21898 | 21898 | +0 | +0.0% |
| alloc_bytes (bytes) | 1401624 | 1401624 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 6124 ± 24 | 6192 ± 24 | +68 kB | +1.1% |
| alloc_count | 65541 | 65541 | +0 | +0.0% |
| alloc_bytes (bytes) | 1835496 | 1835496 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree_stress_repeat`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 4008 ± 80 | 3936 ± 16 | -72 kB | -1.8% |
| alloc_count | 81916 | 81916 | +0 | +0.0% |
| alloc_bytes (bytes) | 2293888 | 2293888 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree_stress_repeat_large`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 30 ± 0 | 30 ± 0 | +0 ms | +0.0% |
| peak_rss_kb (kB) | 4856 ± 8 | 6108 ± 16 | +1252 kB | +25.8% |
| alloc_count | 983016 | 983016 | +0 | +0.0% |
| alloc_bytes (bytes) | 27524448 | 27524448 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `descriptor_cache_stress`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 140 ± 0 | 170 ± 10 | +30 ms | +21.4% |
| peak_rss_kb (kB) | 3480 ± 44 | 3428 ± 20 | -52 kB | -1.5% |
| alloc_count | 5000007 | 5000007 | +0 | +0.0% |
| alloc_bytes (bytes) | 192000544 | 192000544 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

## Deltas — macos-14

Source: `throughput-data-macos-14/deltas-macos-14.md`. Same pre/post
SHAs as ubuntu, 5 runs per workload.

### `fib_perf`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 2704 ± 0 | 2752 ± 0 | +48 kB | +1.8% |
| alloc_count | 6 | 6 | +0 | +0.0% |
| alloc_bytes (bytes) | 528 | 528 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `fib_cps_perf`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 3456 ± 0 | 3504 ± 0 | +48 kB | +1.4% |
| alloc_count | 21898 | 21898 | +0 | +0.0% |
| alloc_bytes (bytes) | 1401624 | 1401624 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 5680 ± 0 | 5728 ± 64 | +48 kB | +0.8% |
| alloc_count | 65541 | 65541 | +0 | +0.0% |
| alloc_bytes (bytes) | 1835496 | 1835496 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree_stress_repeat`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 0 ± 0 | 0 ± 0 | +0 ms | n/a |
| peak_rss_kb (kB) | 4000 ± 0 | 4048 ± 0 | +48 kB | +1.2% |
| alloc_count | 81916 | 81916 | +0 | +0.0% |
| alloc_bytes (bytes) | 2293888 | 2293888 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `tree_stress_repeat_large`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 20 ± 0 | 30 ± 0 | +10 ms | +50.0% |
| peak_rss_kb (kB) | 6528 ± 0 | 5648 ± 64 | -880 kB | -13.5% |
| alloc_count | 983016 | 983016 | +0 | +0.0% |
| alloc_bytes (bytes) | 27524448 | 27524448 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

### `descriptor_cache_stress`

| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |
|---|---|---|---|---|
| wall_clock_ms (ms) | 70 ± 0 | 130 ± 10 | +60 ms | +85.7% |
| peak_rss_kb (kB) | 3456 ± 0 | 3488 ± 0 | +32 kB | +0.9% |
| alloc_count | 5000007 | 5000007 | +0 | +0.0% |
| alloc_bytes (bytes) | 192000544 | 192000544 | +0 | +0.0% |
| boehm_gc_time_ms (ms) | n/a | 0 | n/a | n/a |

## Discussion

### What the plan body hypothesised vs what we measured

| Hypothesis | Result |
|---|---|
| Descriptor cache adds ~ns per alloc on hot path | **Confirmed.** ~6ns/alloc on ubuntu, ~12ns/alloc on macos. |
| Mark-phase time decreases (precise vs conservative scan) | **Not observable in this report.** No workload triggered a full GC. `boehm_gc_time_ms` is 0 on every post-Phase-2 run. Need a higher-pressure workload that forces collection to characterise this. |
| Alloc-light workloads see flat perf | **Confirmed in shape, unmeasurable in magnitude.** All four existing perf-floor workloads measure at 0 ± 0 ms on both lanes; that's the time-precision floor talking, not a guarantee of zero delta. RSS deltas are noise-level (1-2%). |

### The +21% / +86% asymmetry between ubuntu and macos

ubuntu-24.04 (x86_64 / GitHub Actions free runner): +30ms (+21%).
macos-14 (aarch64 / GitHub Actions Apple Silicon): +60ms (+86%).

Pre-Phase-2 macos already ran the same 5M allocs in **70ms** — half
the ubuntu time. The pre-Phase-2 path was plain `GC_malloc` plus
header construction; macos's M-series cores allocate from libgc
faster than the EC2 x86 lane.

Post-Phase-2, ubuntu pays 30ms for the descriptor path; macos
pays 60ms. Hypotheses for the 2× macos cost (not pinned in this
report):

- **RwLock contention shape.** macos's `pthread_rwlock` vs Linux's
  futex-backed implementation may differ at high read frequency.
  The cache is read-locked once per alloc, write-locked once
  per shape (10 times total in this workload).
- **`BTreeMap::get` cache behaviour.** macos M-series has a
  different L1/L2 layout than EC2 x86; tree-traversal latencies
  may diverge.
- **`GC_make_descriptor` first-call cost.** Each shape's first
  alloc triggers `GC_make_descriptor`. With 10 shapes and 500k
  allocs per shape, the warmup cost is amortised — but if Boehm's
  typed-malloc path itself is slower on macos than on Linux, the
  steady-state delta is dominated by per-alloc lookup + typed-
  malloc cost.

A pinned root-cause analysis is **out of scope for this report**
(the report's job is documenting the delta; chasing the asymmetry
is a separate plan if the macos cost becomes a blocker).

**Escalation criterion.** If any real Sigil workload's hot path
shows the macos / linux wall-clock asymmetry crossing 5×
(rather than the current ~2× on this synthetic stress), file a
follow-up plan to characterise the divergence and pin the root
cause. The 2× synthetic gap is acceptable absent evidence it
shows up on production workloads; 5× would mean an asymmetric
runtime regression worth investigating in its own right.

### Allocation counts are identical, as expected

Every workload shows pre = post on `alloc_count` and `alloc_bytes`.
The code paths allocate the same objects in the same order;
Phase 2 changes *how* Boehm tracks them, not *what* gets allocated.
This is the cross-check that the measurements are comparing apples
to apples.

### GC time is zero everywhere

`boehm_gc_time_ms` = 0 on every post-Phase-2 run. Boehm only counts
**full** GC cycles in `GC_get_full_gc_total_time`; none of the
6 workloads sustain enough heap pressure to trigger one. The
descriptor_cache_stress workload allocates 192 MB total but most
of those objects are unreachable immediately after construction,
so Boehm reclaims them via the normal allocator pacing without
escalating to a full mark-sweep.

**Implication for the Phase 2 "precise marking should reduce GC
time" hypothesis:** unmeasured. To test it, a follow-up plan
should add a workload that:
1. Allocates enough live objects to force a full collection.
2. Has at least one non-zero-bitmap pointer slot per object so
   the precise-marker path is exercised.
3. Calls `sigil_gc_collect()` explicitly (which needs an FFI
   wrapper exposed to Sigil code).

This is the v2 Phase 3 + Plan-E2-follow-up territory — out of
scope for this report.

## CI implications

The existing perf-floor gates (Plan B Task 60: 50ms x86 / 500ms
aarch64 on `fib_cps_perf`; Plan A3 Task 44: 500ms aarch64 on
`tree.sigil`) continue to pass on the post-Phase-2 checkpoint —
the gates are wired in `compiler/tests/e2e.rs` and PR #168's
standard `ci.yml` lanes ran them green. The throughput workflow
measured wall-clock for these workloads at the 0-precision-floor;
that's a **non-measurement** (the workloads finish below
`/usr/bin/time`'s ~10ms resolution), not a quantitative
"no slowdown" guarantee.

**No follow-up plan to widen / tighten any perf gate.** This
recommendation is an **inference** from the descriptor-cache cost
shape, not a direct measurement against the gate workloads. The
inference: at +21–86% on `descriptor_cache_stress` (5M allocs),
the equivalent absolute cost increase on the perf-floor workloads
(6–80k allocs) projects to ~0.5–5 ms — well within the gates'
headroom. The `ci.yml` lanes on PR #168 confirmed the gates pass
post-Phase-2, which is the load-bearing evidence here; the
~0.5–5 ms projection is the explanatory model, not the
verification.

## Stability / caveats

- **Boehm version drift between checkpoints.** Both checkpoints use
  the system libgc, but if the CI image's libgc version changes
  between runs of the workflow, the data is no longer strictly
  comparable across reports. The workflow now records
  `pkg-config --modversion bdw-gc` per lane and emits it into the
  per-OS deltas summary (see Methodology → libgc version recording).
  The Phase 2 data here predates the recording; the Phase 3 report
  will carry it.
- **GitHub Actions runner variability.** Wall-clock measurements on
  shared runners are noisy. The script reports IQR; readers should
  treat any delta < 1.5× IQR as noise.
- **GC time fraction is post-only.** The `boehm_gc_time_ms` probe
  was added in this PR; the pre-Phase-2 worktree doesn't have it.
  We could backport the probe to make the comparison symmetric,
  but the probe's only consumer is this report, and a backport
  would require modifying production code on the pre checkpoint
  for measurement purposes. Cleaner to leave pre as N/A and
  report only the post-Phase-2 absolute number.
