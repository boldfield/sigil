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
  - status: todo
  - commits: []
  - notes: Deep-recursion regression test, multi-shot stress test, selective-CPS correctness test. The real tests compile .sigil programs that depend on generics / effects and so can only pass after Stages 5/6 land. Stage 4.5 wires the steps as `scripts/plan-b-invariants.sh` (executed from CI) that currently short-circuits with a guard message; each invariant flips from short-circuit to real assertion as the underlying feature lands.
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
  - commits: [HEAD]
  - notes: `check_match` tracks an `any_arm_erred` flag by snapshotting `self.errors.len()` before/after each arm's pattern + body check. If any arm added to the error list, the exhaustiveness branch (both the `Ty::User` E0120 path and the primitive `is_exhaustive` E0066 path) is skipped for this match so the user fixes the arm-level error first. Three tests: (1) suppression on arm-body type error (arithmetic on String), (2) suppression on arm-pattern E0117 (tuple pattern on user type), (3) regression-guard that E0120 still fires on clean-but-non-exhaustive arms. E0120 catalog long-form updated to describe the suppression behavior.
- Carryover — Tagged-vs-raw Int ABI decision
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: Resolved to option (c) applied narrowly — raw `i64` internally in user-fn calls; tag at the C-ABI boundary only (main return). Stage 6 adds new tagging sites: continuations captured across handlers hold tagged Ints (heap-observable); arena-allocated `NextStep` records hold raw `i64` (arena is reset, not scanned). Both `ishl_imm` / `sshr_imm` sites in `codegen.rs` (main-return tag, C-main-shim untag) replaced their literal `1` with `i64::from(TAG_INT_SHIFT)`. Full rationale + audit table in PLAN_B_DEVIATIONS.md; cross-referenced `[PLAN-B] Tagged-vs-raw Int ABI` entry added to QUESTIONS.md closing the Plan A3 Forward-Implications paragraph.

## Stage 5 — Parametric polymorphism

- Task 47 — Parser: `[A, B]` generic params, explicit row vars `![IO | e]`
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: AST extensions: new `GenericParam` and `RowVar` types; `FnDecl` and `TypeDecl` gain `generic_params: Vec<GenericParam>`; `FnDecl` and `Expr::Lambda` gain `effect_row_var: Option<RowVar>`; `TypeExpr::Apply { name, args, span }` for generic application (`List[Int]`, `Map[String, List[Int]]`). New `TypeExpr::head_name()` and `span()` helpers keep most consumers unchanged. Parser: `parse_generic_params()` for `[A, B]` between name and `(`/`=`; `parse_effect_row()` extracts effects + optional `| rowvar` body and replaces inline loops in `parse_fn_decl` and `parse_lambda_expr`; `parse_type` recognises `[T1, ...]` after a name and returns `TypeExpr::Apply`. **Semantic consumption deferred to Task 48 (HM unification)** — typecheck/elaborate/codegen treat `Apply` equivalently to `Named(head_name, _)`, ignore generic_params, and treat the row variable as if it were a closed row. 12 new parser tests pin the new shapes; all 232 prior compiler-lib tests pass unchanged. Pod-verify green.
- Task 48 — Type checker: HM unification with row variables, closed rows
  - status: todo
  - commits: []
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
