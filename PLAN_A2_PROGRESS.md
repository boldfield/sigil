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
  - commits: [(pending)]
  - notes: This file.
- Task 1.5.2 — Create empty `PLAN_A2_DEVIATIONS.md`
  - status: done
  - commits: [(pending)]
  - notes: Landed atomically with 1.5.1 and 1.5.3 (scaffolding is one unit).
- Task 1.5.3 — Preserve `QUESTIONS.md` across plans with `[PLAN-A2]` prefix convention
  - status: done
  - commits: [(pending)]
  - notes: Appended a tagging convention to QUESTIONS.md header; A1 entries are implicitly `[PLAN-A1]`.
- Task 1.5.4 — `scripts/pod-verify.sh` + README pod-vs-CI section + CI wiring
  - status: todo
  - commits: []
  - notes:
- Task 1.5.5 — Fix cold-target e2e staticlib ordering
  - status: todo
  - commits: []
  - notes:
- Task 1.5.6 — `debug_assert!` on typecheck env insertion (no-shadowing invariant)
  - status: todo
  - commits: []
  - notes:

## Stage 2 — Arithmetic, booleans, conditionals

- Task 20 — Extend lexer (booleans, if/else, match, operators, char literals)
  - status: todo
  - commits: []
  - notes:
- Task 21 — Extend parser (arith/cmp with precedence, if, match, unary, constant-fold `-<lit>`)
  - status: todo
  - commits: []
  - notes:
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
