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

## 2026-04-30 — [DEVIATION Task 66.5] User-side `byte_from_int` / `string_from_bytes` / `from_list` / `to_list` wrappers deferred

**Context.** Plan C Task 66.5's plan body lists six core ops (`length`, `get`, `concat`, `slice`, `from_list(List[Byte]) -> ByteArray`, `to_list(ByteArray) -> List[Byte]`) plus String interop (`string_to_bytes(s) -> ByteArray`, `string_from_bytes(ba) -> Result[String, Utf8Error]`) plus the `Byte` primitive wired up via `byte_from_int(n: Int) -> Option[Byte]`.

The four user-facing wrappers — `byte_from_int(n) -> Option[Byte]`, `string_from_bytes(ba) -> Result[String, Utf8Error]`, `from_list(xs: List[Byte]) -> ByteArray`, `to_list(ba: ByteArray) -> List[Byte]` — each depend on a different stdlib type:

- `byte_from_int` returns `Option[Byte]` → needs `import std.option`.
- `string_from_bytes` returns `Result[String, Utf8Error]` → needs `import std.result` (and a `Utf8Error` declaration).
- `from_list` / `to_list` consume / produce `List[Byte]` → need `import std.list`.

A natural shape for `std/byte_array.sigil` is a single module that imports all three and ships all four wrappers. **That shape collides on the flat namespace.** Each of `std/option.sigil`, `std/result.sigil`, `std/list.sigil` declares a `fn map`; loading all three from one transitive import path produces a duplicated `map` scheme registration where the last-loaded module wins. Inside `std/list.sigil`'s own `map` body, the recursive `map(t, f)` call then resolves to whichever module was registered last — the typechecker reports `expected Result[?,?], got List[?]` cascades inside list.sigil with no obvious user-visible cause.

**Why accepted (defer the wrappers).** The collision is the architectural concern PR #42 review #2 (architectural concerns section) flagged: "Single flat namespace for stdlib imports... collision likelihood grows. The escape hatch (E0136 / duplicate-fn) is correct but loud-by-design." The proper fix is module-qualified names (`std.list.map` vs `std.option.map`), which is a separate Plan C task (queued for the namespace-architecture work in Tasks 67–72 if the collision surfaces sharper).

For Task 66.5, the workaround is:

- Ship the **runtime layer** + **builtin primitives** — these don't need user-side imports.
- Ship `std/byte_array.sigil` as **documentation-only** (mirrors `std/array.sigil` / `std/mut_array.sigil`), added to `BUILTIN_INJECTED` skip-list.
- Defer the four wrappers. Until the namespace fix lands, callers who want them write them in their own program.

**What ships in Task 66.5:**

1. Runtime `runtime/src/byte_array.rs` with 9 FFI primitives:
   `sigil_byte_array_alloc` / `_empty` / `_length` / `_get` / `_concat` / `_slice` (6 ops) plus `sigil_string_to_bytes` / `sigil_string_from_bytes_validate` / `sigil_string_from_bytes_alloc` (3 string-interop ops).
2. Two new helpers in `runtime/src/byte.rs`: `sigil_byte_in_range(n: i64) -> bool`, `sigil_byte_truncate(n: i64) -> u8`. These factor what would have been `byte_from_int`'s body so user-side code can construct `Option[Byte]` directly: `match byte_in_range(n) { true => Some(byte_truncate(n)), false => None }`.
3. New `TAG_BYTE_ARRAY = 0x06` in `header-constants` + 2 counter slots.
4. `ByteArray` registered as a non-generic builtin type (`builtin_types`).
5. 11 builtin schemes (`register_builtin_byte_array_schemes`) covering all 6 ops + 3 string-interop primitives + 2 Byte helpers.
6. Codegen FFI declarations + `lower_call` dispatch arms + `type_of_expr` predictions, all flowing through `BuiltinFuncIds` / `BuiltinFuncRefs` (no per-call-site churn — see PR #42 review #10's consolidation).
7. `std/byte_array.sigil` documentation file, added to `imports::BUILTIN_INJECTED`.

**What doesn't ship (deferred to the namespace-fix task):**

1. `byte_from_int(n: Int) -> Option[Byte]` — user-side wrapper around `byte_in_range` + `byte_truncate`.
2. `string_from_bytes(ba: ByteArray) -> Result[String, Utf8Error]` — user-side wrapper around `string_from_bytes_validate` + `string_from_bytes_alloc`.
3. `from_list(xs: List[Byte]) -> ByteArray` — recursive concat.
4. `to_list(ba: ByteArray) -> List[Byte]` — recursive build via accumulator.
5. `type Utf8Error = | InvalidUtf8(Int)` — user-side declaration alongside `string_from_bytes`.

**Closure path.** Closes when stdlib namespace qualification ships (e.g. `std.list.map` vs `std.option.map`, OR per-module re-export with explicit aliasing). At that point, `std/byte_array.sigil` flips from doc-only to a real importable module that ships the four wrappers + `Utf8Error`. Removing the entry from `BUILTIN_INJECTED` is the structural step. The runtime and builtin-scheme surface stays unchanged.

**Failure mode.** Users wanting `Option[Byte]`-shaped byte construction or `Result[String, Utf8Error]`-shaped UTF-8 decoding write a few lines of sigil against the deferred-wrapper-equivalent surface. The runtime primitives carry the same algorithmic content, so the user code is straightforward — the loss is API ergonomics, not capability.

**Implementing commit(s).** Part 1 (runtime foundation): `5ec5fef`. Part 2 (compiler integration + doc-only stdlib file + typecheck/e2e tests): `6304ba8`.

## 2026-04-30 — [DEVIATION cross-cutting] v2 path: `extern fn` + `opaque type` for stdlib FFI declarations

**Context.** Sigil v1's stdlib has two classes of module:

1. **Sigil-expressible.** `std/option.sigil`, `std/result.sigil`, `std/list.sigil` — variant sum types + helper fns, fully written in sigil. The user-visible source IS the implementation.

2. **Opaque-runtime-backed.** `std/io.sigil`, `std/array.sigil`, `std/mut_array.sigil`, `std/byte_array.sigil`, `std/mut_byte_array.sigil` — heap-allocated objects with non-variant layouts (`{header, length, payload}`) plus FFI functions backed by `runtime/src/*.rs`. The user-visible `.sigil` file is **documentation-only**; the actual type registrations and operation schemes are injected at the typechecker via `builtin_types()` / `register_builtin_*_schemes()` and at codegen via FFI declarations + dispatch arms.

The split exists because Sigil v1's surface doesn't support either:

- **Opaque runtime-managed types.** No syntax says "the layout of this type lives outside sigil — the runtime knows it." Variant sum types can declare `type T = | Foo | Bar(Int)`; there's no way to declare `type ByteArray` with an opaque non-variant payload.
- **External function bindings.** No `extern fn name(args) -> ret` syntax to declare an FFI symbol. The compiler's builtin-injection pattern (Plan B Task 57 for IO/ArithError, Plan C Tasks 65/66/66.5/66.6 for Array/MutArray/ByteArray/MutByteArray) registers schemes directly in the typechecker.

The result: each opaque-runtime stdlib module has roughly the same structure — a doc-only `.sigil` file describing the surface, plus typecheck.rs / codegen.rs additions that mirror the sigil signatures one-to-one. Adding a new opaque builtin (Task 67's `string_builder`, Task 69's `int64`) repeats the pattern.

**Why accepted (defer to v2).** Adding `extern fn` + `opaque type` syntax is a real language change touching parser, AST, typecheck, and a small linkage layer at codegen. Plan A1's "Do not change language semantics" guardrail (carried into Plan C) keeps language-surface work out of the stdlib-growth tasks. The current builtin-injection pattern works correctly; the cost is convention drift between stdlib modules (real vs doc-only) and a small per-module mechanical cost when adding new runtime-backed primitives.

**What v2 enables.** With both features in place, `std/byte_array.sigil` could declare the full surface in sigil source:

```sigil
opaque type ByteArray

extern fn byte_array_alloc(len: Int, fill: Byte) -> ByteArray ![]
  = "sigil_byte_array_alloc"

extern fn byte_array_length(ba: ByteArray) -> Int ![]
  = "sigil_byte_array_length"
// ... etc.

// User-side wrappers stay as ordinary sigil:
fn byte_from_int(n: Int) -> Option[Byte] ![] {
  match byte_in_range(n) {
    true => Some(byte_truncate(n)),
    false => None,
  }
}
```

Compiler internals would consume the `extern fn` declarations directly — no `register_builtin_*_schemes()` boilerplate, no `BuiltinFuncIds` extension per primitive, no separate documentation-vs-implementation drift. `imports::BUILTIN_INJECTED` retires entirely.

**Closure path.** Tracked as a v2 language-surface task. Implementation steps would touch:

1. Parser: `extern fn name(args) -> ret ![row]\n = "C_symbol"` + `opaque type Name`.
2. AST: `Item::ExternFn { name, sig, c_symbol }` and `Item::OpaqueType { name }`.
3. Typecheck: ExternFn registers as a regular `Scheme` with no body; OpaqueType registers in `tc.types` with empty variants (mirroring today's builtin-type injection).
4. Codegen: walk `Item::ExternFn` items at `emit_object` start to populate `BuiltinFuncIds` automatically; lower call sites via the existing dispatch mechanism (no per-name dispatch arm needed — the FFI-call lowering generalises).
5. Stdlib migration: convert `std/io.sigil`, `std/array.sigil`, `std/mut_array.sigil`, `std/byte_array.sigil`, `std/mut_byte_array.sigil`, plus future Task 67 `string_builder`, Task 69 `int64`, etc. from doc-only to fully-declared.

**Failure mode (today).** Adding a new runtime-backed stdlib primitive in v1 requires the full mechanical sweep: runtime FFI + counter/tag wiring + typecheck builtin scheme + codegen FuncId/FuncRef extension + dispatch arm + `type_of_expr` prediction + entry-walker globals + doc-only `.sigil` update + tests. PR #42 review #10 already drove the `BuiltinFuncIds` / `BuiltinFuncRefs` consolidation that absorbed most of the per-call-site cost; the remaining ~5-line-per-primitive overhead is what v2 retires.

**Implementing commit(s).** Tracking entry only. Would land as a separate language-surface task; documented here so Task 67+ implementers know the convention is v1-bounded, not architectural.

## 2026-04-30 — [DEVIATION Task 66.6] Plan A2's `byte_to_int` finally wired to the sigil surface

**Context.** Plan A2 task 25 shipped `sigil_byte_to_int` (i.e. `Byte -> Int` widening) as a runtime FFI primitive, but never wired it through to a sigil-side builtin scheme — the surface plan called for `Char` / `Byte` work to grow incrementally and the loop closure was deferred each time. Task 66.6's e2e tests (`std_mut_byte_array_*`) need to widen a `Byte` back to `Int` for `int_to_string` + `IO.println` printing, surfacing the missing wire.

**Why accepted.** Wiring it through is a 3-line change (codegen FFI declaration + dispatch arm + builtin scheme) that completes a Plan A2 carryover. The alternative — keeping `byte_to_int` as a runtime-only symbol and forcing test code to re-implement the widening via match expressions — is strictly worse and gates byte-typed test scenarios on no real architectural change.

**Closure path.** Closed at this commit. `byte_to_int(Byte) -> Int ![]` is callable from sigil source. Plan A2's task 25 carryover line in `PLAN_A2_PROGRESS.md` is implicitly closed (the runtime symbol's surface exposure no longer pending).

**Failure mode.** N/A — closure-completion entry.

**Implementing commit(s).** `279c2d8` (the Task 66.6 commit pair).

## 2026-04-30 — [DEVIATION Task 68] String surface — byte-indexed v1 + namespace-blocked v2 deferrals

**Context.** Plan C Task 68's plan body lists 20 String operations: `string_length`, `string_byte_at`, `string_char_at`, `string_chars`, `string_concat`, `string_substring`, `string_compare`, `string_starts_with`, `string_ends_with`, `string_contains`, `string_index_of`, `string_split`, `string_join`, `string_trim`, `string_from_int`, `string_to_int`, `string_from_float`, `string_to_float`, `char_to_int`, `int_to_char`. Task 68 part 1 ships 12 of these; the remaining 8 are split across three deferral classes.

**What ships in part 1 (12 ops).** All byte-indexed surface operations: `string_concat`, `string_substring`, `string_byte_at`, `string_compare`, `string_starts_with`, `string_ends_with`, `string_contains`, `string_index_of`, `string_trim`, `string_to_int_validate`, `string_to_int_parse`, `string_length`. Plus `byte_to_int` from the parallel Task 66.6 cleanup. Each registered via `register_builtin_string_schemes` and dispatched in `lower_call`.

**Deferred class 1 — codepoint-aware ops (`string_char_at`, `string_chars`).** Both depend on a runtime UTF-8-aware iterator + the `Char` primitive promoted to user-side surface. v1 `Char` exists at the type level (Plan A2) but has no surface literal syntax; widening it to user-callable construction is a separate v2 task. Defer until the `Char` surface lands.

**Deferred class 2 — List-returning helpers (`string_split`, `string_join`).** Both produce / consume `List[String]`. As with Task 66.5's `from_list` / `to_list`, putting `string_split` in `std/string.sigil` would force `import std.list`, and any user importing `std.string` + `std.option` + `std.result` together hits the flat-stdlib-namespace `fn map` collision (per `[DEVIATION Task 66.5]`). Defer until namespace qualification ships.

**Deferred class 3 — Float helpers (`string_from_float`, `string_to_float`).** Sigil v1 has no `Float` primitive type. Defer until `Float` lands in v2 (the Plan C plan body doesn't queue a Float task; this is a v2 prerequisite).

**Deferred class 4 — Sum-typed wrappers (`string_to_int`, `string_from_int`).** `string_to_int` ships as the validate / parse pair; the `Result[Int, ParseError]` user-facing wrapper is deferred under the same flat-namespace concern as Task 66.5. `string_from_int` is essentially `int_to_string` (which already ships from Plan A2); no new primitive needed.

**Deferred class 5 — `char_to_int` / `int_to_char`.** Both depend on the user-side `Char` surface (same blocker as deferred class 1).

**Why accepted.** The 12 byte-indexed ops give the rest of Stage 7's stdlib (Task 67's `string_builder`, Task 70's `IO.read_file`/`IO.write_file`, demos in Stage 8) a usable working set. The 8 deferred ops have specific, narrow blockers — none are blocked on Task 68 internals. Shipping the 12 unblocks P02's `string_concat` run-portion + P05/etc. that need substring/comparison; deferring 8 keeps Task 68 part 1 mechanical and reviewable.

**Closure path.**
- Codepoint ops: ship alongside `Char` user-surface widening (v2).
- List ops: ship after stdlib namespace qualification.
- Float ops: ship after `Float` primitive lands (v2).
- Sum-typed wrappers: same as List ops (namespace).

**Failure mode.** Users wanting deferred ops write equivalents in their own program against the byte-indexed primitives. Verbose but expressible for ASCII-pinned use cases.

**Implementing commit(s).** `d6401b2`.

## 2026-04-30 — [DEVIATION Task 70] IO op-id reordering + alphabetical ABI

**Context.** Plan C Task 70 grew the builtin `IO` effect from 1 op (`println`) to 5 (`print`, `println`, `read_file`, `read_line`, `write_file`). Op IDs are assigned alphabetically per effect (per the convention from Plan B Task 55 Phase 4f's `BUILTIN_EFFECT_NAMES`). After Task 70:

```
IO.print      → op_id 0
IO.println    → op_id 1   (was op_id 0 pre-Task-70)
IO.read_file  → op_id 2
IO.read_line  → op_id 3
IO.write_file → op_id 4
```

Adding the 4 new ops shifted `println` from op_id 0 to op_id 1.

**Why accepted (alphabetical ordering).** Plan B Task 55 Phase 4f committed to alphabetical-by-effect-then-by-user op_id assignment. The compiler dynamically re-assigns op_ids at typecheck (no hardcoded integer constants in production code), so the runtime ABI continues to work after the shift. The main shim's hardcoded "IO arm at op_id 0" call updated mechanically to install all 5 arms at their alphabetical positions.

**The breaking-change risk.** Pre-Task-70 the partial-handler test `user_discard_k_io_handler_unwinds_helper_at_perform_site` registered a 1-arm handler `IO.println(s, k) => 0`. Post-Task-70 the handler is partial (4 of 5 IO ops uncovered) and trips E0142. **The CI regression on PR #43 (commit `25b8aec`, fixed at `8fc57b0`) is exactly this failure mode** — hardcoded per-effect handler arm sets need to grow when the effect adds ops.

Architectural escalation flagged in PR #43's review: "any test loadbearing on a specific op_id ordering is now silently brittle." Audit confirmed no production code hardcodes op_ids — codegen and typecheck both look them up dynamically — so the breaking change is bounded to test-suite-level handler completeness checks.

**Closure path.** Future ops added to existing builtin effects (Tasks 71-76's user-effect handlers don't touch builtins) face the same op_id-shift risk; partial-handler tests need updating in the same commit. The `extern fn` + `opaque type` v2 path doesn't change this (the typecheck-side enforcement is independent).

**Failure mode.** Same as the CI regression: typecheck rejects partial handlers with E0142. The fix is mechanical (add missing arms with discard-`k` shapes).

**Implementing commit(s).** `25b8aec` (Task 70 + 74); `8fc57b0` (CI fix for the broken partial-handler test).

## 2026-04-30 — [DEVIATION Task 74] Mem ships entirely as a marker effect

**Context.** Plan C Task 74's plan body says: "the handler performs in-place mutation on `MutArray` and rope operations on `StringBuilder`. `main` functions that need mutation declare `![Mem, ...]` in their row." This implies Mem operations are intercepted at a top-level handler frame.

**Why accepted (marker-only).** Per `[DEVIATION Task 66]` Mem ships as a marker effect with zero ops in v1: the Plan C Stage 7 design accommodates generic-typed runtime types (`MutArray[A]`, `MutByteArray`, `StringBuilder`) by gating their ops on the row rather than dispatching through perform. Task 74's "`main` declares `![Mem]`" remains true; the "top-level handler" the plan body references is the type-level absence of a deeper override (no runtime handler frame is installed because there are no ops to dispatch).

**What ships.** `std/mem.sigil` documentation file added to `imports::BUILTIN_INJECTED` skip-list. Doc covers the marker-effect rationale + v2 closure path.

**Closure path.** When `effect Mem[A] { new_array, get, set, ... }` ships as a generic builtin (deferred from Task 66), Task 74 closes by reframing `std/mem.sigil` from doc-only to a real importable module that declares the handler operations. User code stays surface-stable across the v1 → v2 shift.

**Failure mode.** Users trying to intercept Mem operations via `handle ... with { Mem.X(...) => ... }` get E0139 (unknown op on declared effect) until v2 ships generic Mem ops. Documented in `std/mem.sigil`.

**Implementing commit(s).** `25b8aec`.

## 2026-04-30 — [DEVIATION Task 75 + 76] Pseudo-random naming + Int64-blocked deferred handlers

**Context.** Plan C Tasks 75 and 76 specify `Random` and `Clock` effects with `os_seed()` / `seeded(Int64)` and `os_clock()` / `frozen(Int64)` handlers respectively. Two deferral classes apply.

**Class 1 — Naming honesty.** The plan body's `os_seed()` handler implies OS-level entropy (`getrandom(2)` / `BCryptGenRandom`). Sigil v1 has neither a `getrandom`-class crate nor direct syscall plumbing; shipping the runtime-side primitive as `sigil_random_os_int` would mislead users into reaching for it for tokens, salts, session IDs, etc. — a footgun.

**Why accepted (rename to `pseudo`).** The runtime primitive is renamed `sigil_random_pseudo_int`; the sigil-side surface is `random_pseudo_int` + `run_pseudo_random`. The `Random` effect itself stays neutral (`rand_int` op name), since `random_int` is what the user calls and the effect's quality is pinned by the active handler at the use site. Documentation in `runtime/src/random.rs` and `std/random.sigil` carries an explicit "NOT CRYPTOGRAPHICALLY SECURE" warning. v2 will add a real `os_random_int` primitive backed by `getrandom(2)` / `getentropy(3)` / `BCryptGenRandom`, with a parallel `run_os_random` handler; the pseudo-random surface stays for tests and reproducibility.

**Class 2 — Int64-blocked handlers.** The plan-body `seeded(Int64)` (Task 75) and `frozen(Int64)` (Task 76) handlers depend on Plan C Task 69 (`Int64`). Both are deferred to Task 75-followup / Task 76-followup once Int64 ships.

**Class 3 — Clock saturation semantics.** `Clock.now() -> Int` returns 63-bit non-negative nanos since Unix epoch (sigil's `Int` reserves the high bit at the runtime Value layer). Past year ~2262, the value would exceed `i64::MAX`; the runtime saturates at `i64::MAX` rather than wrapping silently. Documented in `runtime/src/clock.rs`.

**Closure path.** Pseudo-rename closes when v2's true OS-entropy primitive lands and the parallel `run_os_random` handler ships in `std/random.sigil`. Int64-blocked handlers close when Task 69 ships. Clock saturation is a long-tail v2 concern (year 2262 ≈ 240 years out).

**Failure mode (Random).** Pre-rename users would reach for `os_random` / `random_os_int` for security-sensitive contexts and get a fully-predictable PRNG. Post-rename, the `pseudo` substring + module-doc warning + std/random.sigil doc warning keep the non-crypto property visible at every call site.

**Failure mode (Clock).** Saturation at year 2262 is silent at the type level (returns `i64::MAX`); user code can detect saturation by `==` comparison.

**Implementing commit(s).** `91d1e55` (initial Tasks 75 + 76 land); [HEAD] (Random rename + scheme-organisation + clock saturation explicit).

## 2026-04-30 — [DEVIATION Task 71] `Raise` ships non-generic; closed-row `catch`; v2 path to `Raise[E]`

**Context.** Plan C Task 71's plan body specifies:

```text
Raise[E] effect + catch : (() -> A ![Raise[E] | e]) -> Result[A, E] !e
```

Three v1 constraints prevent shipping the literal shape:

1. **Parser rejects type-parameterized effect references in rows.**
   `parse_effect_row` in `compiler/src/parser.rs` accepts simple
   effect names (idents) only — `Vec<String>`. The sigil
   `effect Raise[E] { ... }` declaration parses (Plan B Task 53),
   but the row reference `![Raise[E]]` does not. Plan B' state.sigil
   uses the same workaround: a non-generic `effect State { get,
   set }` with concrete `Int` everywhere (per
   `examples/state.sigil`).

2. **No per-op generic params on user-declared effects.** v1
   `effect E { op }` declarations don't accept `op[A]: ...` syntax
   for op-level generics, so `fail: forall a. E -> a` (Koka /
   Effekt's "never returns" shape) is not expressible. Plan B Task
   57's `ArithError.div_by_zero -> Int` placeholder is the
   precedent for "declare `Int` and never resume."

3. **No row-polymorphic fn-typed parameters yet.** The `!e`
   passthrough in `(() -> A ![Raise[E] | e]) -> Result[A, E] !e`
   would require row-poly `Fn` types; Plan B' B.3 ships closed-row
   `Fn` only.

**Why accepted.** v1 ships:

- `effect Raise { fail: (String) -> Int }` — non-generic, fixed
  String error type, `Int` placeholder return.
- `fn raise(e: String) -> Int ![Raise]` — convenience wrapper.
- `fn catch[A](body: () -> A ![Raise]) -> Result[A, String] ![]` —
  generic over body's success type `A`, fixed `String` error,
  closed effect row.

The catch implementation is a single `handle Ok(body()) with {
Raise.fail(e, k) => Err(e), }` discard-`k` arm. Plan B Task 55
Phase 4d MVP closure-capture + tail-position `k` (PR #25) covers
the success path; the discard-`k` arm shape that Phase 4e
captures+ closed (PR #26 — discard-`k` correctness across
function-call boundaries) covers the raise path. Both are
already-verified machinery; Task 71 exercises the user-overridable
half of the surface.

**What's lost vs the plan body.**

- Single error type per Raise (always String). Users wanting
  `Raise[Int]` for error codes write a String wrapper
  (`int_to_string` is in scope), or write their own concrete
  `RaiseInt` effect with the same shape. v2 generalises.
- No effect passthrough — `catch` body must be `![Raise]`-only.
  Users wanting IO + Raise inside the body write a domain-specific
  `catch_io[A](body: () -> A ![IO, Raise]) -> Result[A, String]
  ![IO]` until row-poly Fn lands.

**Closure path.** Three orthogonal v2 surface lifts retire the
deviation:

1. Parser surface for type-parameterized effect references in rows
   (`![Raise[E]]`, `![State[S]]`, etc.).
2. Per-op generic params (`fail[A]: (E) -> A`) so `fail`'s return
   type matches the use site.
3. Row-poly `Fn` type parameters (`(() -> A ![Raise[E] | e]) ->
   ...`).

User code calling `raise(s)` / `catch(body)` stays surface-stable
across the v1 → v2 shift; only the type signatures generalise.

**Failure mode.** Users wanting non-String error types hit the
fixed-String surface and must serialise via `int_to_string` or
similar. Type errors are clear at the call site (`raise(42)`
fires E0044 with String-vs-Int mismatch).

**Implementing commit(s).** [HEAD].

### Subsequent fixup (PR #44 review #1) — return-arm catch idiom deferred

PR #44 review #1 suggested switching `catch`'s implementation from `handle Ok(body()) with { Raise.fail(e, k) => Err(e) }` to the explicit return-arm shape:

```sigil
handle body() with {
  return(v) => Ok(v),
  Raise.fail(e, k) => Err(e),
}
```

The return-arm shape is cleaner in principle and matches `examples/state.sigil`'s convention. **Attempting it tripped a v1 typechecker gap** — when `catch[A]` is monomorphized at a use site, the cross-arm unification (`Ok(v)`'s `Result[A, ?E]` vs `Err(e)`'s `Result[?A, String]` against the declared `Result[A, String]`) typechecks but does not propagate the `?E = String` binding into the return arm's monomorphize pass. Codegen then panics at `cranelift_ty_of_ty` with a `Ty::Var` leak.

`examples/state.sigil` sidesteps this because it is non-generic — A is concrete `Int` everywhere, so no Ty::Var survives.

**Closure path for the return-arm shape.** A v2 typecheck refinement that pins all unconstrained type-vars in handler-arm bodies against the declared handle-overall type (via the same Ty::Var-back-propagation mechanism that pinned the Plan C Task 63 cross-arm unify direction-fix). After v2 lands, `catch` swaps to the cleaner return-arm shape; the `Ok(body())` form below is the v1-compatible workaround documented inline in `std/raise.sigil`.

## 2026-04-30 — [DEVIATION Task 72] `State` ships non-generic concrete-`Int`; `run_state` returns A only

**Context.** Plan C Task 72's plan body specifies:

```text
State[S] effect + run_state: (S, () -> A ![State[S] | e]) -> (A, S) !e
```

Five v1 surface gaps prevent shipping the literal shape — the same trio Task 71's `[DEVIATION Task 71]` enumerated, plus a tuple-return gap and a wrapper-fn-frame composition gap (the latter discovered during PR #45's CI cycle).

**v1 constraints, in priority order:**

1. **Parser rejects type-parameterized effect references in rows.** `parse_effect_row` accepts simple effect-name idents only. `effect State[S] { ... }` declares fine but `![State[S]]` doesn't parse. Plan B' state.sigil established the concrete-`State` workaround (`examples/state.sigil` declares `effect State resumes: many { get: () -> Int, set: (Int) -> Int }` over fixed `Int`).

2. **No tuple type, no stdlib `Pair[A, B]`.** Sigil v1 has variant sum types, records, and primitives, but no tuple syntax. The plan-body `(A, S)` return is not expressible directly. Options for v1:
   - Define `type Pair[A, B] = | Pair(A, B)` in std/pair.sigil (would need its own Plan C task).
   - Drop the `(A, S)` part and return just `A` (matching `examples/state.sigil` precedent).
   - Custom record per use case in user code.
   We pick the second option for v1 — `run_state` returns the body's final value only. Users wanting the final state capture it explicitly via the body's return value.

3. **No `get_state` / `set_state` wrapper functions in v1.** A natural ergonomic addition is wrapper functions `fn get_state() -> Int ![State] { perform State.get() }` and `fn set_state(s: Int) -> Int ![State] { perform State.set(s) }`. **Discovered at PR #45 CI run:** wrappers introduce a function-call frame between the user's call site and the perform; the discharge-with-lambda pattern's `k` continuation captures correctly through the wrapper, but threading the state-bearing lambda chain through that captured frame produces the wrong runtime result (`run_state(5, comp_via_wrappers)` returns `5` instead of the threaded `11` from the canonical set-then-get-plus-1 body). Plan B' Stage 6.8's PR #39 verified the discharge-with-lambda pattern only for *inline* `perform State.set/get` invocations; the wrapper-fn frame composition is a v1 gap. Users do `perform State.get()` / `perform State.set(s)` directly until v2 closes this.

4. **No per-op generic params on user-declared effects.** Same constraint as Task 71's `Raise.fail: (E) -> Int` placeholder. State's ops aren't affected (`get: () -> Int`, `set: (Int) -> Int` are concrete-typed at the signature level), but the underlying gap is shared.

5. **No row-polymorphic fn-typed parameters.** Same closed-row constraint as Task 71's `catch`. `run_state` accepts `body: () -> Int ![State]` only; row-poly passthrough lands in v2.

**Why accepted.** v1 ships:

- `effect State resumes: many { get: () -> Int, set: (Int) -> Int }` — non-generic, concrete `Int` state, multi-shot annotation matching `examples/state.sigil`.
- `fn run_state(initial: Int, body: () -> Int ![State]) -> Int ![]` — discharges State by threading state through the body's perform sites; returns the body's final value.

User code uses inline `perform State.get()` / `perform State.set(s)` directly (per constraint #3 above).

The `run_state` implementation reuses `examples/state.sigil`'s state-bearing-lambda pattern (each handler arm returns `(Int) -> Int ![]`; the handle expression's overall is the chain; applying it to `initial` drives the state through). All Plan B' machinery (B.3 TypeExpr::Fn, B.4 lambda arm bodies, return arm wrap, captures+) is exercised. `examples/state.sigil` is the verified precedent — Plan B' Stage 6.8's PR #39 closed the six-layer canonical run_state runtime chain fix that makes this shape correct end-to-end.

**What's lost vs the plan body.**

- Single state type per State (always `Int`). Users wanting `State[String]` for string accumulators write a String wrapper (e.g., a `MutByteArray`-backed accumulator) or wait for v2's generic generalisation.
- No final-state observation in `run_state`'s return. Users wanting `(A, S)` capture state explicitly, e.g.:
  ```sigil
  fn comp_returning_final() -> Int ![State] {
    // ... do work ...
    let final_s: Int = get_state();
    final_s  // body's return value IS the final state
  }
  ```
  i.e., have the body return the state value when caller wants both. Awkward but expressible.
- No effect passthrough (closed `![State]` row).

**Closure path.** Four orthogonal v2 lifts retire the deviation:

1. Parser surface for type-parameterized effect references in rows (`![State[S]]`). **Open** — Plan D Task 114.
2. Tuple type / `Pair[A, B]` stdlib (or `(A, S)` syntax). **Closed** by Plan D Task 113 (PR #53). Tuple types `(T1, T2, ...)`, tuple values `(e1, e2, ...)`, `Pattern::Tuple` element-wise unification + destructure, and `std/pair.sigil` with `fst[A,B]` / `snd[A,B]` over binary tuples shipped. `run_state` returning `(A, S)` is now expressible at the surface; updating `std/state.sigil` to actually return `(A, S)` is a follow-up Plan-C-completion task — the v1 blocker was the syntax, not the discharge mechanism.
3. Wrapper-fn frame composition fix for the discharge-with-lambda pattern. After this lands, std/state.sigil grows `get_state` / `set_state` ergonomic wrappers. **Deferred** — Plan D Task 112 deferred to Task 117 follow-up; see `PLAN_D_DEVIATIONS.md` `[DEVIATION Task 112]`.
4. Row-poly Fn type parameters (`(() -> A ![State[S] | e]) -> ...`). **Open** — Plan D Task 116.

User code calling `perform State.get()` / `perform State.set(s)` / `run_state(init, body)` stays surface-stable across the v1 → v2 shift; the v2 wrapper-fn additions are additive (don't break existing call sites).

**Failure mode.** Users wanting non-Int state types or tuple returns hit the fixed-Int / final-A-only surface. Type errors are clear at the call site (`set_state("x")` fires E0044 with String-vs-Int mismatch).

**Implementing commit(s).** [HEAD].

## 2026-04-30 — [DEVIATION Task 73] `Choose` ships effect-declaration only; `all_choices` / `first_choice` dischargers deferred to v2

**Context.** Plan C Task 73's plan body specifies:

```text
Choose resumes: many effect + all_choices, first_choice
```

with `Choose.choose(n)` picking a value 0..n-1 and `Choose.fail()` abandoning a branch (per Task 81's Sudoku spec at PLAN_C_PROGRESS.md line 79: "`Choose.choose(9)` picks digits; `Choose.fail()` on constraint violation; `first_choice` handler returns first solution").

**v1 surface gaps.** Six constraints prevent shipping the dischargers — three are inherited from Tasks 71/72 (parser, per-op generics, row-poly Fn) and three are codegen-side gaps specific to multi-shot dischargers over arbitrary-arity `choose(n)` and short-circuiting `first_choice`:

1. **Parser rejects type-parameterized effect references in rows.** Same constraint as `[DEVIATION Task 71]` constraint #1 / `[DEVIATION Task 72]` constraint #1. `parse_effect_row` accepts simple effect-name idents only.

2. **Static-N arm-body chain.** The arm-body recognizer at `compiler/src/codegen.rs::arm_body_n_let_then_pure_tail_shape` (line ~3665) requires arm bodies of the form `{ let r1: T = k(arg1); let r2: T = k(arg2); ...; let rN: T = k(argN); pure_tail }` — N is statically fixed at compile time. `all_choices(body) -> List[Int]` would need runtime-N dispatch (invoking `k(0)`, `k(1)`, …, `k(arg-1)` where `arg` is the perform's runtime value), which is not expressible in v1's flat let-chain shape.

3. **No first-class continuations.** `arm_body_walk` at codegen.rs:1505-1518 explicitly rejects `k` referenced as a value (passed to a helper, captured into a closure, stored in a record) with the diagnostic "first-class continuations are deferred to v2". The closure path that would make `all_choices` expressible — `Choose.choose(arg, k) => list_fold(range(0, arg), Nil, fn (acc, i) => append(acc, k(i)))` — runs `k` inside a hoisted lambda whose closure env captures `k`, which closure_convert can't model in v1.

4. **No conditional / branched `k`-call.** `arm_body_walk` at codegen.rs:1591-1603 rejects "computed conditional `k`-use" — `match k(0) { Some(v) => Some(v), None => k(1) }` and `if cond { k(x) } else { k(y) }` shapes are not in tail position for `k`-detection (the synth-pass detector `arm_body_tail_is_k_call` recurses only through `Expr::Block` tails). `first_choice` short-circuit semantics ("try `k(i+1)` only if `k(i)` failed") cannot be expressed without this.

5. **No per-op generic params on user-declared effects.** Same as `[DEVIATION Task 71]` constraint #2 / `[DEVIATION Task 72]` constraint #4. `fail`'s declared `Int` return is a placeholder per Plan B Task 57's `ArithError.div_by_zero` precedent.

6. **No row-polymorphic fn-typed parameters.** Same as `[DEVIATION Task 71]` constraint #3 / `[DEVIATION Task 72]` constraint #5. Closed-row `![]` only.

The load-bearing v2 blockers specific to Task 73 are #2, #3, #4 — the multi-shot codegen surface.

**Why accepted.** v1 ships:

- `effect Choose resumes: many { choose: (Int) -> Int, fail: () -> Int }` — non-generic, multi-shot annotation matching the plan body and Task 81's spec. The annotation keeps the surface stable across the v1 → v2 shift; v2 dischargers consume the same declaration without a breaking change.

User code uses inline single-shot handlers (always pick `k(0)`, discard-`k` on `fail`) for the cases expressible in v1. Multi-shot dischargers (`all_choices`, `first_choice`) are deferred to v2.

**No perform wrappers (`pick(n)` / `fail_choice()`).** A natural ergonomic addition would be wrapper functions, but Task 72 (PR #45 CI run) discovered that wrapper-fn frames break the discharge-with-lambda pattern that multi-shot Choose handlers will need in v2 (constraint #3 there; tracked in PLAN_C_PROGRESS.md's "Plan B' Stage-6.8-followup architectural carryovers" section). Shipping wrappers in v1 would create a footgun: users would write code against the wrappers, then hit incorrect runtime behaviour when they discharge with a multi-shot handler (the v2 path). v2 ships wrappers as an additive ergonomic layer once the discharge-side gap closes alongside the multi-shot codegen surface.

**What's lost vs the plan body.**

- **`all_choices` / `first_choice` dischargers absent.** Users wanting full enumeration / search-with-short-circuit must write inline handlers in user code that invoke `k` from the static-N let-chain shape, restricting them to a fixed compile-time chain length. This is the load-bearing absence: Plan C Task 81 (Sudoku demo) is gated on these dischargers and defers with this entry.

- **No perform wrappers.** Users do `perform Choose.choose(n)` / `perform Choose.fail()` directly. Surface is stable across v1 → v2; wrappers are additive when they ship.

**Closure path.** Three orthogonal v2 surface lifts retire the deviation. The codegen-side ones (first-class continuations + conditional-k handler-arm tails + wrapper-fn-frame composition) form a coherent architectural cluster but have not yet been scoped at the user-plan level; this deviation captures the technical detail that any such slice would address:

1. **First-class continuations** — `k` as a passable value, captured into closures, threaded through user fns. Lifts constraint #3. Unblocks `all_choices` via the `list_fold(range(0, arg), Nil, fn (acc, i) => append(acc, k(i)))` shape.

2. **Conditional / branched `k`-call** — `match k(0) { ... => k(1) }` and `if cond { k(x) } else { k(y) }` recognised as valid arm-body tails via post-arm-k synth chain extension (a `PostArmKBranchedChain` shape with join blocks). Lifts constraint #4. Unblocks `first_choice` short-circuit.

3. **Wrapper-fn-frame composition** — closes the same gap tracked under `[DEVIATION Task 72]` constraint #3 / Plan B' Stage-6.8-followup carryover. Once shipped, std/choose.sigil grows `pick(n)` / `fail_choice()` wrappers as an additive ergonomic layer.

The parser / per-op-generics / row-poly-Fn lifts (constraints #1, #5, #6) are orthogonal — they generalise the surface from concrete-`Int` to `Choose[A]` parametric over the pick value's type, but don't unblock the dischargers on their own.

**Failure mode.** Users hitting the multi-shot expressivity wall see clear diagnostics from the existing arm-body walker — "references continuation `k` as a value", "uses continuation `k` in non-tail position outside the supported shapes". Both messages point at v2 closure. Type errors at perform sites (`perform Choose.choose("two")` → E0044) are unaffected.

**Sudoku impact.** Plan C Task 81 (`examples/sudoku.sigil`, scheduled for Stage 8) defers with this entry — Sudoku's `Choose.choose(9)` per-cell + `first_choice` orchestration requires both constraint #2 (runtime-N arm body) and constraint #4 (short-circuit) lifted. Captured under PLAN_C_PROGRESS.md's plan-completion note (Task 81 will reference this deviation when the Stage 8 work begins).

**Implementing commit(s).** [HEAD].
