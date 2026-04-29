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

**Implementing commit(s):** Foundation `[HEAD]` (this entry + Stage 6.7 scaffolding); subsequent commits address each task in the order specified by Plan B' (`docs/plans/2026-04-29-sigil-architectural-lifts.md` in designs `in-progress/`). Closeout commits at the end of each stage land the prior-stage hash flips per the Plan B precedent.
