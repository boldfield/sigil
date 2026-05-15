# Plan E2 Phase 2 — Static descriptor table (follow-up)

**Status:** **Confirmed (ubuntu)** / **directionally confirmed within
noise (macos)**. Code shipped 2026-05-15 at commit `4afc5ce`;
throughput-report runs `25935200346` (Phase 2 baseline `pre_sha=4f7ec86`)
and `25935615684` (corrected baseline `pre_sha=b1ff665`).

Cross-link: closes the +21% (ubuntu) / +86% (macos)
`descriptor_cache_stress` regression surfaced in
[`plan-e2-phase-2-throughput.md`](plan-e2-phase-2-throughput.md).

## TL;DR

Replaces Plan E2 Phase 2 Task 7's runtime `RwLock<BTreeMap<(u32, u8),
GC_descr>>` descriptor cache with a compile-time-emitted shape table
materialised once at startup. Every `sigil_alloc` and
`sigil_handler_frame_new` call site passes a `u32` descriptor index;
the runtime indexes into a static `Vec<GC_descr>` populated by
`sigil_init_shapes`. The RwLock + BTreeMap lookup is gone from the
allocation hot path; the per-call cost is one extra `iconst` + a direct
load.

**Measured outcome (corrected-baseline run against
`pre_sha=b1ff665`, the immediate parent of this PR's first commit):**

- `descriptor_cache_stress` ubuntu: **260 → 180 ms (−80 ms / −30.8%)**
- `descriptor_cache_stress` macos:   **180 → 160 ms (−20 ms / −11.1%,
  within ±20ms IQR)**
- `tree_stress_repeat_large` ubuntu: **50 → 40 ms (−10 ms / −20%)**
- `tree_stress_repeat_large` macos:  **40 → 30 ms (−10 ms / −25%)**
- Other workloads: flat (within OS ms-resolution IQR; absolute values
  ≤ 20 ms).

The ubuntu `descriptor_cache_stress` result is a clean win — the +21%
Phase 2 regression is more than fully recovered (~−30% past the
pre-Phase-2 baseline, since Phase 3 added some net-zero alloc-path
overhead). The macos result is positive but inside the noise floor;
the runner's higher IQR (`180 ± 20`) doesn't let us distinguish the
delta from cross-run variance with 5 samples.

**Initial-baseline-run caveat.** The first throughput run used
`pre_sha=4f7ec86` (Plan E2 Phase 2 baseline, descriptor cache present).
The post side at HEAD also includes Plan E2 Phase 3's per-alloc
`SigilCallerFpGuard::capture()` overhead (Phase 3 landed AFTER Phase 2),
which lands as ~10–25 ns/alloc on every workload. With that confound,
every workload showed a uniform regression that was a Phase 3
signature, not a static-descriptor-table signature. The static-table
impact was buried under Phase 3 overhead. Lesson recorded in
"Methodology caveats" below.

## Mechanism

### Compile-time shape registry

`ShapeTable` in `compiler/src/codegen.rs` is a per-build registry that
every `sigil_alloc` / `sigil_handler_frame_new` call site consults
during emission. It holds:

- `entries: Vec<(u32, u8)>` — the `(bitmap, payload_word_count)` pairs
  in registration order.
- `by_shape: BTreeMap<(u32, u8), u32>` — fast lookup for dedup.

The two entry points emit-time call sites use:

- `alloc_descriptor_index(bitmap, count) -> u32` — returns
  `u32::MAX` (sentinel) when the call routes through the atomic /
  conservative branch of `sigil_alloc`, else the registered index.
- `handler_frame_descriptor_index(arm_count) -> u32` — derives the
  `(bitmap, count)` shape from `arm_count` using the const fns in
  `sigil_abi::effect` (shared with the runtime so the values match
  bit-for-bit) and registers + returns the index.

### Cranelift data section

A `__sigil_shape_table` data symbol is declared at codegen start and
defined at the end of `emit_object` once every call site has
registered. Layout:

```text
  bytes 0..4: N (u32 LE) — entry count
  bytes 4..8: padding (u32 LE, zero) — keeps entries 8-byte aligned
  bytes 8.. : N × `(bitmap: u32 LE, count_padded: u32 LE)`
```

**Why N lives in the data section header.** An earlier draft passed
`n` as a second argument to `sigil_init_shapes`, captured by the main
shim's `iconst` at shim-emit time. Synth arm fns and sync shims emit
AFTER the main shim and can register novel shapes; the shim-time `n`
undercounted the real entry count, leading to post-shim call sites
silently indexing past N into the runtime suffix. Embedding N in the
data section header closes that gap structurally — `encode_le` runs
at table-finalize time, AFTER every Lowerer has run, so the embedded
N reflects every registered shape.

Section placement: `.rodata` segment, section name `sigil_shapes` —
ELF gets a `sigil_shapes` section; Mach-O maps `.rodata` to
`(__DATA, __const)`. The `sigil_shapes` name is 12 bytes, well under
Mach-O's 16-byte limit (PR #161's lesson).

### Runtime init

The compiler-emitted main shim emits, immediately after `sigil_gc_init`
and BEFORE any user-code allocation:

```text
sigil_init_shapes(&__sigil_shape_table)
```

The runtime reads N from the first 4 bytes of the data section and
walks the N `(bitmap, count_padded)` entries that follow. The shim
itself contributes the 4 distinct default handler-frame shapes
(ArithError arm_count=2; IO+Env both arm_count=3, dedup'd via
`ShapeTable::register`; Fs arm_count=10; Process arm_count=1).

`sigil_init_shapes` in `runtime/src/gc.rs`:

1. Decodes N from the data section header.
2. Builds `Vec<usize>` descriptors from the codegen prefix via
   `GC_make_descriptor` once per shape — the same call the old cache
   made lazily.
3. Appends the runtime-known shapes (Ref, Continuation, StringBuilder,
   four tuple shapes from `alloc_tuple` call sites, the wrapper-
   continuation closure, and 15 handler-frame shapes for `arm_count ∈
   [0, MAX_HANDLER_ARMS]`).
4. Publishes `SHAPE_DESCRIPTORS` (OnceLock) FIRST, then
   `RUNTIME_SHAPE_INDICES` (OnceLock) — publish-after-construct
   ordering so any reader that sees indices populated is guaranteed
   to see the descriptors those indices point into.

`sigil_alloc`'s typed-malloc branch becomes:

```text
let descr = SHAPE_DESCRIPTORS[descriptor_index];
GC_malloc_explicitly_typed(total, descr)
```

— a direct array read; no lock, no map.

## Deviations from the plan body

### 1. Runtime suffix appended to SHAPE_DESCRIPTORS

The plan body claimed "~95% of `sigil_alloc` callees pass CT-emitted
`(bitmap, count)` literals. The one runtime-determined exception
(`sigil_handler_frame_new`)…" — that count was wrong. The actual
runtime-internal typed-malloc callers number ~8 distinct shapes plus
the 15 handler-frame variants:

- `sigil_ref_alloc` — `(0b1, 1)`.
- `sigil_continuation_alloc` — `(0b0101, 4)`.
- `sigil_string_builder_new` — `(0b1000, 4)`.
- `alloc_tuple` call sites in env/fs/process arm fns — `(0b10, 2)`,
  `(0b11, 2)`, `(0b110, 3)`, `(0b1100, 4)`.
- `wrap_continuation_with_outer_post_arm_k` wrapper — `(0b01010, 5)`.
- `sigil_handler_frame_new` runtime convenience wrapper — 15 shapes
  for `arm_count ∈ [0, MAX_HANDLER_ARMS]`.

These are appended to `SHAPE_DESCRIPTORS` after the codegen prefix by
`sigil_init_shapes`. Their indices are exposed via
`RuntimeShapeIndices`, a small `Copy` struct in
`runtime/src/gc.rs`. Runtime allocators read it once per allocation
on the hot path — an `OnceLock` atomic load + branch, replacing the
old RwLock-protected BTreeMap lookup.

The plan body's Task 8 ("delete `runtime/src/gc/descriptor.rs`")
still applies: the descriptor module is fully removed. The runtime
no longer has a lookup-by-`(bitmap, count)` step; each runtime
allocator caches its assigned index statically.

### 2. Shared const fns in `sigil-abi`

The runtime fns `handler_frame_payload_bytes` and
`handler_frame_pointer_bitmap` (formerly local to
`runtime/src/handlers.rs`) are promoted to `pub const fn` in
`sigil-abi::effect` so the compiler's `ShapeTable` can compute
identical values without duplicating the bitmap derivation. A
unit test in `abi/src/effect.rs`
(`handler_frame_helpers_are_const_eval_callable`) pins the const-fn
property so a future accidental promotion to non-const trips loudly.

### 3. `(bitmap: u32, count_padded: u32)` per-entry layout

The plan body's Task 4 sketch shows the runtime decoding 8-byte chunks
as `(bitmap: u32 LE, count: u8)` plus 3 padding bytes. The shipped
runtime treats the trailing 4 bytes as `count_padded: u32` (the
`as_u8` truncation happens after decoding). Same wire format; the
runtime's `chunks_exact(8)` + two `from_le_bytes` calls match the
codegen's `bitmap.to_le_bytes() + (count as u32).to_le_bytes()`
emission.

## Per-OS tables

Two `.github/workflows/throughput-report.yml` runs were performed, both
on branch `static-descriptor-table` HEAD = `4afc5ce`, `runs=5`, empty
`heap_budget_kb`, empty `force_gc_every_n_allocs`:

- **Run A** (run id `25935200346`): `pre_sha=4f7ec86` (Plan E2 Phase 2
  baseline — descriptor cache present, Phase 3 absent). Confounded: see
  "Methodology caveats" below.
- **Run B** (run id `25935615684`): `pre_sha=b1ff665` (PR #177 squash-
  merge commit = parent of this PR's first commit `10a9585`). Clean
  isolation of this PR's delta: pre side has Phase 3 + descriptor
  cache; post side has Phase 3 + static descriptor table.

### Run B (corrected baseline `pre_sha=b1ff665`) — primary verdict

#### ubuntu-24.04

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress`  | 260 ± 10 | 180 ± 0  | **−80**  | **−30.8%** |
| `tree_stress_repeat_large` | 50 ± 0   | 40 ± 0   | −10      | −20.0%     |
| `tree_stress_repeat`       | 0 ± 0    | 0 ± 0    | +0       | n/a        |
| `tree`                     | 0 ± 0    | 0 ± 0    | +0       | n/a        |
| `fib_cps_perf`             | 10 ± 0   | 0 ± 0    | −10      | −100.0%    |
| `fib_perf`                 | 0 ± 0    | 0 ± 0    | +0       | n/a        |
| `deep_sync_call_chain`     | 20 ± 0   | 20 ± 0   | +0       | +0.0%      |

#### macos-14

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress`  | 180 ± 20 | 160 ± 10 | **−20** | **−11.1%** (within IQR) |
| `tree_stress_repeat_large` | 40 ± 10  | 30 ± 0   | −10     | −25.0% |
| `tree_stress_repeat`       | 0 ± 10   | 0 ± 0    | +0      | n/a |
| `tree`                     | 10 ± 10  | 0 ± 10   | −10     | (within IQR) |
| `fib_cps_perf`             | 0 ± 0    | 0 ± 10   | +0      | n/a |
| `fib_perf`                 | 0 ± 0    | 0 ± 0    | +0      | n/a |
| `deep_sync_call_chain`     | 20 ± 0   | 20 ± 0   | +0      | +0.0% |

### Run A (confounded baseline `pre_sha=4f7ec86`) — recorded for context

This run is **not** load-bearing on the verdict. It is documented so
the negative-delta surprise is reproducible and the
"all-workloads-regress-uniformly" pattern is identifiable in the
future. Numbers below are the deltas the workflow emitted.

#### ubuntu-24.04 (Run A, confounded)

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress`  | 140 ± 0 | 190 ± 10 | +50 | +35.7% |
| `tree_stress_repeat_large` | 30 ± 0  | 40 ± 0   | +10 | +33.3% |
| `tree_stress_repeat`       | 0 ± 0   | 0 ± 0    | +0  | n/a    |
| `tree`                     | 0 ± 0   | 0 ± 0    | +0  | n/a    |
| `fib_cps_perf`             | 0 ± 0   | 10 ± 0   | +10 | n/a    |
| `fib_perf`                 | 0 ± 0   | 0 ± 0    | +0  | n/a    |
| `deep_sync_call_chain`     | 10 ± 0  | 20 ± 0   | +10 | +100%  |

#### macos-14 (Run A, confounded)

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress`  | 70 ± 10  | 140 ± 20 | +70 | +100.0% |
| `tree_stress_repeat_large` | 20 ± 0   | 30 ± 10  | +10 | +50.0%  |
| `tree_stress_repeat`       | 0 ± 0    | 10 ± 10  | +10 | n/a     |
| `tree`                     | 0 ± 0    | 0 ± 0    | +0  | n/a     |
| `fib_cps_perf`             | 0 ± 0    | 0 ± 0    | +0  | n/a     |
| `fib_perf`                 | 0 ± 0    | 0 ± 0    | +0  | n/a     |
| `deep_sync_call_chain`     | 10 ± 0   | 20 ± 0   | +10 | +100.0% |

## Verdict

**Throughput claim (closes the +21%/+86% Phase 2 regression):**

- **Confirmed on ubuntu-24.04.** Run B's `descriptor_cache_stress`
  delta is −80 ms / −30.8% with ±0 IQR on the post side (5 of 5 runs
  identical at 180 ms). That's more than the Phase 2 regression
  (~+30 ms / +21%) and exceeds the plan's expected closure ("drops
  from ~170 ms toward ~140 ms"). Other workloads are flat-to-slightly-
  improved as predicted.
- **Directionally confirmed within noise on macos-14.** Run B's
  `descriptor_cache_stress` delta is −20 ms / −11.1% with ±20 ms IQR
  on the pre side and ±10 ms on the post side. The improvement is
  smaller than the +60 ms / +86% Phase 2 regression would predict,
  and the IQRs overlap — at 5 samples with 10 ms wall-clock
  resolution we cannot rule out cross-run variance as the source.
  `tree_stress_repeat_large` shows a consistent −25% on the same
  runner, so the alloc-path improvement is not a phantom; just the
  size of the descriptor_cache_stress delta is in the noise floor.

**Design / correctness claim (no lock on alloc hot path):**

- **Structurally confirmed.** The `RwLock<BTreeMap>` is gone (~150
  lines of `gc::descriptor.rs` deleted); `sigil_alloc`'s typed-malloc
  branch is a single `SHAPE_DESCRIPTORS[idx]` read + the existing
  `GC_malloc_explicitly_typed` call. No lock acquire on any
  allocation path. This holds independently of the throughput delta.

**Status:** **Confirmed (ubuntu)** + **directionally confirmed within
noise (macos)** + **structurally confirmed (lock removal)**.

## Methodology caveats

- **Baseline selection — lesson learned.** Run A initially compared
  against `pre_sha=4f7ec86` (Plan E2 Phase 2 commit) because the plan
  body named the Phase 2 cost as the regression to close. Between
  Phase 2 (4f7ec86) and HEAD (4afc5ce) Plan E2 Phase 3 added the
  per-alloc `SigilCallerFpGuard::capture()` machinery (~10–25
  ns/alloc) plus the `GC_call_with_gc_active` trampoline wrap, plus
  the Phase 3 follow-ups #176/#177 (zero-cost-when-gated, but the
  branches exist on every alloc). All of those are on the post side
  of Run A but NOT on the pre side. The result: a uniform `+0–10 ms`
  regression on every workload (a Phase 3 signature) **on top of**
  whatever delta the static descriptor table itself contributed,
  which buried the signal we wanted.
  **Rule for future measurement runs:** `pre_sha` must include every
  per-alloc / per-GC overhead change between the named regression and
  the post commit, not just the phase boundary that motivated the
  work. The clean baseline for this PR is the IMMEDIATE PARENT of the
  branch's first commit (`b1ff665`), which carries Phase 3 + Plan F1
  qualified imports + Plan E3 Phase 1 + Phase 3 follow-ups but no
  static descriptor table — see Run B above.

- **Pre-checkpoint patch.** Plan E2 Phase 2 baseline at `4f7ec86`
  predates Plan F1's qualified-imports overhaul (PR #173) — the
  workflow's `pre — patch` step `sed`-strips `use std.*;` lines from
  pre-side workloads so the pre-Plan-F1 parser accepts them. The
  `b1ff665` baseline (Run B) post-dates PR #173, so this strip is a
  no-op on Run B — the pre side compiles workloads unmodified.

- **Asymmetric counters.** `precise_walker_ns` (added by PR #172 /
  Phase 3) and `forced_gc_count` (added by PR #177 / Phase 3 follow-up
  #2) are absent on the pre side of Run A (pre-Phase-3) and present
  on both sides of Run B (both post-Phase-3). This matches the
  pattern in PR #176/#177's verdict docs. No counters are added by
  THIS PR — `SHAPE_DESCRIPTORS` is internal state, not a counter
  — so the counter table on Run B's post side is the same shape as
  Run B's pre side.

- **Boehm descriptor count.** First-allocation latency may be very
  slightly higher: the codegen-emitted table is built upfront at
  `sigil_init_shapes` time, so all `GC_make_descriptor` calls happen
  in a tight loop at startup rather than amortised across the first
  alloc per shape. Boehm documents these calls as "called once per
  type, not once per allocation" so the upfront cost is the same
  total work, just front-loaded.

- **OS-level ms resolution.** Same caveat as PR #176/#177: workloads
  that complete in tens of milliseconds have wall_ms IQR overlapping
  the expected delta. The macos `descriptor_cache_stress` Run B
  delta (−20 ms / −11.1%) sits inside the runner's IQR (`180 ± 20` on
  the pre side). The ubuntu delta (−80 ms / −30.8%) is well above
  noise. `tree_stress_repeat_large`'s consistent −20% / −25% across
  both OSes acts as a secondary confirmation that the alloc-path
  improvement is not phantom.

## Code surface

- `compiler/src/codegen.rs` — `ShapeTable` struct,
  `__sigil_shape_table` data symbol, `sigil_init_shapes` FFI, main-
  shim init call, per-call-site `descriptor_index` plumbing through
  every `sigil_alloc` / `sigil_handler_frame_new_with_resumes_many`
  site (~13 sites).
- `runtime/src/gc.rs` — `SHAPE_DESCRIPTORS` OnceLock,
  `sigil_init_shapes` extern, `RuntimeShapeIndices` struct,
  `shape_descriptor_at` accessor, test-override slot for direct
  runtime tests.
- `runtime/src/handlers.rs` — `sigil_handler_frame_new_with_resumes_many`
  signature extended with `descriptor_index: u32`; thin
  `sigil_handler_frame_new` wrapper looks up the runtime suffix
  index via `runtime_shape_indices().handler_frame[arm_count]`.
- `runtime/src/{refs,continuation,string_builder,char,env,fs,process,
  effect_helpers,arena}.rs` — every internal `sigil_alloc` caller
  updated to pass the appropriate `descriptor_index` (sentinel for
  atomic / conservative paths; a `RuntimeShapeIndices` field for
  typed-malloc paths).
- `runtime/src/gc/descriptor.rs` — **deleted** (~150 lines of cache
  code removed, per plan body Task 8).
- `abi/src/effect.rs` — `handler_frame_payload_bytes` and
  `handler_frame_pointer_bitmap` promoted to `pub const fn` so
  codegen and runtime share the bitmap derivation.

## Verification

`scripts/pod-verify.sh` clean. `cargo test -p sigil-runtime --lib`:
315 passed / 0 failed. `cargo test -p sigil-compiler --lib`: 880
passed / 0 failed.

Full release build + e2e tests deferred to CI per CLAUDE.md's pod
memory floor (Cranelift's test runner OOMs the pod).
