# Plan C — Deviations log

Plan C deviation entries follow the conventions established in `PLAN_B_DEVIATIONS.md` and `PLAN_B_PRIME_DEVIATIONS.md`. Each entry:

- Heading: `## <date> — [DEVIATION Task N] <one-line topic>` (or `[CLOSED]` suffix when retired).
- Body sections: **Context**, **Why accepted**, **Closure path**, **Failure mode**, **Implementing commit(s)**.
- Deviations are logged *before* the implementing commit per Plan B/B' commit discipline.

## 2026-04-29 — [DEVIATION Task 62.0] Stdlib import resolution lands as a Task 62 prerequisite

**Context.** Plan C Stage 7 (Tasks 62–78) prescribes nine stdlib modules written in sigil with Rust-driven tests that "compile a small sigil program and check output". Those small programs need to *use* the stdlib modules, but at Plan C start the import pipeline is dormant:

- `Item::Import(_)` parses (parser enforces `std.*` prefix via E0031) but every downstream pass treats it as a no-op (`compiler/src/{resolve,typecheck,monomorphize,closure_convert,codegen}.rs`).
- `compiler/src/stdlib_embed.rs` exposes a read-only `Dir<'static>` over `std/` via `include_dir!`, but only its own unit test consumes it.
- `std/io.sigil` is documentation-only; the `IO` effect's actual shape comes from `typecheck::builtin_effects()` via builtin injection (per `[DEVIATION Task 57]` in `PLAN_B_DEVIATIONS.md`). The file's own comment names "a future stdlib-loading task" as the resolution path.
- `Option[A]`, `List[A]`, etc. are not predefined; they appear only as example text in `compiler/src/errors/catalog.rs`.

Plan C's plan body does not enumerate "build the stdlib loader" as a numbered task. Two paths surfaced:

1. **Path A — real import resolution.** Make `import std.X` actually load `std/X.sigil` from the embedded tree, parse it, and prepend its items to the program. Aligns with Plan C's "stdlib written *in* sigil" framing in Stage 7.
2. **Path B — extend builtin injection.** Grow `builtin_effects()` and `builtin_fn_env()` to cover every Plan C stdlib type and function. Avoids import-resolution work but couples stdlib types to compiler internals; effectively writes the stdlib in Rust and uses `.sigil` files only as documentation.

**Why accepted.** Path A chosen. Path B contradicts the explicit Stage 7 framing ("All stdlib modules implemented in sigil; every public function unit-tested") and would scale poorly across nine modules with hundreds of functions. Path A is a one-time infrastructure investment whose surface area is small (a single new pass between parser and `resolve.rs`); the syntax already exists, so this does not violate Plan C's "Do not change language semantics" guardrail. The `std/io.sigil` comment explicitly names this work as the future surface.

The pre-Task-62 prerequisite is numbered Task 62.0 to keep Plan C's numbered-task ledger intact while making the work visible in `PLAN_C_PROGRESS.md`.

**Scope (Task 62.0):**

1. New module `compiler/src/imports.rs` exposing `pub fn resolve(program: Program) -> (Program, Vec<CompilerError>)`.
2. Algorithm: DFS over `Item::Import` paths. For each `["std", X, ...]` path:
   - Convert to `X/.../<last>.sigil` against the embedded `STD` tree.
   - If the path is in a `BUILTIN_INJECTED` skip-list (initially `["io"]`), no-op.
   - Else lex + parse the embedded source, recurse into its imports (cycle detection via `in_progress` set), then append the loaded module's non-import items to the resolved program.
   - Each module loaded at most once globally (dedupe via `loaded` set).
3. Two new error codes:
   - **E0032** — stdlib module not found in the embedded tree.
   - **E0033** — circular import.
4. Pipeline wiring: insert `imports::resolve` between `parser::parse` and `resolve::resolve` in `compiler/src/pipeline.rs::compile` and `dump_color`.
5. `typecheck.rs`'s `pipeline` test helper updated to thread imports::resolve so the discipline sweep `no_user_facing_error_uses_e0001` covers E0032/E0033.

**Out of scope (deferred to later Plan C work or to a v2 import system):**

- User-code imports (E0031 parser-side rejection stays).
- Selective imports (`import std.option.Some` — Plan C uses module-level imports only).
- Renaming / aliasing.
- Visibility (`pub`/`priv`) — every top-level item in a stdlib module is public to importers.
- Nested namespacing in name resolution. Imported items live in a single flat namespace alongside user items; collisions surface via existing typecheck duplicate-fn / duplicate-type / E0136 paths.

**Closure path.** Closed at the implementing commit. Path A is the v1 import system; v2 may layer selective/aliased imports on top without re-architecting Task 62.0's pass.

**Failure mode.** If a future Plan C stdlib module redeclares a name already provided by builtin injection (e.g. `std/io.sigil` declaring `effect IO`), the resolver's skip-list silently no-ops the import. If the skip-list grows out of sync with `builtin_effects()` / `builtin_fn_env()`, the failure surfaces as a duplicate-effect / duplicate-fn diagnostic from typecheck — loud, not silent.

**Implementing commit(s).** [HEAD] (this commit lands the deviation entry; the next commit lands the resolver).

## 2026-04-29 — [DEVIATION Task 63] bind_ty_var direction fix for two-param sum-type cross-arm unify

**Context.** While drafting `std/result.sigil` (Plan C Task 63: `Result[A, E]` plus `map`, `map_err`, `and_then`), every helper body of the form

```
match r {
  Ok(x)  => Ok(...),
  Err(e) => Err(...),
}
```

tripped E0132 ("type parameter `A` of `Result` is unconstrained at this construction site"). Reduced reproducer:

```
type Result[A, E] = | Ok(A) | Err(E)
fn id[A, E](r: Result[A, E]) -> Result[A, E] ![] {
  match r {
    Ok(x) => Ok(x),
    Err(e) => Err(e),
  }
}
```

This is structurally identical to Plan B Task 51's `generic_match_returning_generic_unifies_arms` test (which exercises `List[A]` and passes), except `Result` has *two* type parameters and each ctor only fixes one of them.

**Root cause** (verified by instrumented `apply_ty` trace at the pending-ctor sweep): cross-arm unify in `check_match` unifies `Result[A_outer, ?fE_ok]` (arm 1) with `Result[?fA_err, E_outer]` (arm 2). The first-param sub-unify is `Var(A_outer) ~ Var(?fA_err)`. `unify_ty`'s `(Ty::Var(id), other)` arm calls `bind_ty_var(A_outer, &Var(?fA_err))`, which inserts `subst[A_outer] = Var(?fA_err)`. After the cross-arm unify, `apply_ty(Var(?fA_ok))` follows the chain `?fA_ok → A_outer → ?fA_err`, returning `Var(?fA_err)` — a fresh ctor-instance var that is *not* in `outer_fn_var_ids`. The pending-ctor E0132 sweep fires.

The bug is the **bind direction**. With two unbound vars, the existing logic always binds the FIRST argument to the SECOND, regardless of which is outer-canonical and which is fresh-locally-allocated. `List[A]` never tripped this because there's only one type param and an unbound arm-var has no competing already-bound counterpart at cross-arm time.

**Why accepted.** This is a Plan-B-era latent bug surfaced by Plan C's first two-param generic sum type. Result is the canonical sum type for fallible computation; deferring it until v2 isn't an option. The fix is a small, well-known HM convention (union-find-by-min): when binding two unbound type-vars, prefer to make the higher-id var point at the lower-id one. Within a single `check_fn` invocation, outer-fn type-vars are allocated by `fresh_generic_subst` BEFORE any body fresh vars (line 2206), so within a fn body lower-id is the outer-canonical representative. Cross-fn ordering doesn't affect cross-arm unify because match arm bodies never span fn boundaries.

**Scope of fix.** Lines 1421-1466 of `compiler/src/typecheck.rs`'s `bind_ty_var`: when `t` derefs to `Ty::Var(other)`, if `other != id`, swap so the higher-id slot is bound to `Var(canonical_lower)`. The non-Var path (binding to a User / Fn / primitive type) is unchanged. Occurs-check fires symmetrically against the new bind target.

**Test pin.** `compiler/src/typecheck.rs::tests::two_param_sum_type_match_each_arm_constrains_one_param_typechecks` is the targeted regression test on the reduced reproducer. The `import_std_result_*` typecheck tests and `tests::std_result_*` e2e tests are the user-observable surface. All 552 existing tests pass with the fix; one Result test surfaced a *separate* bug in its own match-arm shape (mixed-type arms producing E0065) that the prior E0132 short-circuit had been masking.

**Closure path.** Closed at the implementing commit. The fix is permanent; no follow-up needed.

**Failure mode.** A future test relying on the OLD non-deterministic bind direction would surface as a regression. None of the existing 552 tests do; the discipline sweep + Plan B Task 51's coverage tests pin the correct surface.

**Implementing commit(s).** [HEAD+1] (this commit lands the deviation entry; the next commit lands the fix and Task 63).

## 2026-04-29 — [DEVIATION Task 64] for_each deferred to v2; remaining list helpers ship under closed `![]` rows

**Context.** Plan C Task 64's `std/list.sigil` enumerates eight helpers: `map`, `filter`, `fold`, `length`, `reverse`, `append`, `range`, `for_each`. Seven of the eight ship cleanly under closed `![]` effect rows — they're pure transformations that operate on a list and return a list / int / generic value. `for_each` is structurally different: it iterates and calls a side-effecting function for each element, with the per-element callback's effects threading through the iteration.

A useful `for_each` requires three Sigil v1 surface features that are not currently expressible together:

1. **A `Unit` literal expression.** The empty-list arm `Nil => ???` needs to produce a `Unit` value. Sigil v1 has `Ty::Unit` and uses it as the return type of `perform IO.println`, but no surface syntax constructs a `Unit` value directly. Today's only Unit-producing expressions are calls whose return type is Unit (e.g., `perform IO.println(...)`). With no element to operate on in the `Nil` arm, there's nothing to call.
2. **Sequencing in match arm bodies.** The `Cons(h, t)` arm needs to do two things: invoke `f(h)` (for its side effect, discarding the Unit), then recurse via `for_each(t, f)`. Sigil v1 parses match arm bodies as expressions, not blocks; `=> { let _: Unit = f(h); for_each(t, f) }` is not accepted (the `{` after `=>` is a parse error per Plan B's `parse_handle_op_arm` / `parse_match_arm` shape).
3. **Row-polymorphic fn-typed parameters.** Even with the above two solved, `f: (A) -> Unit ![]` (the closed-row form) is useless — pure-row callbacks can't print, mutate state, etc. A useful surface needs `f: (A) -> Unit ![ | e]` with the row variable threaded through `for_each`'s own row. This shape may parse today (top-level fn declarations support `![IO | e]`) but the cross-product with fn-typed-parameter typing isn't exercised in any existing test.

The combined surface (Unit literal + arm-body sequencing + row-poly fn-typed params) is a genuine Plan C scope ask not enumerated by any of Plans A1–B'. Each of the three pieces is independently small; together they widen the language surface in ways that risk Plan C's "Do not change language semantics" guardrail.

**Why accepted.** Shipping seven of eight list helpers immediately is strictly more useful than blocking on `for_each`. Callers needing per-element effects can write a direct recursive `match` helper today (the same shape these helpers use internally — see e.g. how `length` recurses). The seven shipped helpers cover the `range` / fold / map / filter / append / reverse / length surface that the spec validation prompts (Stage 9) actually exercise; `for_each` is a v2 ergonomics ask, not a Stage 9 blocker.

**Closure path.** Closed when one of the three feature gaps lands and unlocks a useful `for_each`. Three orderings are viable:

- **Path A (cheapest):** Add a `Unit` literal expression at the parser/elaborate layer (purely surface; Ty::Unit already exists). Combined with a separate sequencing primitive (e.g., a `seq` builtin that takes `(Unit, T)` and returns `T`), this lets `for_each` ship under closed `![IO]` row. Useful for the dominant printing-each-element case.
- **Path B (cleaner):** Allow blocks as match arm bodies. Larger parser change but matches v2's intent.
- **Path C (most general):** Row-polymorphic fn-typed parameter syntax. Needed regardless for full v2 effect ergonomics.

**Failure mode.** None — the documentation on `std/list.sigil` is explicit that `for_each` is intentionally absent and points users at the "write a recursive match helper" workaround.

**Implementing commit(s).** [HEAD] (this entry); shipped alongside `std/list.sigil` and the seven non-`for_each` helpers in [HEAD+1].

## 2026-04-29 — [DEVIATION Task 65] [CLOSED] Split into runtime foundation (part 1) and compiler integration (part 2)

**Context.** Plan C Task 65 ships immutable `Array[A]` with five operations: `array_alloc`, `array_length`, `array_get`, `array_set`, plus `from_list` / `to_list` (per the plan body). Unlike Tasks 62–64 (`std/option`, `std/result`, `std/list`), Task 65 explicitly requires "runtime support for array allocation; extend `runtime/`." That extension is the foundation for Tasks 66 (`mut_array`), 66.5 (`byte_array`), 66.6 (`mut_byte_array`), 67 (`string_builder`), and 69 (`int64`) — all of which need similar TAG / heap-layout / FFI work.

The full Task 65 (runtime + compiler integration + sigil source + tests) is a ~600–800 LOC change spread across:
- `runtime/src/array.rs` (5 FFI primitives + Rust unit tests).
- `header-constants/src/lib.rs` (TAG_ARRAY = 0x04).
- `runtime/src/counters.rs` (2 new counter slots).
- `compiler/src/typecheck.rs` (builtin `Array[A]` type registration via `tc.types`; builtin generic `Scheme`s for the 5 operations in `tc.fn_schemes`).
- `compiler/src/codegen.rs` (5 FFI declarations; 5 special-case `Expr::Ident` dispatch arms in `lower_call`; corresponding `type_of_expr` arms).
- `std/array.sigil` (documentation file; the operations are builtin so the file doesn't carry implementations).
- Tests (typecheck unit tests + e2e tests).

**Why accepted.** The runtime foundation is independently useful: each of Tasks 66 / 66.5 / 66.6 / 67 / 69 will reuse the TAG-based heap-layout pattern. Shipping the runtime in part 1 lets CI verify the foundation in isolation before piling compiler integration on top, reducing the blast radius of any latent bug in either layer.

**Scope split:**

- **Part 1 (this commit pair):** runtime/src/array.rs with the 5 FFI primitives, TAG_ARRAY constant, counters wiring, 7 Rust unit tests covering zero-length / fill / empty / immutable-set / set-chain / Sudoku-size (past the 6-bit count cap) / header-tag invariants. **No compiler integration yet** — the symbols exist in `libsigil_runtime.a` but aren't reachable from sigil source.
- **Part 2 (follow-up):** typecheck builtin Array type + builtin generic schemes; codegen FFI declarations + dispatch; `std/array.sigil` (likely documentation-only, like `std/io.sigil`); typecheck-level tests; e2e tests; PROGRESS / coverage update. Splits cleanly because the compiler integration can be developed against the already-merged runtime foundation.

**Closure path.** Closed when part 2 ships and Task 65 reaches user-observable parity with the plan body.

**Failure mode.** Stage-7 progress is bottlenecked on part 2 — Tasks 66+ depend on a working `Array[A]` for some demo programs (sudoku.sigil uses `MutArray[Int]`). Part 2 is non-optional Plan C work, just sequenced after part 1.

**Implementing commit(s).** Part 1 runtime foundation: `1ec8ce3`. Part 2 compiler integration: `3b4b7ab` + monomorphize fix `fe14243`. Closure-path satisfied: `Array[A]` is reachable from sigil source with the immutable surface working end-to-end across e2e.

## 2026-04-30 — [DEVIATION Task 65] `array_empty` ships in place of `from_list` / `to_list`

**Context.** Plan C Task 65 plan body lists `array_alloc`, `array_length`, `array_get`, `array_set`, plus `from_list` / `to_list` as the Array surface. What part 2 actually shipped: those four plus a fifth FFI primitive `array_empty()` — and `from_list` / `to_list` are absent. Two related but distinct deviations from the plan body; each calls for its own justification.

**Why `array_empty` was added.** Codegen lowers `Expr::Ident("array_empty")` against a generic builtin scheme `forall A. () -> Array[A]`. With zero value args there is no caller-supplied default to pass to `sigil_array_alloc(len, fill)`. A pure-codegen workaround (synthesise a default by element type) would require monomorph-time knowledge of `A` and a per-type default-value table; cleaner to expose `sigil_array_empty()` as a separate FFI symbol that allocates a zero-length array without touching `fill`. Mechanically trivial in `runtime/src/array.rs:96` (`sigil_array_empty` delegates to `sigil_array_alloc(0, 0)` — the fill byte is irrelevant when there are no slots).

**Why `from_list` / `to_list` are deferred.** Both are pure-sigil-implementable once Tasks 71–76 ship the effect-handler stdlib (`Raise`, `State`, `Choose`, `Mem`). Concretely: `from_list[A](xs: List[A]) -> Array[A]` walks the list while threading a `MutArray[A]` index counter (needs `Mem`), then returns the immutable snapshot via `array_freeze` (would need either an `Array[A] <-> MutArray[A]` cast op or a dedicated runtime primitive). `to_list[A](arr: Array[A]) -> List[A]` is structurally a fold from `array_length(arr) - 1` down to `0`, building `Cons(get(arr, i), acc)` — straightforward in pure sigil once recursive typecheck on `Array[A]` indexing types stabilises in Stage 7. Today's Task 65 surface (alloc/empty/length/get/set) is sufficient for Sudoku (Stage 8) and the spec-validation prompts that exercise array work.

**Why accepted.** Pushing `from_list` / `to_list` to a separate stdlib task keeps Task 65 focused on the runtime + compiler-integration foundation. The closure path is mechanical (write-in-sigil, no codegen surface change); deferring it does not block Tasks 66+ or the Stage 8 demos.

**Closure path.** Lands as a follow-up commit (or as part of Task 67 / 68 string work if scheduling overlaps) once `Mem` mutation is available, OR as a doctest authoring exercise alongside Task 78. Implementing surface: `std/array.sigil` gains documentation comments + the user-side `from_list` / `to_list` helpers; no runtime change required.

**Failure mode.** Users wanting `Array <-> List` interop in v1 must hand-roll a recursive `match` over `List[A]` plus immutable `array_set` chaining. Verbose but expressible.

**Implementing commit(s).** This deviation entry only — closes a documentation gap from PR #42 mid-flight review #6.

## 2026-04-30 — [DEVIATION Task 66] `Mem` ships as a marker effect; MutArray ops are gated-by-row, not perform-dispatched

**Context.** Plan C Task 66's plan-body wording — "`MutArray[A]` operations exposed through the `Mem` effect. Runtime support in `runtime/src/mem.rs`: in-place array mutation under the top-level `Mem` handler" — admits two implementation shapes:

1. **Effect-dispatch shape.** `effect Mem { new_array_int: (Int, Int) -> MutArray, ... }` declared as a builtin; user calls `perform Mem.new_array_int(10, 0)`; the runtime arm fn for `Mem.new_array_int` allocates and returns. A top-level Mem handler frame in the main shim wires each op to a runtime arm fn (mirrors Plan B Task 57's IO + ArithError pattern).
2. **Marker-effect shape.** `effect Mem { /* zero ops */ }` declared as a builtin; `MutArray[A]` is a builtin generic type alongside `Array[A]`; the four operations (`mut_array_new` / `_length` / `_get` / `_set`) are builtin generic functions whose effect rows declare `![Mem]`. Users in fns whose row includes `Mem` can call them; users without `Mem` in their row get E0042. No runtime Mem handler frame; no `perform` machinery.

**Why accepted (marker-effect shape).** The effect-dispatch shape requires generic operations on a non-generic effect (`Mem.new_array_int` has return type `MutArray[A]` for the caller's `A`), which Sigil v1's effect declarations don't currently support cleanly — `effect Mem[A] { ops }` works syntactically per Plan B Task 53, but builtin_effects()'s shape is non-generic. Per-element-type op variants (`new_array_int`, `new_array_string`, ...) would balloon the effect surface and tie API ergonomics to the typechecker's primitive-type set.

The marker-effect shape preserves every user-observable invariant Plan C cares about:

- Code that mutates declares `![Mem]` in its row.
- The compiler rejects mutation calls from rows that don't contain Mem (E0042).
- `main` declares `![Mem]` to permit mutation; the type-level "top-level Mem handler" is the absence of a deeper override.
- Runtime mutation primitives live in `runtime/src/mem.rs` per the plan.

**What's lost.** Users cannot intercept Mem operations via `handle ... with { Mem.X(...) => ... }` — there are no Mem ops to intercept. A v2 path that ships `effect Mem[A] { ... }` (generic-effect builtin support) restores this. The handler-swap testing pattern Plan C documents in Stage 9's spec applies to user-declared effects (`Raise`, `State`, `Choose`); Mem mutations are intentionally non-overridable in v1, mirroring how `IO.println` wasn't user-overridable until Plan B Task 57's row-polymorphic refactor.

**Closure path.** Closes when (a) `effect Mem[A] { new_array: (Int, A) -> MutArray[A], ... }` ships as a generic builtin effect, OR (b) Sigil grows per-op generic params `effect Mem { new_array[A]: (Int, A) -> MutArray[A], ... }`. Either path lets v2 swap Mem to true effect-dispatch shape without changing Plan C-era user code (call sites stay `mut_array_new(...)`; only the lowering changes).

**Failure mode.** A v2 user trying to mock Mem via `handle ... with { Mem.new_array(args, k) => ... }` gets E0139 (unknown op on declared effect) until v2 ships generic Mem ops. Documented in `std/mut_array.sigil`.

**Implementing commit(s).** [HEAD+1] (this commit lands the deviation; the next commit lands the implementation).
