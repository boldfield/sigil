# Lambda-of-State Progress

Validation log for Plan B preview: lambda-of-state runtime correctness.

## 2026-05-09 — Phase 3 validation rerun

### P19: State threading via lambda-of-state (Plotkin-style)

**Source:** `/tmp/sigil-lambda-state/p19_resumes_many.sigil`

**Shape:** `count_elements` recurses over a 5-element `IntList` via
sum-type match (`Nil`/`Cons`). Each `Cons` arm performs `State.get()`
and `State.set(cur + 1)`. `run_state` uses the canonical Plotkin
encoding: `return(v) => fn(s) => s`, `get(k) => fn(s) => k(s)(s)`,
`set(s2, k) => fn(_s) => k(s2)(s2)`.

**Result:** stdout `5\n`, exit 0. First-compile, first-run pass.

**Fix applied:** Extended Pattern C's `detect_pattern_c_dispatch` to
classify N-arm sum-type Match patterns as `PatternCDispatch::SumType`.
`seed_branch_work_sum_type` allocates branch chains with per-arm
pattern binding captures. Post-classification validation at the FINAL
dispatch call site rejects SumType dispatch when any PerformChain arm
contains CPS calls (avoids nested run_loop regression for shapes like
the g5 `eval` pattern).

### e2e test suite

**Result:** 504 passed, 3 failed (pre-existing perf timing failures:
`fib_perf`, `fib_cps_perf`, `tree`). No correctness regressions.

### New e2e tests added

1. `lambda_of_state_p19_sum_type_match_returns_5` — P19 literal, oracle `5\n`
2. `lambda_of_state_binary_dispatch_unchanged` — existing binary dispatch shape, oracle `42\n`
3. `lambda_of_state_sum_type_with_cps_calls_falls_through` — g5 eval pattern (CPS calls in DivE arm fall through to standard-tail), oracle `tick\ntick\n2\n`
