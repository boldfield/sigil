# Plan A3 Progress

Task-by-task tracker for Plan A3 (`in-progress/2026-04-21-sigil-core-a3.md`
in `boldfield/designs` while active; moves to `done/` when merged). Each
entry tracks: the task ID, current status, linked commits, and optional
notes on deviations. Deviations are logged separately in
`PLAN_A3_DEVIATIONS.md` *before* the implementing commit.

Status values: `todo`, `in-progress`, `done`, `done-pending-ci`.

**Acceptance reminder (from plan's "Local verification strategy"):** a task
is not `done` until CI is green on both `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin`. Local pod checks are necessary but not sufficient.
`done-pending-ci` is the interim state for tasks whose local verification
is complete but whose CI run has not yet reported green.

## Stage 3.5 — Plan A3 scaffolding

- Task 3.5.1 — Create `PLAN_A3_PROGRESS.md`
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: This file.
- Task 3.5.2 — Create empty `PLAN_A3_DEVIATIONS.md`
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Landed atomically with 3.5.1 (scaffolding is one unit).
- Task 3.5.3 — Plan A3 questions use `[PLAN-A3]` prefix in `QUESTIONS.md`
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Convention already documented in QUESTIONS.md header (added in Plan A2 Task 1.5.3). A3 entries follow the same convention; no header update required.

## Stage 4 — User-defined types and pattern matching

- Task 36 — Extend lexer: `type`, `|`
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Added `TokenKind::Type` keyword and `TokenKind::Pipe` single-char token. `||` lookahead still wins for `OrOr`. 4 new lexer unit tests pin the new tokens, the `||` regression, and a full `type Option = | None | Some(Int)` skeleton.
- Task 37 — Extend parser: type decls + record literal + constructor/variable/tuple patterns
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Added AST variants (`Item::Type`, `Expr::RecordLit`, `Pattern::Var/Tuple/Ctor`, `TypeDecl`, `Variant`, `VariantFields`, `RecordFieldDecl`, `RecordFieldLit`, `CtorPatternFields`, `CtorPatternField`); parser extensions (`parse_type_decl` for sum + single-ctor record shorthand; extended `parse_pattern` for Var/positional-Ctor/record-Ctor/tuple; record-literal recognition gated by new `no_record_lits` flag disabled in `if` cond / `match` scrutinee). E0110 rejects or-patterns / guards / as-bindings at the match-arm parser (post-pattern-parse — a valid first pattern followed by `|`/`if`/`as` is the user-visible failure mode). New catalog entry E0110 with full long-form explanation. Downstream passes (typecheck / elaborate / closure_convert / codegen) gained stubs: `Item::Type` is a no-op (task 38 flesh-out), `Expr::RecordLit` emits a staged E0111 in typecheck (task 38 replaces with real constructor resolution) and is passed through in elaborate/closure_convert, `Pattern::Var/Tuple/Ctor` return `None` in `pattern_ty` and hit `unreachable!` in codegen's `pattern_as_immediate` (task 41 rewrites the lowerer). Pod-verify green. +17 parser tests at initial push (48 parser tests total, 158 compiler lib total). Follow-up commit addressing PR #12 review adds E0111 catalog entry, migrates the RecordLit stub from E0001→E0111, extends `no_user_facing_error_uses_e0001` with a record-literal program, and adds `record_literal_in_call_arg_of_match_scrutinee`: +2 tests → 49 parser tests total, 159 compiler lib total.
- Task 38 — Extend typechecker: nominal sum types + record field access + pattern matching with Maranget exhaustiveness
  - status: todo
  - commits: []
  - notes:
- Task 39 — Extend elaboration: compile pattern match to nested switch + field loads
  - status: todo
  - commits: []
  - notes:
- Task 40 — Extend runtime: allocate sum-type and record values with discriminant + fields; layout descriptors
  - status: todo
  - commits: []
  - notes:
- Task 41 — Extend codegen: allocation, discriminant read, field load, record construction
  - status: todo
  - commits: []
  - notes:
- Task 42 — `examples/option_demo.sigil`
  - status: todo
  - commits: []
  - notes:
- Task 43 — `examples/tree.sigil` with recursive `sum_tree`
  - status: todo
  - commits: []
  - notes:
- Task 44 — Performance floor: `sum_tree` on depth-15 tree runs <500ms on both hosts
  - status: todo
  - commits: []
  - notes:
- Task 45 — Exhaustiveness regression test (E0120 + counterexample witness)
  - status: todo
  - commits: []
  - notes:
- Task 46 — Seed prompt bank (P11–P15)
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Added P11 (list length), P12 (list sum), P13 (Option-returning safe lookup over list), P14 (2D Point record + dist_sq via nested destructuring — no field-access syntax in A3 by design), P15 (map_inc: hard-coded increment over a list since `TypeExpr::Fn` + generics are deferred). Each prompt's oracle-notes block spells out the Plan A3 machinery exercised (type-tag allocation, constructor lowering, match-as-discriminant-test, field loads, nominal exhaustiveness) so downstream graders can cross-reference the semantic target. P13 documents the two-sum-types-one-program case.
