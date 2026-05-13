# Plan E2 Phase 2 throughput report

**Status:** scaffolding (waiting for measurement data — see
`Methodology → Reproducing` below). Once the
`throughput-report.yml` GitHub Actions workflow has been run on
both lanes (ubuntu-24.04 + macos-14), this file's
"Pre-Phase-2 measurements" / "Post-Phase-2 measurements" /
"Deltas" sections will be filled in from the artifact JSON +
the rendered `deltas-<os>.md` summary the workflow produces.

**Spec:** [`designs/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md`](https://github.com/boldfield/designs/blob/main/docs/plans/2026-05-13-sigil-plan-e2-throughput-reports-design.md)
**Implementation plan:** [`designs/in-progress/2026-05-13-sigil-plan-e2-phase-2-throughput-report.md`](https://github.com/boldfield/designs/blob/main/in-progress/2026-05-13-sigil-plan-e2-phase-2-throughput-report.md)

## Methodology

### Checkpoints

| Checkpoint | SHA | What's there |
|---|---|---|
| Pre-Phase-2 | `4f7ec86c52c4aa7335571c0be5e7e771e766c0ad` | Plan E2 Phase 2 Tasks 6 + 7 merged (Boehm precise-mode API spike + descriptor cache). Task 8 (the dispatch flip from `GC_malloc` to `GC_malloc_explicitly_typed`) NOT yet applied — every non-zero-bitmap object still routes through plain `GC_malloc` for conservative full-payload scan. |
| Post-Phase-2 | `270f6b14e8b8eaf3c1ff9aaa28b8d0f6` (`270f6b1`) | Plan E2 Phase 2 Tasks 6–9 fully merged. `sigil_alloc` routes through the three-branch dispatch (atomic / conservative-by-count / precise typed-malloc). The descriptor cache holds one `GC_descr` per shape. The false-retention reproducer (`runtime/src/gc.rs::tests::false_retention_reproducer_precise_marker_drops_aliased_address`) is the load-bearing precision proof. |

### Workloads

Five workloads, all in `examples/`:

1. **`fib_perf.sigil`** — naïve recursive `fib(20)`. No heap allocations
   for the recursion itself; the only alloc is the `int_to_string` result
   that `IO.println` consumes. Pins the alloc-free perf floor —
   any regression on the precise-marking work would mean the descriptor
   cache lookup costs something even on near-zero-alloc workloads.

2. **`fib_cps_perf.sigil`** — CPS-color `fib(20)` via effect handlers.
   Per-call CPS allocates closure records → ~2³⁰ allocations across the
   recursion. Pins the closure-shape (TAG_CLOSURE, count=2, bitmap=0b10)
   alloc-path cost.

3. **`tree.sigil`** — depth-15 binary tree (65,535 nodes). Each node is
   `Node(Int, Tree, Tree)` → count=3, bitmap≈0b110 (two pointers, one int).
   Pins the multi-pointer shape descriptor-cache cost.

4. **`tree_stress_repeat.sigil`** — 10 rounds of depth-12 tree build +
   fold + drop, ~81,910 allocations across sustained alloc pressure. Pins
   the alloc-path cost under retention churn (Boehm's heap-growth
   threshold may trigger more collections than the single-tree workload).

5. **`descriptor_cache_stress.sigil`** — new workload for this report.
   10 distinct sum-type shapes × 10,000 allocations each = 100,000
   total. Exercises the descriptor cache at its widest shape diversity
   (10 entries; one cache miss per shape, ~99.99% hit rate steady-state).

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

### `descriptor_cache_stress.sigil`

New. 10 distinct sum-type shapes × 10,000 allocations each. Each
shape has a unique `(payload_count, pointer_bitmap)` pair, so the
descriptor cache builds 10 entries (one cache miss per shape) and
serves 99,990 hits.

## Pre-Phase-2 measurements

_Pending — fill from `throughput-data/pre/*.json` once the
workflow has run._

## Post-Phase-2 measurements

_Pending — fill from `throughput-data/post/*.json` once the
workflow has run._

## Deltas

_Pending — paste the per-OS `deltas-<os>.md` artifact below this
heading once the workflow has run._

## Discussion

_Pending — once the data lands, answer per the spec doc's section 6:_

- Did the descriptor-cache lookup add the expected ~ns/alloc?
- Did mark-phase time decrease as expected (precise vs conservative)?
- Did `descriptor_cache_stress` and `tree_stress_repeat` agree on the
  cache-hit-path cost?
- Did the alloc-light workloads (`fib_perf`) show flat / regressed
  perf? (Either is fine; the report just names what happened.)
- Surprises — calls out anything the plan body's hypothesis missed.

## CI implications

_Pending — does `fib_cps_perf` still pass the 500ms (aarch64) /
50ms (x86_64) Plan B Task 60 gate? If not, file a follow-up plan to
adjust the gate (don't adjust here)._

## Stability / caveats

- **Boehm version drift between checkpoints.** Both checkpoints use
  the system libgc, but if the CI image's libgc version changes
  between runs of the workflow, the data is no longer comparable
  across reports. Mitigation: each workflow run records the libgc
  version it observed (`pkg-config --modversion bdw-gc`) and the
  report should record it alongside the SHAs.
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
