# Lambda-of-State Deviations

Deviations from `in-progress/2026-05-09-sigil-lambda-of-state-runtime.md`.
Each entry is logged **before** the implementing commit (per the plan's
commit discipline). Entries remain after the plan closes as a permanent
record.

Format mirrors `PLAN_A_DEVIATIONS.md`:

```
## <date> — [DEVIATION Phase N] <one-line topic>

**Context:** ...

**Deviation:** ...

**Rationale:** ...

**Implementing commit(s):** <SHAs>
```

Untagged sweep / chore entries use `[CHORE]` instead.

## 2026-05-08 — [DEVIATION Phase 1] Instrumentation scope narrower than plan; trace strategy different

**Context:** Plan Task 2 prescribed `eprintln!` traces at four specific
sites: `lower_k_pair_call` widened_arg write, synth-cont args_ptr[0] load,
`sigil_continuation_invoke` arg/body_val/wrapped, and `sigil_perform` Done
dispatch. The plan expected the traces to isolate the offending step
within the runtime's value-passing pipeline.

**Deviation:** Diagnosis used runtime-only tracing (no codegen-emitted
traces). Annotated `sigil_run_loop` entry/exit with `rl=` (run_loop
nesting depth) and `opak=` (OUTER_POST_ARM_K depth), plus effect-id and
tag annotations on DISCHARGED/DONE terminal traces, `sigil_perform`
dispatch trace, and CALL dispatch trace. The `sigil_continuation_invoke`
traces from the prior session were already present and retained.

The structural difference was visible from run_loop nesting alone: in the
working (Sync) case, body dispatch and arm dispatch share a single
run_loop; in the broken (CPS) case, the arm dispatches in a nested
run_loop and the DISCHARGED value flows back as the perform resume value.
The plan's prescribed trace sites would not have surfaced this nesting
asymmetry.

**Rationale:** The bug is at the level of run_loop topology, not
individual value-passing steps. The `rl=`/`opak=` annotations on existing
trace sites were sufficient to confirm the root cause. Adding codegen-
emitted traces at `lower_k_pair_call` would have required a compiler
rebuild cycle targeting a path that turned out not to be the offending
step.

**Implementing commit(s):** (uncommitted diagnostic instrumentation; will
be removed before Phase 2 final commit)

## 2026-05-09 — [DEVIATION Phase 2] Fix scope is architectural, not point-fix; plan task underspecified

**Context:** Plan Phase 2 prescribed a single Task 4 ("Implement the
runtime/codegen fix") under a one-week scope. The Phase 1 diagnosis named
the symptom (DISCHARGED value flows as perform resume) and proposed two
fix shapes (Option A: codegen check `TerminalResult.tag` after inline
run_loop; Option B: return NextStep to trampoline instead of inline
drive). Both options assumed the offending perform site was a single
identifiable codegen path. The plan body did not enumerate the codegen
data flow that produces the inline-drive pattern, leaving Phase 2 to
discover the structure during implementation.

**Deviation:** Phase 2 cannot land as a point-fix. The investigation
revealed the actual structure:

1. `count_elements` (P19) classifies as CPS via Pattern C with
   `chain_length == 0` (`is_let_yield_prefix_then_branched_cps_tail_body`
   accepts the body's bare `match xs { Nil => 0, Cons(_, rest) => {...} }`
   tail).

2. The body emit dispatches a Final synth-cont via `is_zero_chain`
   (codegen.rs:11279). The synth-cont's body emit reaches the FINAL
   step's tail dispatch at codegen.rs:14948 (`ChainStepRole::Final`).

3. The Final emit calls `detect_pattern_c_dispatch(tail_expr, ...)` to
   decide between Pattern C work-stack dispatch (real continuations) vs
   "Non-Pattern-C standard tail" (`lower_expr_in_tail_pos` →
   `lower_match` → `lower_perform_to_value` with **identity-k**).

4. **`detect_pattern_c_dispatch` only recognizes `Expr::If` and
   `Expr::Match`-with-2-`BoolLit`-arms** (codegen.rs:29137-29220).
   Sum-type Match patterns (`Nil`, `Cons(_, rest)`) fall through to
   `None`. The Final emit then takes the standard-tail path, which uses
   identity-k for performs in arm bodies — exactly the bug the diagnosis
   identified.

5. The override gate at codegen.rs:9982-10006
   (`tail_has_no_arm_pattern_bindings`) intentionally routes
   pattern-binding shapes (e.g., `Cons(_, rest)`) to B.2 — but B.2's
   classifier (`is_compound_match_with_arm_perform_body`) only accepts
   arm bodies with `stmts.len() == 1`, which rejects `count_elements`
   (Cons arm has 2 perform stmts). Result: count_elements is
   architectural no-man's-land — Pattern C accepts it but its dispatch
   doesn't handle the shape; B.2 doesn't accept it at all.

The proper fix requires extending three machinery layers in concert:

- **Classifier:** Generalize `seed_branch_work` (codegen.rs:3643) and
  `detect_pattern_c_dispatch` (codegen.rs:29123) to accept N-arm
  sum-type Match patterns. Both return shapes change from 7-tuple
  `(cond, then_*, else_*, then_kind, else_kind)` to a `Vec<ArmInfo>`.

- **Work-stack dispatch:** Extend codegen.rs:15113+ to emit per-arm
  pattern tests + binding extraction (using `emit_pattern_test`
  infrastructure already used by B.2 at codegen.rs:10229), then
  dispatch each arm's leaf via the existing Pure / CpsCall / Perform /
  PerformChain emit paths.

- **Branch chain captures:** Update `collect_branch_chain_allocs`
  (codegen.rs:3469) to include arm pattern bindings as
  `SynthContCapture` entries. The PerformChain leaf emit at
  codegen.rs:15643 already loads captures from the closure record;
  the change is purely in the capture-collection pass.

- **Override gate:** Either remove `tail_has_no_arm_pattern_bindings`
  entirely (the new dispatch handles bindings) or update it to allow
  pattern-binding shapes when chain_length is consistent with what
  Pattern C now accepts.

**Rationale:** Failed point-fix attempts surfaced the architectural
nature of the bug:

- *Attempt 1* (`emit_terminal_out_reset_to_done` +
  `emit_discharge_propagation_check` at `lower_call`'s CPS branch +
  `lower_perform_to_value`): broke Bug-2-era discharge routing.
  DISCHARGED escaped the handle boundary because the propagation
  bypassed the handle's three-way branch. P19 still output 0;
  pattern_c_use_x still crashed.

- *Attempt 2* (`emit_discharge_propagation_check` at
  `lower_perform_to_value` only, gated on `user_fn_abi == Cps`):
  Regressed `task_78_5_g5_continuation_in_handler_lambda_through_-
  mono_runs_post_119b`. The regression test's `eval` function uses
  Pattern C with chain_length=5; the Final synth-cont's body emit
  reaches sum-type Match (eval's body-level `match e { IntE | DivE }`)
  through the same standard-tail path as count_elements. With the fix,
  the State.get arm's discharge propagated up too eagerly, terminating
  eval at the first perform. The test's `tick\ntick\n2\n` output
  required the prior broken behavior (closure-pointer-as-cur with
  execution continuing) to fire IO.println twice; my fix's
  proper-discharge propagation produced `0\n` instead.

The shared root cause across both failures: identity-k at the perform
site is a structural mismatch with lambda-of-state semantics. No
post-hoc check at the perform site can recover the captured continuation
(synth-cont chain) that was discarded when codegen routed through
`lower_perform_to_value`. The fix must be at the routing decision —
extend Pattern C dispatch to keep the perform site on the chain
machinery's real-continuation path for sum-type Match.

**Implementing commit(s):** Investigation surfaced the architectural
scope; full implementation deferred. The `user_fn_abi` field
infrastructure (9 Lowerer construction sites + 1 struct field) committed
first as a no-op foundation for future per-ABI codegen decisions. The
plan moves to `failed/` with this deviation as the authoritative scope
re-estimation; a follow-on plan should be queued with the four-layer
machinery extension (classifier → dispatch → work-stack → branch chain)
broken into independently testable seams.

**Note on P19's documented oracle.** The design doc claims P19's
expected output is `5`, but tracing the program semantics (lambda-of-
state with `return(v) => fn(_s) => v` and `count_elements`'s `Nil => 0`
base case ignoring final state) gives `0`: `runner(initial)` =
`(fn(s) => k(s)(s))(0)` unwinds through the deep-handler chain to
`return arm closure` applied to state, returning the body's terminal
value (`0`), not the final state. The working baseline test
`pattern_c_in_branch_perform_state_threading_returns_42` works because
its `comp` ends with `let v = perform S.get(); v` — explicitly reading
final state. P19's `count_elements` never reads state, so its program
semantics produce `0`. The bug is observable via `pattern_c_use_x`
(prints pointer-shaped integers + crashes "handler stack empty"), which
is the more precise acceptance gate. The follow-on plan should restate
P19's expected output as `0` (or rewrite `count_elements` to read final
state via `Nil => perform State.get()`) to align expected with semantic
ground truth.
