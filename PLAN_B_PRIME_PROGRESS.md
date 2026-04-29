# Plan B' — Architectural lifts (Stages 6.7 + 6.8)

Tracks Plan B''s execution against `boldfield/designs/in-progress/2026-04-29-sigil-architectural-lifts.md`. Closes the four architectural lifts deferred from Plan B's deviation entries (B.1 Slice C N-chain, B.2 chained-synth-cont, B.3 TypeExpr::Fn, B.4 arm-body-lambda) before Plan C runs the spec validation gate. B.5 scope_id remains deferred per `[DEVIATION Task 55] Phase 4f` concern #5.

Plan B closed at sigil/main `1229149` on 2026-04-28.

## Stage 6.7 — Chained-closure-record lifts (PR-β equivalent)

Goal: close B.1 + B.2. Generalize the 2-step Slice C chain (arm-side) and the 1-stmt helper-side classifier to N steps. Both surfaces share chained-closure-record allocation discipline.

- 6.7.1 — Create `PLAN_B_PRIME_PROGRESS.md` (this file).
  - status: done (this commit)
- 6.7.2 — Create `PLAN_B_PRIME_DEVIATIONS.md`.
  - status: done (this commit)
- 6.7.3 — `[PLAN-B-PRIME]` prefix added to `QUESTIONS.md` discipline.
  - status: done (this commit)
- 6.7.4 — Foundation `[DEVIATION Plan B' overview]` entry framing the bundled lifts + per-stage review-checkpoint discipline + closure-point cross-references to `PLAN_B_DEVIATIONS.md`.
  - status: done (this commit)
- Task 93 — B.2 Phase A: classifier + data-shape refactor (`is_simple_let_yield_then_pure_tail_body` 1-stmt cap → N stmts; `CpsContinuationKind::ChainedLetBindThenTail` variant).
  - status: todo
  - commits: []
- Task 94 — B.2 Phase B: pre-pass FuncId allocation for N synth-cont chain.
  - status: todo
  - commits: []
- Task 95 — B.2 Phase C: helper body emit (first perform's k_fn = synth_cont_step_0) + chain step emit (middle steps perform next; final step runs tail).
  - status: todo
  - commits: []
- Task 96 — B.2 acceptance e2e tests (2/3/5-perform helper bodies; helper with forward data dependency).
  - status: todo
  - commits: []
- Task 97 — B.1 Phase A: classifier + data-shape refactor (`arm_body_multi_let_then_pure_tail_shape` 2-let cap → N lets; `MultiLetPostArmKChain` 9 hardcoded fields → `Vec<ChainStep>`).
  - status: todo
  - commits: []
- Task 98 — B.1 Phase B: post_arm_k synth fn definition pass (N synth fns; each step's closure carries `(k_closure, k_fn) + r_1..r_step_idx`).
  - status: todo
  - commits: []
- Task 99 — B.1 acceptance e2e tests (3/5-let arm bodies; `arg_step_expr` references prior `r_*`; `tail_expr` references all bindings).
  - status: todo
  - commits: []
- Task 100 — Invert pinning tests (`slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` → positive; `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` → positive).
  - status: todo
  - commits: []
- Task 101 — Update existing examples to natural shapes (`multishot_stress.sigil` literal "10+ resumes"; `choose.sigil` literal two-flip pair generator; `multishot_perf.sigil` literal "3-element Choose combinator").
  - status: todo
  - commits: []

### Stage 6.7 review checkpoint

Pending — request human review of: N-chain post_arm_k closure-record allocation discipline; chained synth-cont state-of-bindings discipline; multi-shot correctness with N>2 resumes; multi-perform helper bodies under multi-shot; walker diagnostic surface for newly-rejected shapes.

## Stage 6.8 — First-class function types (PR-γ equivalent)

Goal: close B.3 + B.4. Together these unblock the literal `run_state` higher-order helper shape and the canonical algebraic-effects state-threading idiom.

- Task 102 — B.3 Phase A: parser surface for `TypeExpr::Fn`.
  - status: todo
  - commits: []
- Task 103 — B.3 Phase B: typecheck + monomorphize integration.
  - status: todo
  - commits: []
- Task 104 — B.3 Phase C: closure-convert + codegen for fn-typed values.
  - status: todo
  - commits: []
- Task 105 — B.3 Phase D: codegen-entry walker update.
  - status: todo
  - commits: []
- Task 106 — B.3 acceptance e2e tests (id_fn-as-value; apply higher-order fn; make_adder fn-returning-fn; compose generic).
  - status: todo
  - commits: []
- Task 107 — B.4: arm-body-lambda lift (drop `arm_body_walk` rejection; closure-convert side-table extension).
  - status: todo
  - commits: []
- Task 108 — B.4 acceptance e2e tests (arm body returning lambda; arm body IIFE; full `run_state` lambdas-of-state shape).
  - status: todo
  - commits: []
- Task 109 — Update existing examples + invert pinning tests (`state.sigil` literal `run_state`; `higher_order.sigil` docstring; `TypeExpr::Fn` rejection tests; arm-body-lambda rejection tests).
  - status: todo
  - commits: []
- Task 110 — Prompt-bank graded-end-to-end flips (P09 / P10 / P17 / P19 / P20; P02 stays compile-only pending stdlib `string_concat`).
  - status: todo
  - commits: []

### Stage 6.8 review checkpoint

Pending — request human review of: TypeExpr::Fn parser/typecheck/codegen full surface; HM unification on Ty::Fn for generic fn-typed parameters; indirect-call codegen correctness; arm-body-lambda interaction with multi-shot; run_state higher-order helper end-to-end; closure-convert side-table extension for lifted arm-body lambdas.

## Plan B' completion criteria

- All Stage 6.7 + 6.8 acceptance criteria met on both hosts (CI green).
- Prior-stage regression tests (Stages 1–6 + Stage 6 cleanup) still pass.
- Multi-shot continuation runtime test (10+ resumes within a single arm body) compiles + runs in `examples/multishot_stress.sigil`.
- `examples/state.sigil` uses literal `run_state` higher-order helper.
- `examples/choose.sigil` uses literal two-flip pair generator.
- Prompt bank graded-end-to-end count rises from 14/20 to 19/20 (P02 deferred to Plan C Stage 7).
- `PLAN_B_PRIME_PROGRESS.md` reflects reality; all tasks marked done with commit references.
- `PLAN_B_DEVIATIONS.md` closure points (B.1 / B.2 / B.3 / B.4) marked closed with cross-references to Plan B' implementing commits.
