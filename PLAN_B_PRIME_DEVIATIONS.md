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

## 2026-04-29 — [DEVIATION Task 100] B.1 arg2-op-arg pinning-test inversion deferred to follow-up

**Context:** Plan B' Task 100 specifies inverting two pinning tests:
1. `slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` → positive (3-let arm bodies now ACCEPT).
2. `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen` → positive (arg_i references to user op-args now ACCEPT).

Inversion #1 is delivered in this Task 100 commit (the test is deleted; positive coverage lives in `slice_c_chain_three_let_arm_body_invokes_k_three_times` from Task 99). Inversion #2 requires the arm-side captures-bearing extension — chain step closures must additionally carry the arm fn's user op-args (analogous to B.2 helper-side `ChainedLetBindStep::captures` carrying helper user params). The walker, pre-pass, arm-fn body emit, and Middle/Final synth-fn emit all need updates to thread op-args through the chain's closure records, with bitmap derivation per-slot for pointer-typed op-args. Estimated ~150-200 LOC of new emit code.

**Closure point chosen:** defer inversion #2 to a follow-up commit before Stage 6.7 review checkpoint closes. The current Task 100 commit (this one) lands inversion #1 + legacy-types deletion (Phase D-equivalent for B.1) so the activation work flows through CI cleanly. Inversion #2 + the captures-bearing extension lands as Task 100b before the Stage 6.7 review checkpoint commit.

**Why split:** Task 99's six e2e tests (added in commit `e62aa30` and reaffirmed by the ANF-handling fixup in `96f834a` + walker-lift fixup in `2daf60c`) demonstrate the N-let chain at runtime for chain-internal references (binding-to-binding forward data dependencies). The remaining op-arg-into-chain extension is a separable surface area — Task 99's tests don't exercise it, so the CI-green signal on inversion #1 alone validates the activation discipline. Splitting keeps the commits focused and matches the lockstep-or-feature-gate pattern the B.2 reviewer recommended for activations.

**Failure mode:** if Task 100b is deferred indefinitely, the arg2-op-arg pinning test stays as a rejection test forever, and natural-shape multi-shot demos with op-args (e.g., `Choose.choose(arg, k) => k(arg+10); k(arg+20); ...`) compile-fail. Plan B' Task 101 ("update existing examples to natural shapes") may need to lower its bar accordingly if Task 100b doesn't land in time.

**Implementing commit(s):** Task 100a (this commit): inversion #1 + legacy types deleted. Task 100b (next commit): captures-bearing extension + inversion #2.
