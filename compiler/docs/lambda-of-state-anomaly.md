# Lambda-of-state runtime anomaly — Phase 1 diagnosis

**Plan:** `in-progress/2026-05-09-sigil-lambda-of-state-runtime.md`.
**Spec:** `/Users/boldfield/projects/designs/docs/plans/2026-05-09-sigil-lambda-of-state-runtime-design.md`.
**Date:** 2026-05-08.
**HEAD:** sigil@main (branch `lambda-of-state-runtime`).

## TL;DR

The Plotkin-style lambda-of-state handler encoding (`S.get(k) => fn(s) =>
k(s)(s)`) produces wrong results when `run_state` is CPS-colored — i.e.,
when its effect row includes any effect beyond the handled one. P19
(`resumes: many` state counter) returns `0` instead of `5`; pattern_c with
observable IO in the helper prints pointer-shaped integers and crashes with
"handler stack empty."

**Root cause:** when `run_state` is CPS, the CPS-lowered perform site
drives the arm's `NextStep::Call` through an inline `sigil_run_loop` call
and uses the return value as the perform's resume value. When the arm body
evaluates a lambda (capturing k without calling it) and DISCHARGEs, the
DISCHARGED value — a closure pointer — flows back as the perform's resume
value instead of as the handle expression's overall result.

When `run_state` is Sync (pure `![]` effect row), `body()` is dispatched
via a Sync shim that owns its own `sigil_run_loop`. The arm DISCHARGE
terminates that run_loop and returns the closure as the handle expression's
result. `state_fn(initial)` then calls the closure correctly, invoking
`k(s)(s)` to resume the continuation with the state value.

## Structural evidence: `--dump-color` ABI classification

```
# pattern_c_verbatim (working — run_state effects=[]):
run_state         native    # Sync ABI

# pattern_c_use_x (broken — helper has IO, propagates to run_state):
run_state         cps       # CPS ABI — "row contains effect `IO`"
```

The sole structural difference between the passing and failing programs is
the ABI of `run_state`. Adding `IO` to helper's effect row (via
`perform IO.println(int_to_string(x))`) propagates to `run_state`'s
inferred row, flipping it from Sync to CPS.

## Trace evidence

All traces captured with `SIGIL_TRACE=1` using enhanced runtime
instrumentation (`RUN_LOOP_NESTING` counter, `opak=` depth annotations on
all trace lines).

### pattern_c_verbatim (Sync `run_state`) — correct

```
[TRACE run_loop_enter] rl=0 tag=1 value=0x0 opak=0          # body's Sync shim run_loop
[TRACE call] rl=0 fn=... argc=3 args[0]=0x3 ...              # CPS comp dispatched
[TRACE perform] eid=6 oid=0 user_args=[] opak=0               # S.get() perform
[TRACE call] rl=0 fn=... argc=2 args[0]=0x100512f90           # arm body dispatched (SAME run_loop)
[TRACE run_loop] DISCHARGED terminal: rl=0 value=0x100512f60  # arm DISCHARGEs — run_loop terminates
[TRACE run_loop_enter] rl=0 tag=1 value=0x0 opak=0            # NEW run_loop for k(s)(s)
[TRACE call] rl=0 fn=... argc=3 args[0]=0x0 ...               # k(0) resumes continuation with s=0
```

Flow: `body()` via Sync shim → run_loop → performs and arm dispatches
happen within same run_loop → DISCHARGE terminates run_loop → Sync shim
returns closure as `state_fn` → `state_fn(0)` invokes lambda → `k(0)` via
`lower_k_pair_call` starts NEW run_loop → continuation resumes with
correct state value.

### P19 `resumes: many` (CPS `run_state`) — broken

```
[TRACE run_loop_enter] rl=0 tag=1 value=0x0 opak=0          # outer run_loop (main Sync shim)
[TRACE call] rl=0 fn=... argc=3 args[0]=0x0 ...              # CPS dispatch at rl=0
[TRACE perform] eid=6 oid=0 user_args=[] opak=0               # State.get() perform
[TRACE run_loop_enter] rl=1 tag=1 value=0x0 opak=0            # NESTED run_loop for arm dispatch
[TRACE call] rl=1 fn=... argc=2 args[0]=0x0                   # get-arm body: fn(s) => k(s)(s)
[TRACE run_loop] DISCHARGED terminal: rl=1 value=0x1026b9ea0  # arm DISCHARGEs at rl=1
[TRACE perform] eid=6 oid=1 user_args=[4335574689] opak=0     # State.set(cur+1) — cur = 0x1026b9ea0!
```

The DISCHARGED value `0x1026b9ea0` (a closure pointer) flows directly as
`cur` (the resume value for `perform State.get()`). `State.set(cur + 1)`
receives `0x1026b9ea0 + 1 = 4335574689`. The CPS perform-site code drove
the arm's NextStep through `sigil_run_loop` (rl=1) and used the return
value as the resume value without distinguishing DONE from DISCHARGED.

### pattern_c_use_x (CPS `run_state`) — broken

```
[TRACE perform] eid=6 oid=0 user_args=[] opak=0               # S.get() — NO run_loop before this!
[TRACE run_loop_enter] rl=0 tag=1 value=0x0 opak=0            # run_loop for arm dispatch
[TRACE call] rl=0 fn=... argc=2 args[0]=0x0                   # get-arm body
[TRACE run_loop] DISCHARGED terminal: rl=0 value=0x10450efc0   # arm DISCHARGEs
[TRACE perform] eid=1 oid=1 user_args=[4367384512] opak=0      # IO.println(int_to_string(x))
```

Same mechanism: DISCHARGED closure pointer `0x10450efc0 = 4367384512`
flows as `x` from `perform S.get()`. The helper prints it via
`IO.println(int_to_string(x))`. After 4 iterations (n=3,2,1,0), the
handler stack is exhausted and `IO.println` crashes with "handler stack
empty."

## Root cause mechanism

In the Plotkin-style encoding, arm bodies evaluate a lambda that
**captures** k without **calling** it:

```sigil
S.get(k) => fn(s: Int) -> Int ![IO] => k(s)(s)
```

The arm body allocates a closure and returns it. Since k is not called,
the arm DISCHARGEs. In correct semantics, the DISCHARGED value (the
closure) should be the handle expression's overall result — it's `state_fn`
in `let state_fn = handle body() with { ... }`. The outer code then calls
`state_fn(initial)`, invoking the lambda, which calls `k(s)(s)` to resume
the continuation with the correct state value.

When `run_state` is CPS, the perform site's generated code drives the
arm's `NextStep` through an inline `sigil_run_loop`. The arm DISCHARGEs
inside this run_loop. The run_loop returns the DISCHARGED value. The
perform-site code uses it as the perform's resume value — binding it to
`x` or `cur` in the user program. This is wrong: the DISCHARGED value is a
closure pointer (a state-threaded lambda), not a state integer.

The code never reaches `state_fn(initial)` in the broken case because the
continuation is incorrectly "resumed" with the closure pointer, and
execution proceeds from the perform site with garbage data until the
handler stack is exhausted or the program produces wrong output.

## Fix shape (proposed for Phase 2)

The fix must ensure that when an arm DISCHARGEs, the DISCHARGED value
flows as the **handle expression's** result rather than as the **perform
site's** resume value. Two approaches:

**Option A — Codegen: CPS perform-site must check TerminalResult.tag.**
After `sigil_run_loop` returns for the arm dispatch, the CPS perform-site
code loads `TerminalResult.tag`. If DONE, the value is the perform's
resume value (k was called, continuation is already running). If
DISCHARGED, the value is the handle expression's result — the perform-site
code should propagate it as a DISCHARGED terminal to the enclosing
run_loop, bypassing the rest of the body.

**Option B — Codegen: return NextStep to trampoline instead of driving
inline.** The CPS perform-site code should return the arm's NextStep to
the enclosing trampoline (like a standard CPS continuation return) rather
than driving it through its own `sigil_run_loop`. The trampoline would
dispatch the arm, handle DISCHARGE correctly (it already has the
DISCHARGED handler), and route the value appropriately. This aligns with
the standard CPS discipline where performs don't create nested run_loops.

Option A is more surgical — it patches the existing inline-drive pattern.
Option B is architecturally cleaner but may require restructuring the CPS
chain step lowering. Both may be needed in combination.

## Invariants preserved

- Sync `run_state` (pattern_c_verbatim): unaffected — body dispatches via
  Sync shim, DISCHARGE terminates the body's run_loop correctly.
- One-shot effects: arms that call k(value) produce NextStep::Done, which
  the inline-drive path already handles correctly.
- Existing e2e tests (`state_example_canonical_run_state_returns_11`,
  `pattern_c_in_branch_perform_state_threading_returns_42`): these use
  Sync `run_state` (pure effect rows), so they are unaffected.
