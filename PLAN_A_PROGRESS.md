# Plan A — Multi-shot post-perform-tail correctness — Progress

Tracks deviations and validation outcomes for
`in-progress/2026-05-08-sigil-multi-shot-tail-correctness.md` (Plan A).
Companion to `PLAN_A_DEVIATIONS.md` (which logs prospective re-scope
decisions ahead of commits) and `compiler/docs/multi-shot-tail-anomaly
.md` (Phase 1 diagnosis).

## Phase 3 Task 9 — Validation rerun on pilot subset

The plan asks for a rerun of the 9-prompt fresh-session pilot from
2026-05-08, with two specific gates:

1. P20 must pass first-compile + first-run with stdout `16\n25\n34\n
   43\n52\n61\n` exit 0.
2. P19 with the `resumes: many` patch must produce `5\n` exit 0 at
   runtime (it still fails first-compile until Plan B lifts the
   escape barrier).

Both gates as written are TIGHTER than what Plan A's codegen-only fix
delivers. Plan A's body classifier surface covers the chained-let-yield
shape with mid-body `Stmt::Perform` normalization and elaborator-ANF
inlining — it does NOT cover all shapes the LLM-written prompts use.
The validation rerun documents the gap so the PR review can land
the correct framing.

### P20 (Choose pair enumeration)

**Literal LLM-written shape** (`spec/validation-prompts.md` §P20):

```sigil
fn pairs() -> Int ![Choose, IO] {
  let a: Int = perform Choose.pick(1, 6);
  let b: Int = perform Choose.pick(1, 6);
  if a + b == 7 {
    perform IO.println(int_to_string(a * 10 + b));
    0
  } else {
    0
  }
}
```

**Outcome with Plan A:** compiles, runs, **empty stdout**, exit 0.

The body's tail is an `If` whose `then`-branch contains a `Stmt::Perform
(IO.println)`. After elaborator desugaring, the tail becomes `Match
{ true => Block { Stmt::Perform; IntLit(0) }, false => IntLit(0) }`.
This shape fails every Cps-ABI body classifier in
`compute_user_fn_abi`:

- `is_simple_chained_let_yield_then_pure_tail_body` requires the tail
  to be perform-free (`!expr_contains_perform(tail)`); the
  perform-bearing match arm fails.
- `is_let_yield_prefix_then_branched_cps_tail_body` requires
  branches to be either pure or a single direct call to a Cps user
  fn; a perform-bearing block isn't either.
- The other classifiers don't match the let-yield-prefix shape.

`pairs()` therefore falls back to `UserFnAbi::Sync`, where multi-shot
is structurally broken. The P20 stdout is empty because the body's
post-perform IO never runs. **This is the same root cause Plan A
diagnosed; Plan A's fix targets a different sub-shape (mid-body
`Stmt::Perform` in chained-let-yield bodies) that does not cover this
case.**

The fix shape that would close this gap: hoist if/match-tail branches
that contain performs into synthesized Cps user fns (so the resulting
body matches `is_let_yield_prefix_then_branched_cps_tail_body`'s
"single Cps call" branch rule). This requires synthesizing top-level
fn declarations at codegen time and routing them through the
typecheck / monomorphize / closure-conversion / color-inference
pipeline — a moderate lift that a follow-on plan should handle. Out
of Plan A's "no expansion beyond multi-shot bodies" scope guardrail
(plan body, "Scope guardrails (hard rules)").

**Reduced shape that DOES pass** (e2e test
`multi_shot_choose_pair_enumeration` in `compiler/tests/e2e.rs`):
2x2 enumeration over two distinct multi-shot effects (`OuterC` ×
`InnerC`) with the IO println in the body's chained-let-yield tail
position (no if/match in the tail). Oracle `12\n15\n62\n65\n`. This
demonstrates Plan A's per-resume body execution under nested multi-
shot handlers with the body shape Plan A's fix supports.

**Verdict:** Plan A's fix is necessary but not sufficient for the
literal LLM-written P20. The reduced 2x2 e2e test pins the
mechanism Plan A targets; the literal P20 needs a follow-on plan
extending the classifier to if/match-tail-with-perform-branches.

### P19 (State threading via lambda-of-state)

**Literal LLM-written shape with `resumes: many` patch applied** at
`/tmp/sigil-plan-a/p19_resumes_many.sigil`:

```sigil
effect State resumes: many { get: () -> Int, set: (Int) -> Int }
type IntList = | Nil | Cons(Int, IntList)
fn count_elements(xs: IntList) -> Int ![State, IO] {
  match xs {
    Nil => 0,
    Cons(_, rest) => {
      let cur: Int = perform State.get();
      let _: Int = perform State.set(cur + 1);
      count_elements(rest)
    },
  }
}
fn run_state(initial: Int, comp: () -> Int ![State, IO]) -> Int ![IO] {
  let runner: (Int) -> Int ![IO] = handle comp() with {
    return(v) => fn (_s: Int) -> Int ![IO] => v,
    State.get(k) => fn (s: Int) -> Int ![IO] => k(s)(s),
    State.set(s2, k) => fn (_s: Int) -> Int ![IO] => k(s2)(s2),
  };
  runner(initial)
}
fn main() -> Int ![IO] {
  let xs: IntList = Cons(10, Cons(20, Cons(30, Cons(40, Cons(50, Nil)))));
  let final_count: Int = run_state(0, fn () -> Int ![State, IO] => count_elements(xs));
  perform IO.println(int_to_string(final_count));
  0
}
```

**Outcome with Plan A:** compiles, runs, prints `0\n`, exit 0
(expected: `5\n`).

The lambda-of-state encoding has handler arm bodies that are LAMBDAS
(`fn (s: Int) -> Int ![IO] => k(s)(s)`) — not the multi-let-arm-body
shape (`let r1 = k(arg1); ...; combine`) that Plan A and the existing
multi-shot machinery target. The arm-body classifier rejects lambdas;
the arm falls back to a non-multi-shot path that doesn't iterate `k`.

Plan A's fix targets HELPER bodies (the body of `count_elements` here),
not arm bodies. With `resumes: many` patched onto the State effect,
the helper's body classification gets per-resume execution support
from Plan A — but the arm body's lambda-of-state encoding requires
separate machinery (lifting the dynamic-extent restriction so
continuations can be reified into a lambda return value, per the
plan's cluster note that Plan B handles this).

**Verdict:** Plan A doesn't make P19 produce `5\n` even with the
`resumes: many` patch. The design doc's claim that "Plan A with
resumes: many patch" suffices is inconsistent with what Plan A's
diagnosis surfaced (the bug is at ABI selection for helpers; arm-
body lambdas are a separate issue). The runtime gap remains until
Plan B's first-class continuation work, OR until a separate plan
extends arm-body classification to lambdas-returning-from-arm.

P19 with prompt rewrite to cell-backed encoding (`run_state` allocates
a `Ref[Int]` and threads via arms that return Int) is what
`examples/state.sigil` already does and what Plan B' Stage 6.8
followup landed. That alternative spelling produces the correct `5`
without lambda-of-state encoding. The current `examples/state.sigil`
canonical `run_state(initial, comp)` shape is pinned by
`state_example_canonical_run_state_returns_11` in the e2e test
suite — that path is unaffected by Plan A.

## Validation rerun summary

| Prompt | Plan A outcome | Plan A's design-doc expectation | Match? |
|--------|---------------|--------------------------------|--------|
| P20 literal | empty stdout | `16\n25\n34\n43\n52\n61\n` | NO — body shape outside Plan A |
| P20 reduced (2x2) | `12\n15\n62\n65\n` | (n/a — not the literal prompt) | YES, demonstrates mechanism |
| P19 with `resumes: many` patch | `0\n` | `5\n` | NO — arm-body lambda outside Plan A |

## Recommendation

1. Plan A lands the codegen fix as committed. The fix is real and
   closes a class of multi-shot helper-body bugs (Sync ABI fallback);
   the e2e test surface and `compiler/docs/multi-shot-tail-anomaly.md`
   are the canonical record of what Plan A delivers.
2. Spec §8.3 documents the per-resume semantics and the v1 body-shape
   eligibility surface. Bodies outside that surface fall back to the
   non-Cps lowering (Plan A's PR text notes this explicitly).
3. A follow-on plan ("Plan A2"?) should:
   - Extend the body classifier to support if/match-tail with
     perform-bearing branches (needed for literal P20).
   - Extend arm-body classification to lambda-of-state encoding
     (needed for literal P19; may overlap with Plan B's escape
     barrier lift).
4. The existing `examples/state.sigil` cell-backed-run_state shape is
   the recommended encoding for State threading until lambda-of-state
   lands; the spec validation prompt bank should be updated with
   guidance accordingly (Plan C Stage 7's spec-validation script
   work is the natural place).

## File pointers

- Plan body: `/Users/boldfield/projects/designs/in-progress/2026-05-08-sigil-multi-shot-tail-correctness.md`
- Design doc: `/Users/boldfield/projects/designs/docs/plans/2026-05-08-sigil-multi-shot-tail-correctness-design.md`
- Phase 1 diagnosis: `compiler/docs/multi-shot-tail-anomaly.md`
- Deviations: `PLAN_A_DEVIATIONS.md`
- Validation prompts: `spec/validation-prompts.md` §P19, §P20
- E2E test surface: `compiler/tests/e2e.rs::multi_shot_*` (5 tests added in `[Plan A Phase 3]`)
- Spec update: `spec/language.md` §8.3 (per-resume semantics)
- Repro / probe sources: `/tmp/sigil-plan-a/*.sigil` (9 probes documented in `compiler/docs/multi-shot-tail-anomaly.md` Bisection table)
