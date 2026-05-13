# Plan E2 — v2 precise GC + real Cranelift stackmaps

Tracks Plan E2's execution against
`boldfield/designs/in-progress/2026-05-08-sigil-v2-precise-gc.md`
(moves to `done/` on Phase 3 completion). Plan E1 (runtime profile-
data emission surface) merged as PR #148; Plan E2 builds on it and
on PR #151's Cranelift 0.131 stackmap API spike.

Plan E2 has three sequential phases — each ships independently with
its own acceptance gate. Per-task PRs (the cadence default), bundled
or split per task scope.

## Phase 1 — Cranelift stackmaps (real v1 content)

### Task 1 — Cranelift stackmap API spike

- status: **done** (PR #151, squash-merge `86069a7`)
- deliverable: `compiler/docs/cranelift-stackmap-spike.md` +
  `compiler/tests/cranelift_stackmap_spike.rs` (2 integration tests:
  `value_variant_flag_filters_live_set_at_safepoint`,
  `var_variant_emits_stackmap_for_phi_confluence`).
- escalation: none — Cranelift 0.131.0 has every capability Plan E2
  needs. `=0.131.0` exact pin stays.

### Task 2 — Mark GC refs in Sigil codegen

Plan body lists three categories: (1) alloc returns, (2) heap-pointer
loads, (3) "phi confluences" (block-args in sigil since codegen uses
pure SSA + block-args, not Variables). Shipped in two tranches:

- **Task 2a** — Category 1 (alloc returns). status: **done** (PR #156,
  squash-merge `5755e22`). 62 marked sites, verified against
  source-of-truth grep on `runtime/src/**/*.rs` for `pub extern "C"
  fn sigil_* -> *mut u8`. Helper `lower_alloc_call` landed at
  `compiler/src/codegen.rs` (used at one representative site,
  `float_add`); other 61 sites surgical pending Task 2b helper
  refactor.
- **Task 2b** — Categories 2 (heap-pointer loads) + 3 (block-arg
  confluences) + helper rollout to all 62 alloc sites. status:
  **in PR #159 review**. Coverage:
  - **Cat 1 helper rollout**: complete. All 62 alloc sites funnel
    through `lower_alloc_call`. The only `declare_value_needs_stack_map`
    site outside the helper is inside the helper itself.
  - **Cat 2 (heap-pointer loads)**:
    - `lower_closure_env_load_from` (renamed from `lower_closure_env_load`)
      is now the centralised path for closure-env captures from EITHER
      `self.closure_ptr` OR a `synth_closure_ptr` argument.
    - `load_field_value` + `Pattern::Tuple` arm flag by `field_ty` /
      `elem_ty`.
    - 41 named heap-pointer loads flagged (24 in the first sweep + 17
      surfaced by PR #159 review M1).
    - `sigil_ref_deref` result flagged when T is heap-bearing.
    - `lower_heap_pointer_load` helper added — every new heap-pointer
      load must funnel through it. Exercised at one representative
      site; rolling out to existing sites is mechanical follow-up
      (see "Open follow-ups" below).
  - **Cat 3 (block-arg confluences)**:
    - Sync user-fn entry-block user-args flagged via
      `flag_heap_pointer_user_args`.
    - 14 `let closure_ptr = block_params[0]` / `synth_closure_ptr`
      extractions at Cps/synth-fn entries flagged (PR #159 review M2).
      Closure_ptr at block_params[0] is always a heap pointer.
    - 4 high-confidence merge-block params flagged (NextStep merges +
      Option[Char] merges).
  - **Task 3's closure dependency** (fn-entry block-params for
    tail-callable fns) is now satisfied by the Sync user-fn user-args
    + closure_ptr flagging. Re-audit Task 3 after PR #159 lands.

### Task 3 — Annotate safepoints (audit)

- status: **doc-only closure** (PR #157, this PR)
- deliverable: spike doc updated with the audit findings.
- finding: 2 `return_call*` sites (codegen.rs:19987 direct, 20428
  indirect). Cranelift treats both as non-safepoints; ownership of
  live GC refs transfers to the callee. *No annotation needed at
  either site.* Conclusion contingent on Task 2b's fn-entry
  block-param marking — re-audit after Task 2b lands.
- plan-body test ("stackmap section non-empty after a small program
  compile") covered transitively by PR #151's spike tests + PR #156's
  Task 2a marking integration into real codegen.

### Task 4 — Stackmap section v1 writer

- status: **in PR (Task 4 branch)**
- v1 wire format: per-function blocks. Section header (12B) + per-fn
  header (12B: name_len, record_count, text_offset) + name + per-record
  header (12B: pc_offset, frame_size, entry_count, flags) + 5B entries
  (kind:1 + sp_offset:4). Constants in `sigil-abi::stackmap`.
- v0 writer + `push_placeholder` + `function_code_offset` retired
  entirely (120 dead call sites + `Lowerer.stackmap` field removed).
- Writer integration: post-`define_function` reads
  `ctx.compiled_code().unwrap().buffer.user_stack_maps()` via a single
  helper `define_fn_and_capture_stackmap` used at all 10 codegen sites.
- Per-entry type byte: `STACKMAP_ENTRY_KIND_HEAP_POINTER = 0x01`
  (the only kind v1 emits — Phase 2 may add scalar kinds for the
  cross-check). Cranelift's `(ir::Type, sp_offset)` discarded in favour
  of the heap-pointer-only contract (all Sigil-side flags are heap
  pointers via `lower_alloc_call` / `lower_heap_pointer_load`).
- Runtime parser `runtime/src/stackmap.rs` updated to v1; v0 sections
  rejected as stale build artifacts (`UnknownVersion(0)`).
- **G1 verification test lands**: `compiler/tests/e2e.rs` →
  `stackmap_section_parses_v1_with_real_safepoints` compiles
  `examples/choose_demo.sigil`, parses the `__SIGIL,__stackmaps`
  section, asserts ≥1 fn block + ≥1 safepoint record + every entry
  kind is `STACKMAP_ENTRY_KIND_HEAP_POINTER`. Bring-up measurement:
  7 fn blocks, 8 records, 9 total entries.
- **IR-level unit test lands** (per plan-body's "hand-build a known
  IR" spec): `compiler/tests/stackmap_v1_round_trip.rs` →
  `hand_built_alloc_and_call_round_trips_through_v1_section` builds
  a one-fn IR (alloc → flag → consume call → return live ptr) via
  `FunctionBuilder` + `ObjectModule::define_function`, captures
  `user_stack_maps` into `StackMapV1Builder`, serializes, parses
  back via `sigil_runtime::stackmap::parse_section`, and asserts
  exactly one fn block / ≥1 record / ≥1 heap-pointer entry.
- **C-shim handler-frame allocs flagged**: 5 raw
  `builder.ins().call(frame_new_ref, ...)` sites in `emit_main_shim`
  (ArithError / IO / Env / Fs / Process handler frames) +
  `Expr::Handle` lowering's `self.handler_frame_new_ref` call site
  refactored to use `lower_alloc_call` so their results are
  `declare_value_needs_stack_map`'d (PR #156 / #159 missed these
  because they're not at user-code alloc sites). Closes a Task 2c
  residual surfaced by the G1 bring-up.

### Task 5 — Runtime stackmap reader + cross-check

- status: **in PR** (Task 5 branch)
- `runtime/src/stackmap.rs` v1 reader. Section locator: ELF uses
  `dlsym("__start_sigil_stackmaps")` / `dlsym("__stop_sigil_stackmaps")`
  (no extern statics — avoids undef-symbol link error in unit-test
  binaries that don't link compiler-emitted code); Mach-O uses
  `getsectiondata(_dyld_get_image_header(0), "__SIGIL", "__stackmaps",
  ...)`. Per-fn symbol bases resolved via `dlsym(symbol_name)`.
- ELF section name renamed from `.sigil_stackmaps` to `sigil_stackmaps`
  (no leading dot) so the GNU linker auto-generates the start/stop
  symbols (requires valid C-identifier section name).
- Public API: `init_index() -> Option<&'static StackmapIndex>`,
  `StackmapIndex::lookup(pc) -> Option<&ParsedRecord>`, `walk_for_gc()
  -> Vec<RootLocation>`.
- fp-chain walker (`walk_for_gc`) reads x86_64 `rbp` / aarch64 `x29`
  via inline asm, walks the chain to frames whose return-PC matches a
  known safepoint, yields absolute addresses `(fp - frame_size +
  entry.sp_offset)` for every entry.
- Cross-check harness in `runtime/src/stackmap_xcheck.rs`. Activated
  by `SIGIL_GC_CROSS_CHECK=1`; called from `sigil_alloc`. Asserts
  every precise root address lies in `[sp, stack_base)` (B ⊆ A) AND
  the value at the address is heap-pointer-shaped (aligned, ≥ 0x1000).
  Divergence aborts via `std::process::abort` with a stderr
  diagnostic. Production paths skip via cached env-var atomic; cost
  on the fast path is a single relaxed load + branch.
- E2E cross-check tests in `compiler/tests/e2e.rs`:
  `cross_check_hello_runs_cleanly`,
  `cross_check_option_demo_runs_cleanly`,
  `cross_check_choose_demo_runs_cleanly`, and
  `cross_check_tree_stress_runs_cleanly` (the last is the plan-body
  stress test — `tree.sigil` allocates 65,535 nodes, so every alloc
  fires the cross-check; exit 0 = zero divergence across all 65,535
  safepoints).
- Documented in `compiler/docs/stackmap-v1.md`.

## Phase 2 — Precise heap marking

### Task 6 — Boehm precise-mode API spike

- status: pending

### Task 7 — Descriptor cache

- status: pending

### Task 8 — `sigil_alloc` registers precise descriptors

- status: pending

### Task 9 — Drop conservative heap scan

- status: pending — Phase 2 ship gate (false-retention reproducer).

## Phase 3 — Precise stack roots

### Task 10 — Per-thread root config spike

- status: pending

### Task 11 — Thread registration discriminator

- status: pending — depends on PR #148's drainer thread spawn site.

### Task 12 — Drop conservative stack scan on Sigil threads

- status: pending — Phase 3 ship gate.

## Deviations

- **Task 3** — plan-body's "stackmap section non-empty" test was
  covered transitively rather than via a fresh test (see the Task 3
  entry above).

- **Task 4 wire format** — diverges from plan-body's literal spec.
  Plan body says: `record = (PC offset, frame size, live-value
  bitmap, register set)`. Shipped: per-record header
  (`pc_offset:4 | frame_size:4 | entry_count:2 | flags:2`) + variable
  per-entry list (`kind:1 | sp_offset:4`). Rationale: PR #151's
  Cranelift 0.131 spike doc found Cranelift exposes
  `(ir::Type, sp_offset)` per entry and recommended carrying the
  per-entry kind byte instead of a packed bitmap (free from
  Cranelift, useful for Phase 2's bitmap-vs-typecheck cross-check).
  No "register set" field — Cranelift's `UserStackMap` does not
  expose register-resident GC refs; all entries are spilled to the
  frame and addressed via `sp_offset`. Per-function blocks (not in
  plan body) added because the runtime needs to map PC ranges to
  the symbol owning them; without grouping the flat record stream
  cannot be resolved against `dlsym`-style base lookups (Task 5).

- **Task 4 unit-test interpretation** — plan body's "hand-build a
  known IR with one alloc + one fn call; emit; parse the section
  back" lands as an integration test
  (`compiler/tests/stackmap_v1_round_trip.rs`) that exercises the
  ObjectModule → user_stack_maps → StackMapV1Builder → serialize →
  parse_section path. The G1 e2e test
  (`compiler/tests/e2e.rs::stackmap_section_parses_v1_with_real_safepoints`)
  covers the same wire-format contract through the full
  `emit_object` + object-file path on a real Sigil program
  (`choose_demo.sigil`). Two layers of coverage on the same shape.

## Open follow-ups

None — Task 2b's scope is closed in PR #159. Specifically:

- **`lower_heap_pointer_load` helper rollout**: complete. The
  bulk-refactor sweep in PR #159's final commit migrated 40+1 surgical
  heap-pointer load sites to use the helper. Only the helper itself
  contains an internal `declare_value_needs_stack_map(ptr)` call —
  every external surgical pattern is gone. A future contributor
  cannot add an unmarked heap-pointer load via the established
  pattern; the helper is the only path.

- **7 type-aware merge-block params**: complete. Each of codegen.rs's
  7 `append_block_param(*, pointer_ty / result_ty / handler_overall_ty)`
  sites that needed Sigil-Ty threading now uses
  `expr_is_known_heap(arms[0].body, &preview)` (or the body Expr at
  the no-return-arm Handle site) gated by `result_ty == pointer_ty`.
  The four sites with unambiguous heap merges (NextStep / NextStep /
  Option[Char] / Option[Char]) flag unconditionally; the three sites
  where the merge is `arms[0].body`-dependent (lower_match cont in
  Sync + Cps + Cps-match-to-next-step) and the two handler sites
  (return-arm + no-return-arm) gate on the predicate.

  `expr_is_known_heap` is conservative on genuinely ambiguous AST
  shapes (returns `false`): non-ctor `Ident`, `Perform`, `Handle`,
  `Lambda`, `Cast`, `TupleLit`, `Try`. Phase 3 acceptance gating
  re-verifies; if any such ambiguous case becomes load-bearing the
  helper grows additional shapes.

## Open dependencies

- **Task 3 → Task 2b** — Task 3's no-annotation conclusion at
  `return_call*` sites depends on Task 2b flagging fn-entry
  block-params of pointer type on tail-callable fns. **Satisfied
  by PR #159** (`flag_heap_pointer_user_args` for Sync user-fn
  user-args + closure_ptr-at-block_params[0] flagging for Cps/synth
  fns). Re-audit Task 3 after PR #159 lands.
- **Task 4 → G1** — Task 4 lands the v1 section writer + reader path;
  G1's end-to-end verification test ("compile alloc-bearing program,
  assert section has entries") lands with Task 4.
- **Task 11 → Plan E1's drainer spawn site** — already in
  `runtime/src/profile/cpu.rs` since PR #148.
