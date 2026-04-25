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
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: This file.
- Task 4.5.2 — Create empty `PLAN_B_DEVIATIONS.md`
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: Landed atomically with 4.5.1 (scaffolding is one unit).
- Task 4.5.3 — Plan B questions use `[PLAN-B]` prefix in `QUESTIONS.md`
  - status: done-pending-ci
  - commits: [7aed987]
  - notes: Convention already documented in `QUESTIONS.md` header (added in Plan A2 Task 1.5.3). B entries follow the same convention; no header update required.
- Task 4.5.4 — Extend `.github/workflows/ci.yml` for Plan-B-specific invariants
  - status: todo
  - commits: []
  - notes: Deep-recursion regression test, multi-shot stress test, selective-CPS correctness test. The real tests compile .sigil programs that depend on generics / effects and so can only pass after Stages 5/6 land. Stage 4.5 wires the steps as `scripts/plan-b-invariants.sh` (executed from CI) that currently short-circuits with a guard message; each invariant flips from short-circuit to real assertion as the underlying feature lands.
- Task 4.5.5 — Create `sigil-abi` leaf crate; consolidate stackmap and cross-boundary constants
  - status: done-pending-ci
  - commits: [e1c4286]
  - notes: New `abi/` workspace member, `#![no_std]`, zero deps. Holds stackmap wire-format constants + `StackMapRecordV0` struct + tagged-Value bit masks + `TAG_INT_SHIFT`. `runtime::stackmap` and `runtime::value` `pub use`-re-export from it; the old `STACKMAP_*` pins in codegen.rs were removed. `sigil-header-constants` stays untouched (adjacent but distinct scope — the 8-byte object header). Pod-verify green.

## Plan A3 carryover

- Carryover — Full nested Maranget exhaustiveness
  - status: todo
  - commits: []
  - notes: Plan A3 ships top-level coverage only; nested patterns inside ctor fields fall through to `TRAP_NONEXHAUSTIVE_MATCH`. Plan B extends `is_exhaustive` to descend into nested ctor/tuple/record patterns and generates the nested-shape counterexample witness.
- Carryover — Suppress E0120 when an arm body fails type-checking
  - status: done-pending-ci
  - commits: [HEAD]
  - notes: `check_match` tracks an `any_arm_erred` flag by snapshotting `self.errors.len()` before/after each arm's pattern + body check. If any arm added to the error list, the exhaustiveness branch (both the `Ty::User` E0120 path and the primitive `is_exhaustive` E0066 path) is skipped for this match so the user fixes the arm-level error first. Three tests: (1) suppression on arm-body type error (arithmetic on String), (2) suppression on arm-pattern E0117 (tuple pattern on user type), (3) regression-guard that E0120 still fires on clean-but-non-exhaustive arms. E0120 catalog long-form updated to describe the suppression behavior.
- Carryover — Tagged-vs-raw Int ABI decision
  - status: todo
  - commits: []
  - notes: Resolve in QUESTIONS.md alongside `sigil-abi` work in Stage 4.5; audit `ishl_imm 1` / `sshr_imm 1` sites against the chosen rule. Logged as a `[PLAN-B]` QUESTIONS.md entry before the implementing commit.

## Stage 5 — Parametric polymorphism

- Task 47 — Parser: `[A, B]` generic params, explicit row vars `![IO | e]`
  - status: todo
  - commits: []
- Task 48 — Type checker: HM unification with row variables, closed rows
  - status: todo
  - commits: []
- Task 49 — Monomorphization: reachability-bounded, typed IR preserved
  - status: todo
  - commits: []
- Task 50 — Color inference: per-monomorph, SCC-aware, `--dump-color`
  - status: todo
  - commits: []
- Task 51 — `examples/generic_map.sigil` + e2e
  - status: todo
  - commits: []
- Task 52 — Validation prompts P16, P17
  - status: todo
  - commits: []

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
