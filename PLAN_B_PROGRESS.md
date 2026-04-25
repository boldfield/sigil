# Plan B Progress

Task-by-task tracker for Plan B (`in-progress/2026-04-21-sigil-effects.md`
in `boldfield/designs` while active; moves to `done/` when merged). Each
entry tracks: the task ID, current status, linked commits, and optional
notes on deviations. Deviations are logged separately in
`PLAN_B_DEVIATIONS.md` *before* the implementing commit.

Status values: `todo`, `in-progress`, `done`, `done-pending-ci`.

**Acceptance reminder (from plan's "Local verification strategy"):** a task
is not `done` until CI is green on both `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin`. Local pod checks are necessary but not sufficient.
`done-pending-ci` is the interim state for tasks whose local verification
is complete but whose CI run has not yet reported green.

## Stage 4.5 — Plan B scaffolding

- Task 4.5.1 — Create `PLAN_B_PROGRESS.md`
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: This file.
- Task 4.5.2 — Create empty `PLAN_B_DEVIATIONS.md`
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: Landed atomically with 4.5.1 (scaffolding is one unit).
- Task 4.5.3 — Plan B questions use `[PLAN-B]` prefix in `QUESTIONS.md`
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: Convention already documented in `QUESTIONS.md` header (added in Plan A2 Task 1.5.3). B entries follow the same convention; no header update required.
- Task 4.5.4 — Extend `.github/workflows/ci.yml` for Plan-B-specific invariants
  - status: done-pending-ci
  - commits: [8645f8c]
  - notes: `scripts/plan-b-invariants.sh` runs as a CI step; three named invariants (deep-recursion trampoline, multi-shot continuation, selective CPS) drive off specific `.sigil` example files and e2e test names. Each prints `[SKIP]` until the underlying example lands in Stage 5/6, at which point the step flips to running the test. CI logs surface the invariant scoreboard from day one so it cannot be dropped before acceptance.
- Task 4.5.5 — Create `sigil-abi` leaf crate; consolidate stackmap and cross-boundary constants
  - status: done-pending-ci
  - commits: [e1c4286]
  - notes: New `abi/` workspace member, `#![no_std]`, zero deps. Holds stackmap wire-format constants + `StackMapRecordV0` struct + tagged-Value bit masks + `TAG_INT_SHIFT`. `runtime::stackmap` and `runtime::value` `pub use`-re-export from it; the old `STACKMAP_*` pins in codegen.rs were removed. `sigil-header-constants` stays untouched (adjacent but distinct scope — the 8-byte object header). Pod-verify green.

## Plan A3 carryover

- Carryover — Full nested Maranget exhaustiveness
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: New recursive `match_witness(scrut_ty, &[&Pattern])` on `Tc` returns the first uncovered witness across user-type variants and their field patterns. `user_type_witness` now delegates to it. Primitive field rules unchanged (Bool → both literals required unless catchall; Int/Char/String/Byte/Fn → wildcard required). Nested witness formatting via `positional_witness_with_hole` and `record_witness_with_hole` — preserves declared field order and fills other slots with `_`. Six new tests cover: Some(true) missing `Some(false)`, Some(false) missing `Some(true)`, both-literals exhaustive, `Some(_)` catchall is exhaustive, `Holds(Leaf)` missing `Holds(Node(_, _, _))` on nested user types, `P { a: true, b: true }` missing `P { a: false, b: _ }`, and Int-field literal-only producing `Some(_)` witness (infinite-domain fallback). E0120 catalog long-form updated.
- Carryover — Suppress E0120 when an arm body fails type-checking
  - status: done-pending-ci
  - commits: [HEAD, REVIEW-FIXUP]
  - notes: `check_match` tracks an `any_arm_erred` flag by snapshotting `self.errors.len()` before/after each arm's pattern + body check. If any arm added to the error list, the user-type E0120 emission is suppressed (per reviewer's narrowing — the primitive E0066 path runs unconditionally because primitive scrutinees rarely cascade the same way). Four tests pin the behavior: (1) suppression on arm-body type error (arithmetic on String), (2) suppression on arm-pattern E0117, (3) regression-guard that E0120 still fires on clean-but-non-exhaustive arms, (4) E0066 still fires on a non-exhaustive Bool match even when an arm body errs. E0120 catalog long-form updated.
- Carryover — Tagged-vs-raw Int ABI decision
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Resolved to option (c) applied narrowly — raw `i64` internally in user-fn calls; tag at the C-ABI boundary only (main return). Stage 6 adds new tagging sites: continuations captured across handlers hold tagged Ints (heap-observable); arena-allocated `NextStep` records hold raw `i64` (arena is reset, not scanned). Both `ishl_imm` / `sshr_imm` sites in `codegen.rs` (main-return tag, C-main-shim untag) replaced their literal `1` with `i64::from(TAG_INT_SHIFT)`. Full rationale + audit table in PLAN_B_DEVIATIONS.md; cross-referenced `[PLAN-B] Tagged-vs-raw Int ABI` entry added to QUESTIONS.md closing the Plan A3 Forward-Implications paragraph.

## Stage 5 — Parametric polymorphism

- Task 47 — Parser: `[A, B]` generic params, explicit row vars `![IO | e]`
  - status: done-pending-ci
  - commits: [HEAD, REVIEW-FIXUP]
  - notes: AST extensions: new `GenericParam` and `RowVar` types; `FnDecl` and `TypeDecl` gain `generic_params: Vec<GenericParam>`; `FnDecl` and `Expr::Lambda` gain `effect_row_var: Option<RowVar>`; `TypeExpr::Apply { name, args, span }` for generic application (`List[Int]`, `Map[String, List[Int]]`). New `TypeExpr::head_name()` and `span()` helpers keep most consumers unchanged. Parser: `parse_generic_params() -> Option<Vec<GenericParam>>` for `[A, B]` between name and `(`/`=` (errors propagate via `?` consistently with `parse_effect_row`); `parse_effect_row()` extracts effects + optional `| rowvar` body and replaces inline loops in `parse_fn_decl` and `parse_lambda_expr`; `parse_type` recognises `[T1, ...]` after a name and returns `TypeExpr::Apply`. **Semantic consumption deferred to Task 48** — typecheck rejects `TypeExpr::Apply` with E0124 and explicit row variables with E0125 so partial semantics cannot silently slip through; the new diagnostics live in the catalog with full long-form text and Task-48 fix examples. 12 parser tests + 9 typecheck tests cover the new shapes and the placeholder errors; all prior tests pass unchanged. Pod-verify green.
- Task 48 — Type checker: HM unification with row variables, closed rows
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Added `Ty::Var(u32)` and changed `Ty::User(String)` → `Ty::User(String, Vec<Ty>)` so generic instantiations (`List[Int]`) round-trip through the inferred IR. New `FnSig.effect_row_var: Option<u32>` distinguishes closed rows from open rows. New `Subst` carries type-var → `Ty` and row-var → `Row` substitutions; `apply_ty` / `apply_row` resolve through it with cycle-safe walks. Unification: `unify_ty` does occurs-check (E0126), structural matching for `User` / `Fn` and recursive arg/param unification; `unify_row` enforces closed-row equality, closed-vs-open absorption, open-vs-open shared-tail merging, all with E0128 on mismatch and E0127 on row occurs failure. Schemes (`type_vars`, `row_vars`, `body`) get instantiated at every call site of a top-level fn via `instantiate`; non-generic fns end up with empty bound-var lists so the legacy direct-call path stays zero-cost. `check_fn` populates `current_generic_subst` from the fn's `[A, B]` list and `current_row_var_subst` from `![ ... | e]`, threading them through every `ty_from_type_expr_here` call inside the body. Constructors of generic user types (`Cons(1, Nil)`) allocate fresh vars per declared generic param via `fresh_user_instance_with_subst`; field types resolve under the merged subst; arg-vs-field types unify, pinning the user-type args. E0124 / E0125 placeholder errors deleted from the catalog; replaced with E0126 (occurs), E0127 (row occurs), E0128 (row mismatch), E0129 (arity mismatch), E0131 (Apply on primitive / generic-param). Codegen-entry guard `contains_apply_or_generic_ref(&Program)` walks the AST for surface generic syntax and asserts at `emit_object` entry; closes the verification-debt entry "Codegen path for un-monomorphized generic params". 13 new unit tests cover generic id, two-instantiation calls, generic compose-shape, generic ctor / variant / return type, arity mismatch (E0129), Apply on primitive (E0131), unknown head (E0112), open caller / closed callee, occurs check, non-generic round-trip, and generic-without-args rejection. All 263 typecheck tests pass; pod-verify green.
- Task 49 — Monomorphization: reachability-bounded, typed IR preserved
  - status: todo
  - commits: []
- Task 50 — Color inference: per-monomorph, SCC-aware, `--dump-color`
  - status: todo
  - commits: []
- Task 51 — `examples/generic_map.sigil` + e2e
  - status: todo
  - commits: []
- Task 52 — Validation prompts P16, P17
  - status: todo
  - commits: []

### Stage 5 review checkpoint

Pending — request human review of row-unification, let-generalization, color inference, and monomorphization naming determinism before Stage 6 begins.

## Stage 6 — Algebraic effects and handlers

- Task 53 — Parser: `effect Name[T] { ... }`, `resumes: many`, `handle expr with { ... }`
  - status: todo
  - commits: []
- Task 54 — Type checker: row-polymorphic effect checking, handler typing, one-shot linearity (E0220)
  - status: todo
  - commits: []
- Task 55 — CPS transform on CPS-color monomorphs; arena-allocated `NextStep` records
  - status: todo
  - commits: []
- Task 56 — Runtime: `HandlerFrame`, arena, `sigil_perform`, `run_loop`, counters
  - status: todo
  - commits: []
- Task 57 — Refactor IO shortcut; `Raise[ArithError]` replaces `sigil_panic_arith_error`
  - status: todo
  - commits: []
- Task 58 — Multi-shot rigor: `examples/choose_demo.sigil`, `examples/multishot_stress.sigil`
  - status: todo
  - commits: []
- Task 59 — Examples: `catch.sigil`, `state.sigil`, `choose.sigil` + e2e
  - status: todo
  - commits: []
- Task 60 — Performance floor: native fib(20) <50ms; CPS-forced fib(20) <500ms; arena escape ≤1%
  - status: todo
  - commits: []
- Task 61 — Validation prompts P18, P19, P20 (prompt bank complete)
  - status: todo
  - commits: []

### Stage 6 review checkpoint

Pending — request human review of linearity, handler stack semantics, multi-shot correctness, IO refactor, arena correctness, color decisions before declaring Plan B complete.
