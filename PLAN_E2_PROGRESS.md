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
  **pending**. **Task 3's closure depends on Task 2b** flagging
  fn-entry block-params of pointer type for any fn callable via
  `return_call` / `return_call_indirect`. The plan body's
  category-2 / category-3 wording covers this, but the contract is
  tighter than "block-arg confluences in general" — fn-entry params
  on tail-callable fns are load-bearing for soundness at the
  tail-call boundary.

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

- status: pending
- bumps `STACKMAP_VERSION_V1` to authoritative; section carries real
  PC-keyed entries from `code.buffer.user_stack_maps()`.
- recommendation from PR #151's spike doc: extend the v1 record with
  a per-entry type byte (free from Cranelift; useful for Phase 2's
  bitmap-vs-typecheck cross-check).
- **G1 verification test lands here** per PR #156's deferral.

### Task 5 — Runtime stackmap reader + cross-check

- status: pending
- `runtime/src/stackmap.rs` v1 reader + `SIGIL_GC_CROSS_CHECK=1`
  harness in `runtime/src/gc.rs`. Phase 1 ship gate.

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

None recorded yet. Plan-body Task 3's "stackmap section non-empty"
test was covered transitively rather than via a fresh test — see
the Task 3 entry above.

## Open dependencies

- **Task 3 → Task 2b** — Task 3's no-annotation conclusion at
  `return_call*` sites depends on Task 2b flagging fn-entry
  block-params of pointer type on tail-callable fns. Re-audit Task 3
  after Task 2b lands; if Task 2b's coverage is incomplete, this
  becomes a real soundness gap at Phase 3 (when conservative stack
  scan is dropped).
- **Task 4 → G1** — Task 4 lands the v1 section writer + reader path;
  G1's end-to-end verification test ("compile alloc-bearing program,
  assert section has entries") lands with Task 4.
- **Task 11 → Plan E1's drainer spawn site** — already in
  `runtime/src/profile/cpu.rs` since PR #148.
