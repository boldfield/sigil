# Plan E2 Phase 2 — Static descriptor table (follow-up)

**Status:** code shipped 2026-05-15; measurement run **TODO(operator)**
via `.github/workflows/throughput-report.yml` against
`pre_sha=4f7ec86c52c4aa7335571c0be5e7e771e766c0ad` (Plan E2 Phase 2 baseline
with the descriptor cache present).

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

Expected outcome: `descriptor_cache_stress` ubuntu wall_ms drops from
~170 ms toward ~140 ms (the pre-Phase-2 baseline); macos drops from
~130 ms toward ~70 ms. Other workloads should be flat or slightly
faster (less per-alloc overhead). `precise_walker_ns` is unchanged
(Phase 3 cost is independent).

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

## Per-OS tables — TODO(operator)

Trigger `.github/workflows/throughput-report.yml` via the Actions UI on
the `static-descriptor-table` branch with:

- `pre_sha = 4f7ec86c52c4aa7335571c0be5e7e771e766c0ad`
  (Plan E2 Phase 2 baseline, descriptor cache present).
- `runs = 5`.
- `heap_budget_kb = ""` (default Boehm heuristic).
- `force_gc_every_n_allocs = ""` (no injection).

Expected ~20 min per OS lane. Download per-OS artifacts and replace
the placeholders below.

### ubuntu-24.04 — TODO

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress` | TODO | TODO | TODO | TODO |
| `tree_stress_repeat_large` | TODO | TODO | TODO | TODO |
| `tree_stress_repeat` | TODO | TODO | TODO | TODO |
| `tree` | TODO | TODO | TODO | TODO |
| `fib_cps_perf` | TODO | TODO | TODO | TODO |
| `fib_perf` | TODO | TODO | TODO | TODO |
| `deep_sync_call_chain` | TODO | TODO | TODO | TODO |

### macos-14 — TODO

| Workload | pre wall_ms | post wall_ms | Δms | Δ% |
|---|---|---|---|---|
| `descriptor_cache_stress` | TODO | TODO | TODO | TODO |
| `tree_stress_repeat_large` | TODO | TODO | TODO | TODO |
| `tree_stress_repeat` | TODO | TODO | TODO | TODO |
| `tree` | TODO | TODO | TODO | TODO |
| `fib_cps_perf` | TODO | TODO | TODO | TODO |
| `fib_perf` | TODO | TODO | TODO | TODO |
| `deep_sync_call_chain` | TODO | TODO | TODO | TODO |

## Decomposition (per workload, both OSes — TODO)

- **savings** = `pre wall_ms - post wall_ms` (descriptor-cache cycles
  removed from the alloc hot path).
- **net** = `post wall_ms - pre wall_ms` (negative = improvement).

`descriptor_cache_stress` is the headline target. Other workloads
should be flat (within IQR) or slightly improved (less per-alloc
overhead is a global win).

## Methodology caveats

- **Pre-checkpoint patch.** Plan E2 Phase 2 baseline at
  `4f7ec86` does NOT have `sigil_init_shapes`. The post-side calls it
  from the main shim; the pre side does not. This is the apples-to-
  apples-ish baseline: pre-Phase-2 (which had no descriptor cache and
  no static table) would also lack the call. The throughput workflow's
  pre-side cherry-pick discipline (matching PR #176/#177's pattern)
  keeps the rest of the diff clean — sources that newly `use` an std
  module are sed-stripped on the pre side per PR #173's qualified-
  imports pattern.

- **Asymmetric counters.** The post side has counters absent from the
  pre side (none added by this PR — `SHAPE_DESCRIPTORS` is internal
  state, not a counter). The Phase 3 follow-up's
  `precise_walker_ns` / `forced_gc_count` asymmetry pattern does not
  apply here.

- **Boehm descriptor count.** First-allocation latency may be very
  slightly higher: the codegen-emitted table is built upfront at
  `sigil_init_shapes` time, so all `GC_make_descriptor` calls happen
  in a tight loop at startup rather than amortised across the first
  alloc per shape. Boehm documents these calls as "called once per
  type, not once per allocation" so the upfront cost is the same
  total work, just front-loaded.

- **OS-level ms resolution.** Same caveat as PR #176/#177: workloads
  that complete in tens of milliseconds have wall_ms IQR overlapping
  the expected delta. `descriptor_cache_stress` runs ~170 ms ubuntu /
  ~130 ms macos at Phase 2; the expected ~30 ms / ~60 ms reduction is
  well above noise floor. Smaller workloads' deltas may be
  inconclusive at this resolution.

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
