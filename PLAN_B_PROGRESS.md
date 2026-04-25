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
  - status: done
  - commits: [01e0e13]
  - notes: This file. Shipped in PR #14 (Stage 4.5 + A3 carryover + Task 47 parser scaffolding).
- Task 4.5.2 — Create empty `PLAN_B_DEVIATIONS.md`
  - status: done
  - commits: [01e0e13]
  - notes: Landed atomically with 4.5.1 (scaffolding is one unit) in PR #14.
- Task 4.5.3 — Plan B questions use `[PLAN-B]` prefix in `QUESTIONS.md`
  - status: done
  - commits: [01e0e13]
  - notes: Convention already documented in `QUESTIONS.md` header (added in Plan A2 Task 1.5.3). B entries follow the same convention; no header update required.
- Task 4.5.4 — Extend `.github/workflows/ci.yml` for Plan-B-specific invariants
  - status: done
  - commits: [01e0e13]
  - notes: `scripts/plan-b-invariants.sh` runs as a CI step; three named invariants (deep-recursion trampoline, multi-shot continuation, selective CPS) drive off specific `.sigil` example files and e2e test names. Each prints `[SKIP]` until the underlying example lands in Stage 5/6, at which point the step flips to running the test. CI logs surface the invariant scoreboard from day one so it cannot be dropped before acceptance.
- Task 4.5.5 — Create `sigil-abi` leaf crate; consolidate stackmap and cross-boundary constants
  - status: done
  - commits: [01e0e13]
  - notes: New `abi/` workspace member, `#![no_std]`, zero deps. Holds stackmap wire-format constants + `StackMapRecordV0` struct + tagged-Value bit masks + `TAG_INT_SHIFT`. `runtime::stackmap` and `runtime::value` `pub use`-re-export from it; the old `STACKMAP_*` pins in codegen.rs were removed. `sigil-header-constants` stays untouched (adjacent but distinct scope — the 8-byte object header). Pod-verify green.

## Plan A3 carryover

- Carryover — Full nested Maranget exhaustiveness
  - status: done
  - commits: [01e0e13]
  - notes: New recursive `match_witness(scrut_ty, &[&Pattern])` on `Tc` returns the first uncovered witness across user-type variants and their field patterns. `user_type_witness` now delegates to it. Primitive field rules unchanged (Bool → both literals required unless catchall; Int/Char/String/Byte/Fn → wildcard required). Nested witness formatting via `positional_witness_with_hole` and `record_witness_with_hole` — preserves declared field order and fills other slots with `_`. Six new tests cover: Some(true) missing `Some(false)`, Some(false) missing `Some(true)`, both-literals exhaustive, `Some(_)` catchall is exhaustive, `Holds(Leaf)` missing `Holds(Node(_, _, _))` on nested user types, `P { a: true, b: true }` missing `P { a: false, b: _ }`, and Int-field literal-only producing `Some(_)` witness (infinite-domain fallback). E0120 catalog long-form updated.
- Carryover — Suppress E0120 when an arm body fails type-checking
  - status: done
  - commits: [01e0e13]
  - notes: `check_match` tracks an `any_arm_erred` flag by snapshotting `self.errors.len()` before/after each arm's pattern + body check. If any arm added to the error list, the user-type E0120 emission is suppressed (per reviewer's narrowing — the primitive E0066 path runs unconditionally because primitive scrutinees rarely cascade the same way). Four tests pin the behavior: (1) suppression on arm-body type error (arithmetic on String), (2) suppression on arm-pattern E0117, (3) regression-guard that E0120 still fires on clean-but-non-exhaustive arms, (4) E0066 still fires on a non-exhaustive Bool match even when an arm body errs. E0120 catalog long-form updated.
- Carryover — Tagged-vs-raw Int ABI decision
  - status: done
  - commits: [01e0e13]
  - notes: Resolved to option (c) applied narrowly — raw `i64` internally in user-fn calls; tag at the C-ABI boundary only (main return). Stage 6 adds new tagging sites: continuations captured across handlers hold tagged Ints (heap-observable); arena-allocated `NextStep` records hold raw `i64` (arena is reset, not scanned). Both `ishl_imm` / `sshr_imm` sites in `codegen.rs` (main-return tag, C-main-shim untag) replaced their literal `1` with `i64::from(TAG_INT_SHIFT)`. Full rationale + audit table in PLAN_B_DEVIATIONS.md; cross-referenced `[PLAN-B] Tagged-vs-raw Int ABI` entry added to QUESTIONS.md closing the Plan A3 Forward-Implications paragraph.

## Stage 5 — Parametric polymorphism

- Task 47 — Parser: `[A, B]` generic params, explicit row vars `![IO | e]`
  - status: done
  - commits: [01e0e13]
  - notes: AST extensions: new `GenericParam` and `RowVar` types; `FnDecl` and `TypeDecl` gain `generic_params: Vec<GenericParam>`; `FnDecl` and `Expr::Lambda` gain `effect_row_var: Option<RowVar>`; `TypeExpr::Apply { name, args, span }` for generic application (`List[Int]`, `Map[String, List[Int]]`). New `TypeExpr::head_name()` and `span()` helpers keep most consumers unchanged. Parser: `parse_generic_params() -> Option<Vec<GenericParam>>` for `[A, B]` between name and `(`/`=` (errors propagate via `?` consistently with `parse_effect_row`); `parse_effect_row()` extracts effects + optional `| rowvar` body and replaces inline loops in `parse_fn_decl` and `parse_lambda_expr`; `parse_type` recognises `[T1, ...]` after a name and returns `TypeExpr::Apply`. **Semantic consumption deferred to Task 48** — typecheck rejects `TypeExpr::Apply` with E0124 and explicit row variables with E0125 so partial semantics cannot silently slip through; the new diagnostics live in the catalog with full long-form text and Task-48 fix examples. 12 parser tests + 9 typecheck tests cover the new shapes and the placeholder errors; all prior tests pass unchanged. Pod-verify green.
- Task 48 — Type checker: HM unification with row variables, closed rows
  - status: done
  - commits: [70756de]
  - notes: Added `Ty::Var(u32)` and changed `Ty::User(String)` → `Ty::User(String, Vec<Ty>)` so generic instantiations (`List[Int]`) round-trip through the inferred IR. New `FnSig.effect_row_var: Option<u32>` distinguishes closed rows from open rows. New `Subst` carries type-var → `Ty` and row-var → `Row` substitutions; `apply_ty` / `apply_row` resolve through it with cycle-safe walks. Unification: `unify_ty` does occurs-check (E0126), structural matching for `User` / `Fn` and recursive arg/param unification; `unify_row` enforces closed-row equality, closed-vs-open absorption, open-vs-open shared-tail merging, all with E0128 on mismatch and E0127 on row occurs failure. Call-site row check uses asymmetric `subsume_row` (callee's effects ⊆ caller's; callee's row var absorbs caller's leftovers) so the caller's declared row variable stays free for generalisation. Schemes (`type_vars`, `row_vars`, `body`) get instantiated at every call site of a top-level fn via `instantiate`; non-generic fns end up with empty bound-var lists so the legacy direct-call path stays zero-cost. **Pre-pass scheme seeding** registers a polymorphic `Scheme` in `fn_schemes` for every user fn before any body is checked, so forward and self references resolve through fresh-instantiation rather than a Unit-fallback `fn_env` entry — closing the source-order hole flagged in PR review. `check_fn` populates `current_generic_subst` from the fn's `[A, B]` list and `current_row_var_subst` from `![ ... | e]`, threading them through every `ty_from_type_expr_here` call inside the body. `check_lambda` now inherits the enclosing fn's `current_generic_subst` so a lambda inside `fn id[A]` can reference `A`; lambda body-vs-return also routes through `unify_ty`. Constructors of generic user types (`Cons(1, Nil)`) allocate fresh vars per declared generic param via `fresh_user_instance_with_subst`; field types resolve under the merged subst; arg-vs-field types unify, pinning the user-type args. **Pattern matching** likewise installs a per-pattern subst from the scrutinee `Ty::User(_, args)` zipped with the type's generic_params before resolving sub-pattern field types, so `match xs: List[Int] { Cons(h, _) => ... }` correctly types `h` as `Int`. E0124 / E0125 placeholder errors deleted from the catalog; replaced with E0126 (occurs), E0127 (row occurs), E0128 (row mismatch), E0129 (arity mismatch), E0131 (Apply on primitive / generic-param). Codegen-entry guard `contains_apply_or_generic_ref(&Program)` walks the AST for surface generic syntax and asserts at `emit_object` entry; the walker recurses into `Apply` args so a nested generic-param ref doesn't slip through. Four walker unit tests directly exercise the rejection / acceptance paths — closes the verification-debt entry "Codegen path for un-monomorphized generic params" fully (no Task 49 dependency). 33 new unit tests cover generic id, two-instantiation calls, generic compose-shape, generic ctor / variant / return type, arity mismatch (E0129), Apply on primitive (E0131), unknown head (E0112), open caller / closed callee, occurs check (direct unify_ty test for E0126), row-unifier direct tests for closed-vs-closed (E0128), closed-vs-open absorption, closed-vs-open missing-effect (E0128), open-vs-open shared-tail merge, row occurs (E0127), forward-reference between generic fns, pattern bind from generic ctor, Pair instantiation arg mismatch (E0044), self-recursive generic fn, lambda-internal type var, four codegen-walker rejection cases, plus all the prior Task 48 scenarios. All 281 typecheck tests pass + 4 codegen tests; pod-verify green.
- Task 49 — Monomorphization: reachability-bounded, typed IR preserved
  - status: done
  - commits: [858b4c2]
  - notes: New `compiler/src/monomorphize.rs` (~960 lines initial; ~1100 final after rounds 2–3) replaces the Plan A1 identity stub. Adds three-step pipeline: (1) instantiation capture during typecheck — new `pending_call_instantiations` and `pending_ctor_instantiations` on `Tc` track fresh-var ids per `Expr::Ident` use of a top-level fn (via `instantiate_with_vars`) and per ctor use of a generic user type (via `fresh_user_instance_with_subst_and_ids`); end-of-typecheck resolves them through `subst` to produce concrete `Vec<Ty>` per site, exposed as `CheckedProgram::call_site_instantiations` and `CheckedProgram::ctor_site_instantiations` keyed by use-site span; (2) post-elaborate worklist BFS rooted at `main` — `program_has_generics` early-returns for non-generic programs (every Plan A1/A2/A3 program), preserving zero overhead; otherwise `Monomorphizer::run` drains fn and type worklists to fixpoint, cloning each generic decl per (name, type-args) tuple; (3) AST rewrite pass — every cloned body has its `TypeExpr::Apply` and generic-param `TypeExpr::Named` resolved to concrete primitives or mangled user-type names, every fn-call callee Ident rewritten to `mangle_fn(name, args)`, every ctor site rewritten to `mangle_ctor(name, type_args)`. **Canonical mangling** pinned via PR #16 round-2 fixup (`994f083`): `$` separator within a single type-arg, `$$` between top-level args. Top-level fn → `f$$<canon(T1)>$$<canon(T2)>$$...`. Top-level type → `Foo$$<canon(T1)>$$...`. Ctor of generic type → `C$$<canon(T1)>$$...`. `canon(User(name, args))` = `name` for arity 0, else `name$<canon(a1)>$...`. The `$` character is lexer-rejected in surface syntax (precedent: A2's `$lambda_N` synthetic names) so post-mono identifiers cannot collide with user identifiers — closed the round-1 soundness hazard where `type List_Option[A]` instantiated at Int could silently collide with `List[Option[Int]]` in `type_seen`. Plan example pinned by tests: `list_map$$List$Option$Int$$List$Option$Int`. New E0132 ambiguous-polymorphism diagnostic at end-of-typecheck. canon_ty `Ty::Var` / `Ty::Fn` arms hardened to `unreachable!`. **Effect rows preserved** per Plan B v1: codegen-entry walker at `compiler/src/codegen.rs:128` no longer rejects `f.effect_row_var.is_some()` or `Expr::Lambda { effect_row_var: Some(_), .. }`. **Pattern-ctor rewrite** uses `match_scrut_tys` + a new `ctor_to_type` index built from the original TypeDecls; round-3 fixup (`7a76c43`) added per-sub-pattern field-type threading via `Monomorphizer::variant_field_types(owning, ctor, type_args)` — closes the regression where nested generic ctor patterns over `Option[List[Int]]` panicked at the Plan A1 fall-through `unreachable!`. **`CheckedProgram::fn_schemes`** now exposed publicly so monomorph can map fresh-var ids back to declared generic-param names. New `GenericInstantiation` type. 40 net new unit tests across rounds 1–3 cover: `canon_ty` rendering for all primitive and user variants, mangle_fn/mangle_type/mangle_ctor for empty / single / multi / nested arg cases including the plan's `list_map$$List$Option$Int$$List$Option$Int` example, Substitution::apply_to_ty resolving Var/User/Fn through subst, ty_to_type_expr round-trip, 9 end-to-end pipeline tests through `lex → parse → resolve → typecheck → elaborate → monomorphize`, `nested_generic_ctor_pattern_threads_inner_scrut_ty` (round-3 reproducer with outer `Some$$List$Int`/`None$$List$Int` and inner `Cons$$Int`/`Nil$$Int`), and `effect_row_var_preserved_on_clone_passes_codegen_walker`. 281 → 321 compiler lib tests across the rounds.
- Task 50 — Color inference: per-monomorph, SCC-aware, `--dump-color`
  - status: done
  - commits: [82e0f97]
  - notes: New `compiler/src/color.rs` replaces the Plan A1 always-Native stub. Per-monomorph local analysis: open effect row (`![IO | e]`) → CPS; non-`IO` effect in row → CPS; body (or nested lambda body) performs a non-`IO` effect → CPS; else native. Reasons are stable, machine-readable, and human-readable. **Iterative Tarjan SCC** over the call graph (no recursive frames; safe under monomorph counts that grow with typeclass dictionary specialization). **Reason text per-node names each fn's own most-proximate cause**: intrinsic-CPS members keep their specific local reason; bridge members (with outgoing edge to a CPS SCC in a different SCC) get `cps: transitively calls <callee> which is cps`; non-bridge members propagated through SCC membership get either `cps: in SCC with intrinsically-cps member <name>` or `cps: in SCC bridging to cps callee via <name>` depending on which peer caused the taint. **Edge insertion is precise**: drives off `CheckedProgram::call_site_instantiations` (typecheck records every Ident — call or value position — that resolved to a top-level fn under env-precedence rules), so a parameter or `let` binding shadowing a top-level fn name does NOT produce a spurious edge. **`--dump-color` CLI flag** lands in `cli.rs` + `main.rs` + new `pipeline::dump_color` helper: prints `<name> native|cps <reason>` per fn (one line per monomorph in program order) to stdout, no codegen. `-o <path>` is accepted under `--dump-color` (shell-history ergonomics) and the path is recorded so `main.rs` prints a stderr warning that no executable was produced; `--print-runtime-stats` conflicts and emits a usage error. **`pipeline::DumpColorError` enum** distinguishes ReadFailed from FrontEndErrors(usize). **20 color::tests** unit tests cover all classifications + SCC algorithm corner cases: open-row CPS, non-IO-row CPS, perform-non-IO CPS, caller-callee both native, mutual recursion native, mutual recursion with one CPS member tainting whole SCC, transitive CPS taint, unrelated cycle non-pessimization, lambda body perform tainting parent, **self-loop single-node SCC** (native + CPS variants), **let-bound-fn-value taints parent via outgoing edge**, **3-SCC chain transitive propagation**, **transitive-only SCC reason branch** (no intrinsic member, only bridge member), **parameter shadowing precision** (now discriminating — see review-fixup-2 below). 11 cli::tests (4 for --dump-color: default + human formats, -o-recorded-for-warning, conflict with --print-runtime-stats). 2 e2e tests exercise `sigil <input> --dump-color` end-to-end. Pod-verify green; full lib tests deferred to CI. Review-fixup-1 commit: addresses all six "must-fix" items from PR #17 review (iterative Tarjan, span-keyed edges, per-node reason proximity with two-variant SCC fallback, `Result<String, DumpColorError>`, `-o` warning, renamed `unused_param_warning_silenced`). Review-fixup-2 commit: addresses the non-blocking residual concern that `parameter_shadowing_top_level_fn_does_not_taint_caller` was non-discriminating under Stage 5 (typecheck rejects non-IO effects, so `dangerous` ended up Native and the test passed under either edge regime). Reviewer's option 3 applied — added `unique_span()` test helper (atomic-counter span generator) plus `synth_program_with_calls(items, calls)` test helper that takes an explicit calls map. Rewrote the test as a synthetic program: `dangerous` is intrinsically CPS (Raise effect), `unrelated` is Native, `caller(dangerous: Int)` body has both a param-ref `dangerous` (shadow Ident with unique span deliberately omitted from calls map) AND a real call to `unrelated()` (Ident span recorded in calls map). Asserts `caller` Native — discriminating: under the old name-only heuristic caller would acquire a spurious edge to dangerous (CPS) and falsely classify CPS, but the precise calls-map-driven edge logic excludes the shadow Ident.
- Task 51 — `examples/generic_map.sigil` + e2e
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: New `examples/generic_map.sigil` declares `type List[A] = | Nil | Cons(A, List[A])`, `fn map[A](xs: List[A]) -> List[A] ![]` (structure-preserving traversal — Sigil v1 has no `TypeExpr::Fn` surface syntax, so the design doc's canonical higher-order `map[A,B,e](xs, f) -> List[B]` is not expressible; this is the closest Plan B can express, semantically `map id`), and `fn length[A](xs: List[A]) -> Int ![]` (generic fold to a concrete scalar). `main` instantiates each generic at `Int` and `String` in the same scope, producing four monomorph clones (`map$$Int`, `map$$String`, `length$$Int`, `length$$String`) plus mangled ctor sites (`Nil$$Int`, `Cons$$Int`, `Nil$$String`, `Cons$$String`). Two e2e tests in `compiler/tests/e2e.rs`: `generic_map_example_prints_3_and_2` exercises the full lex → parse → typecheck → elaborate → monomorphize → color → codegen → run pipeline (the FIRST end-to-end test of user-authored generic syntax through codegen, per PR #17 reviewer's Stage 5 review note); `generic_map_dump_color_all_native` exercises Task 50's per-monomorph color inference, pinning each expected mangled name independently so a mangling-format slip on any single one (e.g. `map$$String`) lands on a directed assertion. `scripts/smoke.sh` and `scripts/reproducibility.sh` updated to include the new example so the per-host byte-stable-binary invariant covers the full generic pipeline.
- Task 52 — Validation prompts P16, P17
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: P16 — generic identity at Int and String; fully expressible in Plan B's surface (no `TypeExpr::Fn` required); oracle `"42\nsigil\n"`. P17 — generic compose applied across types; requires `TypeExpr::Fn` for the higher-order parameters (same dependency as P09 / P10), so deferred to whenever first-class function types ship; until then, graded only against "program compiles". P17's distinguishing feature versus P10 is `A != C` (the result type genuinely travels through composition rather than absorbing into the trivial endo-functor case). Bank now at 17/20; P18-P20 land with Stage 6 Task 61 (Raise-based parser, State-threaded counter, multi-shot Choose).

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
