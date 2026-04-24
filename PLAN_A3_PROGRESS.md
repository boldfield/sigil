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
  - status: in-progress
  - commits: []
  - notes: Split into 38.1 (symbol table + Ty::User + E0112 unknown type + E0113 duplicate type), 38.2 (constructor resolution for positional Call + RecordLit), 38.3 (pattern typing: Var promotion to nullary Ctor, Tuple, Ctor; E0117 pattern type mismatch), 38.4 (Maranget's exhaustiveness + E0120 witness + E0130 reserved). Per reviewer constraint on PR #12, the E0111 emission on `Expr::RecordLit` stays live until Task 41 flip so no valid surface-syntax program reaches codegen's `unreachable!` at any checkpoint.
    - 38.1 (done-pending-ci): added `Ty::User(String)` variant; types registry built in pre-pass from `Item::Type` and moved into `CheckedProgram.types`; `ty_from_type_expr` threaded through `&BTreeMap<String, TypeDecl>`; new `Tc::check_type_expr_known` sweep emits E0112 against unresolved type names in fn signatures + variant fields/positional variants; E0113 emitted on duplicate `type` declaration at the second (and subsequent) offender; first-writer wins in the registry. New `EnvSlotKind::User` (is_pointer=true) for closure-captured user-type values; codegen env-slot stores/loads treat User identically to String/Closure. `is_exhaustive` Ty::User arm falls through to "only wildcard" until 38.4 refines with Maranget. 10 new typecheck unit tests.
    - 38.2 (done-pending-ci): added constructor registry `ctors: BTreeMap<String, CtorInfo>` built alongside types pre-pass; ctor-name collisions across types surface as E0118 at the second offender. Three resolver helpers on `Tc` — `resolve_ctor_unit_use` (bare Ident), `resolve_ctor_positional_use` (Call with Ident callee), `resolve_ctor_record_use` (RecordLit). Each emits the staged E0111 gate on successful shape/field-name/field-type check and returns `Ty::User(type_name)`; shape mismatches emit E0115, arity mismatches E0043, field-type mismatches E0044, unknown ctor names E0114. Expr::Ident, Expr::Call, Expr::RecordLit arms intercept before their generic handlers so `None`, `Some(42)`, `Point { x: 1, y: 2 }` all resolve cleanly. Reviewer constraint honored: E0111 stays live at every successful resolution site so no well-formed user program reaches codegen. 16 new typecheck unit tests.
    - 38.3 (done-pending-ci): replaced the coarse `pattern_ty()` + "wildcard-or-error" loop in `check_match` with a recursive `check_pattern(pat, scrut_ty, bindings)` that verifies structural pattern-vs-scrutinee shape and collects `Pattern::Var` bindings. Bindings are inserted into `self.env` for the arm body's scope and restored after. Ctor patterns recurse into declared field types so nested variables (`Cons(x, xs) => ...`) bind with the right types. Nullary-ctor promotion: a bare `Pattern::Var(name)` whose name matches a Unit variant of the scrutinee's user type becomes a zero-arity ctor pattern (no binding). Tuple patterns always fire E0117 in A3 v1 (no tuple types). Explicit `Ctor(args)` or `Ctor { .. }` with an unregistered name fires E0114. Shape mismatches across ctor patterns and declared variants fire E0115. Literal-pattern-vs-primitive mismatches continue to fire E0064 (Plan A2 code preserved); constructor/variable/tuple mismatches fire the new E0117. Removed dead `pattern_ty()` helper. 13 new typecheck unit tests.
    - 38.4 (done-pending-ci): top-level exhaustiveness check for `Ty::User` scrutinees via `user_type_witness(type_name, arms)` with paste-able witness string built by `ctor_witness_string(variant)` (Unit → `Foo`, Positional → `Foo(_, ..)`, Record → `Foo { x: _, .. }`). Catch-all arms (wildcard OR Pattern::Var whose name is not a nullary ctor of the scrutinee's type) short-circuit to exhaustive. Missing variant → E0120 with witness embedded in the message. Primitive scrutinees retain the Plan A2 `is_exhaustive` rule → E0066. Plan A3 v1 only checks top-level coverage; nested non-exhaustiveness inside ctor fields falls through to the runtime `TRAP_NONEXHAUSTIVE_MATCH` trap (documented in E0120 catalog long-form; Plan B refines to full nested Maranget). E0130 catalog entry reserved (emitted only when Task 40 codegen detects a type layout >63 payload words). 10 new exhaustiveness tests.
- Task 39 — Extend elaboration: compile pattern match to nested switch + field loads
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: The decision-tree lowering proper belongs to codegen (Task 41.2) since it uses Cranelift-level concepts — discriminant byte layout, field offsets, block-based dispatch. Elaborate's Plan A3 role is correctness-preserving pass-through: `Expr::RecordLit` field values flow through `elab_expr` unchanged (no ANF flattening — compound field values are evaluated in order by Task 41's allocator); `Expr::Match` arm bodies hoist their own synthetic lets into an `Expr::Block` wrapper so the pattern's Var bindings scope correctly over arm-local flattening (already in place for primitive matches pre-A3). What this task DOES fix is the capture-analysis bug: `collect_free_vars` and `closure_convert::rewrite_expr` both track match-arm `Pattern::Var` bindings as arm-local. New `pattern_bindings(&Pattern, &mut BTreeSet)` helper recursively extracts binding names from Ctor/Tuple/Var patterns (including nested) and is called at each arm boundary to extend `locals` before walking the arm body, then restore. Without this fix, a lambda containing `match o { Some(n) => n, None => 0 }` would incorrectly treat `n` as a captured outer variable and emit a spurious `ClosureEnvLoad`. 2 new typecheck tests (pattern_bindings helper on nested Positional and Record Ctor patterns).
- Task 40 — Extend runtime: allocate sum-type and record values with discriminant + fields; layout descriptors
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: New `compiler/src/layout.rs` module: `TypeLayout { type_tag, variants: Vec<VariantLayout> }` and `VariantLayout { name, discriminant, payload_words, pointer_bitmap, field_tys, field_names }`. `build_layouts(&types)` assigns tags starting at `USER_TAG_START = 0x10` in BTreeMap (alphabetical) order for reproducibility, computes pointer_bitmap per variant (bit 0 always 0 for discriminant word, bits 1..N reflect `is_gc_pointer_ty(field_ty)`), and returns `LayoutError::PayloadTooLarge` → E0130 when `1 + field_count > 63` (6-bit count-field ceiling). `build_ctor_index(&layouts)` produces the O(log n) constructor-name → (type_name, variant_index) map for codegen use-site lookup. `variant_header_word(type_tag, variant)` composes the 8-byte header via the shared `sigil_header_constants::header_word` const fn so the bit layout lives in exactly one place. Wired into `codegen::emit_object`: layout table built once before any function lowering, threaded through to `Lowerer` as `type_layouts` + `ctor_index` (allow(dead_code) until Task 41.1 consumes them). 9 layout unit tests covering Option (tag=0x10, payload=2 for Some), List (Cons bitmap=0b100 for pointer tail), Result (Err String bitmap=0b10), record field-name preservation, alphabetical tag ordering, E0130 shape error at 64 words, ctor-name round-trip, header-word composition, and `is_gc_pointer_ty` classification.
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
