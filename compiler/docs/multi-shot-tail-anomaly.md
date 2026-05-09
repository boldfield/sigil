# Multi-shot post-perform-tail anomaly — Phase 1 diagnosis

**Plan:** `in-progress/2026-05-08-sigil-multi-shot-tail-correctness.md` (Plan A).
**Spec:** `/Users/boldfield/projects/designs/docs/plans/2026-05-08-sigil-multi-shot-tail-correctness-design.md`.
**Date:** 2026-05-08.
**HEAD:** sigil@32b4356 (`[diagnostics] LLM-friendlier rejections for non-tail k / E0145 escape`).

## TL;DR

The 2026-05-08 spec-validation pilot reported that v1's static N-let-chain
multi-shot arms produce wrong output when the body's post-perform tail
performs an observable effect. The reported symptom was: the body's tail
fires once with `x` bound to a folded value matching the arm's combine
expression evaluated with `r_i := arg_i`.

**Root cause:** the helper fn whose body has a perform followed by an
observable effect — e.g., `let x = perform Choose.choose(seed); perform IO
.println(int_to_string(x)); x*1000` — fails every Cps-ABI body classifier
in `compute_user_fn_abi` (codegen.rs:189) and falls back to `UserFnAbi
::Sync`. The synchronous interop path lowers the perform via `lower_perform
_to_value` (codegen.rs:20107), which passes `k_fn = sigil_continuation
_identity` to `sigil_perform`. With identity-k, the arm's `r_i` is bound
to `arg_i` (identity passes through) instead of `body_tail(arg_i)`. The
arm's combine expression therefore evaluates with `r_i := arg_i`. The
helper's body post-perform tail then runs **once** synchronously after the
arm returns, with `x` bound to the arm's combine value.

**The bug is not specific to "the body's effectful tail."** It's the same
shape as any Sync-ABI multi-shot helper: `r_i = arg_i` regardless of body
shape. The pilot only saw it through the effectful-tail probe because the
existing pure-tail-bearing examples (`multishot_stress`, `multishot_perf`,
`choose_demo`) all use body shapes that are accepted by a Cps-ABI
classifier. They reach the helper synth-cont chain, where per-resume
execution is correct.

The pure-tail-with-impure-body-classifier reproducer ("probe5"
`let x = perform Choose; x*1000`) appears to give the correct numeric
output (`711000`) only because its tail `x*c` is **homogeneous** in `x`:
`combine(body(arg_i)) == body(combine(arg_i))` for `body(x) = x*c` and
linear combine. Probe 9 below uses a non-homogeneous tail (`x*1000 + 5`)
and exposes the underlying breakage even without an effectful tail.

## Reproduction

Build:

```
cd /Users/boldfield/projects/sigil
cargo build --release --bin sigil
```

Background-section reproducer (writes `repro.sigil`):

```sigil
effect Choose resumes: many { choose: (Int) -> Int }

fn helper(seed: Int) -> Int ![Choose, IO] {
  let x: Int = perform Choose.choose(seed);
  perform IO.println(int_to_string(x));
  x * 1000
}

fn main() -> Int ![IO] {
  let total: Int = handle helper(5) with {
    Choose.choose(arg, k) => {
      let r1: Int = k(7);
      let r2: Int = k(11);
      r1 * 100 + r2
    },
  };
  perform IO.println(int_to_string(total));
  0
}
```

```
$ ./target/release/sigil repro.sigil -o repro && ./repro
711
711000
```

Spec-implied output (per §8.3 "k may be invoked multiple times per arm"):
`7\n11\n711000\n`.

## Bisection

Five probes narrowed candidates from the design doc.

| Probe | Body / arm | Output | Interpretation |
|---|---|---|---|
| 1 | N=3 chain `r1=k(7); r2=k(11); r3=k(13); r1*10000+r2*100+r3` | `71113\n71113000\n` | Body's println fires **once** with `x = 71113` = `arg1*10000+arg2*100+arg3`. Confirms `x = arm.combine{r_i := arg_i}`; fold scales with N. |
| 2 | Body has `perform IO.println("HIT")` (literal, no `x` dependency); arm N=2 | `HIT\n711000\n` | Whole post-perform tail runs **once**, regardless of x dependency. Tail is collapsed structurally, not just `x` being miscomputed. |
| 3 | Body `perform IO.println(int_to_string(x)); 0` (constant pure-return) | `711\n0\n` | x in the println = 711. Pure return is independently correct (constant 0). |
| 4 | Inline helper into main (no fn boundary) | `711\n711000\n` | Bug persists. Rules out **(c4)** selective CPS color / fn-frame composition. |
| 5 | Body `let x = perform Choose; x*1000` (no IO in body) | `711000\n` | Pure-tail path appears correct. (See "homogeneity caveat" below.) |
| 8 | Body `let x = perform Choose; x*1000 + 5` (Cps ABI helper) | `711505\n` | Cps ABI: r_i = arg_i*1000+5; combine = 7005*100+11005 = 711505. **Algebraically correct.** |
| 9 | Body `let x = perform Choose; perform IO.println(...); x*1000 + 5` (Sync ABI helper) | `711\n711005\n` | Sync ABI: arm combine{r_i := arg_i} = 711, then helper tail = 711*1000+5 = 711005. **Wrong** (expected 711505). |

### Verdicts

- **(c1) Helper synth-cont args buffer reuse — NO (in the form proposed).**
  The value seen in the body's println isn't a stale args buffer (which
  would yield arg_1 or arg_N or accumulator). It's structurally
  `arm.combine{r_i := arg_i}`, which is what the arm computes when k is
  identity. Args buffer reuse isn't the mechanism.
- **(c2) CPS-chained nested perform inside post-perform tail — NO.**
  The body's CPS chain is never built in this case. The helper falls to
  Sync ABI; its body is lowered as straight-line native code with one
  synchronous `lower_perform_to_value` call per perform site. Probe 2
  would have shown N firings of the IO println if the chain existed and
  per-resume re-issuance were the bug; the IO println fires once.
- **(c3) Trampoline frame state not reset between resumes — NO.**
  In Sync ABI there's no per-resume re-entry into the body. The body
  runs once after `sigil_run_loop` terminates. Trampoline state isn't
  the mechanism.
- **(c4) Selective CPS color / fn-frame composition — NO.**
  Inlining the helper into main keeps the bug. fn-frame boundary is
  irrelevant.

The actual cause is **none of the candidates as stated**: it's a
fundamentally different mechanism — falling back to Sync ABI and
losing per-resume body execution entirely.

## Trace: how the value `711` ends up in `x`

`SIGIL_TRACE_PLAN_A=1` was added temporarily to `compute_user_fn_abi`
to print the chosen ABI per fn. Trace runs (env var stripped from the
committed code; recreate by adding two `eprintln!` lines guarded on
`std::env::var("SIGIL_TRACE_PLAN_A").is_ok()` at the two return sites
of `compute_user_fn_abi`):

```
$ SIGIL_TRACE_PLAN_A=1 ./target/release/sigil repro.sigil -o /dev/null
[trace-plan-a] compute_user_fn_abi: fn `helper` -> Sync (no body shape matched)

$ SIGIL_TRACE_PLAN_A=1 ./target/release/sigil probe5_pure_tail.sigil -o /dev/null
[trace-plan-a] compute_user_fn_abi: fn `helper` -> Cps (chained-let-yield, chain_length=1, captures=1)

$ SIGIL_TRACE_PLAN_A=1 ./target/release/sigil probe9_sync_nonhomog.sigil -o /dev/null
[trace-plan-a] compute_user_fn_abi: fn `helper` -> Sync (no body shape matched)
```

The reproducer and any helper with a mid-body `Stmt::Perform` (or with
impure perform args, since `expr_is_pure` rejects non-ctor calls
including `int_to_string`) classify as **Sync ABI**. Probe 5 — the
pure-tail body with no observable effect — classifies as **Cps ABI**.

### Sync-ABI multi-shot dispatch

`compute_user_fn_abi` (codegen.rs:189) selects `UserFnAbi::Sync` for any
Cps-color helper whose body shape matches none of:

- `is_simple_tail_perform_with_pure_args_body` (tail = perform).
- `is_simple_yield_then_constant_tail_body` (1-stmt + IntLit tail).
- `is_simple_chained_let_yield_then_pure_tail_body` (every stmt is
  `Stmt::Let { value = Expr::Perform | Expr::Call cps-wrapper }` with
  pure args).
- `is_let_yield_prefix_then_branched_cps_tail_body` (let-yield-prefix +
  If-tail with pure-or-Cps-call branches).
- `is_compound_match_with_arm_perform_body` (compound match body).

Our reproducer's body fails all five:
- `Stmt::Perform { IO.println(int_to_string(x)) }` is not `Stmt::Let`,
  rejecting the chained-let-yield classifier on its first iteration.
- The tail `x*1000` is `Expr::Binary`, not a perform, IntLit, If, or
  Match.
- `int_to_string(x)` would fail the perform-args purity check anyway,
  even if the stmt were rewritten as `Stmt::Let { _u: Unit = perform IO
  .println(int_to_string(x)) }`.

So the helper is `UserFnAbi::Sync`. Each `perform` in its body is
lowered by `Lowerer::lower_perform_to_value` (codegen.rs:20107):

1. Pack `args` into a stack slot.
2. Build `NextStep::Call(arm, args, k_closure=null, k_fn=sigil_continuation_identity)`.
3. Drive `sigil_run_loop` synchronously to the next `Done`.
4. Narrow the `Done` value back to the op's declared return type.

`sigil_continuation_identity` (runtime/handlers.rs:1635) is the
"no further continuation" terminal. With `args_len == 1` it returns
`Done(args[0])`. With `args_len == 3` and a non-null, non-self
post_arm_k_fn at slot 2, it dispatches into the post_arm_k chain.

For the static N-let-chain arm, the arm-fn is allocated with a post_arm_k
chain (codegen.rs:5430 — `arm_body_n_let_then_pure_tail_shape` →
`PostArmKChain` with N entries). At `k(arg_i)` in the arm, codegen
emits `NextStep::Call(loaded_k_pair, args=[arg_i, post_arm_k_closure,
post_arm_k_fn], args_len=3)`. The trampoline dispatches into the loaded
`k_fn`, which is `sigil_continuation_identity` (passed in by the
synchronous lowering). Identity sees args_len=3 and a non-null, non-self
post_arm_k_fn → dispatches the post_arm_k chain step's func with `args
= [arg_i]` (`runtime/handlers.rs:1686-1697`).

Each post_arm_k chain step runs as expected:
- `step_0` (Middle) binds `r1 = args_ptr[0] = 7`, lowers next_arg = 11,
  packs the closure record with prior_bindings = [r1=7], dispatches
  step_1 via the same loaded k_pair (still identity).
- `step_1` (Final) binds `r2 = args_ptr[0] = 11`, lowers tail = `r1*100
  + r2 = 7*100 + 11 = 711`, returns `Done(711)`.

`sigil_run_loop` returns 711 to the perform site. Helper's `x = 711`.
Helper continues the body in straight-line code: `perform IO.println(int
_to_string(711))` prints `711`, returns 711*1000 = 711000.

Per-resume body execution is **never** wired up because the body never
becomes a synth-cont chain. The arm's chain steps drive themselves
recursively via post_arm_k — but each k(arg_i) just hops through identity
back into the next chain step, never re-entering the body.

### Why probe 5 (pure-tail) appears correct

Probe 5 helper: `let x = perform Choose; x*1000`. Body shape matches
`is_simple_chained_let_yield_then_pure_tail_body` (one `Stmt::Let` with
pure perform args, pure tail). This is **Cps ABI**.

In Cps ABI the helper has a synth-cont (`ChainedLetBindStep` Final)
emitting per-resume. Each `k(arg_i)` from the arm dispatches this synth-
cont with `args_ptr[0] = arg_i`; the synth-cont binds `x = arg_i`,
lowers `x*1000`, returns `Done(arg_i * 1000)`. Per-resume execution is
correct: `r_1 = 7000`, `r_2 = 11000`, combine = 711000.

Probe 8 (Cps ABI + non-homogeneous tail `x*1000 + 5`) confirms: output
711505, exactly the algebraically correct value.

## Affected programs

Any helper with multi-shot perform sites whose body falls into Sync ABI:

- Body has `Stmt::Perform` mid-body (no let-binding) — pre-perform or
  post-perform tail with discardable side effects.
- Body has `Stmt::Let { value = Expr::Perform with non-pure args }` —
  `int_to_string`, builtin calls, user fn calls, etc. in perform args.
- Body has tail that's not a perform / IntLit / If-with-pure-or-cps-call
  branches / Match.

The pilot's P19 (State threading via lambda-of-state) and P20 (Choose
pair enumeration) both have helpers in this category. The existing
example suite (`multishot_stress`, `multishot_perf`, `choose_demo`,
`state.sigil`) all happen to use body shapes that match a Cps-ABI
classifier; no existing example exercises this gap.

## Proposed fix shape (input to Phase 2)

Two complementary directions:

### Option A — broaden the Cps-ABI body classifiers (preferred)

Two extensions:

1. **Accept `Stmt::Perform` as a chain step** in `is_simple_chained_let_
   yield_then_pure_tail_body` (codegen.rs:27820) and the corresponding
   emit pass (codegen.rs:9219+). A bare `Stmt::Perform` is a chain step
   with no binding (its result is discarded — `args_ptr[0]` ignored at
   step entry, semantics matches `Stmt::Perform` proper). Existing
   `CpsContinuationKind::ChainedLetBindStep` handles it cleanly: skip
   the binding-bind in the synth-cont's body and proceed to either the
   next step's perform dispatch (Middle) or the tail emit (Final).

2. **ANF-lift impure perform args** at body-shape classification time.
   The classifier currently rejects `perform IO.println(int_to_string
   (x))` because `int_to_string(x)` isn't `expr_is_pure`. A pre-pass
   AST rewrite can hoist impure args into preceding pure-trailing-lets:

   ```
   let x = perform Choose.choose(seed);     // chain step 1
   let _s: String = int_to_string(x);       // tail-prefix-let
   perform IO.println(_s);                  // chain step 2 (pure args now)
   x * 1000
   ```

   The existing `tail_prefix_lets` machinery (codegen.rs:9233+) already
   handles non-yielding intermediate lets between the last yield and the
   pure tail — but it expects the last yield to come first, not interleaved.
   We'd need to sequence: chain steps THEN tail-prefix-lets, instead of
   chain-prefix-lets-between-steps. This may require a different
   normalization shape — TBD by Phase 2.

   Alternative: hoist into the perform's let-binding directly:
   ```
   let _arg: String = int_to_string(x);
   let _u: Unit = perform IO.println(_arg);
   ```
   `_arg` is a pure-trailing-let computed *between* two yields. The
   classifier currently allows pure-trailing-lets only AFTER the last
   yield (`seen_pure_after_yield` gate at codegen.rs:27837+). Lifting
   that gate to allow inter-step pure lets is a localized classifier +
   emit-side change.

Phase 2 picks the concrete shape after a brief implementation spike.

### Option B — Sync ABI multi-shot rejection (rejected by plan)

The plan explicitly forbids reject-fallback. Documenting for completeness:
codegen could reject `UserFnAbi::Sync` helpers whose effects include any
`resumes: many` declaration with a clear error. This avoids silent
miscompiles but doesn't fix the underlying problem. Plan A's scope is
the runtime fix (Option A).

### Considerations carried into Phase 2

- **Closure-record bitmap caps.** The chained-let-yield Cps ABI has a
  cap of `K + N + 1 < MAX_CLOSURE_ENV_SLOTS = 31` slots
  (codegen.rs:364). Stmt::Perform chain steps don't add a binding, so
  they raise N (chain length) without raising K (captures). Emit-side
  the binding-loading code needs a "no binding" branch.

- **Pre-perform side-effects (no let)** — a separate code path; the
  classifier today enforces "every Stmt is Stmt::Let with perform-rhs."
  Lifting that to "Stmt::Perform OK as a chain step" generalizes both
  pre- and post-perform stmts.

- **Multi-handle interactions.** Probe 7 attempted nested `with { ... }`
  clauses on a single handle — the parser rejected the syntax. Multi-
  effect helpers route each effect through its own handle expression.
  Phase 2 should sanity-check that a fix doesn't regress nested-handle
  unwind semantics.

- **The fix lifts a real correctness gap, not just the pilot's symptom.**
  Sync-ABI multi-shot helpers are silently miscompiled today. A
  homogeneity audit of every existing multi-shot helper / test case is
  cheap insurance: probe 9 demonstrates the bug surfaces with `+ 5` on
  a tail that previously looked correct under `* 1000`.

## Surprises / scope-relevant notes

1. The design doc's four candidate root causes (c1/c2/c3/c4) are all
   internal to the helper synth-cont chain. The actual cause sits one
   level up — at ABI selection. None of the candidates fit the
   evidence as stated. Plan A's Phase 2 scope is therefore broader
   than the design doc's likely-fix-shapes section anticipated.

2. The "pure return values are correct per resume" framing in the
   design doc Background section is misleading. They're correct
   *numerically* in the pilot's reproducer because the body tail is
   `x*1000`, which is homogeneous in `x` and trivially commutes with
   the linear combine. Probe 9 disproves the framing for non-
   homogeneous tails. The spec's per-resume semantics are violated in
   both pure and effectful paths in Sync ABI; the pilot only saw the
   effectful violation because it shows up as a wrong observable side-
   effect rather than a wrong numeric value.

3. `multishot_stress.sigil`, `multishot_perf.sigil`, `choose_demo.sigil`,
   and `state.sigil` are all in-scope under Phase 2's "no regression"
   gate. None exercise this bug today, but Phase 2 should not weaken
   their classifier acceptance — they all rely on the chained-let-
   yield path being fast.

4. The runtime intrinsic `sigil_continuation_identity` already has a
   `args_len == 3` dispatch path (runtime/handlers.rs:1686+) that
   forwards through a non-null, non-self post_arm_k_fn. The arm's
   N-let chain dispatches via this path. The chain itself is correct —
   the bug is that the chain-result becomes the helper's return value
   instead of one resume's `r_i`.

## Files touched

- `compiler/docs/multi-shot-tail-anomaly.md` (this file).

No code changes in Phase 1. Phase 2 lands the fix.

## Phase 1 checkpoint

The plan calls for stopping at Phase 1's end and asking the user to
review before Phase 2 begins. The user may choose to re-scope Plan A
based on this finding. Specifically:

- Plan A's Background section reads as "fix the per-resume execution
  of the body's effectful tail." The diagnosis broadens this to "fix
  the ABI fallback that miscompiles all Sync-ABI multi-shot helpers."
  Same fix shape (broaden Cps-ABI classifiers), broader scope of
  affected programs.
- The fix touches `compute_user_fn_abi` and the chained-let-yield
  classifier + emit pass. No runtime changes (NextStep arena,
  OUTER_POST_ARM_K_STACK, sigil_continuation_identity all unchanged).
  This is a codegen-only fix, which the plan said would have a smaller
  risk profile.
- Phase 2's specific tasks should now be specified concretely:
  1. Extend `is_simple_chained_let_yield_then_pure_tail_body` to accept
     `Stmt::Perform` as a chain step (no binding).
  2. Extend the chain-step emit pass (helper-body emit at codegen.rs:9219+
     and Middle/Final synth-cont emit at codegen.rs:14127+) to handle
     no-binding chain steps.
  3. Decide ANF-lifting policy for impure perform args (separate emit-
     side concern; could be a pre-classification AST rewrite).
  4. Phase 3 test coverage is unchanged (the four positive tests + one
     no-regression test from the plan still pin the behavior correctly).

Phase 2 plan task language to be appended to the plan body before
implementation.
