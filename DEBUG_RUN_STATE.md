# Debug session — `run_state` canonical-shape runtime chain bug

Plan B' Stage 6.8 Task 109 first-cycle attempt at the literal canonical
`run_state(initial, comp)` higher-order helper compiled cleanly but at
runtime returned a closure-record-pointer-shaped value instead of the
threaded integer. See `[DEVIATION Task 109] run_state canonical shape
— runtime chain integration gap` in `PLAN_B_PRIME_DEVIATIONS.md` for
the layered failure analysis.

This doc is a self-contained debug-session prep:

1. Three minimal layered sources (A, B, C) — one per failure layer.
2. Exact build + run commands.
3. Expected outputs vs anticipated-failure outputs.
4. Suspect-site map keyed by which source's failure implicates each.
5. CLIF-dump instructions (one-line patch) for when narrowing requires it.

---

## 1 · Build

```
cd /repos/sigil
cargo build -p sigil-compiler            # produces ./target/debug/sigil
```

(Requires Cranelift; use whatever build host normally compiles the
sigil compiler — pod can't run it without OOM.)

---

## 2 · Three minimal sources

Save each to a fresh tmp file. Each source isolates exactly one of the
failure layers identified in the deviation entry.

### Source A — Layer 1: handle returns a fn-typed value, lambda invoked

**Tests**: arm allocates a constant-shape lambda; handle's overall
result IS that lambda; lambda invoked at top level. NO k-capture, NO
recursive dispatch.

```sigil
// /tmp/dbg_a.sigil
effect Trigger { fire: () -> Int }

fn main() -> Int ![IO] {
  let f: (Int) -> Int ![] = handle {
    let _: Int = perform Trigger.fire();
    fn (x: Int) -> Int ![] => x
  } with {
    Trigger.fire(k) => fn (x: Int) -> Int ![] => x + 100,
  };
  let n: Int = f(7);
  perform IO.println(int_to_string(n));
  0
}
```

Body performs `Trigger.fire`; arm fires; arm body is the lambda
`fn (x) => x + 100`; handle's overall value = that lambda; `f(7)`
invokes via closure-calling-convention.

**Expected stdout**: `107\n` (= 7 + 100).
**Anticipated failure if layer 1 is broken**: large pointer-shaped
integer (similar to the original 94846082251584 from CI), or a panic.

### Source B — Layer 2: k-capturing lambda invoked, k called once

**Tests**: arm body let-binds a k-capturing lambda, invokes it inside
the arm body. Handle's type is Int (NOT a fn), so we don't compound
with layer 1.

```sigil
// /tmp/dbg_b.sigil
effect Trigger resumes: many { fire: () -> Int }

fn main() -> Int ![IO] {
  let r: Int = handle (perform Trigger.fire()) with {
    Trigger.fire(k) => {
      let lam: (Int) -> Int ![] = fn (x: Int) -> Int ![] => k(x);
      lam(7)
    },
  };
  perform IO.println(int_to_string(r));
  0
}
```

Body performs; arm fires. Arm body: build a lambda capturing k, invoke
it with x=7. The lambda's body calls k(7), which resumes the
suspended computation with 7 as the result of `perform Trigger.fire()`.
The body completes with 7. Handle returns 7 (no return arm; body's
value).

**Expected stdout**: `7\n`.
**Anticipated failure if layer 2 is broken**: handle returns wrong
value (could be the lambda pointer, k_closure pointer, or 0); or
runtime crash.

### Source C — Layer 3: recursive `k(s)(s)` chain, single op

**Tests**: combination of layers 1 + 2 + 3 with only one op. If A and
B both pass but C fails, the bug is the recursive `k(s)(s)` shape (the
outer Call dispatch on a fn-typed value returned from k).

```sigil
// /tmp/dbg_c.sigil
effect Trigger resumes: many { fire: () -> Int }

fn main() -> Int ![IO] {
  let f: (Int) -> Int ![] = handle {
    let _: Int = perform Trigger.fire();
    fn (x: Int) -> Int ![] => x
  } with {
    Trigger.fire(k) => fn (x: Int) -> Int ![] => k(x)(x),
  };
  let n: Int = f(7);
  perform IO.println(int_to_string(n));
  0
}
```

Trace:

- Body performs Trigger.fire. Arm fires; returns L1 = `fn (x) => k(x)(x)`.
- Handle's value = L1. `f = L1`.
- `f(7)` invokes L1 with x=7. Body: `k(7)(7)`.
- `k(7)` resumes body with 7. Body's `let _ = 7; fn (x) => x` returns
  `fn (x) => x` (call it Lid).
- `k(7)` returns Lid (no return arm; body completes with Lid).
- L1 body's `(k(7))(7)` = `Lid(7)` = 7.
- `f(7) = 7`. Prints `7\n`.

**Expected stdout**: `7\n`.
**Anticipated failure if layer 3 is broken**: pointer-shaped integer
(typical run_state failure shape).

---

## 3 · Run commands

For each source, build a binary and run:

```
sig=/repos/sigil/target/debug/sigil

for f in /tmp/dbg_a.sigil /tmp/dbg_b.sigil /tmp/dbg_c.sigil; do
  out="${f%.sigil}.bin"
  echo "=== $f ==="
  $sig "$f" -o "$out" --human-errors && "$out"
  echo "exit=$?"
done
```

Record stdout + exit per source.

---

## 4 · Decision tree

| A     | B     | C     | Diagnosis                                                                                                               |
|-------|-------|-------|-------------------------------------------------------------------------------------------------------------------------|
| FAIL  | —     | —     | **Layer 1**: handle-returns-fn-value broken. Bug in handle-expression result lowering or `local_fn_types` registration. |
| OK    | FAIL  | —     | **Layer 2**: k-capturing lambda dispatch broken. Bug in trailing-pair `sigil_next_step_call` path or `lower_k_pair_call`. |
| OK    | OK    | FAIL  | **Layer 3**: recursive `k(s)(s)` outer-call dispatch broken. Bug in `call_callee_tys` for k as outer callee.            |
| OK    | OK    | OK    | run_state itself works for this shape; the original bug must be specific to multi-arm or State-effect interaction.      |

If A fails, jump to suspect-site set §5.1.
If B fails (after A passes), jump to §5.2.
If C fails (after A + B pass), jump to §5.3.

---

## 5 · Suspect-site map

### 5.1 · Layer 1 sites

**Symptom**: `f(7)` returns a pointer-shaped integer instead of the
arm-body lambda's evaluated result.

| File                              | Lines       | Why                                                                                                                                                                                                                          |
|-----------------------------------|-------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `compiler/src/codegen.rs`         | 8745–8751   | `local_fn_types` populated only when `l.ty` is `TypeExpr::Fn`. If post-elaborate / monomorphize loses the annotation on `f`, the catchall in lower_call falls into `unreachable!()` (line 10032). Verify `l.ty` is intact at this point. |
| `compiler/src/codegen.rs`         | ~621, ~870, ~1025, ~1638 | `Expr::Handle` lowering arms — there are 4+ sites. Confirm whichever arm is taken for a fn-typed handle correctly returns the arm's value as the handle's overall SSA result (not a stale phi or wrong block param).         |
| `compiler/src/typecheck.rs`       | handle expr | Handle's `overall_ty` is unified across body, op-arm bodies, and return-arm body. For fn-typed cases, ensure no `Ty::Var` sneaks past unification into the let's type annotation.                                            |
| `compiler/src/elaborate.rs`       | let stmt    | If elaborate strips the `TypeExpr::Fn` annotation when the RHS is a Handle, `local_fn_types` won't populate.                                                                                                                 |
| `compiler/src/closure_convert.rs` | rewrite_let | Confirm the let-binding's type annotation survives closure_convert (it generally does — but verify for the Handle-RHS shape specifically).                                                                                  |

**Fast-confirm**: insert one-line `eprintln!("local_fn_types after let f: {:?}", self.local_fn_types);` after line 8751 and rerun Source A. If `f` is missing, that's the upstream gap.

**Cross-check**: The existing `fn_as_value_via_let_binding_returns_42`
test passes (`let f: (Int) -> Int ![] = double; f(21)`). If Source A
fails, the difference is the RHS being a Handle expression vs an Ident.

### 5.2 · Layer 2 sites

**Symptom**: Source B's lambda invocation dispatches incorrectly; k(x) yields wrong value or crashes.

| File                              | Lines           | Why                                                                                                                                                                                                                          |
|-----------------------------------|-----------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `compiler/src/codegen.rs`         | `lower_k_pair_call` | Loads `k_closure` and `k_fn` from the synth fn's closure record trailing slots, calls `sigil_next_step_call(k_closure, k_fn, 1)`, drives `sigil_run_loop`, narrows. Verify the trailing-slot indices are correct for B's lambda layout. |
| `compiler/src/closure_convert.rs` | k-pair detect   | `arm_k_pair_captures` populated when an arm-body lambda captures the arm's `k_name`. For Source B the arm body has the lambda inside a block expression (let + invoke). Verify the k-pair-capture detection still fires.    |
| `compiler/src/codegen.rs`         | `lower_closure_record` | If `code_fn_name in arm_k_pair_captures`, allocates 2 trailing slots for k_closure + k_fn. Verify both slots are written with the correct values at allocation time.                                                          |
| `runtime/src/effect.rs`           | `sigil_next_step_call`, `sigil_run_loop` | Confirm the runtime ABI matches what codegen emits; existing tests cover top-level perform + arm but not lambda-invoked-k.                                                                                                  |

**Fast-confirm**: Add `eprintln!("ENTERING lambda body, k_closure={:p}, k_fn={:p}", k_closure_v, k_fn_v);` (using whatever debug emit path is convenient) at the start of `lower_k_pair_call`, then run Source B. If it never prints, the lambda isn't being invoked. If it prints with null/garbage pointers, the trailing slots aren't being populated.

### 5.3 · Layer 3 sites

**Symptom**: A and B pass; C produces a pointer instead of an int.

| File                              | Lines       | Why                                                                                                                                                                                                                          |
|-----------------------------------|-------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `compiler/src/codegen.rs`         | 10006–10027 | `lower_call`'s catchall picks `CalleeSig` based on callee shape. For `k(x)(x)`, the OUTER call's callee is `Call(k, [x])` — should hit `Expr::Call` branch and look up `call_callee_tys[outer_call_span]`. Verify the side-table is populated for `k(x)`'s span. |
| `compiler/src/typecheck.rs`       | call typecheck | `call_callee_tys` is populated when typecheck infers a Call's return type as `Ty::Fn`. Verify k's overall type was inferred as `Int -> ((Int) -> Int ![])` (NOT `Int -> Int`), so `k(x)`'s return is `Ty::Fn`.              |
| `compiler/src/typecheck.rs`       | k's type construction | k's type is the handle's CPS continuation: `op_resume_ty -> handler_overall_ty`. For a fn-valued handler_overall_ty, ensure that fn type propagates into k's recorded Ty without being collapsed.                            |
| `compiler/src/monomorphize.rs`    | end-of-mono deref pass | `call_callee_tys` should be deref'd at end-of-typecheck so codegen sees concrete Ty::Fn. Confirm the deref pass covers `call_callee_tys` (it covers `lambda_captures`; check this side-table too).                          |

**Fast-confirm**: Insert `eprintln!("call_callee_tys: {:?}", self.call_callee_tys);` at Lowerer construction, run Source C, look for an entry whose Ty is `Fn(...)` matching `k(x)`'s span. If absent, typecheck didn't record k's fn-typed return.

---

## 6 · CLIF dump (one-line patch)

If the bisect narrows to a specific function and you want to see the
emitted Cranelift IR before `define_function`, add a single
`eprintln!` immediately before each `module.define_function` call:

```rust
eprintln!("=== CLIF for {:?} ===\n{}", ctx.func.name, ctx.func.display());
```

Sites where this matters most (line numbers from current codegen.rs):

| Line | What's defined                                          |
|------|---------------------------------------------------------|
| 5792 | top-level user fn's body                                |
| 5913 | top-level user fn's body (alt path)                     |
| 6107 | `main` entry shim                                       |
| 6766 | synth CPS arm fn                                        |
| 7290 | post-arm-k chain synth fn                               |

For run_state debugging, the most useful are 5792 (lifted lambda body)
and 6107 (main). Filter the eprintln by name to keep output tractable:

```rust
let name = format!("{:?}", ctx.func.name);
if name.contains("lambda") || name.contains("main") {
    eprintln!("=== CLIF {} ===\n{}", name, ctx.func.display());
}
```

Then build, run Source A/B/C, and tee stderr to a file:

```
$sig /tmp/dbg_a.sigil -o /tmp/dbg_a.bin --human-errors 2>/tmp/clif_a.txt
```

The CLIF text is verbose but readable; key things to look for in a
fn-typed-handle context:

- Block params at the synth lambda's entry should match
  `(closure_ptr: i64, x: i64) -> i64`.
- The arm fn's body should contain a `call sigil_next_step_done(...)`
  with the **lambda pointer** (the closure record header address)
  as the value — not a tagged int.
- `f(7)` in main should lower to `load.i64 +8(state_fn_ptr)` followed
  by `call_indirect` against that loaded code_ptr — not a direct
  call with the wrong arity, and not a load-and-return without the
  call.

---

## 7 · Reporting back

Once you've run A/B/C, paste back:

1. Stdout + exit per source.
2. If any failed: the specific E-code/panic/runtime-output.
3. If you ran the suspect-site fast-confirm `eprintln!`s, paste their
   relevant output.
4. If you went to CLIF dump, the relevant function bodies (the lifted
   lambda and main are usually enough).

The decision tree in §4 plus the `eprintln!` outputs will narrow which
of the 14 candidate sites in §5 is the actual bug location. From
there, the fix is targeted enough that we can ship it with high
confidence and re-attempt the literal `run_state` shape in a follow-up
Task 109 fixup commit.
