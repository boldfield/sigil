# Lambda-of-State Progress

Validation log for Pattern C SumType dispatch — lambda-of-state runtime
correctness.

## 2026-05-09 — Phase 3 validation rerun

### State threading via lambda-of-state (Plotkin-style)

**Shape:** `count_elements` recurses over a 5-element `IntList` via
sum-type match (`Nil`/`Cons`). Each `Cons` arm performs `State.get()`
and `State.set(cur + 1)`. `run_state` uses the Plotkin encoding.

**Modified handler** (`return(v) => fn(s) => s`, returns final state):
stdout `5\n`, exit 0. Exercises SumType dispatch state-threading.

**Literal P19 handler** (`return(v) => fn(_s) => v`, returns body value):
stdout `0\n`, exit 0. Semantically correct — `count_elements`' base
case is `Nil => 0` and the handler returns the body's terminal value,
not the final state.

### e2e test suite

**Result:** 504+ passed, 3 failed (pre-existing perf timing failures:
`fib_perf`, `fib_cps_perf`, `tree`). No correctness regressions.

### e2e tests added

1. `lambda_of_state_sum_type_state_threading_returns_5` — modified
   handler returns final state `s`=5, oracle `5\n`
2. `lambda_of_state_literal_p19_body_value_returns_0` — literal P19
   handler returns body value, oracle `0\n`
3. `lambda_of_state_three_arm_sum_type_dispatch` — 3-arm enum
   (Red/Green/Blue) with Pure + PerformChain mix, oracle `10\n100\n0\n`
4. `lambda_of_state_sum_type_with_cps_calls_falls_through` — g5 eval
   pattern (CPS calls in DivE arm fall through to standard-tail),
   oracle `tick\ntick\n2\n`
