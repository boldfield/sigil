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
