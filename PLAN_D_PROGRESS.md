# Plan D — v2 architectural cluster (Stages 11–13)

Tracks Plan D's execution against `boldfield/designs/in-progress/2026-04-30-sigil-plan-d.md` (moves to `done/` on completion). Plan C ~85% complete at sigil/main `dfcd60b` on 2026-04-30 (PRs #44 / #45 / #46 / #47 squash-merged); Plan C completion is a separately queued plan that closes the Plan C ledger after Plan D ships. The two address disjoint scopes — Plan D is the discrete unit of compiler/runtime work that unblocks Plan C completion's stdlib/demos/validation tasks.

## Stage 10.5 — Plan D scaffolding

**Complete before any Stage 11 task work.**

- 10.5.1 — Create `PLAN_D_PROGRESS.md` (this file).
  - status: done (this commit)
- 10.5.2 — Create `PLAN_D_DEVIATIONS.md`.
  - status: done (this commit)
- 10.5.3 — Plan D questions use `[PLAN-D]` prefix (`QUESTIONS.md` preamble updated to include the tag).
  - status: done (this commit)
- 10.5.4 — Open draft `[DEVIATION Plan D overview]` entry in `PLAN_D_DEVIATIONS.md`.
  - status: done (this commit)
- 10.5.5 — Pre-survey `#[ignore]` inventory and partition into closure-targets / test-infra / other-v2-pending.
  - status: done (this commit)
- 10.5.6 — Open or link Plan B' carryover #2 (Sync shim emission gating) tracking artifact.
  - status: done (this commit) — see [CHORE] issue link below.

**Acceptance:** CI still green; new tracking files exist; overview deviation entry drafted; `#[ignore]` partition recorded; Plan B' carryover #2 tracking artifact linked.

### `#[ignore]` partition (recorded per Stage 10.5.5)

Survey at sigil/main `dfcd60b` (Plan D start). The plan estimated ~12 ignored tests; **actual count is 3**. Logged as `[DEVIATION Stage 10.5.5]` in `PLAN_D_DEVIATIONS.md` so the discrepancy is preserved.

**(a) Plan D closure targets** — un-ignore at Task 112 / Task 119:

| Test | Location | Closure step |
|---|---|---|
| `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix` | `compiler/tests/e2e.rs:7014` | Task 112 (wrapper-fn-frame composition fix) |

**(b) Non-architectural test-infra gaps** — leave alone at Task 119:

| Test | Location | Rationale |
|---|---|---|
| `std_io_read_line_via_piped_stdin_pending_test_infra` | `compiler/tests/e2e.rs:6792` | Needs piped-stdin test infrastructure; tracked for Task 78 (Plan C completion). |
| `arena_overflow_aborts` | `runtime/src/arena.rs:489` | Abort tests are not directly observable from `cargo test`; run with `cargo test -- --ignored` and confirm SIGABRT manually. |

**(c) Other v2-pending tests not closed by Plan D** — none surveyed.

### Plan B' carryover #2 tracking

Plan B' Stage-6.8-followup carryover #2 (Sync shim emission gating) is out of Plan D scope but tracked here per Stage 10.5.6 so the carryover has a named owner. GitHub issue link is added on this commit's followup (issue creation requires `gh` and is logged at the bottom of this entry once the issue number is known). Per `PLAN_B_PRIME_DEVIATIONS.md` "Stage-6.8-followup architectural carryovers" entry: every Cps-ABI top-level fn currently emits a `<mangled>__sync_shim` regardless of fn-as-value usage. Bounded bloat (one ~100-byte shim per Cps fn). Gate on `top_level_fn_names_seen_as_value` from closure_convert if Cps-fn count grows.

**Issue link:** https://github.com/boldfield/sigil/issues/48

## Stage 11 — Foundation lifts

- Task 111 — TLS → packed multi-return for `sigil_run_loop` terminal (Plan B' carryover #1, PR #39 §2).
  - status: done-pending-ci ([HEAD]) — `sigil_run_loop` now returns `#[repr(C)] TerminalResult { value: u64, tag: u64 }` via Cranelift `[I64, I64]` register-pair multi-return; on x86_64 SysV the pair lands in `rax:rdx`, on aarch64 AAPCS in `x0:x1`. Runtime: 2 TLS cells (`LAST_TERMINAL_TAG`, `LAST_TERMINAL_VALUE`) and 4 FFI helpers (`sigil_last_terminal_tag` / `_value` / `sigil_reset_*`) deleted; `runtime/src/handlers.rs` carries no globals for terminal tracking; 13 in-file unit tests updated to `.value` access. Compiler: `run_loop_sig` extended with second `I64` return; deleted 4 FFI declarations + 4 `FuncId` fields on `PerFnRefsCtx` + 4 `FuncRef` fields on `PerFnRefs` and `Lowerer` + ~13 threading sites; added 2 `Option<Variable>` fields on `Lowerer` (`last_terminal_value_var`, `last_terminal_tag_var`) with `last_terminal_vars` (lazy declare-and-init), `reset_last_terminal_vars`, and `capture_run_loop_terminal` helpers; updated 5 internal `run_loop_ref` call sites to capture the multi-return into the Variables; updated 2 handle-entry reset emits and 5 handle-exit query emits to use `def_var` / `use_var`. Sync shim's run_loop call (a top-level entry point with no enclosing handle) reads only `inst_results[0]` (value); the second return slot is structurally present but ignored. pod-verify clean.
- Task 112 — Wrapper-fn-frame composition fix (closes `[DEVIATION Task 72]` constraint #3).
  - status: todo

**Stage 11 review checkpoint** (per the plan body): TLS-removal correctness; wrapper-fn-frame fix scope (depth ceiling vs Plan C `state.sigil` test surface); incidental `#[ignore]` closures, if any.

## Stage 12 — Type-system surface

Internal ordering: 114 must precede 115; 113 and 116 are independent.

- Task 113 — Tuples / `Pair[A, B]`.
  - status: todo
- Task 114 — Type-parameterized effect rows (`![Raise[E]]`, `![State[S]]`).
  - status: todo
- Task 115 — Per-op generic params on user-declared effects (`fail[A]: (E) -> A`).
  - status: todo
- Task 116 — Row-polymorphic Fn parameters.
  - status: todo

**Stage 12 review checkpoint** (per the plan body): AST shape consistency; diagnostic quality; closure of Tasks 71/72 deviation surface-area lines; stdlib updates to use the now-expressible generic shapes.

## Stage 13 — Continuation lifts

- Task 117 — First-class continuations (k-as-value). Highest-risk Plan D step; pre-authorized to split into 117a/117b/... per the plan's split-authority criteria.
  - status: todo
- Task 118 — Conditional/branched k-call.
  - status: todo

**Stage 13 review checkpoint** (per the plan body): lifted-lambda closure-record discipline; arena escape rate (Plan B Task 60 baseline = 0%); Step 118 minimality; Sudoku smoke.

## Plan D closeout

- Task 119 — Plan D closeout audit. Walk every `[DEVIATION Task NN]` entry whose v2 closure path points at a Plan D-shipped lift; un-ignore tests; ship Sudoku + JSON parser smoke gates via e2e harness; update spec §14 (v1 limits).
  - status: todo

## Plan D completion criteria

- All Stage 10.5 + 11 + 12 + 13 acceptance criteria met on both hosts (per CI).
- All Stage 11 / 12 / 13 review checkpoints signed off.
- Closeout audit (Task 119) done.
- All tasks marked `done` with implementing commit references in this file.
- Sudoku and JSON parser half compile + run via e2e harness on both hosts; demo-PR landings on `main` are not required for Plan D closure (those belong to Plan C completion).
- Plan file `git mv`'d from `in-progress/` to `done/` once the human review checkpoint after Task 119 signs off.
