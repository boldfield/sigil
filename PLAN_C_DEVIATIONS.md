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
