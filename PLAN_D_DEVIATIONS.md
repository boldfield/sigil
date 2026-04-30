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
