# Cps-call-as-tail diagnosis

## Bug shape

Multi-shot handler bodies with 2+ performs and a CPS-colored function
call as the tail expression silently miscompile. Only the first
resume's side effects fire; subsequent resumes skip the helper's
performs entirely. The `is_simple_chained_let_yield_then_pure_tail_body`
classifier accepts the tail because `!expr_contains_perform(tail)` is
true (the CPS call doesn't textually contain a perform — it's
CPS-colored due to transitive effects). The emit path then treats the
tail as a pure expression, which doesn't set up per-resume
continuation threading for the CPS callee.

## Classifier site

`is_simple_chained_let_yield_then_pure_tail_body` at
codegen.rs:28935. The tail check at line ~29063-29065:

```rust
match &body.tail {
    Some(t) if !expr_contains_perform(t) => Some(yield_count),
    _ => None,
}
```

The `!expr_contains_perform(t)` check is the root cause. A CPS call
like `report(a, b)` where `report` performs IO passes this check
because the Call expression doesn't textually contain `Expr::Perform`.

## Reproducer

```sigil
fn report(a: Int, b: Int) -> Int ![IO] {
  match a + b == 7 {
    true => { perform IO.println(int_to_string(a * 10 + b)); 0 },
    false => 0,
  }
}

fn pairs() -> Int ![Choose, IO] {
  let a: Int = perform Choose.pick(1, 6);
  let b: Int = perform Choose.pick(1, 6);
  report(a, b)    // CPS-call-as-tail — miscompiles
}
```

Observed: `16\n` (one line). Expected: `16\n25\n34\n43\n52\n61\n`
(six lines). Inlining the match body into pairs() produces correct
output.

## Sub-shape bisection

| Shape | yield_count | multi-shot | CPS tail | Observed | Works? |
|-------|-------------|------------|----------|----------|--------|
| 2 performs + CPS helper (IO) | 2 | yes | yes | `16\n` (1 line) | **NO** |
| 1 perform + CPS helper (IO) | 1 | yes | yes | `1\n2\n3\n12\n` | yes |
| 2 performs + Sync helper (pure) | 2 | yes | no | `198\n` (correct sum) | yes |
| 1 perform + CPS helper, single-shot | 1 | no | yes | `15\n30\n` | yes |

**Rejection predicate:** multi-shot effect in scope + yield_count >= 2
+ tail is Expr::Call to CPS-colored callee.

The 1-perform case works because there's only one chain step; the
tail fires once per resume correctly. The 2+-perform case breaks
because the chain machinery's per-resume fork happens at each
chain step, but the CPS-call-as-tail is emitted as a pure tail
(no per-resume continuation threading).

## Runtime fix estimate

The fix would involve extending the chained-let-yield classifier to
recognize CPS-call-as-tail and emit a CpsCall chain step (similar to
how `is_let_yield_prefix_then_branched_cps_tail_body` handles
branched CPS tails). This requires:

1. New chain step kind (CpsCallTail) or reusing CpsCall.
2. Emit site: after the last chain step completes, build a
   NextStep::Call with the CPS callee + caller_k_pair forwarded.
3. Captures: the chain's final closure record needs to capture
   the callee's args.

This is NOT a one-line fix — it touches the classifier, chain
allocation, and emit. Recommend rejection first, runtime fix as
follow-on.
