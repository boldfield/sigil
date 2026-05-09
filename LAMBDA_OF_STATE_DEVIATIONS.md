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
