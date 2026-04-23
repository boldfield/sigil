# Plan A2 Progress

Task-by-task tracker for Plan A2 (`in-progress/2026-04-21-sigil-core-a2.md`
in `boldfield/designs` while active; moves to `done/` when merged). Each
entry tracks: the task ID, current status, linked commits, and optional
notes on deviations. Deviations are logged separately in
`PLAN_A2_DEVIATIONS.md` *before* the implementing commit.

Status values: `todo`, `in-progress`, `done`.

**Acceptance reminder (from plan's "Local verification strategy"):** a task
is not `done` until CI is green on both `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin`. Local pod checks are necessary but not sufficient.

## Stage 1.5 — Plan A2 scaffolding

- Task 1.5.1 — Create `PLAN_A2_PROGRESS.md`
  - status: done
  - commits: [a18876e]
  - notes: This file.
- Task 1.5.2 — Create empty `PLAN_A2_DEVIATIONS.md`
  - status: done
  - commits: [a18876e]
  - notes: Landed atomically with 1.5.1 and 1.5.3 (scaffolding is one unit).
- Task 1.5.3 — Preserve `QUESTIONS.md` across plans with `[PLAN-A2]` prefix convention
  - status: done
  - commits: [a18876e]
  - notes: Appended a tagging convention to QUESTIONS.md header; A1 entries are implicitly `[PLAN-A1]`.
- Task 1.5.4 — `scripts/pod-verify.sh` + README pod-vs-CI section + CI wiring
  - status: done
  - commits: [215ef8a]
  - notes: Script wraps fmt + check + per-crate clippy + runtime lib tests + interior-pointer check + discipline greps. Greps for unwrap/expect/panic are advisory (clippy -D warnings is the authority); false positives inside test modules are expected. CI invokes the script as a new step before the existing build/test matrix.
- Task 1.5.5 — Fix cold-target e2e staticlib ordering
  - status: done-pending-ci
  - commits: [f0a6212, db3ae5e]
  - notes: DEVIATION logged (original and revision). First revision (`f0a6212`) put the rebuild in `compiler/build.rs`; deadlocked under `cargo test --workspace` cold (PR #2 first CI run sat on "cold run 1 of 2" for 47+ minutes on both hosts before being cancelled). Second revision moves the rebuild into `compiler/tests/e2e.rs::ensure_runtime_staticlib`, called at the top of the `hello` test. Runs at test-run time after outer cargo releases its locks; no deadlock. `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD` env var is gone (no longer needed — callers that pre-build the staticlib short-circuit via the existence check). `cold-checkout-test` CI job unchanged.
- Task 1.5.6 — `debug_assert!` on typecheck env insertion (no-shadowing invariant)
  - status: done
  - commits: [00739d3]
  - notes: Extracted a `Tc::env_insert(name, ty)` helper that asserts `prev.is_none()` in debug builds. Both insertion sites (params in `check_fn`, let bindings in `check_block`) use the helper. All 14 typecheck tests still green.

## Stage 2 — Arithmetic, booleans, conditionals

- Task 20 — Extend lexer (booleans, if/else, match, operators, char literals)
  - status: done
  - commits: [b838a9c]
  - notes: Added keywords `true false if else match`; tokens `Plus Minus Star Slash Percent EqEq NotEq Lt Gt LtEq GtEq AndAnd OrOr FatArrow CharLit`; char-literal lexer with `\n \t \r \\ \'` escapes. Two-char lookahead wins over single (arrow vs minus, eqeq/fatarrow vs eq, etc.). 15 lexer unit tests pass (9 new).
- Task 21 — Extend parser (arith/cmp with precedence, if, match, unary, constant-fold `-<lit>`)
  - status: done
  - commits: [964a83c]
  - notes: Pratt-style precedence climbing in `parse_expr_prec`. AST gains `BoolLit`, `CharLit`, `Binary`, `Unary`, `If`, `Match`, `MatchArm`, `BinOp`, `UnOp`, `Pattern`. `-<int-literal>` folds to `IntLit(-n)` at parse time. Parenthesized exprs supported. Typecheck emits E0043 "Stage-2 not yet typed" for the new variants (task 22 replaces with real rules). 15 parser unit tests pass (12 new).
- Task 22 — Extend typechecker (Bool, Char, Byte; binop typing; if unification; match exhaustiveness)
  - status: done
  - commits: [1de46b4]
  - notes: Added `Bool`, `Char`, `Byte` to `Ty`; wired `ty_from_type_expr`/`type_matches` for the new names. New catalog entries E0060 (binop operand type), E0061 (unary operand type), E0062 (if-cond not Bool), E0063 (if-branch disunion), E0064 (pattern/scrutinee mismatch), E0065 (match-arm disunion), E0066 (non-exhaustive match). `check_block` now returns `Option<Ty>` so `if` branch unification can see block types. Exhaustiveness is coarse and documented in the E0066 catalog entry: wildcard → exhaustive; Bool without wildcard → exhaustive iff both `true` and `false` appear; other primitives require wildcard. `< > <= >=` are Int→Int→Bool only; PLAN-A2 QUESTIONS.md entry documents the Byte-ordering discrepancy with the plan's Byte feature paragraph (resolved by implementor as strict form; reviewer may override). 25 new typecheck tests (39 total in the module, 88 total compiler lib tests, all green locally).
- Task 23 — Extend elaboration (if → match on Bool; arith flattened into ANF)
  - status: done
  - commits: [0714454]
  - notes: Added `Expr::Block(Box<Block>)` variant to the AST (post-elaborate only — parser never produces it) so that desugared `if/else` branches with statements can survive as `match` arm bodies. Elaborate is now a proper pass: walks each `Item::Fn`'s body, hoists non-trivial operands of `Binary`/`Unary` into synthetic `let $elab_tN: <TypeExpr> = <expr>;` bindings (names start with `$` which the lexer rejects, so no user-name collision), and desugars `if/else` into `match` on `Bool` with pattern arms `true => ...`, `false => ...`. Pure-expr branches unwrap (`block_to_expr` returns the tail directly); branches with stmts wrap in `Expr::Block`. Scope kept tight per task 23's one-line spec: match scrutinee, perform args, and call args are **not** flattened in this task — plan's `< > <= >=` polymorphism, match scrutinee ANF, and call-site ANF arrive in later tasks. Synthetic let's `TypeExpr` is inferred from the op tag (arith/Neg → Int; comparison/logic/Not → Bool), matching plan A2 task 22's typing rules. 9 new elaborate unit tests; 97 compiler lib tests all pass locally. Pod-verify script green.
- Task 24 — Extend codegen (i63 arith with overflow-wrap; icmp; brif; sdiv/srem zero-check)
  - status: done
  - commits: [d4d0682]
  - notes: Replaced the Stage-1 hardcoded hello-world walk in `compiler/src/codegen.rs` with a tree-walking `Lowerer` struct that handles every non-Call `Expr` variant. Internal-value representation is native (`i64` Int, `i8` Bool/Byte, `i32` Char, `pointer_ty` String/heap); `Int` is tagged `(n << 1)` only at the user-`main` return boundary so arithmetic ops are plain Cranelift `iadd`/`isub`/`imul`/`sdiv`/`srem`. Comparisons use `icmp` with `SignedLessThan`/etc. Booleans use `band`/`bor` over `{0, 1}` representation. `match` is lowered as a chain of compare + `brif` blocks joining at a continue block whose single param carries the arm result; wildcard arms jump unconditionally, and a defensive `trap(TRAP_NONEXHAUSTIVE_MATCH)` lives in the fall-through block. Every `sdiv`/`srem` emits a divisor-zero check that `brif`s to a panic block calling `sigil_panic_arith_error(cstr)` (Task 25) for `"division by zero"` or `"remainder by zero"`; a Cranelift `trap(TRAP_ARITH_ABORT)` after the call satisfies the terminator invariant since Cranelift can't model `-> !`. Safepoint metadata (plan A1's placeholder stackmap) pushes a record per new call site, preserving the stackmap discipline. Four new e2e tests in `compiler/tests/e2e.rs` (`arith_integer_ops`, `if_else_produces_value`, `match_primitive_with_wildcard`, `div_by_zero_traps`, `mod_by_zero_traps`). Manual smoke verified locally on the pod: hello.sigil still green, arith programs produce expected exit codes, div/mod-by-zero both print `sigil: arithmetic error: <reason>` and exit 2.
- Task 25 — Runtime primitives (int_to_string, panic_arith_error, checked_add/sub/mul, Byte primitives)
  - status: done
  - commits: [d4d0682]
  - notes: Two new runtime modules: `runtime/src/arith.rs` (`sigil_panic_arith_error` → writes `sigil: arithmetic error: <reason>` to stderr then `std::process::exit(2)`; `sigil_int_to_string` → allocates via `sigil_string_new`; `sigil_checked_add`/`sub`/`mul` returning `#[repr(C)] CheckedInt { value: i64, overflowed: bool }`) and `runtime/src/byte.rs` (`sigil_byte_from_int_checked` returning `#[repr(C)] ByteFromInt { value: u8, in_range: bool }`; `sigil_byte_to_int`, `sigil_byte_add`, `sigil_byte_sub` wrapping). `libc::exit(2)` in the plan's spec → `std::process::exit(2)` in the implementation (runtime has no `libc` dep; `std::process::exit` is equivalent — delegates to `exit(3)` on Unix, flushes stdout/stderr, runs `atexit` handlers, so the counter-dump atexit still fires). Byte primitives are shipped but not yet called from codegen — language-level exposure arrives with Stage 3's user-fn calls (Task 29) and Plan A3's sum types (for `Option[Byte]`). New catalog entry **E0401** ("runtime arithmetic abort") with long-form text explaining the v1-only surface and Plan B's `Raise[ArithError]` successor. 14 new runtime tests (6 arith + 8 byte); 37 total runtime lib tests all green locally. `sigil_panic_arith_error` itself has no unit test because it exits the process — exercised via the Task 24 `div_by_zero_traps`/`mod_by_zero_traps` e2e tests.
    - **Test-concurrency fix** landed with this task: `runtime/src/test_support.rs` exposes a crate-level `Mutex` guard that serialises every GC-allocating test. Rationale is in the module doc and reproduced here: Boehm is built with POSIX thread support but Rust test threads are not auto-registered (`std::thread::spawn` is not `GC_pthread_create`), so Boehm's mark phase can miss pointers on unregistered stacks, collect live objects, and reuse the slots from concurrent allocations. Pre-existing `gc::tests::alloc_empty_string` had been passing by luck (low GC pressure); adding the new arith tests tipped the pressure over the threshold and the test became flaky (~30% failure rate under parallel `cargo test`). The mutex makes every GC-touching runtime test take turns, which dodges the race at ~1ms/test cost. Proper thread registration (`GC_allow_register_threads` + `GC_register_my_thread`) is deferred to Plan B when the precise-GC rewrite happens.
- Task 26 — examples/arith.sigil + examples/div_by_zero.sigil + e2e tests + PR #2 deferrals
  - status: done-pending-ci
  - commits: [6b77340]
  - notes: (scope revised post PR #3 review at designs commit `8e75c43`) Factorial deferred to Stage 3 (Task 33's `fibonacci.sigil` absorbs the recursive-oracle role). Ships `examples/arith.sigil` (mixed arithmetic + `if`/`else`; invariant exit 26) and `examples/div_by_zero.sigil` (test-only trap trigger — exits 2 with the `sigil: arithmetic error: division by zero` banner). Two new file-based e2e tests (`arith_example_exits_26`, `div_by_zero_example_traps`) replace the Task 24 inline `arith_integer_ops` / `div_by_zero_traps` coverage. PR #2 deferrals picked up: `sigil_binary() -> PathBuf` helper wraps `env!("CARGO_BIN_EXE_sigil")` + `ensure_runtime_staticlib` behind a `std::sync::Once`; every e2e test (including the pre-existing `hello` and `stackmap_section_parses_v0_placeholder`) migrated to the helper. `compile_file_and_run` + `compile_and_run` (inline-source wrapper) share the same staticlib-aware path. QUESTIONS.md `[PLAN-A2] factorial-in-Stage-2` resolved as option (a) with full reviewer rationale. Drive-by dedup: the three identical literal-pattern branches in `lower_match` collapsed through a `pattern_as_immediate(&Pattern) -> Option<i64>` helper (~40 lines of duplication removed).
- Task 27 — Stage 2 has no Stage-2-specific perf floor
  - status: done
  - commits: []
  - notes: (scope revised post PR #3 review at designs commit `8e75c43`) The original `factorial(10) < 100ms` perf floor is dropped because Stage 2 has no non-trivial recursive program to benchmark (factorial moved to Stage 3). For Stage 2, verification shrinks to "`cargo test --workspace` completes within CI job limits on both hosts" — covered by every PR's CI run on both hosts. The v1 perf floor lives in **Task 34** as `fib(20) == 6765` in <50ms. No code change required for Task 27 at the Stage-2 cut.
- Task 28 — Seed prompt bank (P05 + P07 only; P04 + P06 deferred to Task 35)
  - status: done-pending-ci
  - commits: [6b77340]
  - notes: (scope revised post PR #3 review at designs commit `8e75c43`) Added P05 (parity check via `%` and `if`/`else`) and P07 (safe divide with explicit divisor check) to `spec/validation-prompts.md`. P07 notes that Unix exit-code truncation converts `-1` to `255`. Prompts can't be validated against a real spec until Plan C — authored as scaffolding. P04 (sum-to-n via recursion) and P06 (multiplication table via nested recursion) require user function calls and move to Stage 3's Task 35 alongside P08-P10.

## Stage 3 — Multi-arg functions, recursion, closures, lambdas

- Task 29 — Extend parser (multi-arg decls, call exprs with args, lambda syntax)
  - status: todo
  - commits: []
  - notes:
- Task 30 — Extend typechecker (function types, application unification, capture analysis)
  - status: todo
  - commits: []
  - notes:
- Task 31 — Extend closure conversion (flat closure records with `{code_ptr, env_fields...}`)
  - status: todo
  - commits: []
  - notes:
- Task 32 — Extend codegen (closure calling convention, indirect call via code_ptr, GC-heap alloc)
  - status: todo
  - commits: []
  - notes:
- Task 33 — examples/fibonacci.sigil + higher_order.sigil + e2e tests
  - status: todo
  - commits: []
  - notes:
- Task 34 — Performance floor: fib(20) prints 6765 in <50ms on both hosts
  - status: todo
  - commits: []
  - notes:
- Task 35 — Seed prompt bank P04, P06, P08–P10 (P04+P06 moved from Task 28)
  - status: todo
  - commits: []
  - notes: (scope revised post PR #3 review at designs commit `8e75c43`) Task 35 now ships five prompts, not three: P04 and P06 moved here from Task 28 because they require recursive user function calls (only available after Task 29). P08-P10 unchanged.
