# Plan D — v2 architectural cluster (Stages 11–13)

Tracks Plan D's execution against `boldfield/designs/in-progress/2026-04-30-sigil-plan-d.md` (moves to `done/` on completion). Plan C ~85% complete at sigil/main `dfcd60b` on 2026-04-30 (PRs #44 / #45 / #46 / #47 squash-merged); Plan C completion is a separately queued plan that closes the Plan C ledger after Plan D ships. The two address disjoint scopes — Plan D is the discrete unit of compiler/runtime work that unblocks Plan C completion's stdlib/demos/validation tasks.

## Stage 10.5 — Plan D scaffolding

**Complete before any Stage 11 task work.**

- 10.5.1 — Create `PLAN_D_PROGRESS.md` (this file).
  - status: done (this commit)
- 10.5.2 — Create `PLAN_D_DEVIATIONS.md`.
  - status: done (this commit)
- 10.5.3 — Plan D questions use `[PLAN-D]` prefix (`QUESTIONS.md` preamble updated to include the tag).
  - status: done (this commit)
- 10.5.4 — Open draft `[DEVIATION Plan D overview]` entry in `PLAN_D_DEVIATIONS.md`.
  - status: done (this commit)
- 10.5.5 — Pre-survey `#[ignore]` inventory and partition into closure-targets / test-infra / other-v2-pending.
  - status: done (this commit)
- 10.5.6 — Open or link Plan B' carryover #2 (Sync shim emission gating) tracking artifact.
  - status: done (this commit) — see [CHORE] issue link below.

**Acceptance:** CI still green; new tracking files exist; overview deviation entry drafted; `#[ignore]` partition recorded; Plan B' carryover #2 tracking artifact linked.

### `#[ignore]` partition (recorded per Stage 10.5.5)

Survey at sigil/main `dfcd60b` (Plan D start). The plan estimated ~12 ignored tests; **actual count is 3**. Logged as `[DEVIATION Stage 10.5.5]` in `PLAN_D_DEVIATIONS.md` so the discrepancy is preserved.

**(a) Plan D closure targets** — un-ignore at Task 112 / Task 119:

| Test | Location | Closure step |
|---|---|---|
| `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix` | `compiler/tests/e2e.rs:7014` | Task 112 (wrapper-fn-frame composition fix) |

**(b) Non-architectural test-infra gaps** — leave alone at Task 119:

| Test | Location | Rationale |
|---|---|---|
| `std_io_read_line_via_piped_stdin_pending_test_infra` | `compiler/tests/e2e.rs:6792` | Needs piped-stdin test infrastructure; tracked for Task 78 (Plan C completion). |
| `arena_overflow_aborts` | `runtime/src/arena.rs:489` | Abort tests are not directly observable from `cargo test`; run with `cargo test -- --ignored` and confirm SIGABRT manually. |

**(c) Other v2-pending tests not closed by Plan D** — none surveyed.

### Plan B' carryover #2 tracking

Plan B' Stage-6.8-followup carryover #2 (Sync shim emission gating) is out of Plan D scope but tracked here per Stage 10.5.6 so the carryover has a named owner. GitHub issue link is added on this commit's followup (issue creation requires `gh` and is logged at the bottom of this entry once the issue number is known). Per `PLAN_B_PRIME_DEVIATIONS.md` "Stage-6.8-followup architectural carryovers" entry: every Cps-ABI top-level fn currently emits a `<mangled>__sync_shim` regardless of fn-as-value usage. Bounded bloat (one ~100-byte shim per Cps fn). Gate on `top_level_fn_names_seen_as_value` from closure_convert if Cps-fn count grows.

**Issue link:** https://github.com/boldfield/sigil/issues/48

## Stage 11 — Foundation lifts

- Task 111 — TLS → packed multi-return for `sigil_run_loop` terminal (Plan B' carryover #1, PR #39 §2).
  - status: **deferred** — see `[DEVIATION Task 111]`. Three implementation attempts on PR #50 demonstrated that the plan body's "register-pair multi-return" framing is structurally insufficient for the actual cross-fn discharge propagation requirement. Cross-fn visibility was the unstated semantic role of the OLD TLS; replacing it with any per-call mechanism (multi-return, out-pointer, per-fn stack slot, or Cranelift Variables) breaks the Sync-ABI call chain's discharge propagation. Closure path: defer to Task 117 first-class-k follow-up or a separate architectural slice. Plan B' carryover #1 stays open with revised closure scope.
- Task 112 — Wrapper-fn-frame composition fix (closes `[DEVIATION Task 72]` constraint #3).
  - status: **deferred** — see `[DEVIATION Task 112]`. The discharge-with-lambda pattern in `std/state.sigil` arms is set up for chained-let-yield body shape; wrapper-fn calls in the body force Sync ABI, which lacks the synth-cont chain the discharge-with-lambda pattern requires for `k`-chaining. Architecturally similar to Task 111: cross-fn behavior (cross-fn synth-cont chain in this case). User-visible inline-perform state threading continues to work; wrappers stay deferred. Closure path: defer to Task 117 first-class-k follow-up (same as Task 111).

**Stage 11 review checkpoint** — both Task 111 and Task 112 deferred; no Stage 11 lifts shipped. Stage 11 collapses; Plan D proceeds directly to Stage 12. Defer-checkpoint covers both tasks.

## Stage 12 — Type-system surface

Internal ordering: 114 must precede 115; 113 and 116 are independent.

- Task 113 — Tuples / `Pair[A, B]`.
  - status: **done** — PR #53 (squash). Full tuple syntax `(T1, T2, ...)` types and `(e1, e2, ...)` values, `Pattern::Tuple` element-wise unification + destructure, `MAX_TUPLE_ARITY = 31` (32-bit pointer bitmap, one bit reserved), `std/pair.sigil` with `fst` / `snd` over binary tuples. Code lands across AST + parser + typecheck + monomorphize + closure_convert + color + elaborate + codegen + header-constants. Five positive-path e2e tests + four negative-path e2e tests (parser empty `()`, parser `(e,)`, E0117 arity mismatch, E0066 non-catchall) + one generic-non-Ident-scrutinee positive test pin the surface. PR R1 surfaced two architectural findings closed in this PR — see `PLAN_D_DEVIATIONS.md` `[DEVIATION Task 113]` for the per-clone `match_scrut_tys_resolved` map (Bug 2) and `MAX_TUPLE_ARITY` named constant (R1 finding 3). PR R1 also flagged `[DEVIATION Task 72]` constraint #2's tuple-type prerequisite as now closed; constraint #2's `run_state` `(A, S)` shape closure remains a follow-up Plan-C-completion item. Closes Plan A3-deferred tuple work consolidated into this task.
- Task 114 — Type-parameterized effect rows (`![Raise[E]]`, `![State[S]]`).
  - status: **done-pending-ci** (PR pending). Five-phase migration shipped: (a) `ast::EffectRef { name, args, span }` introduced; FnDecl/FnTypeExpr/Lambda.effects flip to `Vec<EffectRef>`. (b) Ty-level `EffectInst { name, args }`; FnSig.effects/Row.effects flip to `Vec<EffectInst>`; `unify_row` / `subsume_row` rewritten with structural `Vec` containment over EffectInst; `ty_display` renders generic effects as `Raise[Int, String]`. (c) Pass-through sites in monomorphize / color / closure_convert / codegen updated. (d) Parser surface for `![Raise[E]]` (existing args parser feeds into `EffectRef`); `check_effect_ref_arity` introduces **E0140** with three message shapes (non-generic decl + args; generic decl + bare; arity divergence); arity-check runs at fn-decl row sites and inside FnTypeExpr rows via `check_type_expr_known`. monomorphize substitutes effect-row args under the active generic substitution at clone time. body-row in `check_fn` and `Expr::Lambda` carries args so cross-fn subsumption sees `Raise[Int]` rather than bare `Raise`. 7 typecheck unit tests cover: generic-effect-decl typechecks; `![Raise[Int]]` row typechecks; cross-fn row with matching type-arg unifies; cross-fn row with distinct args fires E0042; arity mismatch fires E0140 (3 cases — wrong arity, bare-name on generic decl, args on non-generic decl). What's NOT yet shipped: (1) std/raise.sigil migration to `effect Raise[E] { fail: (E) -> A }`. Closure path: requires Task 115 (per-op generics) for `fail[A]: (E) -> A`; lands at Stage 12 review checkpoint. (2) perform-site E-substitution: `perform Raise.fail("oops")` under `![Raise[String]]` does not yet thread E := String into fail's op-arg unification. Closure path: same as #1 — Task 115 closes alongside per-op generic instantiation.
- Task 115 — Per-op generic params on user-declared effects (`fail[A]: (E) -> A`).
  - status: **done-pending-ci** (PR pending). `EffectOp` gains `generic_params: Vec<GenericParam>` field; parser accepts `op_name[T1, T2]: (...) -> R` syntax. Effect-decl pre-pass layers per-op generic_subst on top of the effect-decl's substitution when checking each op's params/return; **E0144** (new code) fires when a per-op generic name shadows an effect-decl one. `check_perform` allocates fresh per-op `Ty::Var`s + threads the surrounding fn's row entry's args into the effect-decl's substitution — closes the **Task 114 R1 deferred gap**: `perform Raise.fail("wrong type")` under `![Raise[Int]]` now correctly fires E0044. Also closes the legacy E0140 collision (Task 114 mistakenly used E0140 for row-arg arity, colliding with the existing duplicate-handler-arm E0140; row-arg arity migrates to **E0143** with corresponding catalog entry). 5 new typecheck unit tests + 1 inverted closure-point test (`perform_site_e_substitution_deferred_to_task_115` flipped to `perform_site_e_substitution_closed_by_task_115`). std/raise.sigil migration to `effect Raise[E] { fail[A]: (E) -> A }` continues to defer to the Stage 12 review checkpoint per the plan body.
- Task 116 — Row-polymorphic Fn parameters.
  - status: **done-pending-ci** (PR pending). Lifts E0137's blanket rejection of row-variable-bearing first-class fn types. The AST already carried `effect_row_var: Option<RowVar>` on `FnTypeExpr` (Plan B' Stage 6.8 Task 103); the parser already produced it; the typechecker had row-var infrastructure (Plan B Stage 5: `unify_row` / `subsume_row` / `bind_row_var` / `apply_row` / `Scheme.row_vars`). Task 116 just connects them. New `ty_from_type_expr_with_rows` threads a `row_var_subst: BTreeMap<String, u32>` through the AST→Ty walk; `ty_from_type_expr` becomes a wrapper passing an empty subst. `Tc::ty_from_type_expr_here` passes `self.current_row_var_subst` so inner fn-type row vars (`(...) -> R ![ ... | r ]`) resolve against the enclosing fn's row var when names match. Fn pre-pass (`typecheck.rs:977-996`) seeds `current_row_var_subst` from `f.effect_row_var` before walking param/return/effects. E0137 now fires only when a row-var name is unbound by the enclosing fn (the diagnostic surfaces the missing declaration with a fix-suggestion). 3 typecheck unit tests: `fn_type_with_unbound_row_variable_fires_e0137` (renamed from `_is_e0137`), `fn_type_with_row_var_bound_by_enclosing_fn_typechecks`, `row_polymorphic_passthrough_signature_typechecks`. The plan body's canonical `catch[A, e]` shape is expressible at the surface; full handler-discharge / residual-row passthrough lands at Stage 12 review checkpoint with std/raise.sigil migration.

**Stage 12 review checkpoint** — see `PLAN_D_DEVIATIONS.md` `[DEVIATION Stage 12 review]`. Sign-off summary:

- **AST shape consistency** — `EffectRef`/`EffectInst` split (mirror of Tuple), `EffectOp.generic_params` field, `FnTypeExpr.effect_row_var` (pre-existing Plan B' Stage 6.8). Spans on AST sides, span-free on Ty side. Generic-param scoping: per-op generics layer on top of effect-decl generics with E0144 shadow check.
- **Diagnostic quality** — 5 new error codes shipped: E0117 (tuple-pattern arity), E0140 (duplicate handler arm — pre-existing), E0143 (row-arg arity, renamed from E0140 mid-Stage-12 due to collision), E0144 (per-op shadow), E0137 (narrowed to unbound row var only). All carry source spans pointing at the offending row-site / op-decl / FnTypeExpr.
- **Closure-path edits** — `[DEVIATION Task 71]` constraints #1, #2, #3 closed by Tasks 114, 115, 116 respectively. `[DEVIATION Task 72]` constraints #1, #2, #4, #5 closed (#3 wrapper-fn-frame stays deferred per Plan D Task 112). `[DEVIATION Task 73]` constraints #1, #5, #6 closed (#2, #3, #4 stay open, addressed by Plan D Tasks 117/118).
- **Stdlib migration partial** — std/raise.sigil shipped in this PR; std/state.sigil + std/result.sigil deferred to Plan C completion. The migration attempt surfaced three architectural gaps (handler-discharge type-arg propagation, `rename_ty` not walking EffectInst args, `apply_ty`/`apply_row` not walking EffectInst args) — all three fixed in this PR. `unify_row`/`subsume_row` were rewritten to name-match-with-arg-unify (replacing the structural-equality diff that worked for concrete args only). std/state needs separate lambda-discharge-under-generic-E exercise; std/result needs only verification. See `PLAN_D_DEVIATIONS.md` `[DEVIATION Stage 12 review]` Item 4 for full migration + gap-fix detail.

## Stage 13 — Continuation lifts

- Task 117 — First-class continuations (k-as-value). Eta-expansion design proven dead by validation (PR #59 prelude work, validation tests since removed). Falling back to **Ty::Continuation conservative ABI path** — distinct type the typechecker enforces dynamic-extent on. Substrate stabilization landed in PR #59 (5 latent v1 bugs fixed + Sudoku smoke gate); substrate capability work landed in PR #60 (Ty::Continuation + ScopeId + E0145 escape barrier + RELINK_STACK frame-keyed runtime + bind_ty_var precision fix). The let-bound k *positive capability* (`let f = k; f(arg)`) the brief described turned out to be unreachable under Sigil's mandatory let-annotation policy and is deferred against that language-design constraint. See `PLAN_D_DEVIATIONS.md` `[DEVIATION Task 117] Ty::Continuation + escape barrier — CLOSED on substrate; positive capability deferred against language-design constraint`.
  - status: **done (substrate)** — PR #60 squash-merged at `4b3f0b4`. Closes the user-visible escape barrier (E0145) backed by `Ty::Continuation` + ScopeId tagging + RELINK_STACK frame-keyed runtime + bind_ty_var precision fix. Positive let-bound k capability deferred against language-design constraint (Sigil's mandatory let annotations + no surface for `Ty::Continuation`); resolution requires a separate language-design decision, not bundled into Task 117.
- Task 118 — Conditional/branched k-call.
  - status: todo

**Stage 13 review checkpoint** (per the plan body): lifted-lambda closure-record discipline; arena escape rate (Plan B Task 60 baseline = 0%); Step 118 minimality; Sudoku smoke.

## Plan D closeout

- Task 119 — Plan D closeout audit. Walk every `[DEVIATION Task NN]` entry whose v2 closure path points at a Plan D-shipped lift; un-ignore tests; ship Sudoku + JSON parser smoke gates via e2e harness; update spec §14 (v1 limits).
  - status: todo

## Plan D completion criteria

- All Stage 10.5 + 11 + 12 + 13 acceptance criteria met on both hosts (per CI).
- All Stage 11 / 12 / 13 review checkpoints signed off.
- Closeout audit (Task 119) done.
- All tasks marked `done` with implementing commit references in this file.
- Sudoku and JSON parser half compile + run via e2e harness on both hosts; demo-PR landings on `main` are not required for Plan D closure (those belong to Plan C completion).
- Plan file `git mv`'d from `in-progress/` to `done/` once the human review checkpoint after Task 119 signs off.
