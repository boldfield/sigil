# Plan E3 — Pre-Phase-1 baseline (activation gate)

**Plan:** `boldfield/designs / done/2026-05-08-sigil-v2-per-context-cps.md`
**Branch:** `v2-per-context-cps`
**Date:** 2026-05-15
**Threshold:** activate iff trampoline-related cumulative time ≥ 5%
of total samples on a representative effect-heavy workload.

## Data source

Pod-side compile-and-run of effect-heavy `.sigil` workloads OOMs the
node (Cranelift in the compiler binary); see `CLAUDE.md`'s
"Pod-safe" / "Defer to CI" rules. The activation-gate profile was
extracted from the existing
`task_12_validation_profile_json_sigil_end_to_end` e2e test, which
runs on every CI push and uploads the folded-stacks output as the
`profile-validation-${os}` artifact. The most recent run on main
(`e0c1949`, run id `25893143140`) provided the numbers below.

The e2e test runs two workloads back-to-back under
`SIGIL_CPU_PROFILE` at 999 Hz:

1. **`examples/json.sigil`** — the plan's first-choice workload.
   Runtime is sub-millisecond on a modern host; at 999 Hz no SIGPROF
   tick lands inside the run, so the folded sidecar is empty. No
   measurement possible without a larger input — the plan body
   anticipates this and suggests "a 1MB JSON file generated for this
   purpose" but the current workload only round-trips a tiny demo
   value.

2. **`examples/fib_cps_perf.sigil`** — the test's guaranteed-samples
   evidence run (~250–500 ms wall-clock). 57 samples on Ubuntu, 11
   on macOS. This is the workload the baseline is computed against.

## Numbers

Trampoline-related frames (per the plan body's list) include:
`sigil_run_loop`, `sigil_run_loop_impl`,
`sigil_run_loop_blocking_trampoline`, `sigil_post_yield_cont_*`,
`sigil_next_step_*`, `OUTER_POST_ARM_K_STACK`, `post_arm_k_*`,
`sigil_handler_arm_*`, `sigil_continuation_identity`.

| Host         | Samples | SELF-time in trampoline | CUM stacks touching trampoline |
|--------------|--------:|------------------------:|-------------------------------:|
| ubuntu-24.04 |      57 |                  12.3%  |                        100.0%  |
| macos-14     |      11 |                   9.1%  |                         90.9%  |

**Both hosts pass the ≥5% threshold by SELF-time.** Every
sample's stack contains at least one trampoline frame, so the
trampoline is structurally in the hot path on this workload.

## Workload-fit caveat

The plan body's optimization targets the **`post_arm_k` machinery**:
a Cps-ABI caller emitting `NextStep::Call` records bound through
post_arm_k closures into a NextStep arena. `fib_cps_perf` is
`UserFnAbi::Sync` (per its own header comment lines 19–35: "fib
falls through to `UserFnAbi::Sync` despite being colored Cps").
Each perform site routes through `lower_perform_to_value`'s
per-call-site `sigil_run_loop` driver — a different trampoline
path than the Cps-ABI `post_arm_k` machinery Plan E3 reduces.

So `fib_cps_perf`'s 12.3% trampoline cost confirms **the trampoline
is hot in general**, but it does not directly measure the
`post_arm_k` cost Plan E3 specifically targets. The post_arm_k path
only fires when both caller and callee are `UserFnAbi::Cps`
(currently a narrower set: helpers whose body matches
`is_simple_tail_perform_with_pure_args_body`,
`is_simple_chained_let_yield_then_pure_tail_body`,
`is_let_yield_prefix_then_branched_cps_tail_body`,
`is_compound_match_with_arm_perform_body`, or
`is_simple_yield_then_constant_tail_body`).

`json.sigil`'s mutually-recursive printer/parser fns over `Mem` /
`State` / `Raise` rows are the closer fit for the post_arm_k path,
but with no samples landing under the current tiny workload, we
have no Cps-ABI throughput measurement at baseline.

## Decision the user must confirm

Two reads of the gate:

1. **By the letter of the plan:** trampoline self-time is 12.3% ≥
   5% on a representative effect-heavy workload. Threshold met.
   Activate Phase 1.

2. **By the spirit of the plan:** the workload that actually
   exercises the `post_arm_k` machinery Phase 2 reduces is
   `json.sigil` (or a larger Cps-ABI-bodied helper chain), and we
   don't have samples for it. The 12.3% number measures a different
   trampoline path. If Phase 2 lands and the post_arm_k path turns
   out to be a small fraction of fib_cps_perf's 12.3% — say, 1% —
   the optimization is real but smaller than the headline number
   suggests.

A third option:

3. **Defer activation until a Cps-ABI-bodied baseline exists.** Add
   a larger json.sigil input (or a new effect-heavy benchmark whose
   helpers actually classify as `UserFnAbi::Cps`) to the e2e
   profile suite, re-run, then revisit.

## Recommendation

Option (1) plus a Phase-3 contingency. Activate Phase 1 (analysis
+ diagnostic, read-only — no codegen change). Use Phase 1's
`--dump-discharge` output on `examples/state.sigil`,
`examples/choose_demo.sigil`, `examples/json.sigil`, and selected
`std/*.sigil` modules to inventory how many call sites actually
classify as `FullyDischarged` with a Cps-ABI callee.

**Locked HARD-STOP threshold.** After this doc was written, the
user locked the Phase-1-vs-Phase-2 gate at **< ~10 FullyDischarged
Cps-ABI call sites** across `std/` + `examples/` +
`compiler/tests/e2e.rs` source strings. Below threshold → land
Phase 1 alone (the diagnostic is reusable infrastructure) + close
the plan as `done/`. Do NOT proceed to Phase 2.

This shape lets Phase 1 itself serve as the empirical sharpener
for Phase 2's go/no-go — without commit-and-revert risk, since
Phase 1 ships read-only.

## Phase 1 finding (Status: measured, 2026-05-15)

**Result.** 2 FullyDischarged Cps-color call sites out of 157 total
across 11 effect-heavy `examples/*.sigil` workloads. Well below
the locked HARD-STOP threshold; **Phase 2 not pursued.**

Per-example breakdown (re-runnable via `cargo test -p sigil-compiler
--lib discharge::tests::phase_1_activation_inventory_across_examples
-- --nocapture`):

| example | sites | Full | Full+Cps | Partial | None |
|---|--:|--:|--:|--:|--:|
| examples/state.sigil | 1 | 0 | 0 | 0 | 1 |
| examples/choose_demo.sigil | 1 | 0 | 0 | 1 | 0 |
| examples/json.sigil | 95 | 0 | 0 | 0 | 95 |
| examples/sudoku.sigil | 18 | 0 | 0 | 1 | 17 |
| examples/interpreter.sigil | 21 | 0 | 0 | 0 | 21 |
| examples/nested_effects.sigil | 5 | 0 | 0 | 0 | 5 |
| examples/multishot_perf.sigil | 3 | 0 | 0 | 1 | 2 |
| examples/tree_stress_repeat.sigil | 8 | 0 | 0 | 0 | 8 |
| examples/catch.sigil | 2 | 1 | **1** | 0 | 1 |
| examples/option_demo.sigil | 2 | 0 | 0 | 0 | 2 |
| examples/div_recover.sigil | 1 | 1 | **1** | 0 | 0 |
| **TOTAL** | **157** | **2** | **2** | **3** | **152** |

**Structural finding.** The 0 Partial count in `json.sigil` (95
sites) is load-bearing: if the analyzer were under-counting
`FullyDischarged`, partials would surface in the heaviest effect
workload. They don't — the data shape is real, not an analyzer
artifact.

The cause is Sigil's stdlib effect-helper idiom. `run_state(initial,
body_fn)`, `catch(body)`, etc. invoke `body` via a fn-typed-parameter
*indirect* call. Plan E3's optimization target is the *direct*
top-level-fn call inside a discharge context — and that shape
structurally cannot appear in user code that uses the stdlib
handler pattern. The 2 positive sites (`catch.sigil`,
`div_recover.sigil`) are both toy single-call demos where the
user inlines the `handle` instead of going through a stdlib
wrapper.

**This is a finding about Sigil's effect-handler ergonomics, not
about Plan E3's design.** A future plan that revisits this — e.g.,
specializing `run_state` per body monomorph, or shifting the stdlib
toward macro-like helpers — would shift the discharge landscape;
the `--dump-discharge` diagnostic shipped here is the input for
that audit.

**Phase 2 not pursued.** Optimizing 2 toy call sites is the worst
possible ROI shape for a non-trivial compiler optimization;
landing it would couple codegen complexity to a target shape that
real code doesn't produce.

## Out-of-pod escalation channels exercised

- Pod cannot run `./target/debug/sigil examples/json.sigil` (Cranelift OOM).
- Pod cannot run `cargo test --workspace` (sigil-compiler test OOM).
- CI's `task_12_validation_profile_json_sigil_end_to_end` artifact
  is the only currently-available source of folded-stack samples
  for this branch.
