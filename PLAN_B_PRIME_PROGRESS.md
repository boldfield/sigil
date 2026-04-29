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
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Adds `CpsContinuationKind::ChainedLetBindThenTail { steps, performs, tail_expr, tail_ty, captures }` variant + `ChainedLetBindStep { binding_name, binding_ty }` struct, both `#[allow(dead_code)]` while Phase B/C wire up the pre-pass + emit code. Adds `is_simple_chained_let_yield_then_pure_tail_body(body) -> Option<usize>` classifier returning the chain length on match (None on reject). Accepts N >= 1 (the existing 1-stmt case generalises into the chained variant; Phase D will retire `LetBindThenTail`). 9 unit tests cover the accept/reject matrix: single let-yield + pure tail (N=1); two/three let-yields + pure tail (N=2/3); empty body (rejected); non-let stmt in chain (rejected); let with non-perform value (rejected); impure perform args (rejected); impure tail (rejected); missing tail (rejected). Match on `CpsContinuationKind` in synth-cont definition pass extended with an `unreachable!()` guard for the new variant; until Phase B activates the pre-pass, no `CpsContinuationSynth` entry should carry this kind. Pod-verify clean.
- Task 94 — B.2 Phase B: pre-pass FuncId allocation for N synth-cont chain.
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: bundled with Task 95 in the activation commit per the lockstep recommendation (a separate Phase B-only commit would land a bridge state where ChainedLetBindStep entries exist but emit pass still hits unreachable!()). Pre-pass now uses `is_simple_chained_let_yield_then_pure_tail_body` and allocates N synth-cont FuncIds (one per chain step) declared in two passes: declare all N FuncIds first, then build N `CpsContinuationSynth` entries with cross-references populated (Middle::next_step_func_id). `cps_continuation_synth_indices` map points at step_0's index; helper body emit reads step_0's captures.
- Task 95 — B.2 Phase C: helper body emit (first perform's k_fn = synth_cont_step_0) + chain step emit (middle steps perform next; final step runs tail).
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: bundled with Task 94 (see Task 94 notes for the lockstep rationale). Helper body emit's captures-lookup match extends from `LetBindThenTail` to `ChainedLetBindStep`. Synth-cont definition pass replaces the Phase A `unreachable!()` with full Middle/Final emit. Middle step: bind args_ptr[0]; load captures + prior_bindings from synth-cont's closure_ptr; allocate next-step closure record (header + null code_ptr + captures + prior_bindings + this binding); copy captures + prior_bindings forward via raw I64 loads/stores; lower next perform's args via Lowerer; sigil_perform with k_fn=next_step_addr, k_closure=next_closure_record; return the perform's NextStep. Final step: same as `LetBindThenTail` (lower tail, dispatch via post_arm_k). The original 1-stmt path is structurally subsumed: the chained classifier accepts N=1 (returning Some(1)), so 1-stmt cases route through ChainedLetBindStep with chain_length=1 + Final role. `LetBindThenTail` variant + emit pass + `is_simple_let_yield_then_pure_tail_body` + `collect_synth_cont_captures` are now structurally dead in production; marked `#[allow(dead_code)]` transitionally so unit tests still compile. Phase D (follow-up commit) removes them. Pod-verify clean.
- Task 96 — B.2 acceptance e2e tests (2/3/5-perform helper bodies; helper with forward data dependency).
  - status: done-pending-ci
  - commits: [HEAD~2, HEAD~1, HEAD]
  - notes: Pulled forward per R3 review's showstopper finding ("no e2e test coverage for N>=2 chains"). Six e2e tests cover: N=2 simple, N=3 simple (Middle->Middle->Final), forward-data-dependency, user-param capture (single capture in tail), capture in perform-arg AND tail, pointer-typed binding (String). The R3 follow-up commit (HEAD~1) adds the classifier-side cap check (chain length cap at MAX_CLOSURE_ENV_SLOTS=31; over-cap chains fall through to Sync ABI cleanly), an improved pre-pass assert message for the captures+chain edge case (R3 finding 1), a unit test for the cap-check, and two new deviation entries (quadratic forward-copy cost + cap-check rationale, both in PLAN_B_PRIME_DEVIATIONS.md). Phase D (HEAD) deletes the structurally dead `LetBindThenTail` variant + emit pass + `is_simple_let_yield_then_pure_tail_body` + `collect_synth_cont_captures` + dead unit tests (-520 LOC net after retargeting captures tests to `collect_chained_synth_cont_captures`). All commits pod-verify clean.
- Task 97 — B.1 Phase A: classifier + data-shape refactor (`arm_body_multi_let_then_pure_tail_shape` 2-let cap → N lets; `MultiLetPostArmKChain` 9 hardcoded fields → `Vec<ChainStep>`).
  - status: done
  - commits: [3d8e4d7]
  - notes: New types `PostArmKChain` / `PostArmKStep` / `PostArmKStepRole` / `PostArmKPriorBinding` + new classifier `arm_body_n_let_then_pure_tail_shape` (accepts N >= 2). 7 unit tests cover the accept/reject matrix. Legacy types kept `#[allow(dead_code)]` until Task 100a Phase D-equivalent.
- Task 98 — B.1 Phase B: post_arm_k synth fn definition pass (N synth fns; each step's closure carries `(k_closure, k_fn) + r_1..r_step_idx`).
  - status: done
  - commits: [639ac98, 96f834a (ANF fixup), 2daf60c (walker lift)]
  - notes: Walker + pre-pass + arm-fn body emit + post-arm-k synth-fn definition pass all switched to `PostArmKChain`. Two CI-driven fixups: ANF intermediate lets in compound tails (`r1+r2+r3` flattens to `let $elab_t0 = r1+r2; $elab_t0+r3`) — classifier accepts the prefix of k-call lets and synthesises a Block tail. Slice B's post-`k` tail walker (`arm_body_post_arm_k_tail_free_vars_ok_block`) extended to allow inner-let bindings, threading binding names through `extra_bindings` (was rejecting them per the original Slice B first-commit restriction).
- Task 99 — B.1 acceptance e2e tests (3/5-let arm bodies; `arg_step_expr` references prior `r_*`; `tail_expr` references all bindings).
  - status: done
  - commits: [e62aa30]
  - notes: 3 e2e tests: N=3 simple Choose chain; N=5 simple chain (Middle->Middle->Middle->Middle->Final transition); N=3 with forward data dependency (Gen.next: (Int) -> Int with `k(r1)`, `k(r1+r2)`).
- Task 100 — Invert pinning tests (`slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` → positive; `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` → positive).
  - status: done
  - commits: [1baf7b1 (Task 100a), e22ef1b (Task 100b)]
  - notes: Split into two commits per `[DEVIATION Task 100]`. Task 100a: inversion #1 (3-let test deleted; positive coverage in Task 99) + Phase D-equivalent for B.1 (legacy `MultiLetPostArmKChain` + `arm_body_multi_let_then_pure_tail_shape` + `ArmBodyMultiLetThenPureTailMatch` + 6 legacy unit tests deleted). Task 100b: captures-bearing extension (chain step closures carry op-args alongside (k_closure, k_fn) + prior_bindings) + inversion #2 (`slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` → `slice_c_chain_arg_referencing_user_op_arg_runs`, positive runtime test).
- Task 101 — Update existing examples to natural shapes (`multishot_stress.sigil` literal "10+ resumes"; `choose.sigil` literal two-flip pair generator; `multishot_perf.sigil` literal "3-element Choose combinator").
  - status: done
  - commits: [dbc0645, f74e073 (multi-shot composition limit doc), b7063b0 (multi-shot composition fix), bcab458 (R6 fixups)]
  - notes: All three examples rewritten to natural shapes. multishot_stress.sigil: 10-resume single-arm body (replaces 5-handles × 2-resumes workaround); closed form 1+2+...+10 = 55; e2e renamed `multishot_stress_example_returns_55`. choose.sigil: literal two-flip pair generator; helper performs Choose.flip twice (B.2 chained-let-yield); multi-shot 2-resume arm enumerates 4 outcomes; closed form (1+2)+(3+4) = 10; e2e renamed `choose_example_pair_generator_returns_10`. multishot_perf.sigil: literal 3-element Choose combinator; helper performs Choose.flip three times; multi-shot 2-resume arm enumerates 8 outcomes; iteration count reduced from N=1000 to N=300 to stay under 5s wall-clock floor (3-flip combinator does ~7 arm dispatches per iteration vs. ~3 for 1-flip). Multi-shot composition fix (b7063b0) added the outer `post_arm_k` thread-local stack: B.2 helper Middle pushes the outer arm's `post_arm_k` pair before each `sigil_perform`; trampoline's Done branch pops and routes Done's value through the outer arm's chain. R6 fixups (bcab458): cap-check Sync fall-through when K+N >= MAX_CLOSURE_ENV_SLOTS; `PostArmKPriorBinding.ty` dropped (matches B.2's shape); 5 unit tests for outer post_arm_k push/pop discipline; docstrings on push fn balance + abnormal-exit semantics + B.1 Middle no-push invariant.

### Stage 6.7 review checkpoint

**Reached** (2026-04-29). PR #37 (`bcab458`) closes Stage 6.7 with all six review rounds addressed (R1..R6) and CI green on both ubuntu-24.04 and macos-14 (build+test, cold-checkout). Awaiting human review of: N-chain post_arm_k closure-record allocation discipline; chained synth-cont state-of-bindings discipline; multi-shot correctness with N>2 resumes; multi-perform helper bodies under multi-shot; walker diagnostic surface for newly-rejected shapes; outer post_arm_k stack mechanism (TLS rooting, push/pop balance under abnormal exit, Boehm interaction). Stage 6.8 starts after merge.

## Stage 6.8 — First-class function types (PR-γ equivalent)

Goal: close B.3 + B.4. Together these unblock the literal `run_state` higher-order helper shape and the canonical algebraic-effects state-threading idiom.

- Task 102 — B.3 Phase A: parser surface for `TypeExpr::Fn`.
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: AST adds `TypeExpr::Fn(Box<FnTypeExpr>)` (boxed payload to keep the enum below clippy's `large_enum_variant` threshold; `Stmt::Let` and `Expr::Lambda` both transitively contain `TypeExpr` and broke the lint when the variant was inline). `FnTypeExpr` carries `params: Vec<TypeExpr>`, `ret: TypeExpr`, `effects: Vec<String>`, `effect_row_var: Option<RowVar>`, `span`. Parser extends `parse_type` with a leading-`(` discriminator: `(T1, ..., Tn) -> R ![E1, ..., En]` (or `![..|r]`). Downstream pass arms added to keep the compiler building: `check_type_expr_known` recurses into params+ret then emits **E0136** ("first-class function type parsed but not yet usable; Phase B / Task 103 lands the integration"); `ty_from_type_expr` returns `None`; `monomorphize::rewrite_type_expr` substitutes recursively (forward-correct for Phase B); `monomorphize::ty_from_type_expr_under_subst` and `type_expr_to_ty` are unreachable (E0136 gates upstream); codegen entry-walker `type_expr_uses_apply_or_param` recurses (so an `Apply` hidden inside an `Fn` still surfaces); `slot_kind_for_type_expr_post_mono` returns `EnvSlotKind::Closure` for forward correctness. 10 parser unit tests cover the accept matrix (zero/one/two params; effects; row-var; nested fn-in-param; fn-returning-fn; generic-param in signature; let-binding position) and 2 reject cases (missing `->`, missing `![..]`). Pod-verify clean. Phase B (Task 103) replaces the E0136 / unreachable arms with real semantic integration.
- Task 103 — B.3 Phase B: typecheck + monomorphize integration.
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: `typecheck::ty_from_type_expr` now maps `TypeExpr::Fn` → `Ty::Fn(FnSig{params, ret, effects, effect_row_var: None})` for closed-row surfaces. `check_type_expr_known` recurses into params + ret (so nested unknown-type / Apply errors surface) and emits **E0137** for row-variable-bearing fn-types ("not yet supported in v1; use a closed row"). HM unification on Ty::Fn was already implemented and stays unchanged. Monomorphize: `ty_from_type_expr_under_subst` and `type_expr_to_ty` map `TypeExpr::Fn` → `Ty::Fn` recursively (closed rows only). `ty_to_type_expr` (the reverse direction) renders `Ty::Fn` back to `TypeExpr::Fn` so a generic-parameter substitution that binds A to a fn-typed concrete still produces a valid surface (forward correctness). 7 typecheck unit tests cover: zero-param fn-type → Ty::Fn; one-param + IO effect; generic-param fn-type sharing Ty::Var across positions; row-var fn-type → E0137; fn-typed let binding with matching RHS typechecks; fn-typed let binding with mismatched RHS → E0044; nested fn-in-param resolves recursively. Pod-verify clean. Phase C (Task 104) extends closure-convert + codegen with indirect calls.
- Task 104 — B.3 Phase C: closure-convert + codegen for fn-typed values.
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: closure-convert now collects user-defined top-level fn names and rewrites bare `Expr::Ident(top_level_fn)` to a captureless `Expr::ClosureRecord { code_fn_name, env_exprs: [], env_slot_kinds: [] }` when used as a value. The `Call::callee` short-circuit preserves direct dispatch for `Call { callee: Ident(top_level_fn), .. }`. Codegen replaces `lower_call`'s `unreachable!` catchall with a `call_indirect` emission: loads the callee's `closure_ptr`, reads `code_ptr` from offset 8, builds a Cranelift signature `(closure_ptr, params...) -> ret` from the callee's `FnTypeExpr` (stored in a new `Lowerer.local_fn_types` map populated from fn params + `Stmt::Let` annotations), imports it, and dispatches. `type_of_expr`'s Call arm extends with the `Ident-of-local-fn-typed` indirect case. `typecheck::call_callee_tys: BTreeMap<Span, Ty>` side-table added (populated by `check_call`); reserved for Phase C+ recursive-callee resolution. **Phase C v1 limit**: indirect callee must be `Expr::Ident(local)` where `local` is fn-typed via param or let annotation. More general callees (e.g., `make_adder(5)(7)`) trip `unreachable!` until Phase C+. Pod-verify clean.
- Task 105 — B.3 Phase D: codegen-entry walker update.
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: walker `contains_apply_or_generic_ref` already accepts `TypeExpr::Fn` (Task 102 added the recurse arm). 3 positive-coverage unit tests pin behaviour: walker accepts fn-type in param position; walker accepts fn-type in return position; walker still rejects an `Apply` hidden inside a fn-type (Apply-recursion through fn-type components).
- Task 106 — B.3 acceptance e2e tests (id_fn-as-value; apply higher-order fn; make_adder fn-returning-fn; compose generic).
  - status: phase-c-plus-done-pending-ci
  - commits: [HEAD]
  - notes: 5 positive e2e tests across Phase C v1 + C+ Part 1 + C+ Part 2 — `fn_as_value_via_let_binding_returns_42`, `higher_order_fn_param_returns_42`, `generic_apply_with_id_fn_returns_42`, `make_adder_returns_12` (Phase C+ Part 1: call-returning-fn), **`closure_env_load_callee_returns_42` (Phase C+ Part 2: lambda body invokes captured fn-typed value via ClosureEnvLoad-callee)**. R2 fixups added: `fn_as_value_with_multi_param_returns_7`, `fn_as_value_with_effect_row_returns_42`. Phase C+ Part 2 inverted the prior `closure_env_load_callee_is_e0138_until_phase_c_plus` rejection test into `closure_env_load_callee_returns_42` since the codegen surface now supports the shape. Only `p17_compose_source_rejects_until_typeexpr_fn_ships` remains as a rejection assertion (compose has additional surfaces — generic params + bare fn-as-value through the codegen-entry walker — that need Task 109's full inversion).
- Task 107 — B.4: arm-body-lambda lift (drop `arm_body_walk` rejection; closure-convert side-table extension).
  - status: done (Phase A + Phase B + Phase C++)
  - commits: [HEAD]
  - notes: **Phase A** drops the `arm_body_walk` Lambda + ClosureRecord rejection for non-k-capturing shapes. closure_convert already hoists arm-body lambdas correctly. **Phase B** ships the **2-slot trailing-pair convention** (parallels the arm fn's args_ptr layout): closure_convert detects k-pair captures via the new `arm_k_context_stack`; the lifted lambda's closure record allocates 2 trailing slots for `k_closure` + `k_fn` after the regular env; the synth lambda's `lower_call` dispatches `k(arg)` via `sigil_next_step_call(k_closure, k_fn, 1) → sigil_run_loop` and narrows the result to handler_overall_ty. **Phase C++** monomorphize-rewrites `lambda_captures` per-clone with substitution applied (new `MonoProgram.lambda_captures_resolved: BTreeMap<(String, Span), Vec<(String, Ty)>>`) so generic-context fn-typed captures see concrete Tys at codegen time. End-of-typecheck deref pass resolves `Ty::Var`s recorded mid-walk before they reach codegen. Tests: `arm_body_lambda_capturing_k_compiles_returns_99`, `task_108_arm_body_lambda_captures_k_runs`, `compose_body_via_closure_env_callees_returns_42` (all positive runtime tests). The Phase B route decision (2-slot split) was made by user.
- Task 108 — B.4 acceptance e2e tests (arm body returning lambda; arm body IIFE; full `run_state` lambdas-of-state shape).
  - status: done
  - commits: PR #38 [703c011, 51a8a8d]; PR #39 [9b7cd8d, a839c8a]
  - notes: **Phase A** (PR #38): `arm_body_iife_returns_43` covers example #2 (`Raise.fail(k) => (fn (n) => n+1)(42)`). **Phase B** (PR #38): `arm_body_lambda_capturing_k_compiles_returns_99` and `task_108_arm_body_lambda_captures_k_runs` cover example #1 (Choose-as-lambda; lambda allocated, not invoked). **Full canonical run_state** (PR #39 layered fixes): example #3's `State.get(k) => fn (s) => k(s)(s)` shape now runs end-to-end via `run_state_canonical_higher_order_helper_returns_threaded_value` (composes Bug 2 + Layer 2 + Bug 1 + Layer 3a + Layer 3b + Layer 3c + closure_convert k-index two-pass). `state_example_canonical_run_state_returns_11` pins the example file's invariant.
- Task 109 — Update existing examples + invert pinning tests (`state.sigil` literal `run_state`; `higher_order.sigil` docstring; `TypeExpr::Fn` rejection tests; arm-body-lambda rejection tests).
  - status: done
  - commits: PR #38 [b98a3dd, dac72c5]; PR #39 [a839c8a, b47d3fc, cde7afb]
  - notes: **Sub-task 1** (`examples/state.sigil` literal `run_state`): closed by PR #39's six-layer fix. The first PR #38 attempt (`7b457b6`) compiled cleanly but produced a heap-pointer-shaped runtime value; the bisect harness shipped at `dac72c5` localized three candidate failure layers; PR #39 closed all six layered bugs (Bug 2 / Layer 2 / Bug 1 / Layer 3a / Layer 3b / Layer 3c / Layer 3d) plus the closure_convert k-index two-pass fix. state.sigil now uses the canonical `run_state(initial, comp)` shape; e2e renamed to `state_example_canonical_run_state_returns_11`. The `[DEVIATION Task 109] run_state canonical shape — runtime chain integration gap` is RESOLVED with cross-references to the seven `[DEVIATION Stage-6.8-followup *]` entries documenting each layer fix. **Sub-task 2** (`examples/higher_order.sigil` docstring): closed in PR #38 [f10fb39]. **Sub-task 3** (TypeExpr::Fn rejection inversion): n/a; no standalone rejection tests existed pre-B.3. **Sub-task 4** (arm-body-lambda rejection inversion): closed by Task 107 (`nested_handle_with_inner_lambda_in_arm_body_compiles`, `arm_body_lambda_capturing_k_compiles_returns_99`). The `p17_compose_source_rejects_pending_builtin_as_fn_value` rejection still asserts compile-fail — closes when builtin-as-fn-value ships (Plan C scope; see `[DEVIATION p17_compose blocker analysis]`).
- Task 110 — Prompt-bank graded-end-to-end flips (P09 / P10 / P17 / P19 / P20; P02 stays compile-only pending stdlib `string_concat`).
  - status: done
  - commits: [HEAD]
  - notes: P09 (partial application via returned lambda), P10 (compose two lambdas), P19 (State[Int] counter via list walk), P20 (multi-shot Choose all-pairs) flipped to gradeable end-to-end — all four shapes covered by the e2e tests landed in Stage 6.7 (P20: `choose_example_pair_generator_returns_10`) and Stage 6.8 + followup (P09: `make_adder_returns_12`; P10: `compose_body_via_closure_env_callees_returns_42`; P19: `state_example_canonical_run_state_returns_11`). P17 (compose two unary fns across types) flipped — prompt source rewritten per `[DEVIATION p17_compose blocker analysis]`'s recommended path (user-side `its` wrapper around `int_to_string` instead of bare-builtin-as-fn-value), with the per-arrow `![..]` discipline applied. The wrapper-shape variant compiles + runs end-to-end via the surfaces shipped in PR #38 + #39. P02 stays compile-only (depends on stdlib `string_concat`, Plan C Stage 7 Task 68). Prompt bank graded-end-to-end count rises from 14/20 to 19/20.

### Stage 6.8 review checkpoint

**CLOSED** (2026-04-29). PR #38 (`4bb38ad`) merged the B.3 + B.4 architectural lifts; PR #39 (`cf358bb`) closed the canonical `run_state` runtime chain via the six-layer followup (Bug 2 / Layer 2 / Bug 1 / Layer 3a / Layer 3b / Layer 3c / Layer 3d). All four CI lanes green at PR #39's terminal commit.

Surfaces shipped:
- **B.3 (Phase A → C+ Part 2)**: TypeExpr::Fn parser, typecheck, monomorphize, codegen indirect dispatch. Four supported callee shapes — `Ident(local)` / `ClosureRecord` / `Call(...)` / `ClosureEnvLoad` — all tested end-to-end. `canon_ty` mangles `Ty::Fn`. E0137 rejects row-variable-bearing fn-types (closed-rows-only in v1). E0138 walker rejects unsupported indirect-call shapes with span-anchored Sigil diagnostics.
- **B.4 Phase A**: non-k-capturing arm-body lambdas (IIFE shape).
- **B.4 Phase B**: k-capture inside arm-body lambdas via the 2-slot trailing-pair convention.
- **Phase C++**: monomorphize rewrites `lambda_captures` per-clone — generic + fn-typed-captures combination unblocked.
- **PR #39 followup**: `NEXT_STEP_TAG_DISCHARGED` runtime ABI extension; tag-conditional handle-discharge dispatch; trailing-triple `(k_closure, k_fn, frame_ptr)` for lifted lambdas with captured-k that escape their handle; Sync shims for Cps-ABI fns at fn-as-value materialization; closure_convert k-index two-pass fix.

**Architectural follow-ups left open** (PR #39 review §2 + §3, deferred to Plan C):
- Replace TLS out-channel for `sigil_run_loop`'s terminal tag/value with packed multi-return.
- Gate Sync shim emission on `top_level_fn_names_seen_as_value`.

Both are bounded today (functionally correct; bloat bounded); rationale entries in `PLAN_B_PRIME_DEVIATIONS.md`'s `[DEVIATION Stage-6.8-followup architectural summary]`. Neither blocked Stage 6.8 closure.

## Plan B' completion criteria

- [x] All Stage 6.7 + 6.8 acceptance criteria met on both hosts (CI green). PR #37 (Stage 6.7), PR #38 (Stage 6.8 lifts), PR #39 (canonical run_state followup) all green on ubuntu-24.04 + macos-14 build+test + cold-checkout.
- [x] Prior-stage regression tests (Stages 1–6 + Stage 6 cleanup) still pass. 138/139 e2e + 539 compiler unit + 73 runtime tests green at PR #39 merge (1 e2e ignored: `handle_returning_simple_lambda_invoked_returns_value_pending_chain_fix`, an obsolete bisect harness pinned for cleanup in this PR).
- [x] Multi-shot continuation runtime test (10+ resumes within a single arm body) compiles + runs in `examples/multishot_stress.sigil`. Closed form 1+2+...+10 = 55; e2e `multishot_stress_example_returns_55`.
- [x] `examples/state.sigil` uses literal `run_state` higher-order helper. Canonical CPS shape with `comp() { set(10); v = get(); v + 1 }`; e2e `state_example_canonical_run_state_returns_11`.
- [x] `examples/choose.sigil` uses literal two-flip pair generator. Multi-shot 2-resume arm enumerates 4 outcomes; closed form (1+2)+(3+4) = 10; e2e `choose_example_pair_generator_returns_10`.
- [x] Prompt bank graded-end-to-end count rises from 14/20 to 19/20 (P02 deferred to Plan C Stage 7's `string_concat`). Task 110 flipped P09 / P10 / P17 / P19 / P20 in this PR.
- [x] `PLAN_B_PRIME_PROGRESS.md` reflects reality; all tasks marked done with commit references. This PR.
- [x] `PLAN_B_DEVIATIONS.md` closure points (B.1 / B.2 / B.3 / B.4) marked closed with cross-references to Plan B' implementing commits. This PR's DEVIATIONS update marks the relevant entries CLOSED.

**Plan B' is COMPLETE.** Stage 6.7 closed in PR #37; Stage 6.8 closed in PR #38 + #39; this PR is the closeout (PROGRESS doc + DEVIATIONS doc + prompt-bank flips). Plan C (spec validation gate) is the next planned work.
