# Plan C — Stdlib, demos, spec & polish (Stages 7–10)

Tracks Plan C's execution against `boldfield/designs/in-progress/2026-04-21-sigil-finish.md` (moves to `done/` on completion). Plan B' closed at sigil/main `bd409fb` on 2026-04-29 (PRs #38 / #39 / #40); Plan C inherits all four architectural lifts B.1–B.4 closed.

## Stage 6.5 — Plan C scaffolding

**Complete before any Stage 7 work.**

- 6.5.1 — Create `PLAN_C_PROGRESS.md` (this file).
  - status: done (this commit)
- 6.5.2 — Create `PLAN_C_DEVIATIONS.md`.
  - status: done (this commit)
- 6.5.3 — Plan C questions use `[PLAN-C]` prefix (already in `QUESTIONS.md`'s prefix discipline; no edit needed).
  - status: done (verified)
- 6.5.4 — `scripts/validate-spec.sh` stub: reads prompt bank, iterates entries, prints "not yet implemented" — populated in Stage 9.
  - status: done (this commit)

**Acceptance:** CI still green; new tracking files and stub script exist.

## Stage 7 — Standard library

**Goal:** All stdlib modules implemented in sigil; every public function unit-tested; stdlib doctests extracted and run.

- Task 62.0 — **Stdlib import resolution prerequisite.** Implement minimal import resolution between parse and resolve. `import std.X` loads `std/X.sigil` from the embedded tree, parses it, prepends its non-import items into the program. Cycle detection (E0033), missing-module (E0032). Builtin-injected paths (`std.io`) skip-listed at the resolver. Logged as `[DEVIATION Task 62.0]` in `PLAN_C_DEVIATIONS.md`.
  - status: done-pending-ci ([HEAD~1])
- Task 62 — Write `std/option.sigil` — `Option[A]`, `map`, `and_then`, `unwrap_or`. Tests under `tests/std/option.rs` (each test compiles a small sigil program and checks output).
  - status: done-pending-ci ([HEAD]) — `std/option.sigil` ships with `Option[A]`, `map`, `and_then`, `unwrap_or` (closed-row `![]` helpers; row polymorphism deferred). Typecheck-level tests in `compiler/src/typecheck.rs::tests` (3 tests prefixed `import_std_option_` / `option_helpers_unavailable_without_import`). 6 e2e run-and-check-output tests in `compiler/tests/e2e.rs::std_option_*` covering Some/None across map/and_then/unwrap_or.
- Task 63 — Write `std/result.sigil` — `Result[A, E]`, `map`, `map_err`, `and_then`. Tests.
  - status: done-pending-ci ([HEAD]) — `std/result.sigil` ships with `Result[A, E]`, `map`, `map_err`, `and_then` (closed `![]` rows). Surfaced + fixed a Plan-B-era latent typecheck bug: `bind_ty_var` direction with two unbound vars now prefers higher-id → lower-id (union-find-by-min). See `[DEVIATION Task 63]` in `PLAN_C_DEVIATIONS.md` for the full root-cause analysis. Targeted regression test `two_param_sum_type_match_each_arm_constrains_one_param_typechecks` pins the fix. Two typecheck tests (`import_std_result_*`) + 6 e2e tests (`std_result_*`) cover the surface.
- Task 64 — Write `std/list.sigil` — `List[A] = Nil | Cons(A, List[A])`, `map`, `filter`, `fold`, `length`, `reverse`, `append`, `range`, `for_each`. Tests. `range(0, n)` and `for_each` are the canonical iteration idioms since Sigil has no `for`/`while`.
  - status: todo
- Task 65 — Write `std/array.sigil` — immutable `Array[A]`, `length`, `get`, `set` (returns new), `from_list`, `to_list`. Requires runtime support for array allocation; extend `runtime/`.
  - status: todo
- Task 66 — Write `std/mut_array.sigil` — `MutArray[A]` operations exposed through the `Mem` effect. Runtime support in `runtime/src/mem.rs`: in-place array mutation under the top-level `Mem` handler.
  - status: todo
- Task 66.5 — Write `std/byte_array.sigil` — immutable `ByteArray`. Specialized flat-byte representation, runtime primitives `sigil_byte_array_*`, string interop (`string_to_bytes` / `string_from_bytes`), `Utf8Error` sum type. Wire up `Byte` primitive + `Option[Byte]`-returning `byte_from_int`.
  - status: todo
- Task 66.6 — Write `std/mut_byte_array.sigil` — `MutByteArray` operations through `Mem`: `new_byte_array`, `get`, `set`. Runtime support extends `runtime/src/mem.rs`.
  - status: todo
- Task 67 — Write `std/string_builder.sigil` — `StringBuilder` operations through `Mem`. Runtime-backed rope (segments of fixed max size). `sb_new`, `sb_append`, `sb_finalize`.
  - status: todo
- Task 68 — Extend string runtime primitives (`string_length`, `string_byte_at`, `string_char_at`, `string_chars`, `string_concat`, `string_substring`, `string_compare`, `string_starts_with`, `string_ends_with`, `string_contains`, `string_index_of`, `string_split`, `string_join`, `string_trim`, `string_from_int`, `string_to_int`, `string_from_float`, `string_to_float`, `char_to_int`, `int_to_char`). Write `std/string.sigil` wrapping the primitives.
  - status: todo
- Task 69 — Write `std/int64.sigil` — boxed `Int64`. Runtime support: `Int64` as a heap-allocated record; each arithmetic op allocates one record.
  - status: todo
- Task 70 — Extend `std/io.sigil` with `print`, `println`, `read_line`, `read_file`, `write_file`. Add corresponding runtime primitives.
  - status: todo
- Task 71 — Write `std/raise.sigil` — `Raise[E]` effect + `catch : (() -> A ![Raise[E] | e]) -> Result[A, E] !e`.
  - status: todo
- Task 72 — Write `std/state.sigil` — `State[S]` effect + `run_state: (S, () -> A ![State[S] | e]) -> (A, S) !e`.
  - status: todo
- Task 73 — Write `std/choose.sigil` — `Choose resumes: many` effect + `all_choices`, `first_choice`.
  - status: todo
- Task 74 — Write `std/mem.sigil` — `Mem` effect declaration + top-level handler wiring. The handler performs in-place mutation on `MutArray` and rope operations on `StringBuilder`. `main` functions that need mutation declare `![Mem, ...]` in their row.
  - status: todo
- Task 75 — Write `std/random.sigil` — `Random` effect; `os_seed()` handler reads from OS entropy, `seeded(Int64)` handler installs a deterministic PRNG.
  - status: todo
- Task 76 — Write `std/clock.sigil` — `Clock` effect; `os_clock()` handler reads OS, `frozen(Int64)` handler returns a fixed timestamp.
  - status: todo
- Task 77 — **Doctest tooling.** Implement `sigil doctest <file.sigil>` subcommand that scans for `@example` blocks in doc comments, extracts each as a standalone test program, compiles + runs each, reports pass/fail counts. Add doctests to every stdlib module — every public function has at least one `@example`.
  - status: todo
- Task 78 — **Unit tests.** Every stdlib module has at least two Rust-driven tests under `tests/std/*.rs` covering common cases and edge cases.
  - status: todo
- Task 78.5 — **Imported Koka effect-handler test-suite subset.** Port a 10–20 case subset of Koka's effect-handler test suite (BSD-2 licensed) into `tests/imported/koka/*.rs`. Coverage targets: one-shot/multi-shot composition, nested handlers, row threading edge cases, exception-vs-state composition, common bugs from Koka's bug history. Land before stdlib bulk completes (Tasks 62–78) so corpus exercises stdlib as it grows.
  - status: todo

**Acceptance:** every stdlib module has doctests (all pass) and ≥2 Rust-driven unit tests (all pass). All pass on both hosts. `Mem`, `Random`, `Clock` effects usable from examples. `Int64` box works for protocol correctness. Imported Koka subset compiles and produces expected outputs.

## Stage 8 — Demo programs

**Goal:** Three non-trivial programs compile and run end-to-end.

- Task 79 — `examples/interpreter.sigil` — tree-walking interpreter for a small applied λ-calculus. Uses `Raise[String]` for type errors and unbound variables; top-level `catch` converts to `Result`. e2e test evaluates representative expressions.
  - status: todo
- Task 80 — `examples/json.sigil` — JSON parser + pretty-printer. Parser input is `ByteArray` (not `String`). Numbers in `Int` range parse as `JInt`; larger integers as `JInt64`; non-integer numerics as `JFloat`. Pretty-printer uses `StringBuilder` under `Mem`. **Note:** with B.3+B.4 shipped (Plan B'), the literal `run_state(initial, comp)` higher-order helper IS expressible — the dual-handle workaround mentioned in the original plan body is no longer required. Use `run_state` directly.
  - status: todo
- Task 81 — `examples/sudoku.sigil` — backtracking Sudoku solver. Board: `MutArray[Int]` of length 81 under `Mem`. `Choose.choose(9)` picks digits; `Choose.fail()` on constraint violation; `first_choice` handler returns first solution.
  - status: todo
- Task 82 — **Performance floor (demos):** Interpreter <100ms; JSON round-trip 10kB <500ms; Sudoku easy <5s. On both hosts.
  - status: todo

**Acceptance:** all three demos compile and produce correct output on prepared inputs on both hosts. Performance floor met.

## Stage 9 — Language specification

**Goal:** `spec/language.md` is complete enough to meet the validation pass-rate threshold.

- Task 83 — Write `spec/language.md` with **examples-first structure** (12 worked examples E1–E12 building in difficulty, then lexical structure, grammar, type system, effect system, expressions, pattern matching, modules, stdlib reference, diagnostics, runtime model, testing patterns, external-system effects v2 shape, build/run instructions).
  - status: todo
- Task 84 — Populate every remaining prompt-bank oracle in `spec/validation-prompts.md`.
  - status: todo
- Task 85 — Implement `scripts/validate-spec.sh` (replacing the Stage-6.5 stub). Runs fresh Claude API session per prompt with only `spec/language.md` as context; captures program; compiles; runs; compares to oracle; on compile fail, feeds JSONL error back into a second turn for "after one edit" result. Supports `--model opus|sonnet|haiku`.
  - status: todo
- Task 86 — Run validation. Execute `scripts/validate-spec.sh` against the full 20-prompt bank on at least {Opus, Sonnet}. Capture results in `spec/validation-log.md`.
  - status: todo
- Task 87 — **Spec validation gate (Plan C success criterion).** First-compile pass rate ≥ 70% on Opus AND Sonnet; after-one-edit pass rate ≥ 90% on Opus AND Sonnet. **Human arbitrates "spec gap vs model flake" on ambiguous failures.**
  - status: todo (human-triggered; agent prepares + stops)

**Acceptance:** `spec/language.md` exists with examples-first structure; `scripts/validate-spec.sh` runs end-to-end against all 20 prompts; pass-rate thresholds met on Opus and Sonnet; `spec/validation-log.md` documents every run; any gaps drove specific spec revisions.

## Stage 10 — Polish

- Task 88 — Every compiler error has a stable code, JSONL output with source span, and a catalog entry accessible via `sigil explain`. Audit catalog for completeness.
  - status: todo
- Task 89 — Write top-level `README.md` (replacing Plan A1 placeholder). One-paragraph description (incl. LLM-first thesis + fight-the-priors), quickstart for both hosts, one complete example with commands, link to `spec/language.md`.
  - status: todo
- Task 90 — Run full test suite on both hosts. All tests pass. Mark slow tests `#[ignore]` with note. Full `cargo test` <2 minutes.
  - status: todo
- Task 91 — Final end-to-end sanity: fresh checkout, `cargo build --release`, compile each example, run each example, verify output. `scripts/smoke.sh` passes on both hosts.
  - status: todo
- Task 92 — Run `scripts/validate-spec.sh` once more on a fresh checkout; pass rates still meet thresholds. Commit updated `spec/validation-log.md`.
  - status: todo

**Acceptance:** `cargo test` green on both hosts; `scripts/smoke.sh` passes on both; README example works verbatim on both; final validation run meets the pass-rate thresholds.

## Plan B' Stage-6.8-followup architectural carryovers

PR #39 review's deferred items, queued for "first commit of Plan C" per the Stage 6.8 review-checkpoint carry-forward in `PLAN_B_PRIME_PROGRESS.md`:

- **TLS → packed multi-return for `sigil_run_loop` terminal.** Replace `LAST_TERMINAL_TAG` / `LAST_TERMINAL_VALUE` thread-local out-channel with packed `(value: u64, tag: u32)` multi-return. Worth landing before any Plan C work touches the run_loop / Expr::Handle lowering paths. **Carry-forward target: Stage 7 task whose touched code overlaps the run_loop machinery, OR a standalone `[CHORE]` commit early in Plan C.**
  - status: todo
- **Sync shim emission gating.** Gate Sync shim emission on `top_level_fn_names_seen_as_value` from closure_convert. Bounded bloat (one ~100-byte shim per Cps fn); worth tightening if Cps-fn count grows in stdlib. **Carry-forward target: Stage 7 / Stage 10 if a touched module would benefit.**
  - status: todo

## Plan C completion criteria

- All Stage 7–10 acceptance criteria met on both hosts.
- Spec validation thresholds met on Opus and Sonnet; documented in `spec/validation-log.md`.
- All demos run on both hosts with correct output; performance floor met.
- `PLAN_C_PROGRESS.md` reflects reality; all tasks marked done with commit references.
- Repo ready to hand to a fresh Claude session with nothing but `spec/language.md` and expect pass-rate-level first-try success on the prompt bank.
- **Do not grade your own work.** At completion, finalize progress file and stop. A human verifies before declaring the project complete.
