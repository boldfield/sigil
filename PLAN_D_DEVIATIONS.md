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

## 2026-04-30 — [DEVIATION Task 111] Deferred — cross-fn discharge propagation requires shared state, not a per-call mechanism

**Context.** Plan D Task 111 calls for replacing the `LAST_TERMINAL_TAG` / `LAST_TERMINAL_VALUE` thread-local out-channel with a "packed (value, tag) Cranelift multi-return on `sigil_run_loop`." The plan body's framing assumed the TLS was a per-call out-channel that could be inlined into the call return. Three implementation attempts (PR #50) demonstrated this framing is **structurally insufficient**: the TLS's actual semantic role is **cross-function shared state**, not per-call return marshalling.

**Three attempts, three identical failures.** PR #50:

| Attempt | Commit | Mechanism | Failure |
|---|---|---|---|
| 1 | `4dfdbc7` | Cranelift `[I64, I64]` register-pair multi-return + Rust `#[repr(C)] struct { u64, u64 }` return | 10 e2e tests fail with discharge-class shapes |
| 2 | `670f7a1` | Out-pointer ABI (`*mut TerminalResult` arg) + Cranelift `Variable` per-fn last-terminal vars | Same 10 tests fail with same shapes |
| 3 | `5e2686e` | Out-pointer ABI + per-fn `StackSlot` (explicit `stack_store`/`stack_load` memory ops) | Same 10 tests fail with same shapes |

**Diagnostic confirmation.** A diagnostic eprintln commit (`4086307`) on PR #50 surfaced the actual run_loop terminal writes per-test. For `catch_example_recovers_with_42`:

```
[DEBUG run_loop] DISCHARGED bypass: writing (value=42, tag=2) to out=0x7ffc6c12aac8
[DEBUG run_loop] top-level terminal: writing (value=0, tag=0) to out=0x7ffc6c12ab00
```

The DISCHARGED bypass fires correctly with `(value=42, tag=2)`. The bug: the bypass writes to a stack slot at `0x...aac8` (in `risky`'s frame), but the user-main `handle` expression's exit reads a DIFFERENT slot at `0x...ab00` (in `user-main`'s frame, written later by the unrelated `IO.println` run_loop). Per-fn stack slots don't share across function frames.

**Root cause is architectural.**

1. `risky` has `UserFnAbi::Sync` (its body shape — `let result = raise(...); result + input` — doesn't match any of the three Cps body classifiers; the let-RHS is a fn call, not a `Perform`).
2. `risky`'s body sequentially lowers: `lower_call(raise)` → CPS-callee synch wrap → `emit_run_loop_and_capture` allocates **risky's** `last_terminal_slot` and writes (42, DISCHARGED) to it.
3. `risky`'s body then continues: `result = 42; result + input = 49`. `risky` returns 49 via Sync ABI.
4. `user-main`'s `lower_call(risky)` takes the Sync path (direct call, no run_loop drive); body_val = 49.
5. `user-main`'s handle exit reads **user-main's** slot — never written to by risky's discharge — sees `(0, DONE)` → normal path → recovered = 49.

The OLD TLS approach worked because TLS is **thread-global** — risky's run_loop wrote TLS, user-main's handle read TLS, same storage. Per-fn stack slots, register-pair multi-returns, and Cranelift Variables all fail because they're scoped to the immediate caller, not visible across the synchronous call chain.

**Why accepted (deferral over re-attempt).** Closing the cross-fn visibility gap requires either:

- (C) Threading `*mut TerminalResult` through every function ABI as an extra parameter — high-cost refactor, every fn signature gets +1 arg, every call site threads the pointer.
- (D) Reintroducing a Rust `thread_local` for the (value, tag) accessed via FFI helpers — functionally identical to the OLD design, defeats the plan body's stated goal of "no runtime globals."
- A small architectural-doc framework that lets the discharge tag PIGGYBACK on the synchronous Sync-ABI call's existing return value without a separate channel — speculative, requires Sync ABI extension.

Plan D's hard rule "Do not introduce dependencies beyond the existing crate set" and the per-task PR cadence rule both argue against landing such a refactor as a sub-task of Task 111. The motivation for Task 111 (forward-compatibility with Task 117 first-class continuations) does not require the lift to land BEFORE Task 117 — Task 117 can be designed against either the OLD TLS or a future (C/D) shape, and the choice can be informed by Task 117's actual ABI requirements rather than guessed in advance.

**Failure mode.** None at the user-visible surface — the OLD TLS approach continues to work for all e2e tests. The internal motivation for cleanup remains valid but is now scoped as a future task rather than a Plan D blocker.

**Closure path.** Two orthogonal paths are now open:

1. **Defer to a future task that lands alongside Task 117 first-class-k.** Task 117's continuation-as-value lift will modify the same surface area (run_loop terminal channel); a co-shipped lift can use whatever ABI Task 117 settles on without introducing a separate Plan D-internal pivot point. **This is the recommended path.**
2. **Re-scope to option (C) — thread `*mut TerminalResult` through every fn ABI** — as its own multi-PR architectural slice (comparable to Plan B' B.3 TypeExpr::Fn lift). Out of scope for Plan D unless explicitly authorized.

Plan B' Stage-6.8-followup carryover #1 (TLS → packed multi-return) status updates to "deferred to Task 117 follow-up or a future architectural slice; closure path described in `[DEVIATION Task 111]`."

**Implementing commit.** [HEAD] (this entry).

**Reverted commits (do NOT cherry-pick):** `4dfdbc7`, `670f7a1`, `5e2686e`, `4086307` — all on the abandoned `plan-d-task-111` branch (closed without merge per PR #50). The branch is preserved for the diagnostic record.

## 2026-04-30 — [DEVIATION Task 112] Deferred — wrapper-fn-frame composition is structurally similar to Task 111, defer alongside it

**Context.** Plan D Task 112 calls for a "wrapper-fn-frame composition fix" that closes `[DEVIATION Task 72]` constraint #3 and un-ignores `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix`. The plan body framed this as a "narrow, well-pinned" Stage 11 foundation lift. Investigation surfaced architectural complexity comparable to Task 111.

**Bug shape.** `std/state.sigil`'s discharge-with-lambda arm bodies (`State.set(arg, k) => fn (s) => k(arg)(arg)`) only thread state correctly when the body has the **chained-let-yield** shape (let-perform; let-perform; tail), where the body is Cps and the perform-chain lifts into synth-cont steps. The arm's captured `k` IS a synth-cont step; when state_fn(initial) invokes `k(arg)(arg)`, it re-enters the synth-cont chain at the perform site to thread state.

With wrappers (`set_state(s) = perform State.set(s)`), the calling fn's (e.g., `comp`'s) body shape becomes `let _ = fn_call(args); let v = fn_call(); tail` — **Sync ABI**. Sync calls have run_loop drives at each call site but no synth-cont chain. `set_state` itself is Cps tail-perform, but its emitted `k` is `continuation_identity` (not a chain step). When state_fn(5) invokes `k(10)(10)`, k=identity returns 10, and `10(10)` is a fn-call on an Int — producing the observed "5" via runtime garbage (likely a jump to address 5).

**Why architecturally similar to Task 111.** Both Stage 11 tasks turn out to require cross-fn behavior:

| Task | Structural issue |
|---|---|
| 111 | Cross-fn discharge tag visibility (TLS achieves it implicitly) |
| 112 | Cross-fn synth-cont chain (the wrapper Sync call breaks the chain) |

**Fix paths considered:**

- **(A) Inline `is_simple_tail_perform_with_pure_args_body` wrappers at the call site.** Extend the chained-let-yield body recognizer to treat `let _ = wrapper_call(args)` as `let _ = perform E.op(args)`; emit the chain accordingly. Localized but real codegen change.
- **(B) Wrapper-frame-aware continuation walk** in the discharge-with-lambda machinery. Substantial rework.
- **(C) Defer alongside Task 111.** Stage 11 collapses to no foundation lifts shipped; both tasks land alongside Task 117 first-class-k where the broader continuation surface is open for redesign.

Option (C) chosen by user direction (2026-04-30) on the same architectural-complexity grounds as Task 111. The inline-perform shape (`examples/state.sigil` and `std_state_run_state_set_get_returns_11`) continues to work; user-visible state-threading is preserved. Wrappers stay deferred without breaking anything currently passing.

**Why accepted (deferral over re-attempt).** Quality-of-life improvement, not a correctness-of-existing-tests gate. Inline-perform shape continues to work for state-threading. JSON parser part 2 (Plan C completion's Task 80 part 2), originally cited as the smoke-gate downstream consumer of Task 112, continues to defer with this entry — the parser's recursive-descent shape that needed the wrapper-fn-frame fix can wait for Task 117's broader continuation work.

**Failure mode.** None at the user-visible surface. The `#[ignore]`'d test `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix` stays `#[ignore]`'d.

**Closure path.** Same closure path as `[DEVIATION Task 111]`:

1. **Recommended:** defer to Task 117 first-class-k follow-up. The continuation-surface rework Task 117 entails is the natural co-ship point; whichever architectural choice Task 117 settles on can subsume both Task 111 and Task 112's cross-fn requirements.
2. **Alternative:** ship option (A) wrapper-inline as its own task. Comparable scope to Plan B' B.3 surface lifts.

**Stage 11 implication.** Stage 11 ("foundation lifts: Tasks 111 + 112") has both tasks deferred. Plan D effectively skips Stage 11 and proceeds directly to Stage 12. The Stage 11 review checkpoint is replaced by a single deferral checkpoint covering both tasks.

**Smoke-gate impact.** JSON parser part 2 (Plan C completion's Task 80 part 2) was named as the Stage 11 smoke target via Task 112. With Task 112 deferred, JSON parser part 2 stays deferred to Plan C completion's broader v2 follow-up. Plan D's done-criteria #3 (Sudoku + JSON parser half compile and run) is partially scoped down: the architectural cluster lands without these specific demo gates; the demos remain expressible-after-Plan-D for the components Plan D ships, with Sudoku and JSON parser deferring on the Task 117 / 112 axes respectively.

**Implementing commit.** [HEAD] (this entry).

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

## 2026-04-30 — [DEVIATION Task 113] Per-clone `match_scrut_tys_resolved` map; arity-1 tuple value rejection; `MAX_TUPLE_ARITY` named constant; `expr_is_pure` allows tuples of pure elements

**Context.** Task 113 shipped tuple syntax + `std/pair.sigil` across PR #53. R1 review surfaced two bugs and several documentation / API-shape cleanups. Both bugs and all cleanups landed within PR #53 ahead of merge per the per-task-PR cadence.

**Findings.**

### Finding 1 — `(e,)` produced an arity-1 `Expr::Tuple`

The value-side parser at `compiler/src/parser.rs:1080+` accepted a trailing comma after a single element and emitted `Expr::Tuple` with `elems.len() == 1`. The AST docstring documents arity ≥ 2; the codegen `debug_assert!(n <= MAX_TUPLE_ARITY)` doesn't catch arity-1 (1 ≤ 31), so an arity-1 heap object was synthesized that the type system has no surface spelling for. Type-side parsing was correct (the `while !RParen` loop eats trailing commas without recursing).

**Fix.** Parser explicitly rejects arity-1 tuple values with a diagnostic naming the policy: "tuple values require arity ≥ 2 — `(e,)` with a trailing comma is not a valid tuple. Use `(e1, e2, ...)` for a tuple, or remove the trailing comma to write a parenthesised expression `(e)`." Pinned by `parser_rejects_arity_one_tuple_with_trailing_comma`.

### Finding 2 — `match_scrut_tys` is span-keyed and shared across mono clones; non-Ident scrutinees in generic fns leak `Ty::Var(_)` into codegen

The pre-fix `Lowerer.local_var_tys` papered over the symptom for `Expr::Ident` scrutinees only. A `Call` / nested `Match` / etc. scrutinee in a generic fn fell back to `match_scrut_tys[span]` which (per its docstring) is shared across mono clones and stale for generic clones — `Ty::Var(_)` reached `cranelift_ty_of_ty`'s unreachable.

**Fix.** Per-clone `match_scrut_tys_resolved: BTreeMap<(String, Span), Ty>` populated by `monomorphize` for every `Expr::Match` rewritten inside a generic clone. Codegen's `lower_match` and `type_of_expr`'s Match arm look up `(current_fn_name, span)` first; fall back to span-keyed for non-clone surfaces. Removes the `Ident`-vs-non-Ident discrimination entirely. `local_var_tys` is gone. Pinned by `generic_tuple_scrutinee_via_call_resolves`.

**Synth-fn gap.** Synth helper fns produced by closure_convert (lifted lambdas, handler-arm fns, sync-shim fns, post-arm-k continuations) have a `current_fn_name` distinct from the originating clone fn — the per-clone resolved map is keyed by the clone fn name, so the synth fn's `(synth_name, span)` lookup misses and falls back to the span-keyed side-table. For non-generic synth bodies this is correct (the side-table entry has no `Ty::Var`). For synth fns whose body inherits a Match expression from a generic clone (i.e., a generic fn with a handler block whose arm contains a tuple match), the fallback would still leak `Ty::Var`. **No test exercises this gap today** — it is logged for closure if Tasks 114–116 (which extend the generic surface) surface a failure. Closure path: thread the originating clone fn name into each synth struct (or the closure_convert side-table) and key the resolved-map lookup by the parent clone name when emitting synth fn bodies.

### Finding 3 — `MAX_TUPLE_ARITY` documented as 63 but actually 31; offset documentation said `16+8*i` but actual is `8+8*i`

`header-constants/src/lib.rs:114` referenced a `MAX_TUPLE_ARITY = 63` constant that didn't exist; the actual cap is 31 (32-bit pointer bitmap, one bit reserved). `compiler/src/ast.rs:492` and `compiler/src/typecheck.rs:75` both said tuple elements live at offsets `16+8*i` (sum-type ctor layout); the actual layout is `8+8*i` (no discriminant word — tuples have one ctor per arity). The codegen tuple ctor's "32-bit pointer_ty future-proofing" branch was dead code (sigil targets only 64-bit).

**Fix.** Added `pub const MAX_TUPLE_ARITY: usize = 31;` in `header-constants` with a `max_tuple_arity_matches_pointer_bitmap` test pinning the value vs `BITMAP_BITS`. Codegen's `debug_assert!` reads the constant. Docstrings corrected. Dead 32-bit branch removed.

### Finding 4 — `expr_is_pure` returned `false` for `Expr::Tuple` even when elements are pure

The perform-side classifier rejected helper bodies producing tuple values as not-pure even when every element is a literal / Ident — symmetry with `Expr::RecordLit`'s `all-elements-pure` shape was missed.

**Fix.** Flipped to `elems.iter().all(expr_is_pure)`. Heap allocation alone doesn't break purity in this classifier's sense — `RecordLit` already returns true under the same shape.

**Why accepted.** All four findings are within-PR cleanups that don't change any user-visible test outcome but tighten the surface and remove the per-Lowerer `local_var_tys` band-aid in favor of the structural per-clone fix. Per the per-task-PR cadence, addressed in PR #53 directly rather than a follow-up.

**Failure mode.** Synth-fn-inheriting-from-generic-clone gap (Finding 2) — already described above; no test surfaces it today.

**Closure path.**

- Findings 1, 3, 4 — fully closed by the cited code changes.
- Finding 2 — structurally closed for user-fn surface; synth-fn surface gap awaits Tasks 114–116 if exercised.

**Implementing commit.** `[HEAD]` (this entry + the four code fixes).

## 2026-04-30 — [DEVIATION Task 114] EffectRef/EffectInst split mirrors Tuple; perform-site E-substitution deferred to Task 115

**Context.** Plan D Task 114 introduces type-parameterized effect rows. The plan body specified one structural addition — `RowEntry::Effect { name, args, span }` at the AST — but the practical migration extended into a parallel Ty-level type for clean unification + display.

**Architectural shape.**

- **AST level** — `ast::EffectRef { name: String, args: Vec<TypeExpr>, span: Span }` carries source-attributed effect references. Three AST sites flip to `Vec<EffectRef>`: `FnDecl.effects`, `FnTypeExpr.effects`, `Expr::Lambda.effects`.
- **Ty level** — `typecheck::EffectInst { name: String, args: Vec<Ty> }` (no span — Ty-level structures are span-free across the codebase). `FnSig.effects` and `Row.effects` flip to `Vec<EffectInst>`.

This mirrors the Plan A3 / Plan D Task 113 pattern: `TypeExpr::Tuple { elems, span }` (AST) parallels `Ty::Tuple(Vec<Ty>)` (Ty). The boundary helpers `effect_refs_to_insts` / `insts_to_effect_refs` translate at AST↔Ty crossings.

**Why structural EffectInst over a flat names-list with parallel args.** The pre-114 surface stored rows as `Vec<String>` and matched on string-set diff via `BTreeSet<&String>`. To support `Raise[Int]` distinctly from `Raise[String]`, structural matching is required — two rows sharing a name but instantiating differently must compare unequal. Carrying args structurally on a single `EffectInst` makes `Vec` containment via `iter().any(|e| e == target)` a one-pass diff; alternative shapes (parallel `Vec<Vec<Ty>>` for args, or external arg-table keyed by name) tangle invariants and make `Row::canonicalise` ambiguous.

**`Ord` not derived on `EffectInst`.** `Ty` itself has no total order (Plan B' decision: equality is well-defined but a total order would require choosing among many `FnSig` shapes). `Row::canonicalise` sorts by `name` first (the dominant disambiguator; bare-name effects like `IO`, `Mem` are uniquely identified by name) then dedups by full structural equality — distinct instantiations of the same effect-decl name remain in the row.

**Perform-site E-substitution gap (deferred).** Today, `perform Raise.fail("oops")` under `![Raise[String]]` does NOT thread `E := String` into `fail`'s op-arg unification at the perform site. The op signatures are checked under the effect-decl's local `generic_subst` (built at the effect-decl pre-pass via `fresh_generic_subst`), so for now perform-site arg typing succeeds at op-level `Ty::Var` without the row-site substitution.

**Why deferred.** Threading the row-site type-args into the op-call substitution is intertwined with Task 115 (per-op generic params: `fail[A]: (E) -> A`) — once `fail` itself can be generic per-call, the substitution machinery at the perform site has to handle BOTH effect-decl-level generics AND per-op generics in one step. Doing the effect-decl-only substitution now and re-doing it for per-op generics later would double-touch the same call paths. Task 115 closes both at once.

**std/raise.sigil migration to `effect Raise[E] { fail: (E) -> A }`** also defers to the Stage 12 review checkpoint for the same reason — the v2 shape relies on per-op generics for fail's `A` return type. Today's std/raise.sigil ships with concrete-String per `[DEVIATION Task 71]`; the migration lands as a Stage 12 review item once Tasks 114 + 115 + 116 are all shipped.

**E0140 arity check.** Three message shapes:
1. *Non-generic effect-decl referenced with args*: "effect `IO` is not generic — drop the type-arg list to write `IO` instead of `IO[Int]`".
2. *Generic effect-decl referenced bare*: "effect `Raise` is generic over [E] — write `Raise[E]` with explicit type arguments (bare `Raise` refers to the un-instantiated declaration)".
3. *Arity divergence*: "effect `Raise` is declared with 1 type parameter(s) [E], but 2 argument(s) were provided in the row site".

**Failure mode.** None at the user-visible surface for non-generic effects. Generic effect-decls are now expressible at row sites; the runtime smoke gate (`std/raise.sigil` end-to-end) defers to Task 115 + Stage 12 review per the closure path above.

**Closure path.**

- **Stage 12 review checkpoint** — std/raise.sigil migration; std/state.sigil tuple-return + generic E migration; std/result.sigil generic-error update.
- **Task 115** — per-op generic params close the perform-site substitution gap.

**Implementing commit(s).** PR commits across `plan-d-task-114` branch.

## 2026-04-30 — [DEVIATION Task 115] E0140/E0143 audit fix; per-op generics shadowing E0144; perform-site E-substitution closure (Task 114 R1)

**Context.** Task 115's PR (#55) ships per-op generic params on user-declared effects (`fail[A]: (E) -> A`) and closes the Task 114 R1 deferred perform-site E-substitution gap. The PR also surfaced two audit findings during execution that warrant log entries here so future readers (and `gh pr view` of merged PR descriptions) can trace the cross-task context.

**Audit finding 1 — E0140/E0143 code collision.**

Task 114 (PR #54) introduced a row-arg arity-mismatch diagnostic and allocated it as **E0140**. E0140 was already taken by Plan B Task 54's *duplicate-handler-arm* code. Both lived in the catalog briefly, with the second registration silently shadowing the first. The bug was masked because the duplicate-arm test asserted `has_code(&errs, "E0140")` (true regardless of which entry served), and Task 114's row-arity tests asserted the same (also true). The catalog has a build-time invariant check, but it didn't trip because the registration happened in two unrelated arrays whose dedup wasn't enforced cross-array.

Task 115's PR catches it during the per-op-generics implementation, where `E0144` was the next available code in the 0140-series — prompting a recount of 0140-0144 and discovery of the conflict. **Fix:** migrated row-arg arity from E0140 → E0143 with full catalog entry. The duplicate-handler-arm code stays at E0140 unchanged. Tests renamed `*_fires_e0140` → `*_fires_e0143`. Doc rot in `ast.rs`, `typecheck.rs`, and test docstrings swept to E0143 references. Catalog entry for E0143 explicitly notes: "Plan D Task 114 introduced this check; Plan D Task 115 (PR #55) renamed the code from E0140 → E0143 to disambiguate from the existing E0140 (duplicate-handler-arm). A future agent reading older PR descriptions / commit messages will see references to the original E0140 number and should treat them as referring to this diagnostic."

**Audit finding 2 — `check_handle` per-op generic layering bug.**

The PR R1 review caught a real bug: `check_handle` resolved arm op param / return types under the effect-decl substitution **only** — no per-op generic layer. For an op declared `fail[A]: (E) -> A`, `ty_from_type_expr_here` couldn't find `A` in `current_generic_subst` and returned `None`, falling back to `Ty::Unit`. Two silent miscompiles followed:

- `k_param_ty` (the continuation's arg type) collapsed to `Ty::Unit`, so `k(int_value)` would fire E0044 against `Unit`, or worse, `k(())` would silently typecheck under a wrong contract.
- `user_param_tys[i]` for any per-op-generic-typed binding (e.g. `op[A]: (A) -> Int`) collapsed to `Ty::Unit`.

None of the new Task 115 typecheck tests exercised `handle` over a per-op-generic op, so the original PR corpus didn't flag the bug.

**Fix:** mirror the per-op `fresh_generic_subst` + insert pattern from `check_perform` inside the arm-typing block at `compiler/src/typecheck.rs:4287-4295`. Layer per-op generics on top of `eff_subst` before computing `user_param_tys` / `op_ret_ty`. Added regression test `handle_arm_over_per_op_generic_op_typechecks` exercising `handle 0 with { Raise.fail(e, k) => k(42) }` for `effect Raise[E] { fail[A]: (E) -> A }` — the call `k(42)` requires `k` to type as `Fn(A_var) -> Int`, not `Fn(Unit) -> Int`.

**Audit finding 3 — perform-site E-substitution closure (Task 114 R1 deferred gap).**

Task 114 had a deferred gap: at `perform Raise.fail("wrong type")` under `![Raise[Int]]`, the row-site type-args `[Int]` weren't threaded into `fail`'s op signature. The op was checked under a fresh Ty::Var for E, so wrong-typed args silently bound `E := String` instead of firing E0044 against the row-instantiated `E := Int`. Task 114's PR pinned this with `perform_site_e_substitution_deferred_to_task_115` — a closure-point test marked **INVERT THIS TEST AT TASK 115 LANDING**.

**Fix:** `check_perform` consults the surrounding fn's row entry for the effect; if its args match the effect-decl's arity, builds the substitution from them (precedence: handler-scope subst → fn-row args → fresh). The deferred test inverts to `perform_site_e_substitution_closed_by_task_115`, asserting E0044 fires.

**E0144 introduction.** Per-op generic param shadowing an effect-decl one fires E0144. Catalog entry covers the shadowing rule with a fix-example using canonical Koka idiom (`E` for effect-decl, `A` for per-op).

**Why accepted.** All three findings land in PR #55 within the per-task-PR cadence — the per-op-generics surface and the perform-site E-substitution are tightly coupled (both flow through `check_perform`'s substitution machinery), and the E0140/E0143 audit was discovered during Task 115's catalog walk so it's natural to fold it in here rather than defer. The R1 review caught the `check_handle` gap before merge; the fix + regression test land in the same PR.

**Failure mode.** None remaining at the user-visible surface for the cases shipped. std/raise.sigil migration to `effect Raise[E] { fail[A]: (E) -> A }` continues to defer to Stage 12 review per the plan body.

**Closure path.** Stage 12 review checkpoint — std/raise.sigil + std/state.sigil + std/result.sigil migration to use the now-expressible generic shapes; closure-path edits to `[DEVIATION Task 71]` constraint #2 (`fail`'s concrete-Int return placeholder) and `[DEVIATION Task 72]` constraint #1 (parser surface for `![State[S]]`).

**Implementing commit(s).** PR #55 commits across `plan-d-task-115` branch.

## 2026-04-30 — [DEVIATION Stage 12 review] Sign-off + handler-discharge type-arg propagation gap

**Context.** Plan D Stage 12 ships four type-system surface lifts (Tasks 113 tuples, 114 type-parameterized effect rows, 115 per-op generic params, 116 row-polymorphic Fn parameters). Per the plan body, the Stage 12 review checkpoint requires human review of: (1) AST shape consistency, (2) diagnostic quality, (3) closure-path edits to `[DEVIATION Task 71/72]`, (4) stdlib migration. This entry records sign-off across all four items, plus surfaces a deferred gap discovered during the stdlib-migration attempt.

**Item 1 — AST shape consistency.** ✅ `EffectRef { name, args, span }` (AST) / `EffectInst { name, args }` (Ty) mirror Plan A3's `TypeExpr::Tuple` / `Ty::Tuple` split — spans on the AST side, span-free on the Ty side. `EffectOp.generic_params: Vec<GenericParam>` parallels FnDecl's existing field. `FnTypeExpr.effect_row_var` (pre-existing Plan B' Stage 6.8 Task 103) gained the binding-side wiring through `current_row_var_subst` in Task 116. Generic-param scoping: per-op generics layer on top of effect-decl generics with E0144 shadow check.

**Item 2 — Diagnostic quality.** ✅ 5 codes:
- **E0117** (tuple-pattern arity, Task 113) — span at the pattern.
- **E0140** (duplicate handler arm, pre-existing Plan B Task 54) — span at the second arm. Task 114 mistakenly used E0140 for row-arg arity (collision); Task 115 renamed that to E0143. Catalog entry for E0143 documents the rename.
- **E0143** (row-arg arity, Task 114, code introduced by Task 115's rename) — span at the row entry.
- **E0144** (per-op generic shadowing effect-decl generic, Task 115) — span at the per-op generic-param decl.
- **E0137** (narrowed by Task 116 from "any row-var-bearing fn-type" to "unbound row var only"; pre-existing code) — span at the unbound row-var token. Fix-suggestion renders valid Sigil syntax.

**Item 3 — Closure-path edits.** ✅
- `[DEVIATION Task 71]` (PLAN_C_DEVIATIONS.md) — constraints #1, #2, #3 marked **Closed** by Plan D Tasks 114, 115, 116.
- `[DEVIATION Task 72]` — constraints #1, #2, #4, #5 **Closed** by Tasks 114, 113, 115, 116. Constraint #3 (wrapper-fn-frame) stays **Deferred** per Plan D Task 112; closure path is Task 117 follow-up.
- `[DEVIATION Task 73]` — constraints #1, #5, #6 **Closed**; #2, #3, #4 (multi-shot codegen) stay open, addressed by Tasks 117/118.

**Item 4 — Stdlib migration: std/raise.sigil shipped; std/state + std/result deferred to Plan C completion.**

The first commit of this PR took the conservative interpretation of the plan-overview separation and deferred the entire stdlib migration. R1 reviewer pushed back: *"you didn't include the stdlib migration"*. R2 review accepted std/raise as in-scope (the plan body asks "Are stdlibs updated to use the now-expressible generic shapes?" as a Stage 12 review criterion); the migration ships in this PR. std/state + std/result remain deferred — both have additional shape considerations (state's discharge-with-lambda pattern under generic E + tuple return; result's existing surface needs only a verification pass not a migration).

**What shipped in this PR for std/raise**:

- `effect Raise[E] { fail[A]: (E) -> A }` — generic over error type E (Plan D Task 114) + per-op return-type generic A (Plan D Task 115).
- `raise[A, E](e: E) -> A ![Raise[E]]` — fully polymorphic.
- `catch[A, E](body: () -> A ![Raise[E] | e]) -> Result[A, E] ![| e]` — row-polymorphic via the outer fn's `effect_row_var` (Plan D Task 116).
- 5 e2e tests + 3 typecheck tests updated from `![Raise]` to `![Raise[String]]`.
- 2 example files migrated (`examples/catch.sigil`, `examples/interpreter.sigil`).
- `raise_int_return_in_string_returning_fn_fires_e0044_v1_gap_pin` inverted to `_typechecks_post_task_115` — the v1 gap is closed.

**Three architectural gaps surfaced + fixed during the migration:**

1. **Handler-discharge type-arg propagation** (initially documented as deferred, now fixed). `Tc::check_handle` pushed bare-name `EffectInst::bare(e)` into body_row at the discharge site, losing the discharged effect's instantiation. **Fix**: at the discharge site, look up the effect-decl's `generic_params` and use the active handler subst (`effect_substs[name]`) to recover the args. The handler arm's existing op-typing already allocates these substs; reusing them ensures the body row's `Raise[E_var]` matches body's expected `Raise[E_body_var]` via subsume_row's arg unification. Falls back to bare-name (no args) when the effect-decl declares no generic_params — preserves pre-Stage-12 behavior for non-generic effects (`IO`, `Mem`).

2. **`Tc::rename_ty` Ty::Fn arm didn't rename Ty::Var ids inside EffectInst args.** At scheme instantiation, a row carrying `Raise[E]` left `Var(E_decl_id)` unrenamed; the fresh `Var(E_fresh)` from arg unification was disconnected from the row. **Fix**: rename_ty now walks each EffectInst's args with the same ty_map.

3. **`Subst::apply_ty` and `Subst::apply_row` cloned EffectInst args without applying the substitution.** Symmetric with #2 but at substitution-application sites instead of scheme-instantiation sites. Without this, `apply_ty(Ty::Fn(.. effects: [Raise[Var(N)]] ..))` returned the row with `Var(N)` still unbound even after the unifier bound it elsewhere. **Fix**: both sites now walk EffectInst args via `apply_ty_inner`.

**`unify_row` / `subsume_row` rewrite — name-based matching with arg unification.**

The structural EffectInst-equality diff (Task 114) was correct for concrete-args rows but wrong for Ty::Var-bearing rows: `Raise[Var(N)]` and `Raise[Concrete]` should unify N := Concrete, not error out. The rewrite matches by name first, then unifies args pairwise via `unify_ty`. R3 reviewer flagged that the original silent-skip-on-arity-mismatch was a soundness hole; fixed: arg-arity mismatch now fires E0042 (subsume_row) / E0128 (unify_row) with explicit arity-mismatch messages.

`unify_row` and `subsume_row` thread `unify_ty`'s bool return through to the overall return value — a false from arg-unification (E0044) now fails the row check, not just pushes the error and returns true.

**Diagnostic shape change**: `cross_fn_row_with_distinct_type_args` (Raise[Int] caller calling Raise[String] callee) previously fired E0042 ("row mismatch"); now fires E0044 ("Int vs String type mismatch") at the arg-unification step. The new diagnostic is more precise (points at which arg type is wrong rather than just "row mismatch"). E0042 catalog entry kept as-is — it still fires for the missing-effect case and the new arg-arity-mismatch case at subsume_row.

**Stage 12 sign-off**. ✅ Tasks 113/114/115/116 + closure-path edits + std/raise migration + handler-discharge / scheme-rename / subst-application gap fixes land. std/state + std/result migrations remain deferred to Plan C completion (state needs the lambda-discharge under generic E exercise; result needs only verification).

**Estimated cost reconciliation** (R2 reviewer noted "1-2 sites" was an underestimate). Actual: 4 sites + ~150 lines:
- `apply_ty_inner` Ty::Fn arm — args walking.
- `apply_row_inner` — args walking on r.effects + resolved-row chain.
- `rename_ty` Ty::Fn arm — args renaming.
- `unify_row` / `subsume_row` — name-match-with-arg-unify rewrite.
- `check_handle` — discharge-site EffectInst-with-args push.

The original 1-2 sites estimate was based on a "discharge site only" closure path; the actual fix needed substitution / renaming / row-matching to compose correctly. This pattern (changes at multiple typing layers had to land together) recurred across Tasks 114/115/116 R1 reviews; the cost-estimation lesson is real.

**Implementing commit(s).** This entry + Tasks 71/72/73 closure-path updates in PLAN_C_DEVIATIONS.md + std/raise.sigil migration + handler-discharge/scheme-rename/subst-application fixes. PR `plan-d-stage-12-checkpoint`.

## 2026-05-01 — [DEVIATION Task 117 design validation] Eta-expansion proposal needs validation; split withdrawn

**Context.** Pre-execution recon (2026-05-01, sigil/main `0ff2c3a`) initially proposed splitting Task 117 into 117a (lifted-lambda generalization) + 117b (k-stored-in-record + k-as-fn-arg + smoke gate). User authorized the split. During the design pass, a cleaner mechanism surfaced: **eta-expansion at closure_convert** (rewrite `Expr::Ident(k_name)` in value position to `(fn x => k(x))`; the existing Plan B' Task 107 Phase B `ArmKPairCapture` substrate lifts the lambda; standard indirect-call dispatch reaches `lower_k_pair_call` via the lifted lambda's `arm_k_pair_self`). The eta-expansion proposal would subsume 117a *and* 117b in one mechanism with no codegen changes.

**Why the split is withdrawn.** Brian (2026-05-01) pushed back: *"'no codegen changes needed' against a plan-estimated 6-PR architectural slice is a red flag, not a feature."* The 117a/117b split was scoped under the premise *"117a is mechanically simple, 117b verifies harder cases."* If 117b's harder cases (k-stored-in-record, k-as-fn-arg, multi-shot, frame escape) reveal that eta-expansion fails for them, 117a would have shipped under a design that can't generalize — a sealed sub-PR baking in a wrong choice. Single Task 117 PR after validation completes.

**Three validation tests required before any production code lands.** Each test confirms-or-fails a load-bearing semantic of the eta-expansion design:

1. **Multi-shot through let-bound k.** `let f = k; let r1 = f(true); let r2 = f(false); r1 + r2` inside a `resumes: many` arm. First confirm it fails today (walker rejects `Expr::Ident(k_name)` at `codegen.rs:1556-1571`). Then implement minimal eta-expansion and verify multi-shot produces chained-resume semantics, not N independent trampoline drives. If through-a-lambda forces each resume into its own `sigil_handle_push` + run-loop + pop cycle, eta-expansion is single-shot-only and the design is dead.

2. **Frame escape past handle pop.** `let f = k; return f from inside the handle; invoke f after the handle's `sigil_handle_pop` has run.` The closure captures `frame_ptr` at perform time; that pointer references popped stack memory after the handle exits. `lower_k_pair_call:11479-11484` doesn't re-install popped frames. Verify behavior — works (frame discipline holds), segfaults, or returns garbage. Binary outcome.

3. **Arena escape rate** (only if 1 and 2 pass). Eta-expansion adds an allocation per `let f = k` site; bare-k baseline is zero-alloc. Measure on a representative multi-shot-heavy input. >5% escape-rate jump trips Plan D's HARD perf gate.

**Decision rule.** Pass all three → ship eta-expansion as the single-PR Task 117 implementation. Fail any → fall back to **conservative path**: distinct `Ty::Continuation` (code + closure + frame triple) propagated through typecheck and consumed directly by `lower_k_pair_call`. That's the architectural lift Plan D budgeted for; don't half-and-half a clever shortcut with a real ABI fix in the same PR.

**Foundation commit `e2bc2fb` is superseded by this entry.** The split notation in `PLAN_D_PROGRESS.md` Stage 13 is reverted to "single Task 117, in-validation". Branch name `plan-d-task-117a` retained for git history continuity; PR #59 stays open as the single Task 117 PR's container; the title will be retitled once validation lands.

**Implementing commit.** [HEAD] (this entry + PROGRESS revision).

## 2026-05-01 — [DEVIATION Task 117 split into 117a + 117b] Pre-authorized split surfaced before code-write [SUPERSEDED 2026-05-01 — see Task 117 design validation entry above]

**Context.** Plan D §74 pre-authorizes splitting Task 117 into 117a/117b/... if any of (a) diff exceeds Plan B' PR #38/#39 scope before smoke gate is reachable, (b) more than two distinct test-failure classes surface simultaneously, (c) the lifted-lambda closure-record discipline diverges from the existing N-chain `post_arm_k` substrate. **Stop and re-scope with the user** is reserved for cases where the split is unclear or the cluster requires an architectural lift not enumerated.

A pre-execution recon (sigil/main `0ff2c3a`, branch `plan-d-task-117a`) identified that criterion (a) is structurally certain to fire if Task 117 is attempted as a single PR. The recon partitioned the smoke-gate work (`std/choose.sigil` `all_choices` discharger) into three structurally distinct mechanisms, each with independent ABI / closure-record discipline / test surface:

1. **Lifted-lambda generalization** — drop the `Expr::Ident(k_name)` reject at `compiler/src/codegen.rs:1556-1571`; allow `k` to flow as a value through let-bindings inside the arm body, materialized as a closure-record-shaped value carrying `(k_closure, k_fn)` per the existing `ArmKPairCapture` (`compiler/src/closure_convert.rs:104-149`) + `lower_k_pair_call` substrate. The existing machinery (Plan B' Task 107 Phase B) handles this for syntactically-nested lambdas whose body calls `k(arg)`; this generalization extends to `let f = k; f(arg)` shape (k bound to a fn-typed local, then invoked indirectly).
2. **k-stored-in-record** — k as a slot in a TAG_RECORD value (different layout / bitmap discipline from TAG_CLOSURE).
3. **k-passed-as-fn-arg** — k flows through a regular fn-decl parameter slot. Mutates *callee signatures*, not just closure records.

PR #38 (Task 97/98 N-chain post_arm_k) shipped a single mechanism with extensive deviations; PR #39 (Stage 6.8 followup) bundled six layered bugs and was the largest single PR in the project. Three structurally distinct mechanisms in one PR puts the diff comfortably past PR #39 scope.

**Split scope:**

- **Task 117a — lifted-lambda generalization** (this branch). Walker delta + closure_convert generalization (≈80% of substrate already in place via `ArmKPairCapture`) + minimal `lower_call` change. Acceptance: arena-escape gate (`arena_escape_count_is_zero_below_one_percent_ceiling` at `compiler/tests/e2e.rs:584-728`) stays at 0; existing tests pass; one new positive test (`k captured into fn-typed local then invoked indirectly`).
- **Task 117b — k-stored-in-record + k-passed-as-fn-arg + smoke gate.** Builds on 117a's now-validated lifted-lambda generalization. New ABI for fn params taking k-pairs; new TAG_RECORD slot encoding for k-stored. Smoke gate: `all_choices` discharger compiles and runs against `std/choose.sigil`. Targeted tests: k-stored-in-record positive, k-as-fn-arg positive, arena-reset across N-resume chain.

**Rationale for surfacing pre-execution rather than mid-PR.** The plan body's "stop and re-scope" trigger is reserved for cases where the split itself is unclear. The split above is clear (orthogonal mechanisms by k-shape; existing substrate disambiguates 117a vs 117b cleanly). Per `feedback_sigil_per_task_pr_cadence.md` ("default is one task per PR; bundling requires explicit per-session user authorization"), the split is the natural per-task cadence; bundling 117 as a single PR would require explicit user authorization, not the other way around.

User authorized the split per session 2026-05-01 ("sounds good" on the surfaced split recommendation).

**Closure paths:**

- **Task 117a closure point**: `compiler/src/codegen.rs:1556-1571` (the `Expr::Ident(k_name)` reject in `arm_body_walk`); `compiler/src/closure_convert.rs:104-149` (`ArmKPairCapture` substrate).
- **Task 117b closure point**: same `arm_body_walk` walker for the fn-arg / record-slot rejections; new closure-convert pass for fn-arg k-pair representation; new codegen path for record-slot dispatch.

**Performance acceptance gate.** Plan D Task 117 acceptance gate (post-117 arena escape rate ≤ Plan B Task 60 baseline of 0% on single-shot, ≤ multi-shot driver's existing ceiling) applies to **117a's PR** in addition to 117b's. The split does not relax the perf gate; it just ships it twice (once per sub-PR).

**Failure mode.** None at sigil/main today. The split lands as a PROGRESS / DEVIATIONS bookkeeping change.

**Implementing commit.** This entry + `PLAN_D_PROGRESS.md` Task 117 status update splitting into 117a/117b. No code changes in foundation commit.

## 2026-05-01 — [DEVIATION Task 117] Slice C recognizer constructor-purity fix

**Context.** Plan D Task 117's Sudoku smoke gate exercises a 4×4 backtracking solver whose handler arm body has the canonical Slice C 2-let shape with a constructor-bearing tail:

```sigil
Branch.branch(k) => {
  let r1: Option[Array[Int]] = k(true);
  let r2: Option[Array[Int]] = k(false);
  match r1 {
    Some(s) => Some(s),
    None => r2,
  }
}
```

The first attempt at compiling this shape on `plan-d-task-117a` (commit `82740c5`, Sudoku ArithError-row fix) produced an unexpected codegen rejection:

> `handle` expression at … has arm `Branch.branch` body that uses continuation `k` in non-tail position outside the supported shapes.

This was surprising — the shape matches `arm_body_n_let_then_pure_tail_shape`'s recognized 2-let pattern (Plan B' Stage 6.7 N-chain Slice C). Investigation revealed a latent over-conservative check in `expr_is_pure` (`compiler/src/codegen.rs:15009`):

```rust
Expr::Call { .. } => false,  // unconditional rejection
```

Constructor applications (`Some(s)`, `Ok(v)`, `Cons(h, t)`, …) are parsed as `Expr::Call { callee: Expr::Ident("Some"), args: [...] }` — structurally a Call. The pre-Task-117 classifier rejected them uniformly, miscategorizing pure value constructions as "yield-able" and falling through to the regular walker, which then rejected the surrounding `let r1 = k(arg)` as "k in non-tail position".

**Why latent.** The existing test corpus skews toward primitive returns (Int, Bool) — none of the existing Slice C handler arm bodies re-wrap an Option / Result / List in their tail. The Plan B Task 78.5 Koka-subset import was specifically scoped to surface exactly this kind of convergence-class blind spot (per `feedback_sigil_review_structural_weakness.md`); since that import was deferred to Plan C completion, the gap survived to Task 117.

**Fix.** Added a constructor-aware branch to `expr_is_pure`:

```rust
Expr::Call { callee, args, .. } => {
    if let Expr::Ident(name, _) = callee.as_ref() {
        if ctors.contains(name) {
            return args.iter().all(|a| expr_is_pure(a, ctors));
        }
    }
    false
}
```

`ctors: &BTreeSet<String>` is the set of variant constructor names registered in the program's type registry. Computed once at codegen entry (`emit_object` and `unsupported_handle_construct`) via the new `collect_ctor_names(&program)` helper, then threaded through:

- `expr_is_pure` / `block_is_pure` — direct consumers.
- `is_simple_tail_perform_with_pure_args_body` / `is_simple_yield_then_constant_tail_body` / `is_simple_chained_let_yield_then_pure_tail_body` — CPS-color body classifiers.
- `arm_body_unsupported_construct` / `expr_unsupported_handle` / `block_unsupported_handle` — handle-walker chain.

**Why this scope addition is justified.** The fix is single-purpose and contained: one new helper (`collect_ctor_names`), one new branch in `expr_is_pure`, mechanical threading through ~10 sites. The classifier name "pure" continues to mean "non-yield-able" (per the existing doc) — ctor calls satisfy non-yield-ability because constructors lower synchronously to header + per-field stores (no trampoline yields). Non-ctor calls (user fns, builtins like `int_to_string`) remain rejected; the false-negative class is unchanged for that path.

This is **not** a broader recognizer rework: the recognizer's structural shape (`{ let _ = k(arg); ...; pure_tail }`) is unchanged, only the purity classifier is extended. Future widenings (e.g., color-aware purity for Native-color user-fn calls) would be additional sub-fixes, not a generalization of this branch.

**Regression tests.** Two unit tests in `compiler/src/codegen.rs` `tests` module:

- `expr_is_pure_accepts_ctor_application_of_pure_args` — pins direct ctor purity (`Some(r1)` and `None` accepted; `int_to_string(r1)` rejected).
- `expr_is_pure_accepts_match_arm_body_with_ctor_tail` — pins the canonical Sudoku match-tail shape (`match r1 { Some(s) => Some(s), None => r2 }`) as pure under ctor-aware classifier.

**Failure mode.** None at the user surface. Pre-fix, programs using ctor-bearing tails in handler arm bodies would fall through to the regular walker and produce a confusing "k in non-tail position" diagnostic. Post-fix, those programs compile via Slice C as the recognizer was always intended to support.

**Implementing commit.** PR #59 (Plan D Task 117 (a)) — bundled with the Sudoku smoke gate so the smoke gate's source can use the canonical `Some(s) => Some(s)` shape rather than a workaround.

