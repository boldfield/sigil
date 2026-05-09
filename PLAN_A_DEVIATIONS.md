# Plan A Deviations

Deviations from `in-progress/2026-05-08-sigil-multi-shot-tail-correctness.md`.
Each entry is logged **before** the implementing commit (per the plan's
commit discipline). Entries remain after Plan A closes as a permanent
record.

Format mirrors `PLAN_B_DEVIATIONS.md`:

```
## <date> — [DEVIATION Phase N] <one-line topic>

**Context:** ...

**Deviation:** ...

**Rationale:** ...

**Implementing commit(s):** <SHAs>
```

Untagged sweep / chore entries use `[CHORE]` instead.

## 2026-05-08 — [PLAN-A] Phase 1 diagnosis broadens scope vs. design doc Background

**Context:** Plan A's design doc Background section described the bug as
"the body's post-perform tail runs once with `x` bound to a folded value
matching the arm's combine expression." The four candidate root causes
listed (c1 helper synth-cont args buffer reuse; c2 CPS-chained nested
perform inside post-perform tail; c3 trampoline frame state not reset; c4
selective CPS color / fn-frame composition) were all internal to the
helper synth-cont chain, predicting a localized codegen fix at the synth-
cont level.

**Deviation:** Phase 1's diagnosis (compiler/docs/multi-shot-tail-anomaly
.md) found that the actual root cause is at ABI selection, not synth-cont
chain mechanics. The reproducer's helper falls back to `UserFnAbi::Sync`
because its body shape (post-perform `Stmt::Perform` + non-pure perform
args via `int_to_string(x)`) doesn't match any classifier in
`compute_user_fn_abi` (codegen.rs:189). Sync ABI multi-shot uses
`sigil_continuation_identity` as `k_fn`, which collapses the per-resume
body execution: `r_i = arg_i` (not `body_tail(arg_i)`), and the body's
post-perform tail runs at most once synchronously.

This means the bug's blast radius is wider than the design doc described:
all Sync-ABI multi-shot helpers are silently miscompiled, not just those
with effectful post-perform tails. The pilot saw the effectful symptom
because it shows up as wrong stdout; the same shape with a non-homogeneous
pure tail (e.g., `x*1000 + 5`) also miscompiles to the wrong numeric
value (probe 9 in the diagnosis doc).

**Rationale:** Same fix shape (broaden Cps-ABI body classifiers + emit
pass), but the framing in any future review or follow-up plan should
acknowledge the wider scope. Phase 2's tasks remain inside `compiler
/src/codegen.rs` — no runtime changes, no Plan B-territory continuation
escape work. The fix is codegen-only, consistent with the plan's "no
runtime changes" risk-profile preference.

The deviation is logged here at Phase 1's end so the user can re-scope
Plan A if needed before Phase 2 begins. If the broader framing changes
the design doc materially, the doc would normally not be edited (per the
plan's "Do not modify the design doc" rule); this deviation entry is the
canonical record of the scope shift.

**Implementing commit(s):** TBD (Phase 1 diagnosis lands first; Phase 2
fix in subsequent commit).
