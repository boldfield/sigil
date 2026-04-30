# Plan D — Deviations

Per Plan D commit discipline (mirroring Plan C / Plan B' / Plan B), deviation entries land *before* the implementing commit they describe. Cross-references to prior plans' deviation entries (`PLAN_C_DEVIATIONS.md`, `PLAN_B_PRIME_DEVIATIONS.md`, `PLAN_B_DEVIATIONS.md`) name the closure points each Plan D task addresses.

## 2026-04-30 — [DEVIATION Plan D overview] v2 architectural cluster as Plan D scope; per-stage review checkpoints; per-task PR cadence

**Context.** Plan C closed ~85% of its task ledger at sigil/main `dfcd60b` on 2026-04-30 (PRs #44 / #45 / #46 / #47 squash-merged). The deferred ~15% splits into two coherent units of work: (a) compiler/runtime architectural lifts whose absence forced expressibility constraints across Plan C's stdlib (Tasks 71 Raise, 72 State, 73 Choose) plus one Plan B' Stage-6.8-followup carryover (TLS → packed multi-return), and (b) the Plan C tail-end tasks gated on (a) plus tooling, validation harness, and final polish. Plan D addresses (a); Plan C completion (separately queued) addresses (b).

**Plan D's scope.** Eight numbered tasks across three stages:

- **Stage 11 (foundation lifts)** — Task 111 TLS multi-return, Task 112 wrapper-fn-frame composition fix. Both narrow, well-pinned fixes; the JSON parser smoke target unblocks at Task 112.
- **Stage 12 (type-system surface)** — Tasks 113 tuples, 114 type-parameterized effect rows, 115 per-op generic params, 116 row-polymorphic Fn parameters. Internal ordering: 114 must precede 115 (per-op generics build on effect-decl-level generics); 113 and 116 are independent.
- **Stage 13 (continuation lifts)** — Task 117 first-class continuations (highest-risk; pre-authorized to split into 117a/117b/...), Task 118 conditional/branched k-call. Sudoku smoke target unblocks at Task 118.

Plus Stage 10.5 scaffolding (this file's introduction + `PLAN_D_PROGRESS.md` + ignore-survey + Plan B' carryover #2 tracking) and Task 119 closeout audit.

**Per-stage review checkpoints are explicit acceptance gates.** Stage 11 review signs off on TLS-removal correctness + wrapper-fn-frame depth before Stage 12 begins. Stage 12 review signs off on AST shape consistency + Tasks 71/72 surface-area closure before Stage 13. Stage 13 review signs off on lifted-lambda discipline + arena escape rate (Plan B Task 60 baseline = 0%) + Sudoku smoke. Pattern mirrors Plan B / Plan B' / Plan C review-checkpoint discipline.

**Per-task PR cadence per `feedback_sigil_per_task_pr_cadence.md`.** Default is one task per PR; bundling requires explicit per-session user authorization. Specifically: do not bundle Task 117 (first-class-k) with Task 118 (conditional-k) even though 118 is "small lift on top of 117"; review the heavy lift independently before generalizing.

**Step 117 split-authority (pre-authorized in the plan body).** The executor is pre-authorized to split Task 117 into sub-tasks (117a / 117b / ...) without a re-scope conversation if any of these early-warning signs appear during execution:

1. The diff exceeds Plan B' PR #38 / PR #39 scope before the smoke gate is reachable.
2. More than two distinct test-failure classes surface simultaneously (suggesting two interacting changes that should be sequenced).
3. The lifted-lambda closure-record discipline diverges from the existing N-chain `post_arm_k` substrate.

Splits allocate sub-task numbers (117a / 117b / ...) into `PLAN_D_PROGRESS.md`; per-sub-task PR cadence remains. **Stop and re-scope with the user** is reserved for cases where the split itself is unclear, or where the cluster's architecture appears to require a lift not enumerated in this plan.

**Step 117 performance acceptance gate.** Step 117 must not regress the existing arena escape rate. Re-run the Plan B Task 60 multi-shot driver (currently 0% escape rate); the post-step-117 measurement must remain at 0% for single-shot workloads and must not exceed the existing multi-shot driver's escape-rate ceiling. If the regression is non-zero, **stop and surface to the user** before merging — not just a review observation.

**Out-of-scope items** preserved here for future-session readability: performance floor for downstream demos (Plan C completion Task 82); spec validation harness (Plan C completion Tasks 85/86/87); demo PRs landing on `main` (Plan C completion Tasks 73/80/81); edge-case demo polish; Sync shim emission gating (Plan B' carryover #2 — separate `[CHORE]` commit on `main`); B.5 scope_id per-frame field; Task 78.5 Koka subset import (Plan C completion).

**Closure points (per-task cross-references):**

- **Task 111** (TLS → packed multi-return): closure point in `PLAN_B_PRIME_DEVIATIONS.md` "Stage-6.8-followup architectural carryovers" entry, `LAST_TERMINAL_TAG` / `LAST_TERMINAL_VALUE` thread-local in `runtime/src/`.
- **Task 112** (wrapper-fn-frame composition fix): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 72]` constraint #3 + `PLAN_B_PRIME_DEVIATIONS.md` "Stage-6.8-followup architectural carryovers" entry. `#[ignore]`'d test `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix` at `compiler/tests/e2e.rs:7014` is the discharge.
- **Task 113** (tuples / `Pair[A, B]`): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 72]` constraint #2 (no tuple type / Pair stdlib).
- **Task 114** (type-parameterized effect rows): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 71]` constraint #1 + `[DEVIATION Task 72]` constraint #1 (parser rejects `![Raise[E]]` / `![State[S]]`).
- **Task 115** (per-op generic params): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 71]` constraint #2 (no per-op generic params; `fail`'s return is `Int` placeholder).
- **Task 116** (row-poly Fn parameters): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 71]` constraint #3 + `[DEVIATION Task 72]` constraint #5 (no row-poly Fn parameters; `!e` passthrough deferred).
- **Task 117** (first-class continuations): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 73]` codegen-side gap (b) (k-as-value rejected at `compiler/src/codegen.rs::arm_body_walk`).
- **Task 118** (conditional/branched k-call): closure point in `PLAN_C_DEVIATIONS.md` `[DEVIATION Task 73]` codegen-side gap (c) (conditional/branched k-call rejected at `compiler/src/codegen.rs::arm_body_walk`).

**Implementing commit(s):** Foundation `[HEAD]` (this entry + Stage 10.5 scaffolding) — Tasks 10.5.1-6 commit. Subsequent commits address each task in the order specified by Plan D (`docs/plans/2026-04-30-sigil-plan-d.md` in `boldfield/designs/in-progress/`). Closeout commits at the end of each stage land the prior-stage hash flips per the Plan B / B' / C precedent.

## 2026-04-30 — [DEVIATION Stage 10.5.5] `#[ignore]` survey count diverges from plan estimate (3 actual vs ~12 expected)

**Context.** Plan D's Stage 10.5.5 instructs the executor to pre-survey the `#[ignore]` inventory and partition into (a) Plan D closure-targets, (b) non-architectural test-infrastructure gaps, and (c) other v2-pending tests. The plan body includes the estimate "Expected total: ~12 ignored tests at plan start (verify on execution)."

**What surfaced on execution.** A grep across `compiler/tests/e2e.rs` and `runtime/src/*.rs` at sigil/main `dfcd60b` returned **3 active `#[ignore]` annotations**:

1. `compiler/tests/e2e.rs:6792` — `std_io_read_line_via_piped_stdin_pending_test_infra` — category (b) test-infra; needs piped-stdin test harness; tracked for Plan C completion's Task 78.
2. `compiler/tests/e2e.rs:7014` — `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix` — category (a) Plan D Task 112 closure target.
3. `runtime/src/arena.rs:489` — `arena_overflow_aborts` — category (b) test-infra; abort tests not observable in `cargo test` (need `cargo test -- --ignored` + manual SIGABRT confirmation).

Category (c) is empty.

**Why accepted.** The plan estimate appears to have anticipated more accumulated `#[ignore]` annotations from Plan B / B' / C work than actually persisted. Plan B's Stage 6 cleanup (PR #35) inverted 3 of the previously-`#[ignore]`'d tests (`partial_handler_*`, `..._uses_v_at_narrow_type`, `..._unwinds_helper_at_perform_site`); Plan B''s Stage 6.7 cleanup inverted the Slice C 3-let pinning test; Plan B''s Stage 6.8 cleanup likely inverted additional walker-rejection tests. The pattern is: each closure work surfaces, inverts, lands together — `#[ignore]`'d-test residue across long time spans is rare in this codebase.

**Failure mode.** None. The discrepancy is observational. Plan D Task 119's closeout audit checks the partition in the same shape regardless of count.

**Closure path.** None required. Logged as transparency — future-session executors reading the plan should expect ~3 ignored tests at plan start, not ~12.

**Implementing commit.** `[HEAD]` (this entry + Stage 10.5 scaffolding).
