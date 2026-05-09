# Cps-call-as-tail runtime fix — spike findings

## Status: shipped (PR #123, branch `cps-call-as-tail-runtime`)

The fix is a one-line semantic correction in
`lower_call_in_tail_pos`'s Cps→Cps branch: only drop the OPAK entry
when the callee is the same user fn as the enclosing one (recursive
TCO). Non-recursive multi-shot Cps-call-as-tail was dropping per
resume against a single push, underflowing OPAK by N-1 entries and
collapsing per-resume composition.

## Diagnosis (2026-05-09)

The bug surfaces only at chain >= 2 multi-shot:

| Shape | chain | multi-shot | Pre-fix |
|---|---|---|---|
| Pure tail (e.g. `a*100+b`) | 2 | yes | ✓ |
| Cps-call-as-tail | 1 | yes | ✓ |
| Cps-call-as-tail (`report(a, b)`) | 2 | yes | ✗ outer k(2) doesn't fire |
| `match cond { _ => report(a,b) }` | 2 | yes | ✗ inner k(2) doesn't fire |

Both failing shapes lower to a Cps→Cps tail call (`NextStep::Call`
TCO'd in `lower_call_in_tail_pos`). For non-recursive callees,
`chain_outer_post_arm_k_pushes > 0` but the OPAK entry was pushed
once at chain entry and would underflow on subsequent resumes.

## Fix

Gate the drop on `is_recursive`:

```rust
let callee_func_id = self.user_fns.get(name).map(|e| e.func_id.as_u32());
let enclosing_id = self.enclosing_user_fn_id.or_else(|| {
    if let UserFuncName::User(u) = &self.builder.func.name {
        Some(u.index)
    } else { None }
});
let is_recursive = matches!((enclosing_id, callee_func_id),
                            (Some(a), Some(b)) if a == b);
if is_recursive && self.chain_outer_post_arm_k_pushes > 0 {
    // emit drop
}
```

Synth-cont `Lowerer` construction sets `enclosing_user_fn_id` from
`user_fns[parent_fn_name].func_id` so the recursion check works
inside chain-step closures.

## Test surface

`compiler/tests/e2e.rs::cps_call_as_tail_in_multi_shot_runtime_correct`
asserts the helper-hoist shape compiles, runs, and emits all six
expected pairs (16, 25, 34, 43, 52, 61). The
`tail_recursive_cps_colored_under_nested_handlers` test continues to
pass — verifying the recursion gate hasn't regressed TCO'd
self-recursion (10M-iteration depth still tracks correctly).
