# Plan A1 Progress

Task-by-task tracker for Plan A1 (`docs/plans/2026-04-21-sigil-core.md` in
`boldfield/designs`). Each entry tracks: the task ID, current status, linked
commits, and optional notes on deviations. Deviations are logged separately
in `PLAN_A1_DEVIATIONS.md` *before* the implementing commit.

Status values: `todo`, `in-progress`, `done`.

## Stage 0 — scaffolding

- Task 0.1 — pin every dependency exactly
  - status: done
  - commits: [1248bc7]
  - notes:
- Task 0.2 — clippy.toml lint rules
  - status: done
  - commits: [1248bc7]
  - notes:
- Task 0.3 — CI workflow
  - status: done
  - commits: [26bb600]
  - notes:
- Task 0.4 — progress tracking files
  - status: done
  - commits: [d8c497a]
  - notes:
- Task 0.5 — commit message format check
  - status: done
  - commits: [26bb600]
  - notes:
- Task 0.6 — error code catalog scaffolding
  - status: done
  - commits: [c64059c]
  - notes:
- Task 0.7 — diagnostics output format (JSON Lines on stderr)
  - status: done
  - commits: [c64059c]
  - notes:
- Task 0.8 — sigil explain <code> subcommand
  - status: done
  - commits: [c64059c]
  - notes:
- Task 0.9 — validation prompt bank seed
  - status: done
  - commits: [670f41d]
  - notes:
- Task 0.10 — runtime instrumentation counters
  - status: done
  - commits: [1efcda7]
  - notes: Plan B will populate the arena / handler-walk / trampoline / CPS slots.
- Task 0.11 — safepoint metadata infrastructure
  - status: done
  - commits: [1efcda7]
  - notes: Compiler-side StackMapBuilder ships with task 12.
- Task 0.12 — no-interior-pointers CI check
  - status: done
  - commits: [95abc87]
  - notes:

## Stage 1 — hello-world vertical slice

- Task 1 — initialize Rust workspace + .gitignore + README
  - status: done
  - commits: [1248bc7]
  - notes: Landed with Stage 0 task 0.1; workspace scaffolding is the same commit.
- Task 2 — runtime crate (value, header, gc, io, arena, counters)
  - status: in-progress
  - commits: [1efcda7]
  - notes: counters + stackmap from task 0.10/0.11 already landed.
- Task 3 — compiler crate CLI + stub modules
  - status: done
  - commits: [2a17e83]
  - notes: Landed together with Tasks 4-15 as a multi-task commit; see DEVIATIONS.
- Task 4 — lexer
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS).
- Task 5 — parser
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS).
- Task 6 — name resolution
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS).
- Task 7 — type checker
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS).
- Task 8 — elaboration to ANF
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS).
- Task 9 — color inference stub
  - status: done
  - commits: [2a17e83]
  - notes: Stub landed with the multi-task commit; real inference is Plan B.
- Task 10 — CPS transform stub
  - status: done
  - commits: [2a17e83]
  - notes: Near-identity stub landed with multi-task commit; IO special-case flagged TODO(plan-b). Real CPS transform is Plan B Stage 6.
- Task 11 — closure conversion
  - status: done
  - commits: [2a17e83]
  - notes: Stub — every fn becomes a top-level code block with empty closure record. Real captures handled in Plan A2+.
- Task 12 — Cranelift codegen (with safepoints + headers)
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit (see DEVIATIONS); stackmap section populated at every call site.
- Task 13 — linker driver
  - status: done
  - commits: [2a17e83]
  - notes: Multi-task commit. See DEVIATIONS for the Linux -lgcc_s addition.
- Task 14 — examples/hello.sigil
  - status: done
  - commits: [2a17e83]
  - notes:
- Task 15 — std/io.sigil
  - status: done
  - commits: [2a17e83]
  - notes: Compiler recognises IO.println as a runtime intrinsic; flagged TODO(plan-b) for generalisation in Plan B Stage 6.
- Task 16 — end-to-end test
  - status: todo
  - commits: []
  - notes:
- Task 17 — reproducibility.sh
  - status: todo
  - commits: []
  - notes:
- Task 18 — smoke.sh
  - status: todo
  - commits: []
  - notes:
- Task 19 — prompt bank (3 entries)
  - status: done
  - commits: [670f41d]
  - notes: Seeded alongside Stage 0 task 0.9 since content is identical.
