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
  - status: done
  - commits: [1efcda7, 57d174b]
  - notes: counters + stackmap from task 0.10/0.11 landed in 1efcda7; value,
    header, gc, io, arena landed in 57d174b. See PLAN_A1_DEVIATIONS.md for
    the `sigil_println` signature deviation.
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
  - status: done
  - commits: [8592bde]
  - notes: Test placed at compiler/tests/e2e.rs rather than a separate sigil-tests crate; see DEVIATIONS.
- Task 17 — reproducibility.sh
  - status: done
  - commits: [b83d8bb]
  - notes:
- Task 18 — smoke.sh
  - status: done
  - commits: [21893e9]
  - notes:
- Task 19 — prompt bank (3 entries)
  - status: done
  - commits: [670f41d]
  - notes: Seeded alongside Stage 0 task 0.9 since content is identical.

## Execution status — 2026-04-23 (development moved off headless pod)

All Plan A1 task-level work is landed (Stage 0 + Stage 1, tasks 0.1 through
19). Plan A1 *completion criteria* are **not yet verified** on either host.
Remaining verification is moving to the user's local macOS laptop
(`aarch64-apple-darwin`); the headless Talos pod (`x86_64-unknown-linux-gnu`,
~8–12 GiB) has been abandoned as a build/test environment for sigil.

**What's landed in this hand-off commit (on top of `e634fe0`):**

- `Cargo.toml`: `[profile.dev]` block (debug=1, incremental=false,
  codegen-units=256) + `[profile.release]` filled to spec (debug=0,
  codegen-units=16, lto=false). Completes the profile-settings portion of
  Task 0.1 that the earlier commits had not fully set.
- `.cargo/config.toml` (new): Linux target pins the lld linker via
  `clang -fuse-ld=lld` to cut link-time memory roughly in half.
- `.github/workflows/ci.yml`: installs `lld clang` on Linux; splits the
  build into `cargo build -p sigil-runtime` → `cargo build -p sigil-compiler`
  → `cargo test --workspace --no-fail-fast`; clippy uses `--no-deps`;
  `CARGO_INCREMENTAL=0` set in env.
- `runtime/README.md`: adds the "Memory-constrained builds" section required
  by the Plan A1 completion criterion at plan lines 296–301 (profile
  settings, lld requirement, env var recommendations, build ordering,
  clippy --no-deps guidance).
- Compiler + runtime source: fmt / clippy polish only (rustfmt line
  reflows; `Item::Fn` payload boxed to shrink enum variant size;
  `unwrap_or_else(|_| 0)` → `unwrap_or(0)`). No behaviour change.

**Why the pod was abandoned.** Even with every memory guard the plan
prescribes — per-crate build ordering, `CARGO_BUILD_JOBS=1`, lld, the
profile settings above, `CARGO_INCREMENTAL=0` — the command

```shell
CARGO_BUILD_JOBS=1 cargo test -p sigil-compiler --no-fail-fast
```

OOM-killed the Talos pod. This *contradicts* the claim in the plan's
execution notes and in `runtime/README.md#memory-constrained-builds` that
"the fix is build ordering + `CARGO_BUILD_JOBS=2`, not raising the memory
ceiling." The ceiling matters: compiling the sigil-compiler test binary
(Cranelift + codegen tests linked into a single test executable)
measurably exceeds what an ~8–12 GiB pod can carry at j=1. Prior sessions
also OOM'd on `cargo build --workspace`; a previous session once took the
whole Talos node down.

**What remains for Plan A1 completion (to be done on the laptop):**

- `cargo test --workspace` green on `x86_64-unknown-linux-gnu` (CI) *and*
  `aarch64-apple-darwin` (laptop). Pod runs are out.
- `scripts/smoke.sh` compiling and running `hello.sigil` on both hosts.
- `scripts/reproducibility.sh` asserting byte-identical hello-world
  binaries across two same-host runs on both hosts.
- Verify the runtime counter assertion:
  `sigil --print-runtime-stats examples/hello.sigil -o /tmp/hello && /tmp/hello`
  prints nonzero `SIGIL_COUNTER_BOEHM_ALLOC_COUNT` and zero
  `SIGIL_COUNTER_ARENA_ESCAPE_COUNT`.
- Confirm the `.sigil_stackmaps` / `__SIGIL,__stackmaps` section is
  non-empty and parseable on emitted objects.
- `scripts/check-no-interior-pointers.sh` passes in CI on Linux.

**Gate for Plan A2.** No Plan A2 work starts until the above are green
on the laptop and the user has reviewed the A1 hand-off. The plan's
"do not grade your own work" rule still applies.

**Open follow-up (for the laptop session):** revise
`runtime/README.md#memory-constrained-builds` and the plan's Task 0.1 /
execution-notes memory guidance once real peak-RSS numbers are observable
on macOS. The current prose is the plan's *prescription*, not a
*verified* recipe — it has now been falsified on Linux at j=1.
