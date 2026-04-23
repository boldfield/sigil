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
  - status: todo
  - commits: []
  - notes:
- Task 23 — Extend elaboration (if → match on Bool; arith flattened into ANF)
  - status: todo
  - commits: []
  - notes:
- Task 24 — Extend codegen (i63 arith with overflow-wrap; icmp; brif; sdiv/srem zero-check)
  - status: todo
  - commits: []
  - notes:
- Task 25 — Runtime primitives (int_to_string, panic_arith_error, checked_add/sub/mul, Byte primitives)
  - status: todo
  - commits: []
  - notes:
- Task 26 — examples/factorial.sigil + arith.sigil + div_by_zero.sigil + e2e tests
  - status: todo
  - commits: []
  - notes:
- Task 27 — Performance floor: factorial(10) in <100ms end-to-end on both hosts
  - status: todo
  - commits: []
  - notes:
- Task 28 — Seed prompt bank P04–P07
  - status: todo
  - commits: []
  - notes:

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
- Task 35 — Seed prompt bank P08–P10
  - status: todo
  - commits: []
  - notes:
