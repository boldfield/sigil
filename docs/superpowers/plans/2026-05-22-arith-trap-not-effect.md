# Arith-Trap (not Effect) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `/` and `%` trap on a zero divisor (clean stderr message + exit 2) instead of requiring `![ArithError]` on the enclosing function's effect row, so an LLM can write `n % 2` in a `![]` function and have it compile first-pass.

**Architecture:** Today `/` and `%` are wired into the effect system: typecheck auto-injects `![ArithError]` (`typecheck.rs:6516`), elaborate rewrites each site to `if rhs == 0 { perform ArithError.{div,mod}_by_zero() } else { SdivUnchecked/SremUnchecked }` (`elaborate.rs:197`), and the perform dispatches to a runtime default arm that exits 2. This change severs that wiring: typecheck stops injecting, elaborate stops rewriting, and codegen lowers `BinOp::Div`/`BinOp::Mod` *directly* to an inline zero-check that branches to a new direct runtime trap (`sigil_arith_div_by_zero_trap` / `sigil_arith_mod_by_zero_trap`) which prints the **same** banner and exits with the **same** code 2 — so the `div_by_zero.sigil` oracle is preserved byte-for-byte. The `ArithError` effect/enum, its runtime arm fns, and its always-installed default handler frame all **stay intact** — `ArithError` remains a first-class effect a user can `perform`/`handle` explicitly; operators just no longer auto-use it. Recovery from a zero divisor moves to two new pure stdlib helpers, `checked_div`/`checked_mod -> Result[Int, String]`.

**Tech Stack:** Rust (compiler: typecheck/elaborate/Cranelift codegen; runtime: Boehm-GC'd C-ABI fns), Sigil stdlib (`.sigil`), e2e harness (`compiler/tests/e2e.rs`).

---

## Background the executor must know

- **Runtime crate gotcha (load-bearing):** `compiler/src/link.rs::locate_runtime_lib` prefers `target/release/libsigil_runtime.a` over `target/debug/...`. After editing `runtime/src/*.rs` you MUST run `cargo build --release -p sigil-runtime` or local e2e tests silently link the stale archive and you will chase phantom regressions. Do this in **every** task that touched runtime before running any e2e test.
- **e2e is slow (~3 min full).** For iteration, run a single test: `cargo test -p sigil-compiler --test e2e <test_name> -- --nocapture`. The fast "did I break something load-bearing" check is `./scripts/pod-verify.sh`.
- **e2e helpers** (already exist in `compiler/tests/e2e.rs`):
  - `compile_and_run(source: &str, test_name: &str) -> (String /*stdout*/, String /*stderr*/, i32 /*exit code*/)` — inline source.
  - `compile_file_and_run(source_path: &Path, test_name: &str) -> (String, String, i32)` — file on disk.
  - `repo_root()` returns the workspace root `PathBuf` (used as `root.join("examples/...")`).
- **No over-declared-effect diagnostic exists.** A function may declare `![ArithError]` and never use it; Sigil accepts it (inferred row ⊆ declared row subsumes). This is why removing the auto-injection does NOT break the composition/Nim e2e tests that carry `![ArithError]` in multi-effect rows — they keep compiling untouched. Verified by grep: there is no superfluous/unused-effect E-code. **Do NOT strip `![ArithError]` from those inline e2e test sources** (`task_78_5_nim_mini_perfect_strategy_alice_wins_seven` at e2e.rs:13025, and the Raise/State/IO+ArithError composition programs at e2e.rs:13178 and e2e.rs:19786) — they deliberately exercise ArithError as a multi-effect-row composition marker and the effect still exists.
- **`ArithError` stays.** Do not touch `BUILTIN_EFFECT_NAMES` (typecheck.rs:878), the runtime arm fns `sigil_arith_error_{div,mod}_by_zero_arm` (handlers.rs:1895/1919), the FFI decls for them (codegen.rs:9743), or the default ArithError handler-frame install (codegen.rs:~13613). The frame-shape assertion test (e2e.rs:546, "ArithError arm_count=2") must keep passing.

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `runtime/src/handlers.rs` | runtime trap fns | ADD two direct `extern "C" fn() -> !` traps + factor a shared exit helper; keep existing CPS arm fns |
| `compiler/src/codegen.rs` | Cranelift codegen | ADD trap FuncId/FuncRef plumbing + `TRAP_ARITH_UNREACHABLE` + shared `emit_guarded_div` helper; wire both Div/Mod lowering sites; remove dead `SdivUnchecked`/`SremUnchecked` arms |
| `compiler/src/elaborate.rs` | desugaring | REMOVE the Div/Mod → perform-bearing rewrite; remove unchecked-variant references |
| `compiler/src/typecheck.rs` | typecheck/effects | REMOVE the `register_effect_use("ArithError", …)` injection for Div/Mod; remove unchecked-variant arms |
| `compiler/src/ast.rs` | AST | REMOVE `BinOp::SdivUnchecked` / `BinOp::SremUnchecked` variants |
| `std/int.sigil` | stdlib | ADD `checked_div` / `checked_mod -> Result[Int, String]` |
| `examples/div_recover.sigil` | example | REWRITE around `checked_div` (handler-on-`/` recovery no longer possible) |
| `examples/arith.sigil`, `examples/div_by_zero.sigil` | examples | STRIP now-unneeded `![ArithError]` row + `use`/`import std.raise` |
| `compiler/tests/e2e.rs` | tests | UPDATE `mod_by_zero_traps`, `div_recover_example_returns_999`; ADD bare-`![]` trap + auto-CPS trap + checked_div/checked_mod tests |
| `spec/language.md` | spec | REWRITE §3.3 operator-effects note, §4.2 operator table + gotcha, §10/§13 mentions; document trap semantics + checked_div/checked_mod; keep ArithError as explicit-only effect |

---

## Task 1: Runtime direct-trap functions

**Files:**
- Modify: `runtime/src/handlers.rs:1928-1948` (the `arith_error_default_arm` helper region)

- [ ] **Step 1: Add the two direct-trap fns and factor the shared exit helper**

In `runtime/src/handlers.rs`, replace the existing `arith_error_default_arm` helper (currently at lines ~1928-1948) with a thin wrapper over a new shared `arith_trap_exit`, and add the two new direct-call traps. The existing CPS arm fns `sigil_arith_error_div_by_zero_arm` / `sigil_arith_error_mod_by_zero_arm` (lines 1895/1919) stay unchanged — they keep calling `arith_error_default_arm`.

```rust
/// Shared stderr-banner + exit for every arithmetic-error trap path.
/// Writes `"sigil: arithmetic error: <reason>\n"` to stderr and calls
/// `std::process::exit(2)`. Never returns.
fn arith_trap_exit(reason: &str) -> ! {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "sigil: arithmetic error: {reason}");
    let _ = stderr.flush();
    drop(stderr);
    std::process::exit(2);
}

/// Internal helper for the two `ArithError` *effect* default arm fns
/// (`sigil_arith_error_{div,mod}_by_zero_arm`). Kept for the explicit
/// `perform ArithError.*` path, which still flows through the CPS arm
/// fn ABI. `args_len` is debug-asserted to be 5 (zero user args +
/// (k_closure, k_fn) + (return_arm_closure, return_arm_fn,
/// return_arm_fired_ptr)). Caller (`sigil_perform`) guarantees it.
fn arith_error_default_arm(reason: &str, args_len: u32) -> ! {
    debug_assert!(
        args_len == 5,
        "sigil_arith_error_*_arm: args_len must be exactly 5 (zero user args + \
         (k_closure, k_fn) + (return_arm_closure, return_arm_fn, return_arm_fired_ptr)); got {args_len}"
    );
    arith_trap_exit(reason)
}

/// Direct trap for `/` by zero. Called inline from codegen's
/// `BinOp::Div` lowering (NOT through the effect system) — `/` no
/// longer performs `ArithError`. Preserves the Plan A2 stderr banner
/// + exit-2 behaviour verbatim so `examples/div_by_zero.sigil`'s
/// oracle is unchanged. Never returns; codegen emits a `trap`
/// terminator after the call.
#[no_mangle]
pub extern "C" fn sigil_arith_div_by_zero_trap() -> ! {
    arith_trap_exit("division by zero")
}

/// Direct trap for `%` by zero. Parallel of
/// `sigil_arith_div_by_zero_trap`; banner reads "remainder by zero".
#[no_mangle]
pub extern "C" fn sigil_arith_mod_by_zero_trap() -> ! {
    arith_trap_exit("remainder by zero")
}
```

- [ ] **Step 2: Build the runtime (debug + release) to verify it compiles and refresh the linked archive**

Run: `cargo build -p sigil-runtime && cargo build --release -p sigil-runtime`
Expected: both succeed, no warnings about the new fns. (Release rebuild is mandatory per the runtime-crate gotcha.)

- [ ] **Step 3: Commit**

```bash
git add runtime/src/handlers.rs
git commit -m "$(cat <<'EOF'
[arith-trap] runtime: direct div/mod-by-zero traps

Add sigil_arith_div_by_zero_trap / sigil_arith_mod_by_zero_trap,
called inline from codegen (not through the effect system). Both
reuse the existing stderr banner + exit-2 behaviour via a shared
arith_trap_exit helper, so div_by_zero.sigil's oracle is unchanged.
The ArithError-effect CPS arm fns are untouched.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Codegen — trap plumbing + shared guarded-div helper, wired at both lowering sites

This task adds the FuncId/FuncRef plumbing and the shared helper, and points both `BinOp::Div`/`BinOp::Mod` lowering sites at it. Elaborate STILL rewrites Div/Mod at this point, so the new arms are latent (not yet reached at runtime) — but they compile and the helper has callers, keeping the build green. Behaviour flips in Task 3.

**Files:**
- Modify: `compiler/src/codegen.rs` — `BuiltinFuncIds` struct + its construction; the per-fn `Builtins` struct + its construction (~30192); `AutoCpsExprCtx` struct (28994) + its 3 construction sites (28789, 12030, 20142); `emit_binop` (27670); `lower_auto_cps_simple_expr` (29110); the `TRAP_PANIC_UNREACHABLE` constant region (28763)

- [ ] **Step 1: Add the `TRAP_ARITH_UNREACHABLE` trap code constant**

Next to `const TRAP_PANIC_UNREACHABLE: u8 = 0x43;` (codegen.rs:28763), add:

```rust
/// User trap code emitted after the (noreturn) call to
/// `sigil_arith_{div,mod}_by_zero_trap` so the basic block has a
/// terminator. A distinct code from `TRAP_PANIC_UNREACHABLE` makes a
/// post-mortem trap signature unambiguously identify the arith-trap
/// path if the runtime fn ever returns instead of exiting.
const TRAP_ARITH_UNREACHABLE: u8 = 0x44;
```

- [ ] **Step 2: Declare the two trap FuncIds in module setup and store them in `BuiltinFuncIds`**

Find the `sigil_panic` declaration block (codegen.rs:9200-9204) and add a parallel block right after it. The signature has **no params and no returns** (noreturn — caller emits the trap terminator), mirroring `sigil_panic_sig` minus its one param:

```rust
    // arith traps: sigil_arith_div_by_zero_trap() -> ! and
    // sigil_arith_mod_by_zero_trap() -> !  (no params, no Cranelift
    // returns; the runtime fn exits the process — codegen emits a
    // `trap` after the call so the block has a terminator).
    let arith_trap_sig = Signature::new(isa_call_conv(&module));
    let arith_div_trap = module
        .declare_function("sigil_arith_div_by_zero_trap", Linkage::Import, &arith_trap_sig)
        .map_err(|e| format!("declare sigil_arith_div_by_zero_trap: {e}"))?;
    let arith_mod_trap = module
        .declare_function("sigil_arith_mod_by_zero_trap", Linkage::Import, &arith_trap_sig)
        .map_err(|e| format!("declare sigil_arith_mod_by_zero_trap: {e}"))?;
```

Add two fields to `BuiltinFuncIds` (the struct holding builtin FuncIds — find it via `grep -n "struct BuiltinFuncIds" compiler/src/codegen.rs`):

```rust
    arith_div_trap: cranelift_module::FuncId,
    arith_mod_trap: cranelift_module::FuncId,
```

Populate them where `BuiltinFuncIds` is constructed (the same place `sigil_panic`'s FuncId is folded into a struct — search for the struct literal that lists `sigil_panic,` near codegen.rs:10850):

```rust
    arith_div_trap,
    arith_mod_trap,
```

- [ ] **Step 3: Build to verify the FuncId plumbing compiles**

Run: `cargo build -p sigil-compiler 2>&1 | tail -20`
Expected: compiles. (Unused-field warnings on the two new `BuiltinFuncIds` fields are acceptable at this step — they get consumers below.)

- [ ] **Step 4: Add the shared `emit_guarded_div` free helper**

Add this free function near `emit_binop` (codegen.rs, just above the `fn emit_binop` definition at 27670). It is the single source of truth for guarded integer division/remainder, called from both lowering sites.

```rust
/// Emit a zero-guarded signed division (`is_div == true`) or
/// remainder (`is_div == false`). On a zero divisor, branches to a
/// cold block that calls the (noreturn) runtime trap and emits a
/// `trap` terminator; otherwise computes `sdiv`/`srem` in a fresh
/// block. Returns the quotient/remainder Value in the now-current
/// (ok) block. Shared by `Lowerer::emit_binop` and the auto-CPS
/// residual evaluator so the guard semantics can't drift between
/// the two paths.
fn emit_guarded_div(
    builder: &mut FunctionBuilder<'_>,
    l: Value,
    r: Value,
    is_div: bool,
    div_trap_ref: FuncRef,
    mod_trap_ref: FuncRef,
) -> Value {
    let int_ty = builder.func.dfg.value_type(r);
    let zero = builder.ins().iconst(int_ty, 0);
    let is_zero = builder.ins().icmp(IntCC::Equal, r, zero);
    let trap_block = builder.create_block();
    let ok_block = builder.create_block();
    builder.ins().brif(is_zero, trap_block, &[], ok_block, &[]);

    builder.switch_to_block(trap_block);
    builder.seal_block(trap_block);
    let trap_ref = if is_div { div_trap_ref } else { mod_trap_ref };
    builder.ins().call(trap_ref, &[]);
    builder
        .ins()
        .trap(TrapCode::unwrap_user(TRAP_ARITH_UNREACHABLE));

    builder.switch_to_block(ok_block);
    builder.seal_block(ok_block);
    if is_div {
        builder.ins().sdiv(l, r)
    } else {
        builder.ins().srem(l, r)
    }
}
```

- [ ] **Step 5: Add the trap FuncRefs to the per-fn `Builtins` struct and wire `emit_binop`**

Add two fields to the per-fn `Builtins` struct (the one holding `sigil_panic_ref: FuncRef` at codegen.rs:29904):

```rust
    arith_div_trap_ref: FuncRef,
    arith_mod_trap_ref: FuncRef,
```

Populate them where `sigil_panic_ref` is built via `declare_func_in_func` (codegen.rs:30192):

```rust
        arith_div_trap_ref: module.declare_func_in_func(ids.arith_div_trap, builder.func),
        arith_mod_trap_ref: module.declare_func_in_func(ids.arith_mod_trap, builder.func),
```

Replace the `BinOp::Div | BinOp::Mod => { … unreachable!(…) }` arm in `emit_binop` (codegen.rs:27676-27695) with a call to the helper:

```rust
            BinOp::Div | BinOp::Mod => emit_guarded_div(
                self.builder,
                l,
                r,
                matches!(op, BinOp::Div),
                self.builtins.arith_div_trap_ref,
                self.builtins.arith_mod_trap_ref,
            ),
```

- [ ] **Step 6: Add the trap FuncRefs to `AutoCpsExprCtx` and wire `lower_auto_cps_simple_expr`**

Add two fields to `AutoCpsExprCtx` (codegen.rs:28994):

```rust
    arith_div_trap_ref: FuncRef,
    arith_mod_trap_ref: FuncRef,
```

At each of the **three** `AutoCpsExprCtx { … }` construction sites (codegen.rs:28789, 12030, 20142), add the two refs. Each site already has the module + builder in scope and a `PerFnRefsCtx`/`BuiltinFuncIds` (`per_fn_refs_ctx.builtins` or equivalent) — declare the refs from the `BuiltinFuncIds` FuncIds:

```rust
            arith_div_trap_ref: module.declare_func_in_func(per_fn_refs_ctx.builtins.arith_div_trap, builder.func),
            arith_mod_trap_ref: module.declare_func_in_func(per_fn_refs_ctx.builtins.arith_mod_trap, builder.func),
```

> Note for executor: confirm the in-scope names at each of the three sites. At codegen.rs:28789 the surrounding fn takes `per_fn_refs_ctx: &PerFnRefsCtx<'_>` and a `module`/`builder`; the `alloc_ref` field on the same struct literal is built the same way (`module.declare_func_in_func(..., builder.func)`) — mirror exactly that call shape and module/builder bindings. At 12030 and 20142 the bindings may be named differently (e.g. `m`, `fb`); match whatever `alloc_ref:` uses at that same literal.

Replace the `BinOp::Div => …` / `BinOp::Mod => …` arms in `lower_auto_cps_simple_expr` (codegen.rs:29117-29118) with helper calls:

```rust
                BinOp::Div => emit_guarded_div(
                    builder, lhs_v, rhs_v, true, ctx.arith_div_trap_ref, ctx.arith_mod_trap_ref,
                ),
                BinOp::Mod => emit_guarded_div(
                    builder, lhs_v, rhs_v, false, ctx.arith_div_trap_ref, ctx.arith_mod_trap_ref,
                ),
```

- [ ] **Step 7: Build to verify everything compiles (behaviour still gated by elaborate)**

Run: `cargo build -p sigil-compiler 2>&1 | tail -20`
Expected: compiles cleanly, no unused-field/fn warnings (every new field + the helper now has a consumer).

- [ ] **Step 8: Commit**

```bash
git add compiler/src/codegen.rs
git commit -m "$(cat <<'EOF'
[arith-trap] codegen: zero-guarded div/mod trap plumbing

Declare sigil_arith_{div,mod}_by_zero_trap FuncIds/FuncRefs and add a
shared emit_guarded_div helper that branches to a direct runtime trap
on a zero divisor. Wire both BinOp::Div/Mod lowering sites (emit_binop
and the auto-CPS residual evaluator) to it. Latent until elaborate
stops rewriting Div/Mod (next commit).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Flip behaviour — stop performing/requiring ArithError for `/` and `%`

**Files:**
- Modify: `compiler/src/typecheck.rs:6504-6522` (remove the ArithError injection)
- Modify: `compiler/src/elaborate.rs:179-245` (remove the Div/Mod rewrite)
- Modify/Add: `compiler/tests/e2e.rs` (`mod_by_zero_traps` + two new tests)

- [ ] **Step 1: Write the new failing tests first**

Add to `compiler/tests/e2e.rs` (place near the existing `mod_by_zero_traps` at e2e.rs:1076). These assert the new semantics: a `![]` function may divide, and a zero divisor traps with exit 2 + the existing banner.

```rust
/// arith-trap — a `![]` function (no ArithError on the row) may use
/// `/`; a zero divisor traps with the preserved banner + exit 2.
#[test]
fn bare_div_by_zero_traps_without_row() {
    let source = "fn main() -> Int ![] {\n\
                  let a: Int = 10;\n\
                  let b: Int = 0;\n\
                  a / b\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "bare_div_by_zero");
    assert_eq!(code, 2, "bare `/` by zero exits 2; stderr={stderr:?}");
    assert!(
        stderr.contains("sigil: arithmetic error: division by zero"),
        "expected division-by-zero banner; stderr={stderr:?}"
    );
}

/// arith-trap — `%` traps inside an auto-CPS-promoted recursive fn
/// (the residual-evaluator lowering path). Guards the second Div/Mod
/// lowering site. `count_down` recurses non-tail (so it is promoted
/// to CPS) and performs `100 % (n - n)` == `100 % 0` at the base.
#[test]
fn auto_cps_recursive_mod_by_zero_traps() {
    let source = "fn count_down(n: Int) -> Int ![] {\n\
                  if n <= 0 {\n\
                  100 % (n - n)\n\
                  } else {\n\
                  1 + count_down(n - 1)\n\
                  }\n\
                  }\n\
                  fn main() -> Int ![] {\n\
                  count_down(3)\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "auto_cps_mod_zero");
    assert_eq!(code, 2, "auto-CPS `%` by zero exits 2; stderr={stderr:?}");
    assert!(
        stderr.contains("sigil: arithmetic error: remainder by zero"),
        "expected remainder-by-zero banner; stderr={stderr:?}"
    );
}
```

Update the existing `mod_by_zero_traps` test (e2e.rs:1076-1091) so its source no longer declares `![ArithError]` — this is now the regression guard that bare `%` traps. Replace its `source` literal and the doc line so it reads:

```rust
/// arith-trap — `%` by zero traps with banner + exit 2, with NO
/// `![ArithError]` on the row (operators no longer carry the effect).
/// `examples/div_by_zero.sigil` covers the `/` path via
/// [`div_by_zero_example_traps`]; this test covers the `%` path.
#[test]
fn mod_by_zero_traps() {
    let source = "fn main() -> Int ![] {\n\
                  let a: Int = 10;\n\
                  let b: Int = 0;\n\
                  a % b\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "mod_by_zero");
    assert_eq!(code, 2, "`%` by zero exits 2; stderr={stderr:?}");
    assert!(
        stderr.contains("sigil: arithmetic error: remainder by zero"),
        "expected remainder-by-zero banner; stderr={stderr:?}"
    );
}
```

- [ ] **Step 2: Run the new tests to verify they FAIL (against current behaviour)**

Run: `cargo test -p sigil-compiler --test e2e bare_div_by_zero_traps_without_row -- --nocapture`
Expected: FAIL — currently `fn main() -> Int ![]` with `a / b` fires E0042 (operator `/` requires `ArithError`), so compilation fails before running. (`mod_by_zero_traps` likewise now fails to compile because the row was dropped.)

- [ ] **Step 3: Remove the ArithError injection in typecheck**

In `compiler/src/typecheck.rs`, delete the entire `if matches!(op, BinOp::Div | BinOp::Mod) { … register_effect_use("ArithError", …) }` block (lines 6507-6520, the comment + the `if`), leaving:

```rust
            Expr::Binary { op, lhs, rhs, span } => {
                let lt = self.check_expr(lhs, row, row_tail);
                let rt = self.check_expr(rhs, row, row_tail);
                self.check_binop(*op, lt, rt, lhs.span(), rhs.span())
            }
```

(`span` is now unused in this arm. If clippy flags the unused binding, rename it to `span: _` in the match pattern.)

- [ ] **Step 4: Remove the Div/Mod rewrite in elaborate**

In `compiler/src/elaborate.rs`, delete the entire `if matches!(op, BinOp::Div | BinOp::Mod) { … return (rewritten, hoisted); }` block (lines 185-245, the comment + the `if`). The arm must fall through to the generic rebuild that already follows it:

```rust
            Expr::Binary { op, lhs, rhs, span } => {
                let mut hoisted = Vec::new();
                let (lhs_e, h1) = self.elab_expr(*lhs, true);
                hoisted.extend(h1);
                let (rhs_e, h2) = self.elab_expr(*rhs, true);
                hoisted.extend(h2);
                let new = Expr::Binary {
                    op,
                    lhs: Box::new(lhs_e),
                    rhs: Box::new(rhs_e),
                    span,
                };
                // … (whatever the existing tail of this arm does with `new`/`need_trivial`)
            }
```

> Executor: preserve the existing tail of the arm (the part after line 245 that wraps `new` for `need_trivial`). Only the Div/Mod `if` block and its comment are removed. `PerformExpr` / `Block` imports that were only used by the deleted block may now be unused — remove them if clippy flags them.

- [ ] **Step 5: Build (release runtime already current from Task 1) and run the three tests**

Run:
```
cargo build -p sigil-compiler && \
cargo test -p sigil-compiler --test e2e bare_div_by_zero_traps_without_row -- --nocapture && \
cargo test -p sigil-compiler --test e2e mod_by_zero_traps -- --nocapture && \
cargo test -p sigil-compiler --test e2e auto_cps_recursive_mod_by_zero_traps -- --nocapture
```
Expected: all three PASS (exit 2 + correct banner).

- [ ] **Step 6: Verify the preserved `/` oracle and the still-valid ArithError effect**

Run:
```
cargo test -p sigil-compiler --test e2e div_by_zero_example_traps -- --nocapture && \
cargo test -p sigil-compiler --test e2e task_78_5_nim_mini_perfect_strategy_alice_wins_seven -- --nocapture
```
Expected: both PASS. `div_by_zero_example_traps` still gets exit 2 + "division by zero" (oracle preserved). The Nim test still compiles/runs despite `pick`'s now-over-declared `![ArithError]` (proves over-declaration is accepted and ArithError remains a valid effect).

- [ ] **Step 7: Commit**

```bash
git add compiler/src/typecheck.rs compiler/src/elaborate.rs compiler/tests/e2e.rs
git commit -m "$(cat <<'EOF'
[arith-trap] flip: `/` and `%` trap instead of requiring ArithError

Stop typecheck auto-injecting ![ArithError] for Div/Mod and stop
elaborate rewriting them to perform-bearing form. Div/Mod now lower
directly to the zero-guarded trap added in the previous commit. A
![] function may divide; a zero divisor exits 2 with the unchanged
banner. ArithError remains a valid explicit effect. Adds bare-row and
auto-CPS trap regression tests; mod_by_zero_traps drops its row.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Remove the now-dead `SdivUnchecked` / `SremUnchecked` variants

Nothing constructs these anymore (elaborate was their only producer). Remove the variants and all match arms.

**Files:**
- Modify: `compiler/src/ast.rs:685-689`
- Modify: `compiler/src/elaborate.rs:619-620`
- Modify: `compiler/src/typecheck.rs:6795`, `:9114-9115`
- Modify: `compiler/src/codegen.rs:27696-27697`, `:28361`, `:29147-29148`

- [ ] **Step 1: Remove the AST variants**

In `compiler/src/ast.rs`, delete the `SdivUnchecked,` and `SremUnchecked,` variants (lines 685-689, including their doc comments) from the `BinOp` enum.

- [ ] **Step 2: Remove every match arm referencing them**

Delete or fold each arm (the compiler will point you at every non-exhaustive match if you miss one):
- `compiler/src/elaborate.rs:619-620` — in `binop_result_type` (or similar), drop the `| BinOp::SdivUnchecked | BinOp::SremUnchecked` from the `=> "Int"` arm (keep `Div`/`Mod` there).
- `compiler/src/typecheck.rs:6795` — drop the `BinOp::SdivUnchecked | BinOp::SremUnchecked => { … }` arm in `check_binop`.
- `compiler/src/typecheck.rs:9114-9115` — drop the `BinOp::SdivUnchecked => "/"` / `BinOp::SremUnchecked => "%"` display arms.
- `compiler/src/codegen.rs:27696-27697` — drop the `BinOp::SdivUnchecked => sdiv` / `BinOp::SremUnchecked => srem` arms in `emit_binop` (Div/Mod are now handled by `emit_guarded_div`).
- `compiler/src/codegen.rs:28361` — in `type_of_expr`'s Binary arm, drop the `| BinOp::SdivUnchecked | BinOp::SremUnchecked` from the `=> types::I64` arm (keep `Add|Sub|Mul|Div|Mod`).
- `compiler/src/codegen.rs:29147-29148` — drop the `BinOp::SdivUnchecked => sdiv` / `BinOp::SremUnchecked => srem` arms in `lower_auto_cps_simple_expr`.

- [ ] **Step 3: Build to verify exhaustiveness**

Run: `cargo build -p sigil-compiler 2>&1 | tail -20`
Expected: compiles. If a `non-exhaustive patterns` error appears, it points at a missed match site — fix and rebuild.

- [ ] **Step 4: Sanity-run the trap tests again (no behaviour change expected)**

Run: `cargo test -p sigil-compiler --test e2e bare_div_by_zero_traps_without_row -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add compiler/src/ast.rs compiler/src/elaborate.rs compiler/src/typecheck.rs compiler/src/codegen.rs
git commit -m "$(cat <<'EOF'
[arith-trap] remove dead SdivUnchecked/SremUnchecked BinOp variants

Elaborate was their only producer; with the Div/Mod rewrite gone they
are unreachable. Remove the variants and all match arms.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Add `checked_div` / `checked_mod` to the stdlib

These are the recoverable alternative to the trapping operators. Pure (`![]`), return `Result[Int, String]`.

**Files:**
- Modify: `std/int.sigil` (imports at top + two new fns at the end)
- Add: tests in `compiler/tests/e2e.rs`

- [ ] **Step 1: Write the failing tests**

Add to `compiler/tests/e2e.rs`:

```rust
/// arith-trap — checked_div returns Err on a zero divisor (no trap).
#[test]
fn checked_div_returns_err_on_zero() {
    let source = "import std.int\n\
                  import std.io\n\
                  import std.result\n\
                  use std.int.{checked_div, int_to_string};\n\
                  use std.io.{IO};\n\
                  use std.result.{Err, Ok, Result};\n\
                  fn main() -> Int ![IO] {\n\
                  match checked_div(10, 0) {\n\
                  Ok(v) => perform IO.println(int_to_string(v)),\n\
                  Err(msg) => perform IO.println(msg),\n\
                  };\n\
                  0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "checked_div_err");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "division by zero\n", "stderr={stderr:?}");
}

/// arith-trap — checked_div returns Ok(quotient) for a nonzero divisor;
/// checked_mod returns Ok(remainder).
#[test]
fn checked_div_and_mod_return_ok() {
    let source = "import std.int\n\
                  import std.io\n\
                  import std.result\n\
                  use std.int.{checked_div, checked_mod, int_to_string};\n\
                  use std.io.{IO};\n\
                  use std.result.{Err, Ok, Result};\n\
                  fn show(r: Result[Int, String]) -> Int ![IO] {\n\
                  match r {\n\
                  Ok(v) => perform IO.println(int_to_string(v)),\n\
                  Err(msg) => perform IO.println(msg),\n\
                  };\n\
                  0\n\
                  }\n\
                  fn main() -> Int ![IO] {\n\
                  show(checked_div(17, 5));\n\
                  show(checked_mod(17, 5));\n\
                  0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "checked_div_mod_ok");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "3\n2\n", "stderr={stderr:?}");
}
```

- [ ] **Step 2: Run to verify they FAIL**

Run: `cargo test -p sigil-compiler --test e2e checked_div_returns_err_on_zero -- --nocapture`
Expected: FAIL — `checked_div` is not defined in `std.int` (resolve/typecheck error).

- [ ] **Step 3: Add the imports and the two functions to `std/int.sigil`**

At the top of `std/int.sigil`, after the existing `import std.option` / `use std.option.{…}` lines (int.sigil:41-42), add:

```sigil
import std.result
use std.result.{Err, Ok, Result};
```

At the end of `std/int.sigil`, add:

```sigil
// `checked_div(a, b)` returns `Ok(a / b)` for a nonzero divisor and
// `Err("division by zero")` when `b == 0`, instead of trapping. Pure
// (`![]`) — the `/` in the `Ok` arm only runs when `b != 0`, so it
// never trips the runtime div-by-zero trap. Use this when a zero
// divisor is a recoverable condition rather than a program bug.
fn checked_div(a: Int, b: Int) -> Result[Int, String] ![] {
  if b == 0 {
    Err("division by zero")
  } else {
    Ok(a / b)
  }
}

// `checked_mod(a, b)` is the `%` parallel of `checked_div`: `Ok(a % b)`
// for a nonzero divisor, `Err("remainder by zero")` when `b == 0`.
fn checked_mod(a: Int, b: Int) -> Result[Int, String] ![] {
  if b == 0 {
    Err("remainder by zero")
  } else {
    Ok(a % b)
  }
}
```

- [ ] **Step 4: Run the tests to verify they PASS**

Run:
```
cargo test -p sigil-compiler --test e2e checked_div_returns_err_on_zero -- --nocapture && \
cargo test -p sigil-compiler --test e2e checked_div_and_mod_return_ok -- --nocapture
```
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add std/int.sigil compiler/tests/e2e.rs
git commit -m "$(cat <<'EOF'
[arith-trap] std: checked_div / checked_mod -> Result[Int, String]

The recoverable alternative to the trapping `/` and `%` operators.
Pure; pre-checks the divisor so the inner operator never traps.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Migrate examples (div_recover rewrite + strip dead annotations)

Handler-based recovery from `/` is gone (`/` traps, doesn't perform), so `examples/div_recover.sigil` must be rebuilt around `checked_div`. `arith.sigil` and `div_by_zero.sigil` carry `![ArithError]` that is now dead.

**Files:**
- Rewrite: `examples/div_recover.sigil`
- Modify: `examples/arith.sigil`, `examples/div_by_zero.sigil`
- Modify: `compiler/tests/e2e.rs` (`div_recover_example_returns_999` assertion stays the same — stdout "999", exit 0 — only the recovery mechanism changed)

- [ ] **Step 1: Rewrite `examples/div_recover.sigil`**

Replace the entire file with a `checked_div`-based recovery (preserves the test invariant: stdout `999\n`, exit 0):

```sigil
// examples/div_recover.sigil
//
// arith-trap — recovering from a zero divisor without trapping.
//
// `/` and `%` trap (clean stderr + exit 2) on a zero divisor and do
// NOT introduce an effect, so a zero divisor cannot be intercepted
// with a `handle ... with { ArithError.* }` arm. When a zero divisor
// is a recoverable condition rather than a bug, use `checked_div` /
// `checked_mod` (std.int), which return `Result[Int, String]`.
//
// `safe_divide` returns the quotient on success and a fallback of
// 999 on a zero divisor. Invariant: stdout reads `999\n`, exit 0.

import std.int
import std.io
import std.result
use std.int.{checked_div, int_to_string};
use std.io.{IO};
use std.result.{Err, Ok, Result};

fn safe_divide(a: Int, b: Int) -> Int ![] {
  match checked_div(a, b) {
    Ok(q) => q,
    Err(msg) => 999,
  }
}

fn main() -> Int ![IO] {
  let recovered: Int = safe_divide(10, 0);
  perform IO.println(int_to_string(recovered));
  0
}
```

> The `Err(msg)` arm binds `msg` but doesn't use it. If Sigil flags unused pattern bindings, change it to `Err(_) => 999`. (Check by compiling in Step 3.)

- [ ] **Step 2: Strip dead `![ArithError]` from `arith.sigil` and `div_by_zero.sigil`**

`examples/arith.sigil` — remove `import std.raise` (line 21), `use std.raise.{ArithError};` (line 22), and change the `main` row from `![ArithError]` to `![]` (line 23). Update the header comment block (lines 7-12) to:

```sigil
// `/` and `%` trap (clean stderr + exit 2) on a zero divisor and do
// not introduce any effect — divide-by-nonzero needs no effect row.
// This program's row is `![]`.
```

`examples/div_by_zero.sigil` — remove `import std.raise` (line 24), `use std.raise.{ArithError};` (line 25), and change `main`'s row from `![ArithError]` to `![]` (line 26). Update the header comment (lines 8-18) to:

```sigil
// arith-trap — `/` by zero traps directly: the runtime fn
// `sigil_arith_div_by_zero_trap` writes
// `sigil: arithmetic error: division by zero` to stderr and exits 2.
// `/` no longer performs an effect, so the row is `![]`. This file
// exists so the e2e suite can assert the trap fires; it is excluded
// from smoke.sh as a success-path example.
```

- [ ] **Step 3: Update the `div_recover_example_returns_999` test doc + verify behaviour**

In `compiler/tests/e2e.rs` (the `div_recover_example_returns_999` test at ~1108), the assertions (`code == 0`, `stdout == "999\n"`) are unchanged — only the doc comment needs to reflect the new mechanism. Update the doc lines (1093-1107) to describe `checked_div`-based recovery instead of handler-based. Then run:

```
cargo build --release --workspace && \
cargo test -p sigil-compiler --test e2e div_recover_example_returns_999 -- --nocapture && \
cargo test -p sigil-compiler --test e2e arith_example_runs -- --nocapture && \
cargo test -p sigil-compiler --test e2e div_by_zero_example_traps -- --nocapture && \
cargo test -p sigil-compiler --test e2e cross_check_div_recover_runs_cleanly -- --nocapture
```
Expected: all PASS (div_recover → "999\n"/exit 0; arith → exit 26; div_by_zero → exit 2 + banner; cross-check clean).

- [ ] **Step 4: Run smoke + reproducibility (examples must still compile deterministically)**

Run: `./scripts/smoke.sh && ./scripts/reproducibility.sh`
Expected: both pass. (smoke doesn't run the three changed files, but reproducibility compiles every example twice and checks byte-identical output — the rewritten div_recover must build deterministically.)

- [ ] **Step 5: Commit**

```bash
git add examples/div_recover.sigil examples/arith.sigil examples/div_by_zero.sigil compiler/tests/e2e.rs
git commit -m "$(cat <<'EOF'
[arith-trap] examples: checked_div recovery + drop dead ArithError rows

div_recover.sigil now recovers via checked_div (handler-on-`/`
recovery is impossible once `/` traps). arith.sigil and
div_by_zero.sigil drop their now-unused ![ArithError] rows + raise
imports. Test invariants unchanged (999/exit 0; exit 26; exit 2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Update the spec

`spec/language.md` is canonical. Do NOT hand-edit `docs/` (tag workflow syncs it). The change: `/` and `%` trap (like array OOB / panic) and carry NO effect; `ArithError` is documented as a still-valid effect users may `perform`/`handle` explicitly but operators no longer auto-use; `checked_div`/`checked_mod` are the recoverable path.

**Files:**
- Modify: `spec/language.md` — §3.3 operator-effects note (~972-990), §4.2 operator table + gotcha (1031-1080), the E4 example (~113-137), §10 `fn main` note (~770), §13 builtin-effects mention (~1267-1272 — leave intact, ArithError still exists)

- [ ] **Step 1: Fix the §4.2 operator table row**

Change the "Arithmetic (may abort)" table row (language.md:1036) from:

```
| Arithmetic (may abort) | `/`, `%` | `(Int, Int) -> Int ![ArithError]` |
```

to:

```
| Arithmetic (traps on zero) | `/`, `%` | `(Int, Int) -> Int ![]` |
```

- [ ] **Step 2: Replace the §4.2 gotcha block**

Replace the entire `> **Effect-row gotcha — `/` and `%` carry `ArithError`.** …` block (language.md:1041-1080) with:

```markdown
> **`/` and `%` trap on a zero divisor — they carry no effect.**
> Division and modulo abort the process on a zero divisor: the
> runtime writes `sigil: arithmetic error: division by zero`
> (resp. `remainder by zero`) to stderr and exits with status 2.
> This is a *trap*, like an out-of-bounds array access or `panic` —
> not an effect. A function that divides needs NO `ArithError` (or
> any other) entry on its row; `fn main() -> Int ![]` may contain
> `n % 2` and compiles.
>
> A trap cannot be intercepted by a `handle` arm. When a zero
> divisor is a recoverable condition rather than a programming bug,
> use `checked_div` / `checked_mod` from `std.int`, which pre-check
> the divisor and return `Result[Int, String]`:
>
> ```sigil
> import std.int
> use std.int.{checked_div};
> use std.result.{Err, Ok, Result};
> fn main() -> Int ![] {
>   let r: Result[Int, String] = checked_div(10, 0);  // Err("division by zero")
>   0
> }
> ```
>
> The `ArithError` effect still exists and can be performed and
> handled explicitly (`perform ArithError.div_by_zero()` inside a
> `![ArithError]` row), but the `/` and `%` operators no longer
> perform it.
```

- [ ] **Step 3: Fix the §3.3 operator-effects note**

In the "Operator effects to remember" admonition (language.md:972-990), replace the `/` and `%` bullet (lines 975-982) with:

```markdown
> - **`/` and `%`** trap (abort + exit 2) on a zero divisor and
>   carry NO effect — a dividing function needs nothing on its row.
>   For recoverable division use `checked_div` / `checked_mod`
>   (`std.int`), which return `Result[Int, String]`. See §4.2.
```

- [ ] **Step 4: Fix the E4 sum-type example**

The E4 example (language.md:113-137) uses `![ArithError]` purely because of `num / den`. Update it so it reflects the new semantics — `safe_div` becomes `![]` and the `std.raise` import + `use` are dropped:

```sigil
import std.int
import std.io
import std.option
use std.int.{int_to_string};
use std.io.{IO};
use std.option.{None, Option, Some};

fn safe_div(num: Int, den: Int) -> Option[Int] ![] {
  match den {
    0 => None,
    _ => Some(num / den),
  }
}

fn main() -> Int ![IO] {
  match safe_div(10, 0) {
    Some(v) => perform IO.println(int_to_string(v)),
    None => perform IO.println("zero divisor"),
  };
  0
}
```

- [ ] **Step 5: Fix the §10 `fn main` effects note**

The `fn main` allowed-effects list (language.md:770) lists `ArithError — div-by-zero / mod-by-zero default handlers`. `ArithError` is still a valid `main` row entry (the default frame is still installed for explicit performs), but the description is stale. Change line 770 to:

```markdown
- `ArithError` — explicit `perform ArithError.*` only (the `/` and
  `%` operators trap directly and do not perform it); default
  handler exits 2
```

Leave the §13 builtin-effects list (language.md:1267-1272, `ArithError = 0` fixed ID) **unchanged** — the effect and its ID still exist.

- [ ] **Step 6: Verify the spec examples are valid Sigil**

The spec isn't compiled by CI, but the inline examples should be correct. Eyeball that the E4 and gotcha examples match the new stdlib (`checked_div` signature, `Result[Int, String]`). Optionally compile the E4 body by pasting it into a temp file and running `target/release/sigil /tmp/e4.sigil -o /tmp/e4`.

- [ ] **Step 7: Commit**

```bash
git add spec/language.md
git commit -m "$(cat <<'EOF'
[arith-trap] spec: `/` and `%` trap, carry no effect

Document that division/modulo trap on a zero divisor (exit 2, like
array OOB / panic) and carry no effect row; checked_div / checked_mod
are the recoverable path. ArithError remains a valid explicit effect.
Updates §3.3, §4.2, the E4 example, and the §10 main note.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Full-suite gate

**Files:** none (verification only)

- [ ] **Step 1: Rebuild release runtime + workspace (mandatory before authoritative e2e)**

Run: `cargo build --release -p sigil-runtime && cargo build --release --workspace`
Expected: clean.

- [ ] **Step 2: Run the fast load-bearing check**

Run: `./scripts/pod-verify.sh`
Expected: PASS (fmt + per-crate clippy + workspace check + runtime lib tests + discipline greps). Fix any clippy lint (e.g. unused imports left over from the elaborate deletion) before proceeding.

- [ ] **Step 3: Run the full authoritative e2e suite**

Run: `cargo test -p sigil-compiler --test e2e 2>&1 | tail -40`
Expected: ALL pass. Pay special attention to the ArithError-composition tests that were intentionally left untouched — they must still pass via over-declaration:
- `task_78_5_nim_mini_perfect_strategy_alice_wins_seven`
- the Raise/State/IO+ArithError composition tests (e2e.rs:13178, 19786)
- the frame-shape assertion (e2e.rs:546, "ArithError arm_count=2")

If any composition test fails to compile because of a now-unused `![ArithError]`, that contradicts the over-declaration assumption — STOP and report (it would mean a superfluous-effect check exists that the grep missed; the fix would be to strip those rows, but verify the premise first).

- [ ] **Step 4: Run smoke + reproducibility + plan-b-invariants**

Run: `./scripts/smoke.sh && ./scripts/reproducibility.sh && ./scripts/plan-b-invariants.sh`
Expected: all pass. (`plan-b-invariants.sh` guards CPS/continuation charter invariants — confirm the auto-CPS Div/Mod guard didn't disturb them.)

- [ ] **Step 5: Final review commit (only if Steps 1-4 surfaced fixes)**

If any step required a fix, commit it:

```bash
git add -A
git commit -m "$(cat <<'EOF'
[arith-trap] gate fixups

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**Spec coverage:**
- "`/`/`%` trap, no effect row" → Tasks 1-3 (runtime trap, codegen guard, typecheck/elaborate removal) + Task 7 (spec).
- "keep ArithError enum/effect" → explicitly preserved; verified by Task 3 Step 6 (Nim) + Task 8 Step 3 (frame-shape + composition tests). Runtime arm fns + default frame untouched.
- "clean up dangling `![ArithError]` annotations" → Task 6 (examples) + Task 7 (spec). e2e composition fixtures deliberately retained (documented rationale).
- "add `checked_div`/`checked_mod -> Result[Int]`" → Task 5 (as `Result[Int, String]`; `Result` needs two type params, `String` chosen as the error type — no new types).
- "preserve div_by_zero oracle (exit 2 + message)" → Task 1 reuses the banner+exit via shared helper; verified Task 3 Step 6 + Task 6 Step 3.
- Both binop lowering sites guarded → Task 2 (emit_binop + lower_auto_cps_simple_expr via shared `emit_guarded_div`); auto-CPS path proven by `auto_cps_recursive_mod_by_zero_traps` (Task 3).

**Placeholder scan:** No TBD/TODO. Every code step shows complete code. The three `AutoCpsExprCtx` construction sites (Task 2 Step 6) carry an explicit executor note to match the in-scope `module`/`builder` bindings used by the adjacent `alloc_ref:` field — this is concrete guidance, not a placeholder, because the exact local identifiers differ per site and must be read from the source.

**Type consistency:** `emit_guarded_div(builder, l, r, is_div: bool, div_trap_ref: FuncRef, mod_trap_ref: FuncRef) -> Value` — same signature at both call sites. `BuiltinFuncIds.arith_{div,mod}_trap: FuncId`; `Builtins.arith_{div,mod}_trap_ref: FuncRef`; `AutoCpsExprCtx.arith_{div,mod}_trap_ref: FuncRef` — consistent naming. Runtime symbols `sigil_arith_{div,mod}_by_zero_trap` match the codegen `declare_function` strings. `checked_div`/`checked_mod` return `Result[Int, String]` consistently across stdlib + all tests + spec.
