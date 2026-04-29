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
