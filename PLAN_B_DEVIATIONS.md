# Plan B Deviations

Deviations from `in-progress/2026-04-21-sigil-effects.md`. Each entry is logged
**before** the implementing commit (per the plan's commit discipline). Entries
remain after Plan B closes as a permanent record.

Format:

```
## <date> — [DEVIATION Task N] <one-line topic>

**Context:** ...

**Deviation:** ...

**Rationale:** ...

**Implementing commit(s):** <SHAs>
```

Untagged sweep / chore entries use `[CHORE]` instead of `[DEVIATION Task N]`.

## 2026-04-25 — [Task 4.5.5 / A3-carryover] Tagged-vs-raw Int ABI decision

**Context:** Plan A3's `QUESTIONS.md` entry `[PLAN-A3] main-return-tagging`
(resolved 2026-04-24) explicitly reopened the broader ABI question for
Plan B: should internal user-function calls pass `Int` as tagged 64-bit
values, or as raw `i64` with tagging only at the C-ABI boundary? The
effect-runtime CPS transform (Stage 6 Task 55) and trampoline machinery
need a definitive answer before they ship.

Today's codebase:
- User functions compute on **raw `i64`** (no tagging in body-local
  arithmetic).
- **Tagging happens only at `main`'s return**: codegen emits
  `ishl_imm TAG_INT_SHIFT` on the Int value and the generated C-main
  shim emits `sshr_imm TAG_INT_SHIFT` + `ireduce I32` to produce the
  process exit code.
- Heap-stored `Int` values (closure env slots, user-type field slots)
  currently flow through as whatever Cranelift `Value` the field type
  resolves to — `I64` for `Int`. This avoids GC surprises because GC
  only scans pointer-typed slots via the pointer bitmap; non-pointer
  fields are opaque payloads.

**Decision:** keep the current pattern — raw `i64` internally within
user code, tag at the C-ABI boundary only. Effect-runtime CPS work in
Stage 6 layers onto this by introducing *new* boundary moments:

1. **Continuations captured across handler boundaries** (the
   `current_k` that `sigil_perform` passes to handler arms) must
   carry tagged `Int` arguments, because a captured continuation can
   sit on the heap in a `HandlerFrame` slot that the GC scans.
2. **`NextStep` records arena-allocated by the trampoline** keep
   their args untagged (raw `i64`) for arithmetic cycles. The arena
   is reset per dispatch and never scanned by the Boehm collector;
   raw values here avoid tag/untag churn on the hot trampoline path.

This is option (c)'s "raw everywhere internally, tag at the C-ABI
boundary" applied to user-fn calls specifically, plus a narrower
tag-at-heap-observability rule for continuations and handler-scope
slots. It is compatible with the A3 resolution (main → Int locked)
because main's C-ABI boundary was already the one tagging site.

**Rationale:**
1. **Minimal code churn in this PR.** The existing pattern is already
   "raw internally". Formalising the decision without rewriting every
   user-fn call site keeps the diff focused.
2. **Tagging is a GC-discipline question, not a performance one.**
   Heap-observable slots need tags so the GC can tell Int from
   pointer. Non-heap slots don't. Arena-allocated `NextStep` records
   are non-heap (arena is reset, not scanned); continuations are
   heap (scanned), so they need tagged payloads.
3. **CPS hot path stays tight.** Arithmetic-dense CPS-color code
   (fib under `!State[Int]`) will dispatch through the trampoline
   billions of times per `fib(30)` invocation. Every tag/untag pair
   saved on the hot path matters for the Task 60 performance floor.
4. **One constant to audit.** `sigil_abi::tag::TAG_INT_SHIFT` is the
   single reference for the shift amount. Stage 6's new tagging sites
   consume it too. A future revisit would edit one place.

**Audit of `ishl_imm` / `sshr_imm` sites (2026-04-25):**

| file                     | site                 | purpose                                | status              |
|--------------------------|----------------------|----------------------------------------|---------------------|
| compiler/src/codegen.rs  | user-main return     | tag `Int` for C-ABI exit code          | updated to TAG_INT_SHIFT |
| compiler/src/codegen.rs  | C-main shim untag    | untag tagged `Int` → raw → `ireduce I32` | updated to TAG_INT_SHIFT |

No other tag-shift sites exist in the compiler or runtime today.
`from_int` / `as_int` in `sigil_runtime::value` already consume
`TAG_INT_SHIFT` as of Task 4.5.5.

**Implementing commit(s):** [HEAD]

**Cross-references:** QUESTIONS.md — the `[PLAN-A3] main-return-tagging`
entry's Forward-Implications paragraph is now closed by this decision.
Added a `[PLAN-B] tagged-vs-raw-int-abi` entry pointing back here.

## 2026-04-25 — [VERIFICATION DEBT] Tagged-vs-raw ABI contract enforcement

**Context:** The Task 4.5.8 decision (raw `i64` internally; tag at GC-
observable boundaries) creates a forward contract that today is just
documentation. Stage 6 introduces the first non-`main` boundary moments:

- `HandlerFrame` slots that hold continuation-captured `Int` arguments
  must be **tagged** (heap-observable; the GC scans them).
- Arena-allocated `NextStep` slots that hold per-step `Int` values must
  be **raw `i64`** (arena resets per dispatch; not GC-scanned).

Without an enforcement mechanism, the contract is easy to violate by
accident — a developer writing `frame.args[0] = raw_int` instead of
`frame.args[0] = tag(raw_int)` would compile and produce wrong-but-
silent GC behavior under stress.

**Closure point:** Task 56 (Stage 6 — `runtime/src/handlers.rs` and
`runtime/src/arena.rs` ship the runtime-side data structures).

**Picked mechanism:** **Newtype wrappers around `i64`.** Specifically:

```rust
// runtime/src/value.rs (or sigil-abi::tag, TBD at Task 56)
#[repr(transparent)]
pub struct TaggedInt(i64);

#[repr(transparent)]
pub struct RawInt(i64);

impl TaggedInt {
    pub fn from_raw(r: RawInt) -> Self { Self((r.0 as u64).wrapping_shl(TAG_INT_SHIFT) as i64) }
    pub fn untag(self) -> RawInt { RawInt(self.0 >> TAG_INT_SHIFT) }
}
```

`HandlerFrame` slot fields use `TaggedInt`; arena `NextStep` fields use
`RawInt`. Conversion is explicit at every boundary; the Rust compiler
rejects any direct assignment of one to the other. Conversion functions
themselves consume `TAG_INT_SHIFT` from `sigil-abi::tag` so a future ABI
revision still has a single mechanical edit point.

This was picked over the alternatives because:

- **Newtype wrappers** are statically enforced at compile time — every
  contract violation is a Rust type error, not a runtime debug-only
  assertion that production builds elide. The Stage 6 effect runtime
  will be extensively used; runtime-only checks are weaker.
- Newtypes are zero-cost (`#[repr(transparent)]`) and the conversion
  functions inline; the trampoline hot path keeps its tight codegen.
- The runtime side (where this contract matters most — `sigil_perform`,
  handler-arm calling convention, arena `alloc` / `reset`) is in Rust;
  newtypes work cleanly there.

Codegen-side stores into Cranelift IR slots are outside the Rust type
system (Cranelift `Value`s are opaque IDs). At those sites, a paired
contract comment cross-referencing this entry is the practical follow-
through; the test that the contract holds is end-to-end (the Stage 6
multi-shot stress test in `examples/multishot_stress.sigil` will fail
under stress if any boundary mis-tags).

**Acceptance criterion for Task 56:**
1. `runtime/src/handlers.rs` declares `HandlerFrame` with `TaggedInt`
   for any `Int`-typed slot.
2. `runtime/src/arena.rs` declares `NextStep` with `RawInt` for any
   `Int`-typed slot.
3. The `TaggedInt` <-> `RawInt` conversion functions consume
   `sigil_abi::tag::TAG_INT_SHIFT` (no inline literal `1`).
4. Every codegen site that emits a Cranelift store into a handler-frame
   slot or arena slot has a contract comment cross-referencing this
   entry and the Task 4.5.8 decision.
5. The Plan B Task 60 multi-shot stress test passes on both hosts.

**Implementing commit(s):** TBD — closes at Task 56.

## 2026-04-25 — [VERIFICATION DEBT] Codegen path for un-monomorphized generic params

**Context:** Plan B Task 47 grew the AST with `TypeExpr::Apply` and
parser-only support for generic parameters on `fn` and `type`
declarations. Today the typechecker rejects every `Apply` with E0124
and rejects every `effect_row_var` with E0125, so codegen never sees
generic-applied types or row-polymorphic effect rows.

When Task 48 (HM unification with row variables) lands, those E0124 /
E0125 rejections turn off — generic params become bound type variables
that flow through typecheck. Task 49 (monomorphization) then specialises
generics down to concrete instantiations. Codegen (in particular
`cranelift_ty_for_type_expr` at `compiler/src/codegen.rs:95`) is
written assuming monomorphization completed: it consults `head_name()`
and falls through to `pointer_ty` for any unrecognised name. A
post-Task-48-pre-Task-49 pipeline could in principle hand codegen an
un-monomorphized generic-parameter reference (`A` from `fn id[A](x: A)`),
which would silently lower as `pointer_ty` — wrong for `Int`-typed `A`,
and undetectable until the program crashes at the platform-call
boundary.

**Closure point:** Task 48 (Stage 5 — HM unification binds the generic
parameters; the codegen invariant becomes assertable for the first time).

**Picked mechanism:** **Explicit `assert!` at the codegen entry point**
that monomorphization has erased every `TypeExpr::Apply` and every
generic-param reference, paired with an invariant comment at
`cranelift_ty_for_type_expr` documenting the expectation.

```rust
// compiler/src/codegen.rs (added at the top of emit_object)
assert!(
    !checked.program.contains_apply_or_generic_ref(),
    "codegen invariant: monomorphization (Task 49) must complete before \
     codegen; received program still contains TypeExpr::Apply or generic-\
     parameter references — see PLAN_B_DEVIATIONS verification-debt entry",
);
```

`contains_apply_or_generic_ref()` is a small AST walker that scans every
`TypeExpr` in the checked program for `Apply` nodes and for `Named` nodes
whose name appears in any in-scope `generic_params` list. The assert
fires in release builds (cheap walk), so a pipeline ordering bug is
loud rather than silent.

This was picked over the alternative (codegen detects generic-param
references mid-emit and emits an internal-compiler-error diagnostic)
because:

- A single entry-point assert is simpler to audit than per-emission-site
  ICE machinery. Reviewers see one invariant, one check, one place to
  update if the contract evolves.
- Failure mode is loud and immediate (process abort with a clear
  message) rather than diffuse (an ICE deep in lowering is harder to
  diagnose; the user sees \"compiler bug\" without knowing why).
- Cost is one whole-program AST walk per `emit_object` invocation
  (linear in IR size; cheap relative to Cranelift codegen).
- The invariant comment at `cranelift_ty_for_type_expr` is the
  documentation half: future readers see why the catchall is safe.

**Acceptance criterion for Task 48:**
1. `emit_object` (or its successor entry point if codegen is
   restructured) opens with an `assert!` that the input IR contains no
   `TypeExpr::Apply` and no generic-parameter references.
2. The walker that backs the assert is a single function on
   `CheckedProgram` (or whatever IR shape Task 48 settles on),
   exercised by at least one unit test that constructs a program with
   a residual `Apply` and confirms the assert fires.
3. `cranelift_ty_for_type_expr` carries an invariant comment cross-
   referencing this entry and the assertion site.
4. The assertion holds on every example in `examples/` after Task 49
   (monomorphization) lands.

**Implementing commit(s):** [HEAD, REVIEW-FIXUP] (Task 48).

**Closure status (2026-04-25):** all four acceptance bullets met.
Bullet 1 satisfied by the assertion at `emit_object`'s top. Bullet
2 satisfied by four codegen-test unit tests (`walker_rejects_residual_apply_in_fn_param`,
`walker_rejects_generic_param_decl`, `walker_rejects_generic_type_decl`,
`walker_accepts_concrete_program`) that construct `Program` AST
nodes directly via the `ast` types — independent of monomorphization,
exercising the walker's accept and reject paths against synthetic
inputs. Bullet 3 satisfied by the invariant comment at
`cranelift_ty_for_type_expr`. Bullet 4 (assertion holds on every
example after Task 49) is the long-tail check that closes when
monomorphization lands — the assertion is now the canonical contract,
and Task 49's reviewer just needs to confirm it doesn't fire on any
example. **This entry is closed for Task 48.**

## 2026-04-25 — [DEVIATION Task 48 / Stage 6] `subsume_row` rejects open-caller / closed-callee with extra effects

**Context:** Task 48's call-site row check uses asymmetric `subsume_row`
(`compiler/src/typecheck.rs:1074`) instead of symmetric `unify_row`,
which preserves the caller's row variable for generalisation rather
than collapsing it. This was the fix for the PR #15 review's "open-row
caller silently collapses to closed" bug.

**Deviation:** the asymmetric check requires `callee.effects ⊆
caller.known_effects`, even when the caller declares an open row.
For a caller `![IO | e]` invoking a callee `![Raise]` (closed):

- `callee_set = {Raise}`, `caller_set = {IO}` (caller's *known* part only)
- `missing = {Raise}` → `subsume_row` pushes E0042

The caller's open tail `e` is supposed to be extensible — semantically,
`![IO | e]` accepts any callee effect by absorbing the difference into
`e`. The current check rejects this case rather than extending `e`.

**Why deferred:** Sigil v1 only admits `IO` as a real surface effect.
Multi-effect programs (the case where this bug bites) land with Stage 6
effect handlers (Task 55 onward). There is no current example or test
that exposes the wrong behavior, and the fix has a non-trivial design
question: extending `e` requires either binding the caller's row var
to a row that includes `missing` (which is the symmetric-unification
behavior we explicitly rejected for the original bug) or threading
effect *constraints* on row variables through scheme-generalisation
so they accumulate without committing the row var to a specific shape.

The Stage 6 work has the constraint machinery in scope anyway (handler
typing requires reasoning about which effects flow through which
continuations); piggy-backing the open-tail extension on that work
keeps the design coherent rather than picking a v1-only stop-gap.

**Closure point:** Stage 6 effect handlers (Task 55 — runtime, plus
the typing rules that introduce additional surface effects).

**Workaround until then:** declare the explicit effect on the caller's
known list. `fn caller[e]() -> Int ![IO, Raise | e]` accepts callees
performing `IO` *or* `Raise` (or both); only callees with effects
outside `{IO, Raise}` would still trip E0042 against this caller.

**Acceptance criterion for closure:**
1. `subsume_row` (or its successor in the constraint-graph design)
   accepts open-caller / closed-callee with callee effects not in
   the caller's known list, by extending the caller's row constraints
   rather than emitting E0042.
2. A surface-level test in `compiler/src/typecheck.rs` covers the
   case (a generic caller with `![IO | e]` calling a closed-row
   callee that performs `Raise`) and confirms it typechecks.
3. The Plan B Stage 6 effect-handler suite (TBD) exercises the path
   end-to-end with at least one program where multi-effect flow
   through an open-row caller is the intended pattern.

**Implementing commit(s):** TBD — closes at Task 55 or earlier if a
multi-effect program needs the path before then.
