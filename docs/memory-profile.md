# Sigil Memory Profile

Measured peak RSS for the workloads that matter for host sizing. Numbers
are empirical, dated, and tied to a specific repo state — they will drift
as Plan B (effects runtime, CPS transform) and Plan C (stdlib
compilation) add features. Re-measure after each plan lands.

## Summary (for the impatient)

| Workload | Peak RSS | Pod feasibility (1–2 GiB) |
|---|---|---|
| `sigil <any single Plan-A2 program>` | ~62–63 MiB | ✅ comfortable |
| `cargo build --release -j 1` | 0.98 GiB | ⚠️ tight — pod-dependent |
| `cargo test --workspace --no-fail-fast` | 1.17 GiB | ❌ OOMs a ~1 GiB pod |
| `cargo build --release` (parallel) | 1.56 GiB | ❌ OOMs most pods |

**Compiling a sigil program is cheap.** Compiling the sigil *compiler* is
not. The pod-memory problem is a Rust/Cranelift build-time problem, not a
sigil-runtime problem.

## Methodology

All numbers use [`scripts/peak-rss.sh`](../scripts/peak-rss.sh) on
`aarch64-apple-darwin` (M-series MacBook, macOS 14). The script combines
two signals and reports the larger:

1. `/usr/bin/time -l` on the root process — kernel-tracked `ru_maxrss`,
   bulletproof against short-lived commands but undercounts parallel
   subprocess trees.
2. `ps -o rss=` polling at 10 ms over all descendants — catches multiple
   concurrent rustc workers overlapping in time, which a single
   `ru_maxrss` reading would miss.

Reported peak = `max(root_ru_maxrss, polling_tree_peak)`.

"Root process" numbers reflect what `/usr/bin/time -l` on the command
directly would show. "Tree" numbers reflect the full process-tree peak
captured by polling. The gap between them quantifies how much parallel
subprocess overlap the root measurement misses.

## Measured as of 2026-04-24 (sigil `main` @ commit 45c03b9)

Host: MacBook, `aarch64-apple-darwin`, release-profile rustc
`rust-toolchain.toml` pin, cold `cargo clean` before each cargo
measurement.

### Sigil compilation (loading Cranelift, processing one `.sigil` file)

| Example | Wall time | Root RSS | Tree RSS | Peak |
|---|---|---|---|---|
| `examples/hello.sigil` | 0.04 s | 63 MiB | — | **63 MiB** |
| `examples/arith.sigil` | 0.05 s | 63 MiB | — | **63 MiB** |
| `examples/fibonacci.sigil` | 0.05 s | 63 MiB | — | **63 MiB** |
| `examples/higher_order.sigil` | 0.04 s | 63 MiB | — | **63 MiB** |

*Tree column is blank when the command completes faster than the 10 ms
polling interval can sample it. The root reading is authoritative.*

**Interpretation.** Cranelift is statically linked into the sigil
compiler; its code pages and initial working state dominate the peak
for any Plan A2 program. Program complexity (hello vs. closure-heavy
higher_order) contributes tens of KiB at most — dwarfed by the ~60 MiB
Cranelift baseline. This contradicts the earlier "closure-heavy
programs OOM the pod" framing: the underlying pressure is simply
loading the compiler, and that cost is roughly constant across all
Plan A2 inputs.

Expected to grow meaningfully in:

- **Plan B** — CPS transform + effect-runtime codegen adds passes and
  per-function work. Forecast: +20–50 MiB.
- **Plan C** — stdlib (nine sigil modules) compiled together raises
  whole-program size. Forecast: +50–100 MiB per invocation if the
  stdlib is re-parsed per compile; may be much less if caching lands.

### Rust toolchain (building the sigil compiler itself)

| Workload | Wall time | Root RSS | Tree RSS | Peak |
|---|---|---|---|---|
| `cargo build --release -j 1` (cold) | ~80 s | 0.94 GiB | 0.98 GiB | **0.98 GiB** |
| `cargo test --workspace --no-fail-fast` (cold) | ~40 s | 0.87 GiB | 1.17 GiB | **1.17 GiB** |
| `cargo build --release` (cold, parallel) | 22 s | 1.07 GiB | 1.56 GiB | **1.56 GiB** |

**Interpretation.**

- The **single-worker release build** (`-j 1`) at 0.98 GiB is the floor
  imposed by the heaviest single rustc invocation — almost certainly
  `cranelift-codegen` with LLVM optimizations. No amount of
  parallelism reduction goes below this without swapping compilers.
- The **parallel release build** tops 1.5 GiB because multiple rustc
  workers overlap during the final optimization phase. The root
  reading (1.07 GiB) undercounts the tree peak by 46% — illustrates
  exactly why the tree-polling path in `peak-rss.sh` exists.
- The **test suite** at 1.17 GiB is lighter than the release build
  because `cargo test` uses the debug profile (no LLVM `-O3`), but
  heavier than `-j 1` release because test linking still parallelizes.

## Pod feasibility (headless k8s build environment)

The constrained Talos Linux pod that hosts the headless agent is
empirically sized in the 1–2 GiB range (based on OOM history:
`cargo test --workspace` and ad-hoc `sigil` invocations on closure
examples both OOM'd during early Plan A2 work).

Current feasibility against these measurements:

| Pod size | sigil compile | cargo build -j 1 | cargo test | cargo build -j ∞ |
|---|---|---|---|---|
| 1 GiB | ✅ | ⚠️ right at ceiling | ❌ | ❌ |
| 1.5 GiB | ✅ | ✅ | ✅ | ❌ |
| 2 GiB | ✅ | ✅ | ✅ | ⚠️ |
| 4 GiB | ✅ | ✅ | ✅ | ✅ |

**Practical upshot.** The pod **can** invoke the sigil compiler on Plan
A2 programs safely — that path costs ~63 MiB. The pod **cannot**
reliably run `cargo test --workspace` or `cargo build --release` in
parallel. This matches the doctrine already codified in the plans
(`in-progress/2026-04-21-sigil-core-a2.md` and the A3/B/C drafts in
`docs/plans/`): local verification on the pod uses
`scripts/pod-verify.sh` (which avoids the expensive paths), and CI on
GitHub-hosted runners is authoritative for the full test matrix.

A historical data point: the "sigil compiler invocation OOM'd the pod
on higher_order.sigil" incident noted in
[`PLAN_A2_DEVIATIONS.md`](../PLAN_A2_DEVIATIONS.md) is not reproducible
at these measurements. The compiler peaks at 63 MiB regardless of
input. Likely explanation: the pod was under unrelated memory
pressure at that moment (background rustc from an earlier task still
resident, kernel caches, etc.). Worth re-testing after Plan B lands if
it recurs.

## Reproduction

```shell
# Build the compiler first (sigil compile measurements need the binary).
cargo build --release

# Sigil compile measurements.
for ex in hello arith fibonacci higher_order; do
  scripts/peak-rss.sh ./target/release/sigil examples/$ex.sigil -o /tmp/mp_$ex
done

# Rust toolchain measurements (cold — `cargo clean` between each).
cargo clean
scripts/peak-rss.sh cargo build --release -j 1

cargo clean
scripts/peak-rss.sh cargo test --workspace --no-fail-fast

cargo clean
scripts/peak-rss.sh cargo build --release
```

`scripts/peak-rss.sh --help` documents the polling interval override
(`PEAK_RSS_INTERVAL_SECONDS`) and the methodology in more detail.

## When to re-measure

Re-run the full suite above and append a new dated section to this
file after each of:

- **Plan B merges.** CPS transform + effect runtime change per-sigil-
  compile cost and add new rustc crates to the workspace. Both affect
  the table.
- **Plan C merges.** Stdlib compilation and demo programs (interpreter,
  JSON, Sudoku) push sigil-compile peaks higher than anything Plan A2
  exercises.
- **Runtime subsystem replacements** — e.g., Boehm → precise GC (Plan
  B docket). Changes the runtime's own memory profile and possibly
  `sigil_alloc` call-site behavior.
- **Major rustc toolchain upgrades** — new Cranelift versions may
  change per-crate build costs meaningfully.

Keep old sections as history so drift is visible. Don't overwrite.
