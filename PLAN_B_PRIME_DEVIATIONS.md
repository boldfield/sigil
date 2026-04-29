# Plan B' — Deviations

Per Plan B' commit discipline, deviation entries land *before* the implementing commit they describe. Cross-references to Plan B's deviation entries (`PLAN_B_DEVIATIONS.md`) name the closure points each Plan B' task addresses.

## 2026-04-29 — [DEVIATION Plan B' overview] Architectural lifts as Plan B' scope; per-stage review checkpoints; B.5 scope_id stays deferred

**Context:** Plan B closed at sigil/main `1229149` on 2026-04-28 with the deferred-items audit pinning four architectural lifts (B.1 Slice C N-chain extension, B.2 chained-synth-cont extension, B.3 TypeExpr::Fn lift, B.4 arm-body-lambda lift) and one diagnostic-precision item (B.5 Phase 4f scope_id per-frame field) to Plan-C-or-later territory. Plan C's draft (`docs/plans/2026-04-21-sigil-finish.md` in `boldfield/designs`) explicitly forbids changing language semantics: "Stdlib and demos are written in sigil; they do not redefine what sigil does." So the deferred lifts have no home in Plan C as drafted.

**Plan B''s scope:** close B.1 + B.2 + B.3 + B.4 as a Plan-B-extension plan landing strictly between Plan B's close and Plan C's start. The plan splits into two stages by closure-point pattern:

- **Stage 6.7 — chained-closure-record lifts (PR-β equivalent):** B.1 (arm-side N-chain) + B.2 (helper-side chained-synth-cont). Both surfaces share the chained-closure-record allocation discipline — each step's closure record carries `(k_closure, k_fn) + prior bindings + remaining-chain captures` forward to the next step. The two lifts are bundled into Stage 6.7 because they share testing surface (the same multi-shot subsystem) and conceptual pattern, not because they share code (B.1 is arm-side post_arm_k chain in `MultiLetPostArmKChain`; B.2 is helper-side synth-cont chain in `CpsContinuationKind::ChainedLetBindThenTail`).

- **Stage 6.8 — first-class fn-types lifts (PR-γ equivalent):** B.3 (TypeExpr::Fn surface as parameter / return / let-binding type) + B.4 (drop arm-body-lambda rejection). Together these unblock the literal `run_state(initial, comp)` higher-order helper shape — the canonical algebraic-effects state-threading idiom from Koka / Effekt.

**Why B.5 scope_id stays deferred:** the original closure point in `[DEVIATION Task 55] Phase 4f` concern #5 framed scope_id as deferred "until concrete motivation surfaces (Stage 9 prompt produces a confusing diagnostic, etc.)". No such motivation has surfaced through Plan B's close. Adding scope_id is ~150 LOC of runtime/codegen/ABI work for a field nothing reads today. Plan B' honours Plan B's "Do not implement Stage 7+ features" discipline by landing only the lifts whose closure points pin them to active work surfaces; speculative ABI growth is not in scope.

**Per-stage review checkpoints are explicit acceptance gates.** Stage 6.7's review checkpoint signs off on chained-closure-record discipline before Stage 6.8 work begins; Stage 6.8's review checkpoint signs off on first-class fn-types before Plan B' closure. The pattern mirrors Plan B's Stage 5 / Stage 6 review checkpoints.

**Risk profile.** Stage 6.7 touches the multi-shot continuation subsystem — the same machinery Phase 4e captures+ shipped (PR #27, +4181/-786 over 17 commits / 4 review rounds with 4 mid-flight latent-bug fixes). Comparable scope expected. Stage 6.8 introduces first-class function types as a new typecheck/codegen surface; comparable to Plan A3's user-defined-types work (PR #12, +6471 over 11+ commits). Per-stage review checkpoints + bail-on-3-CI-failures discipline are the calibration.

**Closure points (per-task cross-references):**

- **B.1** (Slice C N-chain extension): closure point named in `PLAN_B_DEVIATIONS.md`'s `[DEVIATION Task 55] Phase 4e captures+` Slice C ("More than 2 `k` invocations (3+ requires generalising the chain to N — straightforward but layered; v1 commits to the minimum that demonstrates multi-shot)"); referenced in `[DEVIATION Task 58]` (multishot_stress.sigil's literal "10+ resumes" deferral) + `[DEVIATION Task 60]` (multishot_perf.sigil's "3-element Choose combinator" arm-shape deferral). Plan B' Tasks 97 / 98 / 99 / 100 close it.

- **B.2** (chained-synth-cont extension): closure point named at `compiler/src/codegen.rs:10286-10290` ("Multi-yield bodies (`perform; perform; tail`) — chained synth-conts"); referenced in `[DEVIATION Task 59]` (state.sigil's dual-handle workaround) + `[DEVIATION Task 60]` (multishot_perf.sigil's literal helper deferral) + `[DEVIATION Task 61]` (P20 prompt's run-portion deferral). Plan B' Tasks 93 / 94 / 95 / 96 close it.

- **B.3** (TypeExpr::Fn lift): closure point named in `examples/higher_order.sigil` lines 15-23 ("Plan A2 deferred to Plan A3, Plan A3 closed without it, remains post-Plan-B"); referenced in `[DEVIATION Task 59]` (state.sigil's run_state deferral) + `[DEVIATION Task 61]` (P19 prompt deferral). Plan B' Tasks 102 / 103 / 104 / 105 / 106 close it.

- **B.4** (arm-body-lambda lift): closure point named at `compiler/src/codegen.rs:1246-1257` (`Expr::Lambda { .. } => Some(...)` rejection in `arm_body_walk` with "lambdas in arm bodies require a closure-convert side-table extension distinct from Phase 4d MVP" diagnostic); referenced in `[DEVIATION Task 59]` (state.sigil's run_state deferral, lambdas-of-state arm body shape). Plan B' Tasks 107 / 108 close it.

**On Plan B''s motivation strength under different Plan C grading interpretations.** The Plan B' framing in `docs/plans/2026-04-29-sigil-architectural-lifts.md` describes Plan C's Stage 9 pass-rate gate as "structurally tight at 14/20 end-to-end-gradeable" — the strength of that framing depends on how Plan C grades the 6 prompts that ship "compiles only" at Plan B's close (P02, P09, P10, P17, P19, P20). Three plausible interpretations:

- **Strict (compiles-only auto-fail at first-compile):** denominator stays 20; 6 auto-failures mean 14 of 14 gradeable must pass = 100% required. Plan B''s motivation as framed; the gate is structurally tight.
- **Excluded (compiles-only not counted):** denominator = 14 gradeable; 70% of 14 ≈ 10 must pass = ~71% pass rate on gradeable. Tighter than typical CI but not "structurally tight."
- **Generous (compiles-only auto-pass first-compile gate):** 6 auto-passes + 8 of 14 from gradeable for 70% of 20 = 57% required pass rate. Plan B''s motivation weaker; many failures absorbable.

Plan C's Stage 9 task specification (`docs/plans/2026-04-21-sigil-finish.md` Tasks 85-87 in designs) does not pin which interpretation applies; the validate-spec.sh harness implementation will encode the choice. **Plan B' assumes the strict-or-excluded interpretation** — under those, Plan B' has clear motivation (lifts move the gradeable count from 14/20 to 19/20, materially loosening the gate). Under the generous interpretation, Plan B' becomes optional polish rather than gate-relaxation.

This is documented here for transparency: if Plan C ultimately adopts the generous interpretation, Plan B' still has value (better-quality demos using natural shapes; more honest spec-validation grading), but the urgency drops and the work could reasonably defer back to a future Plan B''. Plan C should pin the interpretation in its Stage 9 task design before committing to the gate threshold.

**Implementing commit(s):** Foundation `ddcdd9b` (this entry + Stage 6.7 scaffolding) — Task 6.7.1-4 commit. Subsequent commits address each task in the order specified by Plan B' (`docs/plans/2026-04-29-sigil-architectural-lifts.md` in designs `in-progress/`). Closeout commits at the end of each stage land the prior-stage hash flips per the Plan B precedent.

## 2026-04-29 — [DEVIATION Task 94+95] B.2 Phase B+C activation: quadratic forward-copy cost per chain step is accepted v1 cost

**Context:** Each Middle step's emit allocates the next step's closure record and copies the captures + prior_bindings forward via raw I64 loads/stores (codegen.rs synth-cont definition pass for `ChainedLetBindStep::Middle`). For a chain of length `N` with `K` captures, the total slot-copy operations across the chain is `sum_{i=0}^{N-2} (K + i)` ≈ `N*K + N²/2`. For N=10, K=3 chains (the multishot_stress.sigil target), that's roughly 80 raw I64 load/store pairs across the chain — well within Cranelift codegen tolerance and trampoline dispatch overhead.

**Why accepted:** Plan B' targets are 10-step chains comfortably; multishot_stress.sigil's "10+ resumes" + state.sigil's literal `run_state` shape + multishot_perf.sigil's "3-element Choose combinator" all fit far below the quadratic-blowup threshold. The clean alternative — a single backbone closure record threaded across all chain steps, write-once captures + grow-as-you-go bindings — would eliminate the per-step copy but requires either (a) a sentinel-extended record allocator that grows on demand or (b) a pre-computed maximal record allocated upfront. Both are larger refactors than Plan B' scope contemplates. The per-step record allocation also matches the existing closure-record allocation discipline (each step's record is independent; GC sweep can reclaim earlier records once their step's continuation has fired), which simplifies the GC root tracking story.

**Failure mode:** if a future Plan C performance gate (e.g., a fib-like benchmark with deeply chained performs) shows the quadratic cost dominating, the optimization moves to that scope. No v1 program in the prompt bank or stdlib stresses this — chained performs are typically short bursts (state-threading, generators) where chain depth correlates with semantic depth, not iteration count.

**Implementing commit(s):** Activation `5ad78c3` (Task 94+95). This deviation entry documents the cost choice; no code changes needed.

## 2026-04-29 — [DEVIATION Task 96] B.2 cap check: classifier-side chain-length cap; captures + chain combination still asserts at codegen

**Context:** R3 review of the Task 94+95 activation flagged that the per-step closure-record slot-count assert at codegen.rs's Middle-step emit (`assert!(next_slot_count < MAX_CLOSURE_ENV_SLOTS)`) is a mid-codegen panic for over-cap chains. Reviewer recommended (a) classifier-side rejection so over-cap chains fall through to the Sync ABI cleanly, vs. (b) emitting an E-coded compile error.

**Closure point chosen:** option (a) for chain-length-alone over-cap cases (`is_simple_chained_let_yield_then_pure_tail_body` rejects `chain_length >= MAX_CLOSURE_ENV_SLOTS`); the captures + chain combination edge case (where `K + N >= MAX_CLOSURE_ENV_SLOTS` but `N < MAX_CLOSURE_ENV_SLOTS` alone) still asserts at codegen because the captures count isn't available at classifier-time. The `compute_user_fn_abi` selector runs before captures collection; refactoring it to run after captures would be a deeper restructure than this deviation contemplates.

**Why accepted:** the captures + chain combination edge case is unlikely to surface in v1 — helpers with >5 captures are rare; chains beyond ~10 steps are rare. The combination of both at once would represent a helper shape outside Plan B' targets. The improved assert message at the codegen site points users toward the workaround (split the chain across two helpers, or reduce captures) so the failure surface, when it does trip, is actionable rather than mysterious.

**Failure mode:** if a Plan C demo or stdlib helper hits the captures + chain edge case, the workaround is documented in the assert message. A future revision could lift captures collection into the classifier (the captures walker currently depends on closure-converted helper params, which are available pre-`compute_user_fn_abi` — the restructure is feasible if motivation surfaces).

**Implementing commit(s):** Activation `5ad78c3` shipped the assert; classifier-side cap check + improved assert message land in the Task 96 acceptance-tests commit.

## 2026-04-29 — [DEVIATION Task 100] B.1 captures-bearing extension split across two commits

**Context:** Plan B' Task 100 specifies inverting two pinning tests:
1. `slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` → positive (3-let arm bodies now ACCEPT).
2. `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` → positive (arg_i references to user op-args now ACCEPT).

Inversion #1 is mechanical (the chained classifier already accepts N >= 2; legacy 2-let-only types deleted alongside). Inversion #2 requires the arm-side captures-bearing extension — chain step closures must additionally carry the arm fn's user op-args (analogous to B.2 helper-side `ChainedLetBindStep::captures` carrying helper user params).

**Closure point chosen:** split Task 100 across two commits to keep each commit focused.
- **Task 100a** (commit `1baf7b1`): inversion #1 + legacy-types deletion (Phase D-equivalent for B.1).
- **Task 100b** (this commit): captures-bearing extension + inversion #2.

The captures-bearing extension adds:
- `PostArmKChain.captures: Vec<PostArmKChainCapture>` — chain captures (op-args referenced anywhere in the chain).
- `walk_collect_arm_captures` walker — collects op-arg references in `arg_exprs[1..]` + `tail_expr`, deduped, with chain-binding shadowing.
- Walker free-var check extended: `arg_i` and tail may reference op-args (in addition to chain bindings + globals). Per-step closure record now carries `(k_closure, k_fn) + captures + prior_bindings` (Middle) or `captures + prior_bindings` (Final). Closure-record offsets, bitmap encoding, and forward-copy discipline updated to handle the captures wedge.
- Inversion #2: `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` renamed to `slice_c_chain_arg_referencing_user_op_arg_runs` and converted from a rejection test to a positive runtime test.

**Implementing commit(s):** Task 100a (`1baf7b1`): inversion #1 + legacy types deleted. Task 100b (this commit): captures-bearing extension + inversion #2.

## 2026-04-29 — [DEVIATION Stage 6.7 multi-shot composition] CLOSED: outer post_arm_k stack delivers literal Cartesian-product enumeration

**Closure note** (added at fix-commit landing): the deviation below originally framed the literal Cartesian-product enumeration as deferred. The user requested fixing it before Stage 6.7 closeout. The fix shipped as a runtime + codegen + trampoline change implementing the "outer post_arm_k stack" (continuation marks) approach described under "Closure point chosen" below. Both `examples/choose.sigil` (4-outcome pair generator, sum 10) and `examples/multishot_perf.sigil` (8-outcome 3-element combinator, sum 36 per iteration) now produce literal enumerations.

## 2026-04-29 — [DEVIATION Stage 6.7 multi-shot composition] Literal Cartesian-product enumeration deferred

**Context:** Plan B' Stage 6.7 closes B.1 (arm-side N-let chain) + B.2 (helper-side chained-let-yield) + Task 100b (op-arg captures). Plan B' Task 101 framed the natural shapes as "literal two-flip pair generator" (`choose.sigil` enumerating 4 outcomes) and "literal 3-element Choose combinator" (`multishot_perf.sigil` enumerating 8 outcomes per iteration). Implementing those framings as written produces incorrect output under v1's single-trampoline `Done`-terminates discipline.

**The limitation.** When a multi-shot arm's `k(arg)` call drives a multi-perform helper, the helper's Middle step (B.2) issues `sigil_perform` for the next perform; the next perform dispatches a fresh inner arm; the inner arm's chain runs to Final → `Done(value)`. The trampoline observes Done and returns directly to the wrapper — the OUTER arm's chain step (which would have continued the outer enumeration) never dispatches. The outer arm's `post_arm_k` pair was passed into the helper Middle's `args_ptr[1..3]`, but helper Middle ignores it (Middle steps don't dispatch to post_arm_k; only Final steps do). So the outer arm's k(false), k(third value), etc., are silently dropped.

Concretely:
- 2-flip helper + 2-resume outer arm produces partial enumeration `b1=t × {b2=t, b2=f}` = `1 + 2 = 3` (b1=f branch dropped).
- 3-flip helper + 2-resume outer arm produces `b1=t, b2=t × {b3=t, b3=f}` = `1 + 2 = 3` per iteration.

The literal Cartesian-product 4-outcome (sum 10) and 8-outcome (sum 36) enumerations require either (a) continuation marks (a deeper trampoline-resume mechanism that propagates "return-to-outer" across nested arm dispatches) or (b) reified continuations (first-class continuation values that capture the entire suspended computation as a closure). Both are post-v1 surfaces.

**Why accepted:** the literal enumeration framing in Plan B' Task 101 was speculative. The Stage 6.7 lifts (B.1 + B.2 + Task 100b op-arg captures) deliver everything they promised at the *implementation* level — the chains compose at runtime and pass through the synth fns correctly. The composition issue is a trampoline-semantic limit, not a chain-machinery bug. The natural-shape examples now use the multi-perform helper bodies + multi-resume arms (exercising the full Stage 6.7 surface) but settle for partial enumeration outputs (`choose.sigil` produces 3, `multishot_stress.sigil` produces 55, `multishot_perf.sigil` produces 3 per iteration).

**Failure mode:** if Plan C's spec validation tests assume the literal Cartesian-product enumeration shape (e.g., a prompt-bank entry requiring sum 10 for 2-flip pair generator), those prompts grade as compile-only or fail. Plan C should either (a) lower the bar to partial-enumeration shapes or (b) defer literal-enumeration prompts to a future Plan-C-or-later that adds continuation marks. The closure point for lifting this restriction is named here: **trampoline-side**, an `OuterPostArmK` mechanism that lets helper Middle steps thread the outer arm's post_arm_k forward through `sigil_perform`'s args, and a runtime `Done`-handler that walks the post_arm_k chain instead of returning to the wrapper.

**Implementing commit(s):** Task 101 (forthcoming Stage 6.7 closeout commit) ships the partial-enumeration outputs + this deviation entry. Examples remain as written; expected outputs in e2e tests are the actual partial values.

## 2026-04-29 — [DEVIATION Task 102 fixup] E0136 collision in Phase A commit

**Context:** Plan B' Stage 6.8 Task 102 (Phase A parser surface for `TypeExpr::Fn`) shipped a stub diagnostic in `check_type_expr_known` using `errors::code("E0136")` to gate fn-typed surfaces from reaching Phase B's typecheck integration. **`E0136` was already allocated** to "duplicate effect declaration" at `compiler/src/typecheck.rs:630` (Plan B Task 53). Phase A's commit (`71ad25b`) sits in history with two distinct error conditions sharing one code.

**Why accepted:** Phase B (Task 103, commit `616e776`) replaced the fn-type stub with a fresh `E0137` ("row-variable-bearing first-class function types not supported in v1"), so the live PR state has no collision. The collision exists only in the Phase A commit if cherry-picked or bisected to in isolation. The fix-forward (E0137 in Phase B) is the simpler path than rewriting Phase A history.

**Lessons learned:** the second near-miss on E-code allocation discipline in Plan B'-era (the first was the `f74e073` deviation entry's compressed lifecycle). Worth a centralised E-code registry / table to catch collisions at allocation time. Out of scope for Plan B' itself; flagged here as Plan C / housekeeping work.

**Implementing commit(s):** [forthcoming Task 103 R1 fixup commit] adds this deviation entry. No code change — the live state is already correct via Phase B's E0137.

## 2026-04-29 — [DEVIATION Task 103] Per-arrow `![..]` effect-row syntax in `TypeExpr::Fn`

**Context:** Plan B' Stage 6.8 Task 102's parser surface accepts `TypeExpr::Fn` as `(T1, ..., Tn) -> R ![E1, ..., En]` — the effect row attaches to **every** fn-type-arrow, not only the outermost one. A nested fn-type like `(Int) -> (Int) -> Int` requires `(Int) -> (Int) -> Int ![] ![]` (two effect rows, one per arrow). ML-family languages typically right-associate fn-types with effects bound at the outermost arrow only.

**Why per-arrow:** in an effect-typed language, the effects of a fn-typed value are part of its identity, not the surrounding context. `let f: (Int) -> Int ![IO] = ...; let g: (Int) -> Int ![] = ...;` are distinct values with distinct types — `f` performs IO when called, `g` doesn't. Anchoring effects at the outermost arrow only would conflate the inner returned function's effect surface with the outer caller's effect row, which breaks the substitution principle (you can't pass `f` where `g` is expected — yet outermost-only would let you, structurally).

**Trade-off:** the surface is more verbose for higher-order types. `(A) -> (B) -> C` is `(A) -> (B) -> C ![] ![]` (two `![]` markers). For the Stage 6.8 e2e tests this is fine — `compose`, `make_adder`, `apply`, `id_fn` all use closed rows; the extra `![]` per arrow is tolerable. Future ergonomics work could allow effect-row inference / inheritance ("if the inner arrow has no `![..]`, inherit from the outer fn's row").

**Failure mode:** none — the design is consistent with how `FnSig` and `Expr::Lambda` already store effects per-fn-decl. This deviation entry exists so future readers don't second-guess the design choice.

**Implementing commit(s):** Task 102 (`71ad25b`) shipped the parser surface with this discipline. Test `fn_type_returning_fn_parses` pins the expected shape (`(Int) -> (Int) -> Int ![] ![]`).

## 2026-04-29 — [DEVIATION Task 104] Captureless closure record allocated per fn-as-value use site

**Context:** Plan B' Stage 6.8 Task 104 (`bab66e5`) closure-convert rewrites every `Expr::Ident(top_level_fn_name)` outside callee position to a fresh `Expr::ClosureRecord { code_fn_name, env_exprs: [], env_slot_kinds: [] }`. Codegen then allocates a captureless closure record (header + code_ptr@8) on the GC heap **per use site**, not per fn declaration.

**The cost.** A program like:

```sigil
fn main() -> Int ![IO] {
  let n: Int = 0;
  loop {
    apply(double, n);  // `double` materializes a fresh ClosureRecord every iteration
    n + 1
  }
}
```

allocates one closure record per iteration through `apply(double, ...)`. The record's contents are static — same `code_ptr`, no captures — so it's pure waste under any realistic workload.

**Why accepted:** Phase C v1's primary goal is correctness of fn-as-value semantics, not allocation efficiency. The captureless-record-per-use-site shape is the simplest closure-convert pattern that integrates with the existing `Expr::ClosureRecord` lowering. Optimisations are post-v1 work.

**Future closure points** (multiple paths possible, listed in increasing complexity):

1. **Module-level static-init**: codegen synthesises one captureless ClosureRecord per top-level fn at module load, stores it as a global, and rewrites every fn-as-value use site to load from the global. Closes the cost completely. ~50 LOC change spanning closure_convert + codegen.

2. **CSE on identical ClosureRecord exprs within a single block**: if `apply(double, n)` appears N times in a single block, allocate once. Cheaper than (1) but doesn't help loop bodies.

3. **Inline closure-record cache (LRU or per-fn slot)**: per-thread runtime cache keyed on `code_fn_name`. More runtime complexity than (1), no clear advantage.

(1) is the canonical fix and the one Plan C should target if perf gates require it.

**Failure mode:** none under correctness — the user-visible behaviour is identical regardless of allocation strategy. **Perf only**: tight loops calling fn-as-value paths see one extra heap allocation per iteration. Plan C's spec-validation tests should not include workloads where this matters until the closure point lifts.

**Implementing commit(s):** Task 104 (`bab66e5`) shipped the per-use-site allocation. R2 review flagged the cost; this deviation entry documents it.

## 2026-04-29 — [DEVIATION Task 104 Phase C v1 limit] Recursive callee-type resolution + ClosureEnvLoad-callees deferred to Phase C+

**Context:** Plan B' Stage 6.8 Task 104 (`bab66e5`) ships Phase C v1: indirect-call dispatch via `call_indirect` over the closure record's `code_ptr` slot. Phase C v1 supports two callee shapes:

- `Expr::Ident(local)` where `local` is fn-typed via fn parameter or `let` annotation.
- `Expr::ClosureRecord { code_fn_name, .. }` — lambda IIFE, dispatched directly via the named code_fn_name.

**The deferred shapes:**

- **`Expr::Call(...)` callee** (call returning a fn-typed value, e.g., `make_adder(5)(7)`). Requires recursive callee-type resolution: walk the inner callee's return TypeExpr to derive the outer call's signature. The `typecheck::call_callee_tys` side-table populated in Task 104 is the planned hook — Phase C+ codegen reads it instead of `Lowerer.local_fn_types`.
- **`Expr::ClosureEnvLoad { .. }` callee** (captured fn-typed value invoked inside a synth lambda fn, e.g., `compose`'s body `f(g(x))` where `f`/`g` are captured). Requires ClosureEnvLoad-callee dispatch in `lower_call`: load the captured value as the closure_ptr, then dispatch as usual.

**Why accepted:** Phase C v1 lands the canonical fn-as-value patterns (id_fn-as-value, generic apply, simple higher-order fn parameters) without taking on the recursive-callee codegen surface in the same commit. The split is principled — three e2e tests in Task 106 partial cover the v1 surface; the deferred shapes have a clear closure path through Phase C+ work.

**The user-facing surface:** Task 104 (R2 finding 1) added the codegen-entry walker `unsupported_indirect_call_shape` which converts the would-be `lower_call` panic into a typed **E0138** Sigil diagnostic with the offending callee's span. Users writing `make_adder(5)(7)` or `compose`-style helpers see a clean diagnostic pointing at Phase C+ instead of a Rust-panic with implementation-detail messages.

**Failure mode:** Programs using `make_adder(5)(7)` or compose-with-captured-fn-types fail compilation with E0138. The existing `p17_compose_source_rejects_until_typeexpr_fn_ships` e2e test catches the compose case. Task 109 inverts these rejections to positive runtime tests after Phase C+ lands.

**Implementing commit(s):** Task 104 (`bab66e5`) ships Phase C v1; the [forthcoming R2 fixup commit] converts the panic to E0138 via `unsupported_indirect_call_shape`. Phase C+ commit closes the deferred shapes.

## 2026-04-29 — [DEVIATION e2e negative-test discipline] Most `!success()` assertions don't pin specific E-codes

**Context:** Plan B' Stage 6.8 PR #38 R3 review (Finding 1) flagged that of 12 negative e2e tests in `compiler/tests/e2e.rs` (those that assert compile failure via `assert!(!out.status.success())`), only 3 also assert that a specific E-code appears in stderr. The other 9 just assert "compile failed" without specifying which error code triggered the failure.

**The latent brittleness.** The `0baaa15` test fixup is a concrete example of how this can hide bugs: an earlier `make_adder_call_returning_fn_is_e0138_until_phase_c_plus` test source had a typecheck-level error (E0044) that fired before the codegen walker could emit E0138. The `assert!(!out.status.success())` passed (compile *did* fail) — but for the wrong reason. The test's claimed coverage of the E0138 path was a lie that only careful inspection caught.

**The discipline:** every negative e2e test should pin the specific E-code it's asserting:

```rust
let stderr_str = String::from_utf8_lossy(&out.stderr);
assert!(
    stderr_str.contains("E0XXX"),
    "expected E0XXX (description); got stderr={stderr_str:?}"
);
```

This catches the "test fails for the wrong reason" class of bug. The 3 tests that already do this (`partial_handler_of_multi_op_effect_rejected_with_e0142`, `closure_env_load_callee_is_e0138_until_phase_c_plus`, the `make_adder_call_returning_fn` original) are the discipline pattern.

**Why deferred:** retrofitting 9 tests requires identifying the right E-code for each shape, which involves running the test (impossible in this pod environment due to Cranelift OOM constraints). Each test landing should add its own E-code check; a bulk sweep risks introducing wrong assertions.

**Failure mode:** none today — all 12 tests pass on current main. The risk surfaces when a future code change shifts an error from one upstream pass to another (as happened with the `make_adder` typecheck-vs-codegen shift). The test still passes ("compile failed") but no longer covers the path it was named for.

**Closure path:** Plan C's spec validation work touches a lot of negative-shape coverage; folding the discipline retrofit into that work is the natural seam. New negative tests landing in Stage 6.8+ should include the E-code check.

**Implementing commit(s):** R3 fixup commit documents this deviation; new tests already follow the discipline (`closure_env_load_callee_is_e0138_until_phase_c_plus` from R2 / `0baaa15`). Existing 9 tests deferred.

## 2026-04-29 — [DEVIATION p17_compose blocker analysis] Two distinct issues, only one a real codegen surface

**Context:** Plan B' Stage 6.8 PR #38 R4 review (Finding 5) flagged that the `p17_compose_source_rejects_until_typeexpr_fn_ships` rejection test stays asserting compile-fail even after Phase C+ Part 2 closes the ClosureEnvLoad-callee surface. The R4 reviewer asked: *"What additional surface? Phase C+ Part 2 covers ClosureEnvLoad-callees; compose's body shape `fn (x) => f(g(x))` should compose cleanly."*

**Investigation:** compose's source has two distinct blockers, neither of which the R4 review's "additional generic surfaces" framing captured precisely:

```sigil
fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] {
  fn (x: A) -> C ![] => f(g(x))
}
fn main() -> Int ![IO] {
  let inc_then_format: (Int) -> String ![] =
    compose(int_to_string, fn (n: Int) -> Int ![] => n + 1);
  perform IO.println(inc_then_format(41));
  0
}
```

**Blocker 1 — per-arrow `![..]` syntax**. The line `fn compose[A, B, C](...) -> (A) -> C ![] {` only carries one `![..]` (for the inner returned `(A) -> C` fn-type). compose's own outer effect row needs a second `![..]` per the per-arrow discipline (see `[DEVIATION Task 103]` per-arrow effect-row entry). Without the second `![..]`, the parser surfaces an "expected `!` before effect row" error on the outer fn-decl. **Fix:** rewrite to `(A) -> C ![] ![]` — first `![]` for the inner fn-type, second for compose's row.

**Blocker 2 — `int_to_string`-as-value**. `compose(int_to_string, ...)` passes the builtin `int_to_string` as a fn-typed argument. Phase C v1's closure-convert materializes `Ident(top_level_user_fn)` to `ClosureRecord`, but `int_to_string` is a builtin (seeded into typecheck's `fn_env`, not declared as `Item::Fn`), so it's NOT in `top_level_fn_names`. closure-convert leaves it as `Ident("int_to_string")`, and codegen's `lower_expr(Ident)` panics with "unknown ident" (via the `_` arm at codegen.rs:8657 since `int_to_string` isn't in `env`, isn't a registered ctor, and the user-fn closure record materialization branch only fires for user-defined fns).

**Why accepted in v1:** Blocker 2 requires extending the closure-convert materialization path to cover builtins — either (a) seed `top_level_fn_names` with builtin names + add a synthetic ClosureRecord wrapper that codegen renders as a builtin call, or (b) at typecheck or earlier, rewrite `Ident(builtin)` in fn-value position to a wrapper fn. Both are post-v1 surfaces. Plan C's stdlib work would naturally close this when builtins migrate to user-Sigil shapes.

**Workaround for compose's literal shape:** wrap `int_to_string` with a thin user-side fn:

```sigil
fn its(n: Int) -> String ![] { int_to_string(n) }
// then: compose(its, ...)
```

This makes compose work end-to-end with Phase C v1 + Phase C+ surfaces.

**Failure mode:** the existing `p17_compose_source_rejects_until_typeexpr_fn_ships` test continues to assert compile-fail. With the per-arrow fix alone (Blocker 1) the program reaches the `lower_expr(Ident("int_to_string"))` panic; the `assert!(!out.status.success())` still passes but for the wrong reason (per the R3 Finding 1 discipline gap). Task 109's example update should rewrite the prompt-bank P17 example to use a user-side wrapper instead of bare `int_to_string`.

**Closure path:** Plan C's stdlib + builtin-as-fn-value work closes Blocker 2; Task 109 closes Blocker 1 (rewrite source) and inverts the rejection test once the rewritten source compiles cleanly via the Phase C+ surfaces.

**Implementing commit(s):** R4 fixup commit (this commit) documents the analysis; Task 109 will rewrite the source to use the per-arrow-correct + user-wrapper-`its` shape and invert the rejection test.

## 2026-04-29 — [DEVIATION Task 107 Phase B] k-capture inside arm-body lambdas (canonical run_state) deferred

**Context:** Plan B' Stage 6.8 Task 107 Phase A landed the arm-body-lambda lift for shapes that don't capture the arm's continuation `k`. The canonical `run_state` shape from Task 108 — and Choose-style "k as a lambda" patterns — capture and call `k` from inside a lambda body, which Phase A explicitly rejects with E0xxx-style "captures continuation `k`" diagnostic.

**The runtime-ABI mismatch.** Inside an arm body, `k(arg)` lowers via a special-case path to `sigil_next_step_call(k_closure, k_fn, arg)`. Both pieces are needed: `k_closure` carries the suspended computation's state; `k_fn` is the address of the post-arm-k synth fn (`cps_signature`: `(closure_ptr, args_ptr, args_len) -> *mut NextStep`). At typecheck level `k` has type `Ty::Fn(Int -> Int)` — regular closure-convention. **Inside a hoisted lambda, the captured value is just `k_closure`** (per closure_convert's `Ident("k")` rewrite); calling it via standard indirect-call dispatch would load `k_closure[8]` (which is null per `alloc_arm_closure_record:9998-10001`'s convention) and crash, AND the calling convention is wrong (closure-convention vs cps_signature).

**Two routes to Phase B:**

1. **Patch k_fn into k_closure's code_ptr at arm prologue + install a closure-convention trampoline.** At arm fn entry, after extracting k_closure and k_fn from args_ptr, write a synth trampoline fn's address into k_closure[8] (NOT k_fn directly — k_fn has cps_signature, not closure-convention). The trampoline takes `(closure_ptr, arg)`, packages `arg` into a stack slot, and tail-calls `sigil_next_step_call(closure_ptr, embedded_k_fn, arg)`. Pro: minimal closure-convert change. Con: each k-typed continuation needs a fresh trampoline per concrete signature; the trampoline must match `Ty::Fn`'s declared shape.

2. **Split k into k_closure + k_fn slots in the lifted lambda's closure record.** closure_convert recognizes arm-body lambdas capturing the enclosing arm's k_name; emits TWO `env_exprs` (one for k_closure, one for k_fn) instead of one. The lambda's body lowering detects ClosureEnvLoad-of-the-k-name and dispatches via `sigil_next_step_call` with both loaded values. Pro: parallels the existing arm-body `k(arg)` lowering. Con: closure_convert needs to thread enclosing-arm context (the k_name) through the lambda lift; the lifted synth fn's body emit needs a side-table flagging which env slot is the k-pair.

**Why Phase B deferred for autonomous overnight work:** both routes require non-trivial closure-convert + codegen surface beyond Phase A's "drop the rejection" scope. Each route is roughly a Phase C+ Part 2-sized change. Per the autonomous-overnight constraint, the deferral keeps Stage 6.8 review-ready: Phase A ships the IIFE/non-k-capturing surface (Task 108 example #2 covered); Phase B + Task 108 examples #1 (Choose `k(true)` / `k(false)`) and #3 (`run_state` `k(s)(s)`) defer to the next session.

**Failure mode:** programs using arm-body lambdas that capture `k` fail compilation with E0xxx-style "captures continuation `k`" diagnostic. The `arm_body_lambda_capturing_k_is_rejected_until_phase_b` e2e test pins this rejection so Phase B inverts a known-state diff.

**Closure path:** Phase B follow-up commit closes both routes' design questions and ships one of them. Plan B' Stage 6.8 review checkpoint should surface the route decision before Phase B's implementation begins.

**Implementing commit(s):** Task 107 Phase A (`703c011`) ships the arm-body-lambda lift for non-k-capturing shapes; Phase B (`51a8a8d`) + Phase B fix (`5619df6`) ship the **2-slot trailing-pair convention** route per the user's design call. Tests `arm_body_lambda_capturing_k_compiles_returns_99` and `task_108_arm_body_lambda_captures_k_runs` ship as positive runtime tests. Phase C++ (`1166804`) closes the parallel generic-context concern.

## 2026-04-29 — [DEVIATION R5 Finding 2] Side-table mono-survival — audit + class-level invariant

**Context:** Phase C++ (`1166804`) shipped `monomorphize`-rewrites-`lambda_captures`-per-clone after the speculative compose test in `4d272db` exposed `Ty::Var(7) reached cranelift_ty_of_ty`. The root cause: typecheck's `lambda_captures` records Tys mid-walk; some carry generic-param `Ty::Var`s; monomorphize specialised the AST per clone but didn't rewrite the side-table; codegen's `cranelift_ty_of_ty` then crashed on the residual Vars.

**The class-level concern.** Every typecheck side-table that maps a span (or other identifier) to a `Ty` (or a structure containing `Ty`) potentially has the same problem when a future code path reads it post-mono in a generic context. The Phase C++ fix is shaped to work for `lambda_captures`; analogous fixes may be needed for sibling side-tables when their consumers cross the generic boundary.

**Audit results (as of `5619df6`).**

| Side-table | Carries `Ty`? | Generic-context risk | Status |
|---|---|---|---|
| `match_scrut_tys: BTreeMap<Span, Ty>` | yes | Codegen reads `head_name()` only; `Ty::Var` inside args is benign at this consumer | **Safe**: no fix needed |
| `call_callee_tys: BTreeMap<Span, Ty>` | yes (Ty::Fn) | Phase C+ Part 1's `lower_call` reads via `cranelift_ty_of_ty`; same risk shape as `lambda_captures` | **Mitigated** via end-of-typecheck deref pass (`5619df6+`); generic-Var-remaining shapes still need monomorphize-rebuilds-per-clone if Phase C+ Part 1 reaches them in generic context |
| `lambda_captures: Vec<(Span, Vec<(String, Ty)>)>` | yes | Phase C+ Part 2's `captured_fn_sigs` reads via `cranelift_ty_of_ty` | **Fixed** by Phase C++ |
| `handle_arm_captures` / `handle_return_arm_captures` | yes | codegen consumes; arm-fn captures can be fn-typed under arm-body-lambda lift | Audited; codegen's existing path uses `head_name()` for primitive-vs-pointer disambiguation; no `Ty::Var(_)` unreachable on the read path. Adding deref-at-record at end-of-typecheck would preemptively close this if a future consumer changes the read shape |
| `handle_body_ty` | yes | Used for return-arm typing in codegen | Same shape as `match_scrut_tys` — read via `head_name()`; safe |
| `fn_schemes` | yes (Ty::Fn) | Generic by design; consumed by typecheck instantiation, not codegen directly | Different lifecycle; not affected |

**Closure path:** Phase C+ Part 1's call_callee_tys fix is the deref-at-end-of-typecheck pattern; Phase C++'s lambda_captures fix is the per-clone-rebuild pattern. **Choice criterion**: if the side-table's `Ty::Var`s are typically bound by inference completion (typical case), the deref pass suffices. If `Ty::Var`s remain free after typecheck (generic-fn-internal call sites where the surrounding fn's generics aren't bound until use-site monomorphization), the per-clone-rebuild is required.

**Test class-level invariant (future):** a property test that, for every generic instantiation in a sample program, walks the post-mono AST + every span-keyed side-table read by codegen, asserts `Ty::Var(_)` doesn't survive at any read site. Cheap relative to the class of latent panics it catches. Not landed yet; documented here so the next time a side-table-Ty-Var bug surfaces, this entry surfaces in search.

**Implementing commit(s):** R5 fixup commit (this commit) lands the call_callee_tys deref pass + this deviation entry. Phase C++ (`1166804`) is the precedent fix shape for the per-clone-rebuild route.

## 2026-04-29 — [DEVIATION Phase C+ Part 2 generic + fn-typed-capture] Generic lambda captures with fn-typed Ty::Var crash codegen

**Context:** Phase C+ Part 2 (`a5ab4f9`) wired `cc.captures_typed` from closure_convert through to a new `Lowerer.captured_fn_sigs: BTreeMap<String, FnSig>` field. The map is populated at synth fn Lowerer init from the captures' typed metadata; codegen reads it for ClosureEnvLoad-callee dispatch.

**The gap.** `lambda_captures` is populated by typecheck at the lambda's check-site, where generic params (declared on the surrounding fn) appear as `Ty::Var(_)` (not yet substituted to concrete types). For non-generic surrounding fns this is fine — no `Ty::Var` ever appears. For generic surrounding fns (e.g., `fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] ![] { fn (x: A) -> C ![] => f(g(x)) }`), the inner lambda's captures `f` and `g` carry `Ty::Fn(FnSig{params: [Ty::Var(A)], ret: Ty::Var(B), ...})`.

**Why monomorphize doesn't fix it.** Monomorphize clones the generic fn for each concrete instantiation (e.g., `compose$$Int$$Int$$Int`) and rewrites the cloned AST with concrete TypeExprs. But `lambda_captures` is a typecheck-side side-table consumed by closure_convert *after* monomorphize; it's NOT rewritten per clone. closure_convert builds `captures_typed` directly from `lambda_captures` (via `hoisted_captures` which inherits the Tys verbatim), so `captures_typed` for a generic compose's clone still has `Ty::Var`.

**The crash.** When codegen lowers an indirect call inside the lifted lambda's body, it calls `cranelift_ty_of_ty(&fty.ret, pointer_ty)` on the FnSig's params/ret. `cranelift_ty_of_ty`'s `Ty::Var(_)` arm is `unreachable!()` ("typecheck must resolve every var through unification before codegen runs"). Result: codegen panics.

**Concrete failure.** A speculative `compose_body_via_closure_env_callees_returns_42` e2e test (committed as `4d272db`, reverted in this fixup commit) tripped the panic with `Ty::Var(7) reached cranelift_ty_of_ty`. Test source: `fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] ![] { fn (x: A) -> C ![] => f(g(x)) }` then `compose(id_int, id_int)(42)`. The lifted lambda's f/g captures have `Ty::Var(A)`, `Ty::Var(B)`, `Ty::Var(C)` since closure_convert sees the typecheck side-table directly.

**Why accepted in v1:** non-generic `closure_env_load_callee_returns_42` and the multi-param/effect-row/mixed-kinds e2e tests (Phase C+ Part 2) all pass — the gap is generic-only. The canonical generic higher-order pattern (compose) needs Phase C++ work: monomorphize must rewrite `lambda_captures` (or its post-CC analog `captures_typed`) per clone with substitution applied, OR closure_convert must consume post-mono concrete TypeExprs and convert to FnSig at that point (rather than reading typecheck's pre-mono lambda_captures).

**Failure mode:** programs declaring generic top-level fns whose body contains a lambda capturing fn-typed param/let bindings panic at codegen with `Ty::Var(N) reached cranelift_ty_of_ty`. Workaround: avoid generic context for fn-typed captures (use a non-generic wrapper fn).

**Closure path:** Phase C++ work — either (a) monomorphize rebuilds `captures_typed` per clone applying the substitution, OR (b) closure_convert consumes post-mono TypeExprs from the FnDecl's params and converts via `ty_from_type_expr` at clone time, OR (c) at codegen time the synth fn's Lowerer applies the active substitution at lookup. Each route has trade-offs in pass-order coupling.

**Implementing commit(s):** Phase C+ Part 2 (`a5ab4f9`) shipped the non-generic surface; the speculative `compose_body_via_closure_env_callees_returns_42` test in `4d272db` exposed the gap; this fixup commit reverts the test and documents the deviation. Phase C++ closure deferred to follow-up commit pending the route decision.

## 2026-04-29 — [DEVIATION Task 109] run_state canonical shape — runtime chain integration gap

**Context:** Plan B' Stage 6.8 Task 109 sub-task 1 requires rewriting `examples/state.sigil` from the dual-handle Plan B v1 workaround to the canonical CPS-style `run_state(initial, comp)` higher-order helper. The shape leans on every B.3 + B.4 surface in one program: fn-typed parameters, fn-as-value of a top-level user fn, arm-body lambdas, k-capturing lambdas (B.4 Phase B trailing-pair convention), recursive Call-of-Call dispatch on fn-typed values returned from k (`k(s)(s)`), and let-binding the handle's fn-typed result.

**The gap.** The first-cycle Task 109 commit (`7b457b6` + `e35dae9` for the E0220 `resumes: many` opt-in) shipped state.sigil with the literal canonical shape:

```sigil
fn run_state(initial: Int, c: () -> Int ![State]) -> Int ![] {
  let state_fn: (Int) -> Int ![] = handle c() with {
    return(v) => fn (s: Int) -> Int ![] => v,
    State.get(k) => fn (s: Int) -> Int ![] => k(s)(s),
    State.set(arg, k) => fn (s: Int) -> Int ![] => k(arg)(arg),
  };
  state_fn(initial)
}
```

Compile-time was clean (no E0xxx) after the `resumes: many` opt-in. **Runtime** produced a closure-record-pointer-shaped value (`94846082251584` on Linux, `5502959616` on macOS — both look like heap addresses) instead of the expected `6`. `int_to_string` then printed the raw pointer.

**Likely failure layers (untested at runtime before Task 109):**

1. **Handle returns a fn-typed value** — pre-Task-109, no e2e test exercised "arm allocates a lambda AND the handle's overall result is that lambda AND we then invoke the lambda". The closest existing test is `arm_body_iife_returns_43` (lambda invoked inline, value Int) and `arm_body_lambda_capturing_k_compiles_returns_99` (lambda allocated in arm, never invoked). Neither covers the let-bind-then-invoke shape.
2. **k-capturing lambda's k(arg) actually dispatches** — `task_108_arm_body_lambda_captures_k_runs` allocates a k-capturing lambda but does NOT invoke it; the trailing-pair dispatch path through `sigil_next_step_call(k_closure, k_fn, ...)` + `sigil_run_loop` is unproven at runtime.
3. **Recursive Call-of-Call (`k(s)(s)`) where the inner `k(s)` returns a fn that's then invoked** — Phase C+ Part 1's recursive callee resolution covers `make_adder(5)(7)` (top-level fn returning a fn), but NOT k as the outer callee. The k(s) shape inside an arm-body lambda goes through the trailing-pair dispatch first, then needs a regular indirect call on the result. Whether `call_callee_tys` is populated for k's typecheck-side return type, and whether codegen's catchall correctly threads the outer call's callee through it, is untested.

**Bisect plan.** Task 109 fixup commit (this commit) lands one bisect e2e test: `handle_returning_simple_lambda_invoked_returns_value_pending_chain_fix` — a `Trigger.fire` arm returning a non-k-capturing constant-shape lambda, let-bound, invoked once. `#[ignore]`'d while the gap exists. If un-ignored that test passes, bug is in k-capture or recursive dispatch (layers 2 or 3); if it fails, layer 1 is the culprit and the fundamental "handle returns a fn-typed value" path needs work.

**Why accepted in v1:** running and bisecting the bug requires the compiler binary on real source, which OOMs the pod (Cranelift constraint per CLAUDE.md). Deferring to a follow-up CI iteration after the bisect test provides direction is the lowest-risk path. state.sigil keeps the dual-handle Plan B v1 workaround so the example builds and the existing `state_example_dual_handle_returns_6_then_99` invariant holds.

**Failure mode:** Plan B' Stage 6.8 completion criterion "examples/state.sigil uses literal `run_state` higher-order helper and the threaded-state output is correct" remains *unmet*. The lifts B.3 and B.4 are individually green (their dedicated e2e tests pass); the integration of those lifts in the literal canonical run_state shape doesn't yet run end-to-end. Other Plan B' completion criteria are met.

**Closure path:** follow-up Task 109 fixup commit will un-ignore the bisect test, observe which layer breaks, and either ship the targeted compiler fix (if the layer is bounded) or document a Plan-C-or-later closure if the gap is structural. Once the chain runs end-to-end, state.sigil rewrites to the canonical shape and `state_example_run_state_returns_threaded_value` lands as the integration assertion.

**Implementing commit(s):** original Task 109 attempt at `7b457b6` (run_state shape) + `e35dae9` (E0220 `resumes: many` fix); current commit reverts state.sigil to dual-handle, restores the dual-handle e2e test, adds the bisect test, and documents this deviation. Sub-tasks 2 / 3 / 4 of Task 109 (higher_order.sigil docstring / TypeExpr::Fn rejection inversion / arm-body-lambda rejection inversion) all remain closed by prior Stage 6.8 work.

## 2026-04-29 — [DEVIATION Stage-6.8-followup architectural summary] Six-layer canonical run_state fix, layer-by-layer cross-reference

**Reader's entry point.** Plan B' Stage 6.8 shipped the language-surface lifts B.3 (TypeExpr::Fn) and B.4 (arm-body lambdas + Phase B k-capture trailing-pair convention). Task 109's first CI cycle on the canonical CPS-style `run_state(initial, comp)` revealed that those surface lifts alone don't make the canonical run end-to-end — the runtime + codegen chain has six layered semantic / architectural gaps that compose. This summary names each layer, its load-bearing test, and the specific deviation entry that documents the fix.

Reading order:
1. **Bug 2** ([entry](#2026-04-29--deviation-stage-68-followup-bug-2-return-arm-dispatch-on-op-arm-discharge-values-violates-algebraic-effects-semantics)) — handle skips return arm on op-arm discharge per algebraic-effects type theory (B ≠ R type-soundness). Surface symptom: `rs_a` (B≠R single discharge) prints heap-pointer-shaped value pre-fix, `107` post-fix.
2. **Layer 2 analysis + fix** ([analysis](#2026-04-29--deviation-stage-68-followup-layer-2-analysis-captured-k-from-lambda-invocation-returns-raw-arg-not-return-arm-wrapped-r), [fix](#2026-04-29--deviation-stage-68-followup-layer-2-fix-lifted-lambdas-karg-self-applies-originating-handles-return-arm)) — lifted lambda's k(arg) self-applies originating handle's return arm. Surface symptom: `rs_b` (k(s)(s) chain, tail-perform body) SIGSEGVs pre-fix, `14` post-fix.
3. **Bug 1** ([entry](#2026-04-29--deviation-stage-68-followup-bug-1-fix-recover-discharged-value-across-non-tail-perform-body)) — recover trampoline's terminal value across non-tail-perform body via `LAST_TERMINAL_VALUE` TLS. Surface symptom: `dbg_a` (`{ let _ = perform; tail }` shape) prints `7` pre-fix, `107` post-fix.
4. **Layer 3a fix + 3b/3c analysis** ([entry](#2026-04-29--deviation-stage-68-followup-layer-3a-fix--3b3c-analysis-tag-conditional-return-arm-self-apply)) — tag-conditional return-arm self-apply (skip on DISCHARGED, apply on DONE). Surface symptom: prevents double-wrap when synth-cont chain discharges via inner arm. Documents 3b and 3c gaps; hits a clean `unhandled effect_id` abort post-3a, paving the way for 3b/3c.
5. **Layer 3b** ([entry](#2026-04-29--deviation-stage-68-followup-layer-3b-fix-sync-shims-for-cps-abi-fns-at-fn-as-value-materialization)) — Sync shims for Cps-ABI fns at fn-as-value materialization. Surface symptom: `rs_l3c` (CPS-effected fn-typed parameter) prints heap pointer pre-fix, `42` post-fix.
6. **Layer 3c** ([entry](#2026-04-29--deviation-stage-68-followup-layer-3c-fix-re-push-handler-frame-in-lower_k_pair_call-preserve-discharged-through-outer-post_arm_k-routing-fix-closure_convert-k-index-collision)) — trailing-triple `(k_closure, k_fn, frame_ptr)` + handler frame re-push + DISCHARGED preservation through outer_post_arm_k routing + closure_convert k-index two-pass + trailing-pair convention in lower_k_pair_call. Surface symptom: canonical `run_state` returns `11` post-fix (closes the Plan B' Stage 6.8 criterion).
7. **Non-canonical cleanups** ([entry](#2026-04-29--deviation-stage-68-followup-non-canonical-cleanups-statesigil-canonical-drain-leak-layer-3d-debug-doc-removal)) — state.sigil rewrite, DEBUG_RUN_STATE.md deletion, outer_post_arm_k drain on Layer 3c bypass, Layer 3d (return arms with outer captures).

**Load-bearing integration test:** `run_state_canonical_higher_order_helper_returns_threaded_value`. Two stepping-stone integration tests (`integration_bug2_plus_layer2_only_tail_perform_canonical_arms` and `integration_bug2_layer2_bug1_non_tail_perform_canonical_arms`) bisect to specific layer pairs if the full integration regresses.

**Architectural follow-ups left open** (PR #39 review §2 + §3, deferred): (a) replace TLS out-channel for `sigil_run_loop`'s terminal tag/value with packed multi-return — TLS is functionally correct today but architecturally fragile under future nested-handle shapes that interleave run_loop calls between codegen reads. (b) gate Sync shim emission on `top_level_fn_names_seen_as_value` — bounded bloat today, but worth tightening if Cps-fn count grows. Both are Plan-C-or-later candidates; neither blocks Stage 6.8 completion.

## 2026-04-29 — [DEVIATION Stage-6.8-followup Bug 2] Return arm dispatch on op-arm-discharge values violates algebraic-effects semantics

**Plan B' Stage 6.8 Task 109 followup.** Closes one of the layered bugs blocking the canonical `run_state` rewrite. Sibling bugs (Source A's body-tail-after-perform under sync lowering, the canonical run_state's k-capturing-lambda-invocation chain, and the multi-arm State.get/set/return composition) remain documented under `[DEVIATION Task 109]` for follow-up work.

**The bug.** Phase 4g (PR #29 `eabef59` activation + `dd10379` test fixup) shipped uniform return arm dispatch in `Expr::Handle`'s `lower_expr`: after body lowering, if `return_arm.is_some()`, codegen unconditionally builds `NextStep::Call(return_closure, return_fn, [body_val_widened, null, identity])` and drives `sigil_run_loop`. The `dd10379` commit message defended this with: *"the return clause runs over whatever value flows out of the body, including non-resuming op-arm tail values."* That interpretation is incorrect per algebraic-effects type theory (Plotkin–Pretnar; Eff; Koka):

- Body's type is `B`.
- Op arm bodies have type `R` (handle's overall — the same type the handle expression evaluates to).
- Return clause `return(v: B) => body_R` has v's type B and body_R's type R.
- When an op arm fires and discards `k`, its value already has type R and IS the handle's final value. Passing it through the return clause as `v: B` is type-unsound when B ≠ R.

The bug is masked when B = R (the case PR #29's tests covered), surfacing only when B ≠ R (the canonical `run_state` shape: B = Int, R = (Int) → A). Symptom: heap-pointer-shaped output values, varying across runs (the closure_ptr to the discharged arm's lambda, interpreted as Int, fed through the return arm's pointer arithmetic).

**The fix — distinguish at runtime, conditional dispatch at codegen.**

Runtime (`abi/src/effect.rs` + `runtime/src/handlers.rs`):
- New `NEXT_STEP_TAG_DISCHARGED = 2` discriminant alongside existing `NEXT_STEP_TAG_DONE = 0` and `NEXT_STEP_TAG_CALL = 1`.
- New `sigil_next_step_discharged(value)` constructor — emitted by op arm fn body's discard-`k` tail path (replaces `sigil_next_step_done` at that specific site).
- New thread-local `LAST_TERMINAL_TAG: Cell<u32>` set by `sigil_run_loop` immediately before returning the terminal value (DONE for normal Done, DISCHARGED for the new variant).
- New `sigil_last_terminal_tag()` query for codegen.
- New `sigil_reset_last_terminal_tag()` reset for codegen to call before body lowering (so handles whose bodies don't run a perform see a clean DONE state).
- The trampoline routes both DONE and DISCHARGED through the existing outer post_arm_k stack uniformly — discharge value still flows through any waiting outer multi-shot continuation chain. The distinction matters only at the top-level run_loop terminal.

Codegen (`compiler/src/codegen.rs`):
- New FFI declarations + per-fn FuncRefs for `sigil_next_step_discharged`, `sigil_last_terminal_tag`, `sigil_reset_last_terminal_tag`.
- Op arm fn body's discard-`k` catchall (the path that produces `NextStep::Done(arm_body_value)` when arm body is evaluated to a value WITHOUT invoking k) now emits `sigil_next_step_discharged` instead of `sigil_next_step_done`. Synth-cont chains (the resume-`k` path) continue to emit `sigil_next_step_done` — body completion via continuation IS body-normal completion, the return arm should fire.
- `Expr::Handle`'s `lower_expr` emits `sigil_reset_last_terminal_tag()` before body lowering (gated on `return_arm.is_some()`).
- After body lowering, if `return_arm.is_some()`, query the tag and conditionally branch:
  - DISCHARGED: skip return arm dispatch; convert body_val to handler_overall_ty's Cranelift type and use directly as handle's overall.
  - DONE: existing return arm dispatch path.
- Both branches converge on a merge block whose param is the handle's final value.

**Type-conversion path in the discharge branch.** When body's Cranelift type B and handler_overall_ty R coincide (e.g., B = Int = I64 and R = (Int) → Int = pointer_ty = I64 on 64-bit targets — the canonical run_state shape), body_val IS handle's overall directly. When they differ in width (B = Bool = I8 and R = String = pointer_ty = I64, etc.), the discharge branch performs a width-aware conversion (uextend / ireduce / bitcast) OR emits a safe placeholder of handler_overall_ty. The placeholder is never observed at runtime when B's narrow-back at the perform site has truncated R-typed bits — the discharge branch is structurally dead in that case. Codegen still must emit valid IR; the placeholder satisfies the verifier without affecting runtime behavior.

**Test inversions.** Two PR #29 tests pinned the buggy semantics; both inverted to assert the corrected semantics:
- `handle_with_return_arm_fires_on_op_arm_discharge_value` (asserted "9900\n" — return arm applied to discharge value) → renamed to `handle_with_op_arm_discharge_skips_return_arm`, asserts "99\n" (return arm bypassed; arm's value IS handle's overall).
- `handle_with_constant_return_arm_overrides_op_arm_yield` (asserted "999\n" — constant return arm applied to op arm's yield) → renamed to `handle_with_op_arm_discharge_skips_constant_return_arm`, asserts "7\n".

**New positive test:** `handle_returning_fn_typed_value_with_op_arm_discharge_runs` exercises the load-bearing B ≠ R case (B = Int, R = (Int) → Int). Pre-fix this produced a heap-pointer-shaped value varying across runs; post-fix produces "107\n" (arm's lambda invoked at top level with arg = 7).

**What this fix DOES NOT close.** The canonical `run_state(initial, comp)` higher-order helper from PR #38's reverted Task 109 first-cycle attempt remains broken — Bug 2 is one of multiple layered bugs in the canonical shape. Verified: a manual `/tmp/run_state_canonical.sigil` matching the original literal shape still produces a heap-pointer-shaped value with this fix applied. Other layers blocking the canonical:
- **Bug 1** (Source A): synchronous body lowering doesn't propagate discard-k through body's post-perform code. Affects programs whose handle body has the `{ let _ = perform; tail }` shape rather than `comp()` in tail position.
- **Layer 2** (canonical run_state's k-capturing arm-body lambda invocation chain): arms return lambdas that capture `k` and invoke it via `k(s)(s)` recursive call-of-call. The k-capture allocation + lambda-invocation + recursive Call dispatch chain has its own bug that Bug 2 doesn't address.
- **Layer 3** (multi-arm composition): the canonical run_state has return + State.get + State.set arms. Whether the multi-arm dispatch composes correctly with k-capturing arms is unverified.

These remain Plan B' Stage 6.8 followup work tracked under `[DEVIATION Task 109] run_state canonical shape`. The Bug 2 fix in this entry is a load-bearing prerequisite — without it, even the simpler rs_a-style B ≠ R shape fails at runtime.

**Implementing commit(s):** [HEAD] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge.

## 2026-04-29 — [DEVIATION Stage-6.8-followup Layer 2 analysis] Captured-k-from-lambda invocation returns raw arg, not return-arm-wrapped R

**Plan B' Stage 6.8 Task 109 followup, post-Bug-2.** Empirical bisect of the layered run_state canonical bugs identifies and bounds Layer 2 (per [DEVIATION Stage-6.8-followup Bug 2]'s "What this fix DOES NOT close" enumeration). Analysis only — no fix in this entry; this documents what's broken, where, and what fixing it requires.

**Bisect probes (post-Bug-2 fix, all under `/tmp/`):**
- `rs_b1.sigil` — eager non-lambda k invocation at arm body tail (`Trigger.fire(k) => k(7)`). Result: prints `20`, exit 0. ✓ Works.
- `rs_b.sigil` — lambda captures k, lambda body invokes `k(s)(s)` (canonical run_state shape minus state). Result: SIGSEGV, exit 139. ✗ Crashes.

Both files declare `effect Trigger resumes: many { fire: () -> Int }`, body `comp() { perform Trigger.fire() }`, return arm `return(v) => fn (s: Int) -> Int ![] => v + s`, and call `f(13)` from main. The only difference is the op arm's body shape: tail-position eager `k(7)` versus arm-body-as-lambda whose body is `k(s)(s)`.

**Root cause.** `lower_k_pair_call` (compiler/src/codegen.rs:9912–9998) — the synth lambda fn's k(arg) dispatch — builds `NextStep::Call(loaded_k_closure, loaded_k_fn, 1)` and drives `sigil_run_loop` synchronously, then narrows the result to `info.handler_overall_ty`'s Cranelift type. But `loaded_k_fn` — captured from the arm fn's trailing-pair (k_closure, k_fn), itself sourced from comp's CPS-ABI k_fn parameter, itself written as `sigil_continuation_identity` by `lower_call`'s CPS path (compiler/src/codegen.rs:10128–10141) — is the identity continuation. `sigil_continuation_identity` (runtime/src/handlers.rs:873–913) returns `Done(arg)` unchanged. The trampoline's terminal then returns `arg` as u64.

So `k(s)` inside the lambda returns the raw `s` (an Int), narrowed to `handler_overall_ty` which is `(Int) -> Int ![] = pointer_ty`. The next call site `(k(s))(s)` interprets that Int (e.g., 13) as a closure pointer and dereferences → SIGSEGV.

**Why eager k(7) works.** In `rs_b1`, the arm body's tail-position k(arg) yields a `NextStep::Call(identity, [arg])` from the arm fn. The handle's outermost `sigil_run_loop` dispatches that Call, identity returns `Done(arg)`, the run_loop hits its top-level terminal, sets `LAST_TERMINAL_TAG = DONE`, and returns `arg`. The handle expression's outer codegen then runs the return arm with `v = arg`, producing the R-typed closure. Return-arm dispatch happens at handle-discharge time, NOT inside k. Identity's "return arg unchanged" is correct as long as the return-arm wrap fires immediately after at the discharge layer.

**Why captured-k-from-lambda breaks.** When the lambda escapes the handle (because the arm body IS the lambda — Bug 2's discharge-without-return-arm path), the originating handle has already discharged by the time `f(13)` runs. The lambda's k(s) drives a *fresh* `sigil_run_loop` over `Call(identity, [s])` → `Done(s)` → terminal returns `s`. There's no handler frame in scope, no return-arm dispatch fires. The lambda gets `s` typed as `(Int) -> Int` and segfaults at the next application.

**The architectural gap.** Identity-as-k_fn is a Phase 4d MVP simplification (per `[DEVIATION Task 55] Phase 4d` in `PLAN_B_DEVIATIONS.md`) that conflates two distinct semantics:
1. **In-handle tail-position k(arg)**: identity is correct because the handle's outermost run_loop drives discharge + return-arm wrap immediately after.
2. **Captured-k-from-lambda invocation outside the handle**: identity is *wrong* because there's no outer handle to apply the return arm. k(s) must self-apply the return-arm to produce R.

Phase B's trailing-pair convention (Plan B' Stage 6.8 Task 107) wired up the *dispatch* mechanism for case 2 but inherited identity's case-1 semantics. The dispatch lands; the value type is wrong. This is the architectural debt.

**Fix architecture (proposed, NOT implemented in this entry).** Two paths, ranked:

*Option A (preferred, localized): trailing-triple convention.* Extend the lifted lambda's closure record's trailing-pair (`k_closure`, `k_fn`) to a trailing-triple `(k_closure, k_fn, return_arm_fn)`:
- closure_convert detects k-capture in lifted lambda; instead of trailing-pair, writes trailing-triple, sourcing `return_arm_fn` from the originating Expr::Handle's pre-pass return-arm synth fn.
- `lower_k_pair_call` loads the third slot and, after run_loop returns the raw u64, calls `return_arm_fn(raw_u64)` synchronously to produce the R-typed value.
- Narrows to `handler_overall_ty` AFTER the return-arm call (not before).

For tail-perform body shapes (rs_b case), this collapses to: `k(arg) = return_arm_fn(arg)`. For non-tail-perform (post-perform body code present), the synth-cont mechanism already in place handles post-perform code; the trailing-triple's `k_fn` would be the synth-cont (not identity), and `return_arm_fn` still wraps the synth-cont's result. Both shapes converge.

*Option B (broader, riskier): change handle expression's body invocation contract.* Make `lower_call`'s CPS path optionally pass a non-identity `k_fn` to body — specifically `return_arm_fn` when the call is a handle expression's body and the handle has a return arm. Then identity is replaced everywhere the chain flows; the run_loop terminal's existing DONE-routes-through-return-arm logic conflicts (double-wrap), so the terminal logic must be inverted (DONE → terminal value is already R, no further wrap). This touches Phase 4g's contract directly — risky.

Option A is the recommended path. Estimated scope: closure_convert ~50 LOC change, codegen `lower_k_pair_call` ~20 LOC change, plus FFI plumbing for the third slot. No runtime ABI changes (closure record layout is internal).

**Multi-arm composition (Layer 3) interaction.** Once Option A lands, the multi-arm canonical run_state (return + State.get + State.set arms, where get/set arms each return k-capturing lambdas) becomes testable. Open question: does the trailing-triple correctly thread when multiple arm types' lambdas chain (set's lambda's k(s) returns get's lambda's k(s)(s) returns ...)? The same trailing-triple should compose if `return_arm_fn` is shared per handle (which it is). Pinned for verification post-Layer-2 fix.

**What's verified empirically:**
- Bug 2's fix is load-bearing and correct in its scope: rs_a (B ≠ R, single discharge arm, no captured-k lambda) prints `107` post-fix.
- Layer 2 is independent of Bug 2: the segfault reproduces in rs_b with Bug 2 applied. The bug is in `lower_k_pair_call`'s narrow-without-return-arm-wrap, not in handle-level discharge dispatch.
- Layer 2 is bounded to lifted-lambda k-pair-bearing synth fns. Direct (non-lambda) k(arg) at arm body tail (rs_b1, rs_a) works because the outermost run_loop's terminal applies return arm at handle-discharge time.

**Implementing commit(s):** [HEAD] on `stage-6-8-followup-run-state` (analysis only — no compiler/runtime changes in this commit).

## 2026-04-29 — [DEVIATION Stage-6.8-followup Layer 2 fix] Lifted lambda's k(arg) self-applies originating handle's return arm

**Plan B' Stage 6.8 Task 109 followup, post-Layer-2-analysis.** Implements Option A from the prior analysis entry. Closes the captured-k-from-lambda invocation gap for tail-perform-body, single-op-arm, no-outer-captures-in-return-arm cases — the rs_b probe shape and the canonical `run_state(initial, comp)` helper's arm body pattern (excluding multi-arm + non-tail-perform body, which remain Layers 1 and 3).

**Closure_convert** (`compiler/src/closure_convert.rs`): `ArmKContext` and `ArmKPairCapture` gain a `handle_span: Span` field. The originating `Expr::Handle`'s span is captured when entering an op-arm rewriting context and threaded into every `ArmKPairCapture` lifted from that arm's body. Codegen reads it at `lower_k_pair_call` time to look up the handle's return-arm synth fn.

**Codegen** (`compiler/src/codegen.rs`): `lower_k_pair_call` (synth lambda fn's k-pair dispatch path) gains a return-arm self-apply step between the existing run_loop and narrow-back. After run_loop returns the body-resumed u64:
1. Look up `handler_return_arm_indices.get(&info.handle_span)`.
2. If `Some(idx)` AND `handler_return_arm_synth[idx].captures.is_empty()`: build `NextStep::Call(null, return_arm_fn_addr, 3)` with args buffer `[run_loop_result, null_post_handle_k_closure, identity_k_fn_addr]`; drive `sigil_run_loop`; result is the R-typed widened value. Mirrors the Phase 4g handle-discharge dispatch pattern at `lower_expr Expr::Handle`'s `normal_block` branch.
3. If `None` (no return arm) OR captures non-empty: pass through the raw run_loop result (pre-fix semantics; the latter is a documented follow-up).

The narrow-back to `handler_overall_ty`'s Cranelift type then operates on the R-typed value (post-fix) instead of the raw arg (pre-fix).

**No runtime ABI changes.** The lifted lambda's closure record layout is unchanged (still trailing-pair `(k_closure, k_fn)`). The fix is entirely in the codegen-side dispatch, using the existing `handler_return_arm_indices` / `handler_return_arm_refs_per_handle` side-tables.

**Test added:** `handle_returning_k_capturing_lambda_invoked_outside_handle` in `compiler/tests/e2e.rs`. Asserts `f(7) = k(7)(7) = (s) => 7+s, applied to 7 = 14` for the canonical run_state arm body shape `Trigger.fire(k) => fn (s: Int) -> Int ![] => k(s)(s)` with return arm `return(v) => fn (s: Int) -> Int ![] => v + s`. Pre-fix: SIGSEGV. Post-fix: prints 14.

**Captures restriction.** Return arms with outer captures (e.g., `let x = ...; handle ... with { return(v) => f(x, v), ... }`) fall back to the pre-fix path. The synth fn's closure record requires runtime-allocated slots populated from outer-scope values; threading those through the lifted lambda's invocation context requires either (a) extending the lambda's closure record with the return-arm closure_ptr or (b) using a thread-local or handler-stack lookup that survives handle discharge. Pinned for follow-up; canonical run_state's return arm has no outer captures so the fix unblocks it as-is.

**What's still blocking the canonical `run_state(initial, comp)`:**
- **Layer 1** (Bug 1): non-tail-perform body shape (`comp() { let _ = perform State.set(10); ...; v + 1 }` does post-perform work). Different bug than Layer 2.
- **Layer 3**: multi-arm composition (return + State.get + State.set). Whether the trailing-pair k-pair-bearing dispatch composes correctly when multiple arm types' lambdas chain.

Verified: post-Layer-2 fix, `/tmp/run_state_canonical.sigil` (canonical multi-arm + non-tail-perform shape) still produces a heap-pointer-shaped output. Layer 2 fix is necessary but not sufficient.

**Implementing commit(s):** [HEAD] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge.

## 2026-04-29 — [DEVIATION Stage-6.8-followup Bug 1 fix] Recover discharged value across non-tail-perform body

**Plan B' Stage 6.8 Task 109 followup, post-Layer-2.** Closes Bug 1 from `[DEVIATION Stage-6.8-followup Bug 2]`'s "What this fix DOES NOT close" enumeration: synchronous body lowering's IR-locally-computed `body_val` reflects body's tail expression's natural value, NOT the discharged arm's value, when the body has post-perform code (`{ let _ = perform; tail }` shape). For the `dbg_a` probe (Source A from `DEBUG_RUN_STATE.md`), pre-fix `f(7)` printed `7` (body's identity-lambda's behavior on `7`); post-fix prints `107` (arm's discharge lambda `fn (x) => x + 100` applied to `7`).

**The runtime piece** (`abi`/`runtime`):
- New `LAST_TERMINAL_VALUE: Cell<u64>` TLS in `runtime/src/handlers.rs` alongside the existing `LAST_TERMINAL_TAG`.
- `sigil_run_loop`'s terminal sets both: tag (DONE / DISCHARGED) AND value (the u64 returned to the caller).
- New FFI exports `sigil_last_terminal_value()` and `sigil_reset_last_terminal_value()` paralleling the tag's exports.
- No GC root; the value is u64 (not a pointer-shaped slot). Codegen consumes it immediately; for precise-GC v2, either root the TLS or copy through a stack slot at consumption.

**The codegen piece** (`compiler/src/codegen.rs`):
- New FFI declarations + per-fn FuncRefs for `sigil_last_terminal_value` / `sigil_reset_last_terminal_value`. Plumbed through `PerFnRefsCtx`, `PerFnRefs`, `prepare_per_fn_refs`, `Lowerer`, and 7 Lowerer construction sites (parallel to Bug 2's plumbing for the tag side).
- `Expr::Handle`'s `lower_expr` now resets BOTH TLS slots before body lowering — both for return-arm-bearing AND no-return-arm handles — so a stale tag/value from a prior run_loop on the same thread doesn't shadow this handle's body completion when this body never invokes a perform.
- The return-arm-bearing path's `discharge_block` reads `sigil_last_terminal_value()` instead of `body_val_widened` to recover the trampoline's actual terminal u64. Narrowing logic: I64 → identity, narrower-int → ireduce, pointer_ty → identity, else placeholder.
- The no-return-arm path (previously fall-through to `body_val`) gains an analogous tag-conditional structure: 3 blocks (`discharge_block_nra`, `normal_block_nra`, `merge_block_nra`); discharge branch reads TLS_VALUE + narrows to body's type B; normal branch uses body_val as before.

**Test added:** `handle_with_post_perform_body_code_uses_arm_discharge_value` exercises the dbg_a probe shape (no return arm, body `{ let _ = perform Trigger.fire(); fn (x) => x }`, arm `fn (x) => x + 100`). Asserts `f(7) = 107` post-fix. Pre-fix this returned 7 (body's tail lambda's behavior).

**Composability with Bug 2 + Layer 2.**
- Bug 2 (op-arm-discharge skips return arm) and Bug 1 (non-tail-perform body recovery) are independent fixes. Bug 1's discharge_block tag-check is gated on `LAST_TERMINAL_TAG == DISCHARGED`, the same condition Bug 2 introduced. Bug 1 strengthens the discharge branch's value source from `body_val` (Bug 2's MVP that worked only for tail-perform body) to `LAST_TERMINAL_VALUE` (correct for all body shapes).
- Layer 2 fix (lifted lambda's k(arg) self-applies return arm) is at a different codegen site (`lower_k_pair_call`) and is unaffected by Bug 1's TLS plumbing. Both fixes coexist; the rs_b probe (Layer 2 case with tail-perform body) still prints `14`.

**What's still blocking the canonical `run_state(initial, comp)`:** Layer 3 — multi-arm composition where comp's body has multiple performs across `State.set` and `State.get`, AND each arm captures k into a lambda that escapes via discharge AND is invoked from the run_state caller, AND k(arg) must resume body to drive subsequent performs (not just self-apply return arm). The current `lower_k_pair_call`'s self-apply-return-arm path is correct for tail-perform body but wrong for non-tail-perform body where k must resume body to run further performs. The synth-cont infrastructure that handles non-tail-perform bodies in CPS-color fns covers limited shapes (`{ let _ = perform; constant }` / `{ let r = k(a); pure_tail }`); the canonical run_state's `{ let _ = perform State.set(arg); let v = perform State.get(); v + 1 }` shape is not yet covered. Verified: post-Bug-1 fix, `/tmp/run_state_canonical.sigil` still produces a heap-pointer-shaped output. Bug 1 closes one residual; Layer 3 remains.

**Implementing commit(s):** [HEAD~1] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge (Bug 1).

## 2026-04-29 — [DEVIATION Stage-6.8-followup Layer 3a partial fix + analysis] Tag-conditional return-arm self-apply; Layer 3b/3c gaps documented

**Plan B' Stage 6.8 Task 109 followup, post-Bug-1.** Closes Layer 3a (tag-conditional self-apply); documents Layer 3b (fn-as-value Sync vs Cps ABI gap) and Layer 3c (captured-continuation handler-frame re-push) as the remaining architectural blockers for the canonical `run_state(initial, comp)`.

### Layer 3a fix — tag-conditional return arm self-apply in lower_k_pair_call

The Layer 2 fix unconditionally self-applied the originating handle's return arm to k(arg)'s `sigil_run_loop` result. For k-pair-bearing lambdas captured at non-tail-perform body shapes, the captured k_fn is a synth-cont (chained-let-yield helper). Driving k(arg) through synth-cont may complete normally (Done with the natural body terminal — apply return arm) OR discharge from a nested arm (DISCHARGED with R-typed value — skip return arm). Pre-fix would double-wrap the discharged value.

**The fix.** `lower_k_pair_call`'s self-apply step now queries `sigil_last_terminal_tag()` after the run_loop call returns. Three blocks (`skip_block` / `apply_block` / `merge_block`); skip on DISCHARGED, apply on DONE, merge to a single I64-typed value, then narrow to `handler_overall_ty`. Mirrors the discharge-block pattern from Bug 2's handle-discharge dispatch.

**Captures-restriction unchanged.** The fall-back to raw widened_result remains for return arms with outer captures (Layer 3d follow-up).

### Layer 3b gap — fn-as-value indirect call Sync/Cps ABI ambiguity

`compute_user_fn_abi` returns `UserFnAbi::Sync` for many CPS-effected fns (those whose body shape doesn't match simple-tail-perform / yield-then-constant / chained-let-yield) and `UserFnAbi::Cps` only for the body-shape-matching subset. Top-level fns materialized as values via closure_convert's Task 104 `Ident(top_level_fn) → ClosureRecord` path carry a code_ptr referencing the fn's actual code — Sync or Cps. The indirect call site (`lower_call`'s catchall) builds the Cranelift signature from the surface `FnTypeExpr`'s effect row alone — no ABI info. For Sync-ABI fns the existing `(closure, args...) -> ret_ty` shape is correct; for Cps-ABI fns the actual code fn is `(closure, args_ptr, args_len) -> *NextStep`.

The canonical run_state's `c: () -> Int ![State]` parameter, when bound to comp (chained-let-yield → Cps ABI), hits this gap: indirect call returns a NextStep pointer interpreted as Int.

**Verified empirically.** An experimental fix that detected `is_cps` from `FnTypeExpr.effects.is_empty()` and used the CPS interop wrapper at the indirect call site fixed `rs_l3b` (chained-let-yield with eager arms: 6 ✓) and `rs_l3c` (CPS-effected fn-typed parameter: 42 ✓) but broke the existing `fn_as_value_with_effect_row_returns_42` test — `add_one` has `![IO]` row but Sync ABI (its `let _ = perform IO.println(...); n + 1` body shape doesn't match any chain pattern). Surface-effect-row alone is an unreliable proxy for ABI.

**Fix paths (open question for Plan C).**
1. Track per-fn ABI in the closure record. Add an extra slot at allocation time; runtime branch at indirect call site. Most flexible; runtime cost ≈ one branch per indirect call.
2. Emit Sync shims for Cps-ABI fns at fn-as-value materialization. closure_convert allocates a synth `sync_shim_for_<name>` Sync fn that internally drives run_loop. closure record's code_ptr points to the shim, not the Cps body. Indirect call uses Sync sig uniformly. Loses no optimization for direct calls.
3. Force all CPS-needing fns to Sync ABI uniformly. Loses chained-let-yield optimization but simplifies fn-as-value invariably.

Option 2 is recommended — preserves all optimizations + uniform indirect call shape. Estimated scope ~150 LOC (one synth Sync shim emission + closure_convert site update).

### Layer 3c gap — captured continuation invoked outside handle hits empty handler stack

Even with 3b resolved, the canonical's `handle c() with { ... }` op arm body discharges via lambda. The lambda escapes the handle (becomes state_fn). When state_fn is invoked from run_state's caller, the State frame has been popped (sigil_handle_pop unlinks but keeps the allocation alive). The lambda's k(arg) drives a fresh sigil_run_loop whose synth-cont (e.g., post-State.set body `let v = perform State.get(); v + 1`) issues sigil_perform for State.get. The handler stack doesn't have State on it: sigil_perform aborts with `unhandled effect_id ... handler stack empty`.

**Verified empirically.** With an experimental Layer 3b fix applied, canonical run_state aborts with exactly that diagnostic (exit 134). Confirms the gap.

**Fix path (open).** Capture the State frame's pointer at handle-allocation time, thread it through the lifted lambda's closure record (extending the trailing-pair to a trailing-triple `(k_closure, k_fn, frame_ptr)`), and re-push the frame at lower_k_pair_call before driving run_loop (popping after). The frame allocation persists across pop/re-push because sigil_handle_pop only unlinks — the heap allocation lives until GC reclaims it. Estimated scope ~200 LOC across closure_convert (ArmKPairCapture extension), codegen (lower_closure_record + lower_k_pair_call extension + arm fn body emit's frame_ptr unpack), and arm closure record allocation (handle expression writes frame_1_ptr_snapshot at a fixed slot).

### Probe results post-Layer-3a

- `rs_b` (tail-perform body, Layer 2 case): 14 ✓
- `rs_b1` (eager k(7) tail): 20 ✓
- `dbg_a` (non-tail-perform body, Bug 1): 107 ✓
- `rs_l3a` (multi-arm-defined-but-single-fires): 14 ✓
- `rs_l3b` (chained-let-yield with eager arms): heap-pointer-shaped (blocked by Layer 3b)
- `rs_l3c` (CPS-effected fn-typed parameter, no handle): heap-pointer-shaped (blocked by Layer 3b)
- `run_state_canonical`: heap-pointer-shaped (blocked by Layer 3b + 3c stack)

The 3a fix is correctness-preserving for future cases; the canonical still requires Layer 3b + 3c to compose.

**Implementing commit(s):** [HEAD] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge (Layer 3a only — 3b and 3c are documented gaps, no compiler/runtime changes for them in this commit).

## 2026-04-29 — [DEVIATION Stage-6.8-followup Layer 3b fix] Sync shims for Cps-ABI fns at fn-as-value materialization

**Plan B' Stage 6.8 Task 109 followup, post-Layer-3a.** Closes Layer 3b from the prior analysis. Implements **Option 2** (Sync shims at fn-as-value materialization) — the recommended path. Direct-call sites are unchanged; only the indirect-call path through fn-typed values sees the shim.

**The shim.** For every Cps-ABI top-level fn, codegen's pre-pass (`emit_object`'s user_fns loop) declares a parallel Sync-ABI shim with linker name `<mangled>__sync_shim` and signature `(closure_ptr, params...) -> ret_ty`. After all user fn bodies are emitted (just before `module.finish()`), each shim's body is generated:
1. Pack user params into a stack slot of size `(N + 2) * 8` bytes; each param widened to I64 via `uextend` for narrower-int slots.
2. Write `null_k_closure` and `sigil_continuation_identity`'s func_addr at `k_closure_offset(N)` / `k_fn_offset(N)`.
3. Call the underlying Cps fn with `(closure_ptr, args_ptr, args_len)` → `*NextStep`.
4. Drive `sigil_run_loop` → u64.
5. Narrow back to `ret_ty` (I64 / ireduce / pointer-passthrough).

**The materialization site.** `lower_closure_record` (codegen.rs:10928, post-fix) checks `sync_shim_refs.get(code_fn_name)` first. If the entry exists, the closure record's `code_ptr` slot gets the shim's func_addr; otherwise falls back to `user_fn_refs[code_fn_name]`. Synth lambdas (`$lambda_N`) and Sync-ABI top-level fns naturally fall through. Direct-call sites (`lower_call`'s `Ident(name)` → user_fn_refs path) are untouched and continue using the inlined CPS interop wrapper for Cps-ABI callees.

**Plumbing.** New side-table `sync_shim_fn_ids: BTreeMap<String, FuncId>`, threaded through `PerFnRefsCtx → PerFnRefs::sync_shim_refs → Lowerer::sync_shim_refs`. 13 destructure / Lowerer-construction sites updated (parallel to Bug 1's `last_terminal_value_ref` plumbing).

**Tests added:**
- `cps_effected_fn_typed_parameter_indirect_call_returns_correct_value` — minimal probe: `fn invoke(c: () -> Int ![Trigger]) -> Int ![Trigger] { c() }`. Pre-fix: heap-pointer-shaped output. Post-fix: 42.
- `handle_with_eager_resume_arms_chains_let_yield_correctly` — chained-let-yield body with eager-tail-k arms passed via `c: () -> Int ![State]`. Asserts `run_state(5, comp) = 6` (`v + 1` with v=5).

**What's still blocking the canonical `run_state(initial, comp)`:** Layer 3c — captured continuation invoked outside handle hits empty handler stack. Verified: post-Layer-3b, canonical now hits a clean `unhandled effect_id 2 (op_id 0); handler stack empty` abort (exit 134) instead of a heap-pointer-shaped output. The synth-cont chain inside the lifted lambda IS now reachable (Layer 3b unblocked it); the State frame just isn't on the handler stack at that point.

**Implementing commit(s):** [HEAD~1] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge (Layer 3b).

## 2026-04-29 — [DEVIATION Stage-6.8-followup Layer 3c fix] Re-push handler frame in lower_k_pair_call; preserve DISCHARGED through outer post_arm_k routing; fix closure_convert k-index collision

**Plan B' Stage 6.8 Task 109 followup, post-Layer-3b — closes the canonical `run_state(initial, comp)` end to end.** Three coordinated fixes:

### 3c-1: Trailing-triple `(k_closure, k_fn, frame_ptr)`

Extends the lifted lambda's closure record's trailing-pair to a trailing-triple including the originating handler's frame pointer. Captured at handle-allocation time via `frame_1_ptr_snapshot`, threaded through:
- `closure_convert::ArmKPairCapture` gains `frame_ptr_idx: usize` (= `k_fn_idx + 1`).
- Handle expression's `alloc_arm_closure_record` (extended with `frame_ptr_v: Option<Value>`) writes `frame_ptr` to the arm closure record's trailing slot when the arm body has any nested k-pair-bearing `ClosureRecord` (detected via the new `arm_body_has_k_pair_lambda` walker).
- Arm fn body emit reads frame_ptr from the arm closure record at offset `16 + 8 * captures.len()`, populating `Lowerer::arm_frame_ptr_v`.
- `lower_closure_record` writes `arm_frame_ptr_v` to the lifted lambda's closure record's trailing-triple's third slot.
- `lower_k_pair_call` loads frame_ptr from the closure record at `info.frame_ptr_idx`, calls `sigil_handle_push(frame_ptr)` before the run_loop, and `sigil_handle_pop()` after — re-installing the handler frame so synth-cont chains inside `k(arg)` find the originating effect via `sigil_perform`'s handler-stack walk. The handler frame's heap allocation persists across pop/re-push because `sigil_handle_pop` only unlinks; the closure record's `frame_ptr` slot keeps it GC-rooted.

### 3c-2: Trampoline preserves DISCHARGED through outer_post_arm_k routing

The Bug-2-era "discharged-routing-through-outer-post-arm-k" logic uniformly converted DISCHARGED to DONE at the outermost terminal (since the routing builds a `Call` dispatched to identity, which returns `Done`). For `lower_k_pair_call` driving a synth-cont chain that discharges via an inner arm, this lost the DISCHARGED signal we need to skip return arm dispatch on the R-typed discharge value. **Fix:** the trampoline now bypasses outer_post_arm_k routing when `tag == DISCHARGED` — DISCHARGED propagates to the outermost terminal directly, preserving the tag for `lower_k_pair_call`'s Layer 3a check. Algebraic semantics: when ANY arm discharges, the handle terminates; subsequent computations in the body (including outer chain steps) are abandoned. The Bug 2 routing was correct for multi-shot composition where the outer chain's step expects a post-perform value AND the inner arm RESUMES (not discharges); for the discharge case, routing through the chain conflates terminal semantics.

### 3c-3: closure_convert k-index two-pass

Pre-fix `closure_convert` set `k_closure_idx = filtered.len()` AT the moment `k` was encountered in `raw_caps`. When `k` appeared BEFORE other captures (e.g., `fn (s) => k(arg)(arg)` where the body's free-var traversal sees `k` first as callee), `k_closure_idx` was set to 0 — colliding with env slot 0 (the first regular capture). **Fix:** two-pass — first filter out `k` (recording its `Ty` for `op_ret_ty` / `handler_overall_ty` extraction), then assign `k_closure_idx` / `k_fn_idx` / `frame_ptr_idx` based on the FINAL `filtered.len()`. Order-independent. This bug was latent pre-Layer-3c since prior k-pair tests (rs_b: `k` only, no other captures) didn't exercise the order-dependence; Layer 3c surfaces it because canonical `run_state`'s set arm captures `arg` AND `k`.

### 3c-4: Trailing-pair convention in lower_k_pair_call

Pre-fix, `lower_k_pair_call` only wrote `args[0]` for k(arg) dispatch. When k_fn is a synth-cont (chained-let-yield step), the synth-cont's body expects the trailing-pair convention `[arg, post_arm_k_closure, post_arm_k_fn]` at `args[0..3]` — reading `args[1]` and `args[2]` for its own post-arm-k forwarding. Pre-fix, garbage at `args[1..3]` produced dispatched Calls with null fn_ptrs. **Fix:** `lower_k_pair_call` now writes `(null, identity)` at `args[1..3]` (count=3) — same convention as the arm-fn tail-k emit pattern.

**Tests:**
- `run_state_canonical_higher_order_helper_returns_threaded_value` — the canonical `run_state(5, comp)` with comp doing `set(10); v = get(); v + 1`. Asserts `11`. Composes Bug 2 + Layer 2 + Bug 1 + Layer 3a + Layer 3b + Layer 3c + closure_convert k-index fix end-to-end.
- All prior probes (`rs_a`, `rs_b`, `rs_b1`, `dbg_a`, `rs_l3a`, `rs_l3b`, `rs_l3c`, `rs_l3d`) remain green.

**Test suite:** 132/135 e2e (3 perf flakes pre-existing), 539 compiler unit, 73 runtime — all green. **The canonical `run_state` runs end-to-end.**

**What this closes.** Plan B' Stage 6.8's "examples/state.sigil uses literal `run_state` higher-order helper and the threaded-state output is correct" criterion is now met (subject to landing the state.sigil rewrite in a follow-on commit). The `[DEVIATION Task 109] run_state canonical shape` entry's "What this fix DOES NOT close" residual list (Bug 1, Layer 2, Layer 3a, Layer 3b, Layer 3c) is fully resolved.

**Implementing commit(s):** [HEAD~1] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge (Layer 3c).

## 2026-04-29 — [DEVIATION Stage-6.8-followup non-canonical cleanups] state.sigil canonical, drain leak, Layer 3d, debug-doc removal

**Plan B' Stage 6.8 Task 109 followup, post-Layer-3c.** Closes the four non-canonical residuals identified after the canonical `run_state` runtime fix.

### state.sigil rewritten to canonical `run_state` shape

`examples/state.sigil` now uses the literal `run_state(initial, comp)` higher-order helper, replacing the dual-handle Plan B Task 59 workaround. Canonical body: `comp() { let _ = perform State.set(10); let v = perform State.get(); v + 1 }`. Output: `11`. The e2e test renamed from `state_example_dual_handle_returns_6_then_99` to `state_example_canonical_run_state_returns_11`. Plan B' Stage 6.8 completion criterion is now concretely met by the example file (not just the language surface).

### DEBUG_RUN_STATE.md deleted

The bisect debug-prep doc from `e912315` (Source A / B / C harness) is removed now that all three layers are closed. Architectural narrative lives in this `PLAN_B_PRIME_DEVIATIONS.md` file's deviation entries; the standalone debug doc was a transient bridge.

### Layer 3c outer_post_arm_k drain on DISCHARGED bypass

`sigil_run_loop` snapshots `OUTER_POST_ARM_K_DEPTH` at entry. On the Layer 3c DISCHARGED bypass terminal (which previously returned without draining), the depth is restored to entry-time so synth-cont Middle pushes during the bypassed run don't leak across run_loop boundaries. Pre-fix, leaked entries were consumed via the DONE-path routing on subsequent calls — benign for the canonical (entries from `lower_k_pair_call` are uniformly `(null, identity)`, so routing is identity-passthrough) but architecturally questionable for adversarial nesting and a 32-entry capacity-overflow risk for deep chains. The drain restores stack discipline.

### Layer 3d — return arms with outer captures

`lower_k_pair_call`'s self-apply path now loads `return_closure` from the handler frame at offset `HANDLER_FRAME_RETURN_CLOSURE_OFF` instead of hardcoding `null`. The handle expression's codegen wrote it via `sigil_handler_frame_set_return` at handle codegen time; for return arms with empty outer captures it's null (per the runtime helper's null-for-empty discipline), for non-empty captures it's the closure record allocated by `alloc_arm_closure_record(&ret_captures, None)`. Both cases unify through the frame load. The frame's heap allocation is reachable via `frame_ptr_loaded` (already loaded earlier in `lower_k_pair_call` for Layer 3c's re-push); the slot is GC-rooted via the lifted lambda's closure record.

The pre-fix gate `if synth.captures.is_empty() { ... } else { widened_result }` is removed — the unified path handles both.

**Test added:** `handle_return_arm_with_outer_captures_in_k_pair_dispatch_path`. Caller takes `factor: Int`; return arm body is `fn (s) => v * factor + s` (captures factor). Asserts `f(7) = 28` for `caller(3)` (= `7*3 + 7`). Pre-fix this would have hit the bailout fallback and produced wrong output (or segfault depending on factor's type compatibility).

### Test results

- 133/136 e2e tests ✓ (3 pre-existing perf flakes unchanged)
- 539 compiler unit + 73 runtime tests ✓
- All Stage-6.8-followup tests green: state_example_canonical, run_state_canonical, handle_returning_k_capturing_lambda, handle_with_post_perform, handle_returning_fn_typed_value, handle_with_op_arm_discharge_skips_return_arm, handle_with_op_arm_discharge_skips_constant_return_arm, cps_effected_fn_typed_parameter_indirect_call, handle_with_eager_resume_arms_chains_let_yield, handle_return_arm_with_outer_captures.

**Implementing commit(s):** [HEAD] on `stage-6-8-followup-run-state` against `main` post-PR-#38 merge.


