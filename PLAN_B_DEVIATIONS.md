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

## 2026-04-26 — [DEVIATION Task 55] Foundation phase ships separately from CPS codegen on the same branch

**Context:** Task 55 spans (a) the typecheck-side gate lifts for E0133 + E0134, (b) wiring `Item::Effect` and `Expr::Handle` through the codegen entry walker, (c) the full CPS transform for CPS-color monomorphs, (d) per-handler-arm closure synthesis, (e) `sigil_perform` call emission for non-IO `Expr::Perform`, (f) handler-frame setup + `sigil_handle_push`/`sigil_handle_pop` at `Expr::Handle` sites, (g) native↔CPS interop wrappers, (h) trampoline integration for CPS-color `main`, and (i) `CpsCallCount` / `NativeCallCount` counter wiring. Implementation scope is comparable to Task 56 (~1500 LOC + 34 unit tests + 4 review rounds).

**Deviation:** Task 55 lands across multiple commits on a single branch (`plan-b-task-55`) rather than as one monolithic commit. The first commit (foundation phase) ships only the easy and safe pieces — E0133 lift + entry-walker update + e2e test for effect-decl-with-no-handler-use — while leaving the E0134 gate live so well-formed `handle` expressions still surface a clean diagnostic instead of tripping the still-`unreachable!` codegen arms in `lower_expr` / `type_of_expr`. The CPS calling convention, body transform, arm synthesis, frame setup, and counter wiring land in follow-up commits on the same branch. PR opens only when the full Task 55 path actually compiles and runs at least one real handler example end-to-end.

**Rationale:** The all-or-nothing alternative (single commit landing every piece together) would require holding the entire ~2000-LOC change in working state across multiple sessions before any pod-verify or CI checkpoint. Splitting at the asymmetric-gate boundary gives a clean intermediate state where (1) effect-only programs gain a real codegen path immediately, (2) handle-using programs continue to surface E0134 with a clear "in-progress" message, and (3) each follow-up commit can be pod-verified independently. The **single-PR convention** from Tasks 49 / 53 / 54 / 56 still holds — only one PR opens for Task 55, and it includes the full chain of foundation + CPS commits. The squash-merged result will look identical to a one-shot landing.

**Implementing commit(s):** `b3af204` (foundation phase: E0133 lift + entry walker), `2d69b52` (Phase 2 minimum: E0134 lift + handle body-pass-through + effect/op IDs + e2e tests), `ef4be8d` (Phase 3a), `d0aa4c4` + `2e7c0de` (Phase 3b), `adcb897` (Phase 4a), [HEAD] (review-fixup batch).

**Closure point:** PR #22 squash-merge — at which point the multi-commit branch collapses to a single mainline commit. The `[DEVIATION]` entry stays as a permanent record of the split.

## 2026-04-26 — [DEVIATION Task 55] Phase 2 ships handle as body-pass-through; full CPS path in Phase 3+

**Context:** With E0134 lifted, well-formed `handle` expressions reach codegen. The full plan (CPS calling convention + handler-frame setup + per-arm CPS fn synthesis + trampoline integration) is too large for one commit.

**Deviation:** Phase 2 implements the simplest meaningful codegen path: `handle BODY with { arms }` lowers to just `BODY` when the body contains no non-IO `perform` (statically optimised away — handler arms are dead code at runtime). Programs whose body actually performs a non-IO effect are rejected at codegen entry by the new `unsupported_handle_construct` walker, with a clear in-progress message. Phase 3+ replaces this pass-through with the full handler-frame setup + CPS calling convention + arm synthesis + `sigil_perform` wiring.

**Rationale:** This ships a real codegen path that exercises the full pipeline for handle expressions for the first time (parser + typecheck + monomorphize + color + closure_convert + codegen all touch handle now), without committing to the much larger CPS infrastructure. The cost is that handlers don't actually do anything useful yet — but the surface compiles, the test infrastructure works end-to-end, and Phase 3 can build incrementally on this base. The static walker in `unsupported_handle_construct` is intentionally conservative: it inspects only `Expr::Perform` nodes appearing directly in handle bodies, not transitive performs through called fns. A handle whose body calls a fn that itself performs a non-IO effect would slip through this guard and crash at runtime when `sigil_perform` walks an empty handler stack — acceptable for the Phase 2 test program (body is a literal) but a known footgun until Phase 3+ ships the proper handler-frame setup.

**Implementing commit(s):** `2d69b52`; superseded by Phase 3a (`ef4be8d`), Phase 3b (`d0aa4c4` + `2e7c0de`), and Phase 4a (`adcb897`).

**Closure point:** Phase 3b (`d0aa4c4`) replaced the body-pass-through with real handler-frame setup + per-arm CPS fn dispatch + `sigil_perform` lowering. The Phase 2 codegen-entry guard's "no non-IO perform in body" rejection was lifted in Phase 3b for the supported subset; Phase 4b lifts the zero-arg-perform restriction.

## 2026-04-26 — [DEVIATION Task 55] Phase 3a wires frame ABI without arm dispatch (single-arm/single-effect/no-return handles only)

**Context:** Phase 2 (`2d69b52`) shipped `Expr::Handle` codegen as a body-pass-through (no runtime FFI calls). Phase 3 needs to actually invoke the runtime handler ABI from Task 56 — but full arm-dispatch + `sigil_perform` lowering + per-arm CPS fn synthesis is too large for one commit.

**Deviation:** Phase 3a wires `sigil_handler_frame_new(effect_id, arm_count)` + `sigil_handle_push(frame)` + `sigil_handle_pop()` around every `handle` body but does NOT yet set arm fn pointers (leaves them null). This is safe because the existing `unsupported_handle_construct` codegen-entry guard still rejects programs whose body would actually perform the handled effect — the runtime never reads an arm slot for these handles. Phase 3a additionally tightens the guard to reject multi-arm handles, return arms, and zero-arm handles (defensively) so the codegen path only takes single-arm/single-effect/no-return handles. Phase 3b adds per-arm CPS fn synthesis + `sigil_handler_frame_set_arm` calls + `sigil_perform` lowering for non-IO performs in handle bodies; Phase 4+ adds continuation-using arms (`k` actually invoked) + multi-shot + multi-arm/multi-effect handles + return arms.

**Rationale:** Splitting at the frame-ABI / arm-dispatch boundary lets the runtime FFI plumbing get exercised end-to-end (real `sigil_handler_frame_new` + push + pop calls in compiled output, observable via `objdump`) without committing to the much larger CPS calling convention + synthetic fn synthesis. The Phase 2 e2e test `handle_with_no_perform_in_body_compiles_and_runs` continues to pass through Phase 3a, now exercising the frame ABI on the path Phase 3b builds on. The cost is a tiny runtime regression: the no-perform handle now allocates + pushes + pops a frame on every invocation (previously a no-op pass-through). For Phase 3a this is acceptable; Phase 3b makes the frame functional rather than just present.

**Implementing commit(s):** `ef4be8d` (Phase 3a); superseded by Phase 3b (`d0aa4c4` + `2e7c0de`) and Phase 4a (`adcb897`).

**Closure point:** Phase 3b (`d0aa4c4`) wired arm fn pointers via `sigil_handler_frame_set_arm` between `frame_new` and `push`; the Phase 3a "arms are null" stub no longer exists in the codegen path. Single-arm restriction was lifted in Phase 4a (`adcb897`); single-effect remains pending Phase 4e; return arms remain pending Phase 4f.

## 2026-04-26 — [DEVIATION Task 55] Phase 3b Phase-3b restrictions: literal arm bodies, zero-arg ops, no `k` use, single arm, single effect

**Context:** Phase 3a (`ef4be8d`) wired the runtime handler-frame ABI but kept arm fn pointers null. Phase 3b makes arms actually dispatch — handlers now do real work at runtime. To keep this commit tractable, Phase 3b ships the simplest meaningful subset: arm bodies are literal `Expr::IntLit` only, ops have zero user args, arms can't reference the continuation `k`, single arm per handle, single effect per handle, no `return` arm. Phase 4+ lifts each restriction.

**Deviation:** The synthetic arm fn definition pass at the bottom of `emit_object` lowers arm bodies via a small hand-rolled Cranelift sequence (`iconst(value)` → `call sigil_next_step_done(value)` → `return result`) instead of routing through a full `Lowerer`. This is sufficient for `IntLit`-only arm bodies; richer bodies need a CPS-aware lowerer that handles op-arg unpacking from `args_ptr`, `k` usage via `sigil_next_step_call`, and outer-scope captures via a closure record. Phase 4+ ships that lowerer. The `lower_perform_non_io_to_value` helper similarly assumes zero user args (`args_ptr=null, args_len=0`); args-buffer packing on the perform side and unpacking on the arm side ship together in Phase 4+. The `unsupported_handle_construct` codegen-entry guard enforces every Phase 3b restriction so handlers that escape the supported subset get a clear in-progress diagnostic instead of an obscure runtime crash.

**Rationale:** The simplest meaningful test program — `handle (perform Raise.fail()) with { Raise.fail(k) => 42 }` — exercises the entire FFI surface end-to-end (frame_new → set_arm → push → sigil_perform → arm dispatch → next_step_done → NextStep value extraction → pop) without committing to the much larger CPS calling convention infrastructure. The simplifying restrictions can be lifted one at a time, each as its own focused commit. The single-shot one-shot-arm path is also the most common handler shape in practice (Raise-style early-exit), so it's not just a stepping stone — it covers a real use case.

**Implementing commit(s):** `d0aa4c4` (Phase 3b initial), `2e7c0de` (Phase 3b fixup: route perform's NextStep::Call through `sigil_run_loop` instead of reading `(*ns).value` directly), `adcb897` (Phase 4a: multi-arm single-effect handlers).

**Closure point** (per-restriction):
- *Single arm* — closed in Phase 4a (`adcb897`).
- *IntLit-only arm body* — pending Phase 4c (richer arm bodies via dedicated CPS-aware lowerer).
- *Zero-arg ops* — pending Phase 4b (args-buffer packing on perform side, unpacking on arm side).
- *No `k` use* — pending Phase 4d (continuation reification + lambda-lifting of perform's continuation).
- *Single effect per handle* — pending Phase 4e (frame-per-effect).
- *No return arm* — pending Phase 4f (synthetic return-fn registered via `sigil_handler_frame_set_return`).

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

**Closure point:** Task 55 (when codegen lowers the first Int-typed
user arg into `args_buf`). Task 56 (this PR) ships the runtime-side
data structures, but the `HandlerFrame` and `NextStep` slot types
that would consume `TaggedInt` / `RawInt` newtypes are `*mut u8`
pointers (closure_ptr, fn_ptr) and raw `u64` (args_buf entries). User
`Int` args don't enter the frame layout until codegen lowers them via
`args_buf` — that's the Task 55 boundary. Updated 2026-04-26 in this
PR; the original "Task 56" label predicted the contract would land
with the runtime structs, but the structs themselves don't carry
typed Int slots in v1.

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

## 2026-04-25 — [DEVIATION Task 49] Mangled name format and ordering

**Context:** Plan body specifies "Canonical specialization names use
lexicographically-sorted type arguments with a canonical recursive form
for nested generics: `list_map__List_Option_Int__List_Option_Int`."
Two interpretation gaps:

1. *Lexicographic sort of type arguments.* Read literally, this would
   reorder a fn's type-argument list before mangling — losing the
   positional binding between a declared generic parameter (`A`, `B`) and
   its concrete instantiation. `f[A=Int, B=String]` and
   `f[A=String, B=Int]` are different instantiations; lex-sorting their
   args to `[Int, String]` and `[Int, String]` respectively would
   produce identical mangled names but completely different bodies.
2. *Canonical recursive form.* The example `list_map__List_Option_Int`
   shows nested generics rendered with single underscores within a
   type-arg, double underscores between top-level type-args. The
   plan does not formalise the separator rule.

**Deviation:**
1. Type arguments are emitted in the callee's *declared* generic-
   parameter order (positional), not lex-sorted. The lex-sort interpretation
   is treated as a plan misstatement; positional ordering is the only
   reading that preserves correctness.
2. Canonical form is formalised in `compiler/src/monomorphize.rs` module
   docs and pinned by unit tests:
   - Primitives render as themselves: `Int`, `String`, etc.
   - `User(name, [])` renders as `name`.
   - `User(name, [a1, a2, ...])` renders as `name$<canon(a1)>$<canon(a2)>$...`
     (single `$` between parts within a type-arg).
   - Top-level fn/type instantiation: `fn_name$$<canon(a1)>$$<canon(a2)>$$...`
     (double `$$` between top-level type-args).
   - Ctor of generic type: `ctor_name$$<canon(a1)>$$<canon(a2)>$$...`
     (same suffix as the owning type so the global ctor namespace stays
     unique across instantiations).

**Rationale (round 2 — separator change to `$`):** The first round of
this PR used `_`/`__` separators. Reviewer round-2 feedback (PR #16
comment 4318163326 #1) flagged that the asymmetric underscore separator
is **not** unambiguous when user fn / type names contain underscores:
`type List_Option[A]` instantiated at `Int` mangled to `List_Option_Int`,
which collided with `List[Option[Int]]`'s rendering of `List_Option_Int`.
`type_seen` would silently collapse the two distinct instantiations,
producing miscompiled programs the GC would scan incorrectly.

**Switched to `$` / `$$`** because the lexer rejects `$` as an identifier
character (same constraint Plan A2 relied on for `$lambda_N` synthetic
names from closure conversion). `List_Option$$Int` and `List$$Option$Int`
are now structurally distinct strings regardless of underscore density
in user-declared identifiers. Determinism flows from BTreeMap iteration
order over the reachability worklist; argument ordering is positional
because it's the only ordering that preserves the type-var → concrete-Ty
binding. Codegen's `mangle_user_fn` rewrites `$` to `__` for ELF /
Mach-O linker compatibility — the rewrite preserves AST-level
unambiguity since uniqueness is enforced at the AST level (`fn_seen`
/ `type_seen`) before any linker-symbol step.

**Hardening:** `Ty::Var(_)` and `Ty::Fn(_)` reaching `canon_ty` /
`ty_to_type_expr` now trip `unreachable!` rather than rendering a
placeholder string. Reviewer round-1 feedback (PR #16 comment
4318161359 #2) flagged the placeholders as silent collision vectors —
two `Ty::Var(3)` and `Ty::Var(7)` rendering identically would collide
in `fn_seen`. Closing that path requires both:
  - The new E0132 diagnostic at end-of-typecheck rejecting any pending
    instantiation whose type-arg resolves to an unbound `Ty::Var(_)`
    that isn't an outer-fn's free var.
  - The `unreachable!` arms in mangling so a future regression is loud,
    not silent.

**Acceptance criterion for closure:** if a future plan adds first-class
function-typed values (`TypeExpr::Fn` surface syntax), `canon_ty`'s
`Ty::Fn` arm needs to be replaced with a real rendering rule before
the surface ships. Until then the `unreachable!` fires immediately
on any code path that produces a `Ty::Fn` reaching mangling, surfacing
the gap loudly.

**Implementing commit(s):** Same commit as Task 49 (initial impl) +
the PR #16 review fix-up commit (separator change + E0132 +
hardened arms).

## 2026-04-25 — [DEVIATION Task 49] Effect rows preserved through monomorphization (permanent v1 design choice — informational)

**Status:** **permanent v1 design choice. Informational — no closure
path expected.** Reviewer round-2 feedback (PR #16 comment
4318163326 #4) flagged the original "open-ended / closes if v2 lands"
framing as wrong-shape: row-specialised monomorphs are explicitly
reserved for v2 in the design doc, and v1 effect dispatch is
fundamentally runtime-indirect — there's no v1 path that would
reopen this deviation. Reframed as informational so future readers
don't waste time looking for a closure trigger.

**Context:** Plan body's Task 49 entry: "Effect rows are not
monomorphized in v1 — they remain polymorphic through this phase and
are erased at codegen (effect dispatch is runtime-indirect). This is a
v1 optimization-budget choice; the design doc explicitly reserves
row-specialised monomorphs for v2."

The Task 48 codegen-entry walker (`contains_apply_or_generic_ref`)
treated `f.effect_row_var.is_some()` as a hard rejection. That made
sense at Task 48 (before monomorphization existed and any program
reaching codegen with a row variable was unmonomorphised). Task 49
introduces monomorphization which intentionally preserves
`effect_row_var` per the plan, so the existing walker rule needs
relaxing.

**Deviation:** the walker no longer rejects `f.effect_row_var.is_some()`
or `Expr::Lambda { effect_row_var: Some(_), .. }`. The rejection is
narrowed to:

- `f.generic_params.is_empty()` (still rejected)
- `td.generic_params.is_empty()` (still rejected)
- `TypeExpr::Apply { .. }` anywhere (still rejected)
- `TypeExpr::Named(name, _)` referencing a generic-param surface
  name in scope (still rejected)

Monomorphized fns retain their `effect_row_var` field as a sentinel
that the body's row was polymorphic; codegen treats `Some(_)` as a
no-op (effect dispatch is runtime-indirect; v1 codegen doesn't
emit per-row dispatch tables).

**Rationale:** The plan body explicitly carves out effect rows from
monomorphization. v1 effect dispatch is runtime-indirect — the row
variable serves no codegen purpose past type-checking. v2 row-specialised
monomorphs are reserved for the design doc.

**Closure path:** none for v1. If v2 ever introduces row-specialised
monomorphs (a non-decision today), a fresh deviation entry should be
opened then to track the new code path; this entry stays as historical
record.

**Implementing commit(s):** Same commit as Task 49.

## 2026-04-25 — [DEVIATION Task 49] Pattern-ctor rewriting via scrutinee Ty + per-sub-pattern field-type threading

**Context:** Plan body says "Typed IR preserved" but does not specify
how monomorphization should resolve constructor names inside `match`
arm patterns. The typecheck instantiation index keys construction sites
(call / record-lit / unit-ident); pattern-ctor sites are not
constructions and don't get their own instantiation entry.

**Deviation:** Pattern ctors are rewritten by combining two existing
indices: the scrutinee's `Ty::User(name, args)` from
`CheckedProgram::match_scrut_tys`, plus the `ctor_to_type` reverse
index built from the original program's TypeDecls. For
`Pattern::Ctor { name: "Some", .. }` inside a match whose scrutinee
typed as `Option[Int]`, the rewriter looks up "Some" → "Option" via
`ctor_to_type`, observes the scrutinee's args `[Int]`, and produces
`Some$$Int`.

**Per-sub-pattern field-type threading.** Sub-patterns of a generic
ctor pattern see the field's resolved `Ty` as their inner scrutinee,
not the outer scrutinee's `Ty`. For `match opt: Option[List[Int]] { Some(Cons(h, t)) => ... }`:
- Outer pattern `Some(Cons(h, t))` rewrites against scrut_ty `Ty::User("Option", [Ty::User("List", [Ty::Int])])` → mangled `Some$$List$Int`.
- The variant `Some` of `Option` has positional field of declared type `Named("A")`. Under the substitution `A := List[Int]` (built from `Option`'s `generic_params` zipped with the scrut's args), the field's resolved Ty is `Ty::User("List", [Ty::Int])`.
- Inner pattern `Cons(h, t)` rewrites against the field-resolved Ty → mangled `Cons$$Int`.

Implemented via `Monomorphizer::variant_field_types` and the
local `ty_from_type_expr_under_subst` helper in `monomorphize.rs`.
Mirrors the same surface-name → Ty substitution mechanism the rest
of the rewrite pass uses; reuses the existing typecheck-built type
registry for variant field lookup.

**Rationale:** Avoids extending the typecheck instantiation index with
a third map keyed by pattern span. The two-index lookup plus the
field-type threading covers all v1 generic patterns including nested
ctor patterns (`Option[List[Int]]`, `Result[T, E]`, `Tree[T]`, etc.).

**Round-3 review correction:** an earlier version of this code
`unreachable!`d when a pattern ctor's owning type didn't match the
*outer* scrutinee's User type, on the false assumption that v1 surface
couldn't construct the case. v1 surface fully supports nested ctor
patterns via `parser.rs::parse_pattern`'s positional-field recursion;
running `match opt: Option[List[Int]] { Some(Cons(h, t)) => ... }` 
triggered the panic on a legitimate program. The current per-sub-
pattern field-type threading is the correct mechanism. Test
`nested_generic_ctor_pattern_threads_inner_scrut_ty` pins the fix
against the reviewer's exact reproducer.

**Acceptance criterion for closure:** v1's pattern surface ships
unchanged in Tasks 50–52; deviation closes when Stage 5 review
checkpoint accepts the implementation. If Plan C / v2 introduces
GADT-style refinement (where a sub-pattern's type depends on
information *not* recoverable from the parent's variant signature
alone), monomorph gains a per-pattern instantiation index.

**Implementing commit(s):** Initial Task 49 impl commit, plus the
PR #16 round-3 fix-up (per-sub-pattern field-type threading +
nested-generic regression test).


## 2026-04-25 — [VERIFICATION DEBT — flag for Task 60] Perf-floor wall-clock instability on debug builds

Surfaced during PR #18 (Tasks 51 + 52) review. Pre-existing on `main`;
not Task 51's fault, but recorded here so Task 60 (performance floor)
doesn't have to rediscover it.

**Symptom:** `fib_perf_example_prints_6765_under_50ms` and
`tree_example_prints_32767_under_500ms` exceed their declared wall-
clock floors by ~4x on `aarch64-apple-darwin` in debug mode (~200ms
each observed during local manual exercise). CI runs use release
profile and stay green; the gap manifests only when running the e2e
test suite via plain `cargo test --workspace --no-fail-fast` (the
default profile).

**Plan A2 / A3 acceptance text** for both tests does not specify
whether the floor is debug-profile-relative or release-profile-only.
The release-only interpretation is the practical one given Cranelift's
debug-build penalty, but the literal test reads "wall-clock {elapsed:?}
exceeds the 500ms Plan A3 floor" without profile qualification.

**Why this matters now:** Plan B Task 60 introduces THREE new
performance-floor tests (native `fib(20)` <50ms, CPS-forced `fib(20)`
<500ms, multi-shot Choose <5s). The pre-existing instability on debug
builds means a Task 60 author who runs the new tests locally without
remembering to set `--release` will see flaky failures and may waste
time chasing a non-bug. Worse, if the new floors are tightened on
the same debug-vs-release ambiguity, Task 60's CI gates will be
unreliable.

**Resolution options for Task 60:**
1. **Profile-aware floors:** detect `cfg!(debug_assertions)` in the
   test bodies and apply a multiplier (e.g., 10x) for debug builds.
   Backwards-compatible; touches all five existing + new perf tests.
2. **Release-only perf gates:** `#[cfg(not(debug_assertions))]` on the
   timing-assert sections; debug runs verify correctness only and
   skip the wall-clock check. Cleanest semantically; explicit about
   the gate's scope.
3. **Document and leave alone:** add a top-of-file note to
   `compiler/tests/e2e.rs` saying perf gates assume release profile,
   plus an `eprintln!` warning at runtime if `debug_assertions` is
   set. Cheapest; punts the underlying brittleness.

The reviewer of PR #18 explicitly noted: "Either platform-aware
tightening of the perf gates or a release-only mode would close
this." Task 60 is the natural moment to pick option 1 or 2.

**Acceptance criterion for closure:** Task 60 PR documents which
option it picked, applies it across both pre-existing perf tests
and the three new Stage 6 perf tests, and updates this entry to
"closed" with the implementing commit hash.

**Implementing commit:** None yet — this entry tracks the debt
forward. Surfacing entry shipped with PR #18 review fixups.

---

## [DEVIATION Task 53] Handler op-arm shape uses qualified `Effect.op(...)`

**Plan body says** (Stage 6 task 53, paraphrased): handler arms have
the shape `op(args, k) => arm`.

**Implemented form** (this commit): `Effect.op(p1, ..., k) => arm` —
the discharged effect's name is required as a prefix on every
operation arm.

**Why the deviation:**

1. A single `handle` block can discharge operations from more than
   one effect. The bare `op(...)` form would require Task 54 to
   resolve each `op` against the entire effect registry to disambiguate
   when two effects declare an op of the same name; qualified form
   makes the discharge target lexically explicit and removes that
   resolution ambiguity at the parser level.
2. Symmetry with `perform Effect.op(args)`: both the produce-side
   (`perform`) and consume-side (handler arm) name the effect
   explicitly. Sigil's effect-typing rules (Task 54) treat an arm as
   "discharges Effect's op"; the surface mirroring keeps that rule
   readable without an effect-inference step.
3. The plan body's bare-op shape was illustrative — the design doc
   `docs/plans/2026-04-21-sigil-design.md` does not lock in the
   parser surface, only the typing semantics.

**Cost of the deviation:** programs read slightly more verbosely
(`Raise.fail(msg, k) => 0` vs. `fail(msg, k) => 0`). Acceptable for
v1 surface clarity. If Task 54+ ergonomic study finds that the
unqualified form is overwhelmingly preferred, a future plan can lift
the qualifier and resolve operation names against the typed handler
context — this is a strict-extension change, not a breaking one
(qualified arms remain valid).

**Implementing commit:** Task 53 parser scaffolding (this PR).
**Closure point:** open — tracked by `QUESTIONS.md` entry
`[PLAN-B] Task 54: revisit handler arm surface (qualified-only vs
bare-op-as-sugar)` (2026-04-25). Task 54 review can revisit the
choice without code-rewrite cost (the AST already records both
`effect` and `op` names; an unqualified form would just relax the
`effect` field to `Option<String>` and let the typechecker's
context fill it in).

---

## [DEVIATION Task 53] `resumes` and `many` shipped as context-sensitive idents (not lexer keywords)

**Plan body / design doc says** (Stage 6 task 53 + `docs/plans/2026-04-21-sigil-design.md:61` keyword list, paraphrased): the `effect Name resumes: many { ... }` surface form lists `resumes` and `many` alongside `effect`, `handle`, `with` in the keyword set.

**Implemented form** (this commit): only `effect`, `handle`, and `with` are reserved by the lexer. The attribute words `resumes` and `many` stay as plain `TokenKind::Ident(_)` and are matched contextually by string inside `parse_effect_decl` via the dedicated `eat_resumes_many_attr` helper.

**Why the deviation:**

1. **Narrower name reservation.** Reserving `resumes` and `many` as lexer keywords would break user code that legitimately wants `let resumes = 5` or `let many = 9`. Both names are common English words; the cost of reserving them outweighs the benefit, since the surface only references them in the single `effect <Name> resumes : many { ... }` position where context-sensitive matching is unambiguous.

2. **Contextual unambiguity.** The position immediately after `effect Name` (and after the optional `[GenericParams]` header) is the only place either word carries semantic meaning. The parser sees the structure `effect Name [Generics?] (Ident=`resumes` Colon Ident=`many`)? LBrace ...`; the three-token sequence is unambiguous against any other valid continuation (which would be `LBrace` directly).

3. **Forward-compatible.** A future plan that introduces additional `effect`-decl attributes (e.g., a hypothetical `resumes: at_most_one`, or a `linear: true` annotation) can extend the contextual matcher without growing the lexer keyword table.

**Test coverage for the deviation:**

- `lexer::tests::resumes_and_many_remain_idents` pins that `let resumes = many;` lexes as plain Idents.
- `parser::tests::resumes_outside_effect_decl_remains_ident` is the parse-side regression guard: `let resumes: Int = 5; let many: Int = 9; resumes` round-trips through the full parser without misclassifying the names.
- `parser::tests::effect_decl_resumes_without_many_errors` pins the `:` requirement and the `many`-only restriction inside the attribute position.

**Cost of the deviation:** zero, in practice. Programs that want either word as a variable name are not regressed; programs that want the multi-shot annotation continue to write `effect E resumes: many { ... }` exactly as the design doc shows.

**Implementing commit:** Task 53 parser scaffolding (this PR).
**Closure point:** closed — context-sensitive matching is the chosen long-term form. If Task 54+ ergonomic data argues for adding `resumes` to the keyword set later, the change is a strict reservation: any program that broke under it would have used a now-reserved word as an identifier. The lexer test serves as the regression guard.

---

## 2026-04-25 — [DEVIATION Task 54] E0133 / E0134 staged-feature gates remain live through Task 54

**Plan body says** (Stage 6 Task 54, paraphrased): the typechecker
gains row-polymorphic effect checking, handler typing rules
(consumes effect from body's row, produces residual row), partial
handling, and the one-shot linearity check (E0220). The body of
the task reads as if `effect` declarations and `handle` expressions
should be fully accepted by typecheck after this task.

**Implemented form** (this PR): the typechecker now does the full
handler-typing work *internally* — effect declarations are
registered into a real registry; `Expr::Handle` extends the
environment with op-arm parameters and the continuation `k`; the
body is checked under an extended row that includes the discharged
effects; arm bodies are checked under the caller's row with their
op's parameter and continuation types installed; arm bodies unify
with a single handler-overall type; E0220 fires when the linearity
check rejects a path. **But E0133 still fires once per
`Item::Effect` and E0134 still fires once per `Expr::Handle`** as
the user-facing staged-feature gate.

**Why the deviation:**

1. **Codegen-gate alignment with Task 55.** Lifting E0134 in this
   PR would let well-formed handler programs flow into the
   monomorphizer, color inference, closure conversion, and codegen
   — all of which still treat `Expr::Handle` as `unreachable!` (the
   CPS transform that lowers `handle ... with { ... }` into
   `sigil_perform` calls plus a trampoline frame is Task 55's
   scope). A program that types cleanly under Task 54 but trips
   `unreachable!` at codegen would surface as an internal-compiler
   bug (`E0001`) instead of a staged-feature notice. Keeping E0134
   live preserves the user-visible contract: until Task 55 lands,
   `handle` is "recognised but not yet runnable".
2. **Internal infrastructure ships now, gate lifts later.** The
   typecheck work *cannot* wait for Task 55 — Task 55 needs the
   effect registry and the handler-typed AST to drive its CPS
   expansion. Doing the typechecking work behind the gate means
   Task 55's PR is a single focused change ("lift E0134 + wire CPS
   transform") rather than an entangled "type-check + lower"
   cross-cutting change.
3. **E0220 still surfaces alongside E0134.** The linearity check
   runs even though the gate fires; users get the supplementary
   diagnostic so when Task 55 lifts the gate, the linearity rules
   are already enforced and tested. Task 53's
   `handle_arm_bodies_walked_during_e0134_emission` test
   established the precedent that E0134 emission does not suppress
   per-arm-body diagnostics.
4. **Task 53 review item 11 prefers this approach.** PR #19's
   review explicitly recommended "keep E0134 live until Task 55
   lands" as the preferred option for codegen-gate alignment, with
   "extend the codegen walker to short-circuit Expr::Handle" as
   the alternative. We follow the preferred option.

**Cost of the deviation:**

- **User-visible:** programs that use `effect` / `handle` syntax
  still cannot compile under Task 54 — same surface as Task 53
  shipped. Users wait one more task (Task 55) before handlers
  execute.
- **Catalog hygiene:** E0133 and E0134 stay in the catalog one
  task longer than the catalog text suggested ("Until Task 54
  merges" / "Tasks 54 and 55"). Updated the long-form text on both
  entries to make the joint-ownership explicit.
- **Internal complexity:** the typechecker walks handle/effect
  shapes twice — once for E0133/E0134 emission, once for proper
  registration / typing. The two walks are interleaved (the proper
  walk runs first, the gate emission runs at the end) so cost is
  one structural traversal, not two.

**Test coverage for the deviation:**

- `effect_decl_emits_e0133` regression — Task 53's existing test;
  still passes after the registry pre-pass lands.
- `handle_expr_emits_e0134` regression — Task 53's existing test.
- `handle_arm_bodies_walked_during_e0134_emission` — Task 53's
  existing test pins that arm-body diagnostics fire alongside
  E0134; Task 54 keeps this contract.
- New tests for op-arm-binding env extension (item 10): a handler
  arm body referring to `Effect.op`'s declared param names no
  longer fires spurious E0046 alongside E0134.
- New tests for E0220 firing alongside E0134.

**Implementing commit:** [HEAD]
**Closure point:** Task 55 lifts E0134 and wires the CPS expansion;
this deviation entry stays as the rationale trail. E0133 lifts at
the same time (effects-in-codegen need Task 55's `sigil_perform`
machinery before `Item::Effect` can usefully reach codegen).

---

## 2026-04-25 — [DEVIATION Task 54] One-shot linearity check uses path-max syntactic counting; lambda capture is conservative

**Plan body says** (Stage 6 Task 54, paraphrased): "Zero uses
(early exit) is fine; one use is fine; any path that uses `k`
twice, or splits and uses it on both branches, is an error
(E0220). [...] branches must agree on its fate."

**Implemented form** (this PR): the check counts syntactic
occurrences of `Ident(k_name)` in an op-arm body, with branching
constructs (`if` / `match`) using the **maximum** count across
branches and sequential composition (`block` statements,
`Binary` / `Call` argument lists, etc.) using the **sum** count.
A count greater than 1 along any path emits E0220. Bindings in
the arm body that lexically shadow `k` (a nested `let k = ...` or
a nested `handle` whose op-arm rebinds `k` with the same name)
suspend counting for that subtree.

**One additional rule:** any reference to `k` from inside an
`Expr::Lambda` body emits E0220 immediately, regardless of count
along paths inside the lambda. Lambdas can be invoked any number
of times by the surrounding code; capturing `k` into a closure
means the closure could call `k` repeatedly even if its body
references `k` exactly once syntactically. The conservative rule
rejects all such captures up-front.

**Interpretation choice on "branches must agree on its fate":** the
plan's wording admits two readings:

- **(a) path-max:** each path through the arm body uses `k` at
  most once. Branches that disagree (one uses `k`, the other
  doesn't) are still valid — each path independently respects
  the linearity bound. *This is what the implementation does.*
- **(b) path-uniform:** all paths through the arm body must use
  `k` exactly the same number of times — either all use it
  exactly once, or none use it at all.

**Reason for choosing (a):** the explicit "Zero uses (early exit)
is fine" sentence in the plan body endorses early-exit handlers,
which trivially break path-uniformity (the early-exit path uses
`k` zero times; the normal path uses it once). Reading the plan
as (b) would make the example sentence and the rule contradict.
Reading (a) as the rule is consistent with both halves of the plan
text. Linear-logic literature (Linear Haskell, Frank, Eff) also
uses path-max counting for one-shot continuations.

**Cost of the deviation:** users who write a closure-captured `k`
get a strict-pessimistic rejection rather than a more nuanced
analysis. Workarounds: invoke `k` directly from the arm body
(common case for one-shot effects); or annotate the effect with
`resumes: many` if multi-shot semantics are intended. If real
programs surface false-positives, a future plan can refine the
rule using escape analysis without breaking source compatibility
(strict-pessimistic acceptances remain valid).

**Test coverage:**

- `linearity_zero_uses_is_fine` — early-exit handler with no `k`
  reference passes.
- `linearity_one_use_is_fine` — single `k` invocation passes.
- `linearity_two_uses_in_sequence_is_e0220` — `k(0); k(1)` (or
  any sequential pattern) fails.
- `linearity_branches_use_k_independently_is_fine` — `if cond { k(0) } else { k(1) }`
  passes (each path uses `k` once).
- `linearity_branch_then_extra_use_is_e0220` — `if cond { k(0) } else { 0 }; k(1)`
  fails (one path uses `k` twice).
- `linearity_lambda_captures_k_is_e0220` — `let f = fn () -> Int ![] => k(0); f()`
  fails (conservative lambda rule).
- `linearity_multi_shot_skips_check` — `effect E resumes: many`
  arms can use `k` any number of times.
- `linearity_shadowed_k_does_not_count` — a nested binding that
  shadows `k`'s name does not contribute to the outer linearity
  count.

**Implementing commit:** [HEAD]
**Closure point:** open — refinements (escape analysis for
lambda captures; `Linear[Bool]`-style use-count types) can land in
a future plan without breaking the strict-pessimistic surface.

---

## 2026-04-26 — [DEVIATION Task 56] Task 56 lands before Task 55

**Plan body** numbers Task 55 (CPS transform on CPS-color
monomorphs; arena-allocated `NextStep` records) before Task 56
(runtime: `HandlerFrame`, arena, `sigil_perform`, `run_loop`,
counters). The numerical order suggests codegen first, runtime
second.

**Implemented order:** Task 56 ships first in this PR; Task 55
follows in the next PR.

**Reasoning:** Task 55 lowers `Expr::Perform` and `Expr::Handle`
to calls into `sigil_perform`, `sigil_handle_push`,
`sigil_arena_alloc`, and `sigil_run_loop`. Those symbols are
provided by Task 56. Implementing Task 55 first would require
either:

1. Stub runtime symbols that abort at runtime (the pre-Plan-B
   state) — but then no e2e test can run, defeating the
   acceptance signal.
2. Inlining a temporary runtime in Task 55's PR that Task 56
   later replaces — wastes review cycles on throw-away code.
3. Combining 55 + 56 in a single mega-PR — the resulting diff
   would be 3000+ LOC across both subsystems, against Plan B's
   established cadence of one task per PR (PRs #15, #16, #17,
   #19, #20). PR #18 was the only multi-task PR and combined the
   2 LOC P16/P17 prompts with the 600+ LOC generic_map example.

Order swapped because Task 56 is independently testable in
isolation (Rust unit tests against the FFI symbols) and ships a
clean, focused review surface. Task 55 lands as soon as the
runtime is in main and reviewer-approved.

**Plan order vs implementation order:**

| Task | Plan body order | Ship order |
|------|-----------------|------------|
| 55   | first           | second     |
| 56   | second          | first      |

**`PLAN_B_PROGRESS.md` reflects this:** Task 55's entry stays
`todo` after this PR; Task 56's entry flips to `done-pending-ci`.
The Task 55 PR will flip Task 56 to `done` per the Plan A2
PROGRESS-hygiene precedent (next PR closes the prior PR's
done-pending-ci).

**Implementing commit:** 9c6213e
**Closure point:** closed at Task 55 PR merge (Task 55 is the
direct consumer of Task 56's runtime surface; absent Task 55,
Task 56's surface is dead code that the runtime ships but
nothing calls).

---

## 2026-04-26 — [DEVIATION Task 56] Uniform CPS calling convention via packed args buffer

**Plan body** (Stage 6 Task 55, paraphrased): "Handler arms
become closures stored in the handler frame; the continuation
`k` is passed to operation arms as an ordinary argument
(post-CPS, continuations are values)." The plan implicitly
assumes typed direct calls into CPS-color fns (e.g.
`fn(closure_ptr, T1, T2, ...) -> NextStep` per fn).

**Implemented form:** every CPS-color fn shares the uniform
signature

```text
extern "C" fn cps_fn(
    closure_ptr: *mut u8,
    args_ptr:    *const u64,
    args_len:    u32,
) -> *mut NextStep
```

User arguments are widened to `u64` and packed into a
caller-supplied buffer. Codegen emits an unpacking prologue per
CPS-color fn that reads the args from the buffer according to
the fn's known surface signature.

**Reason for the deviation:** the trampoline (`sigil_run_loop`)
dispatches `NextStep::Call` records by invoking the carried fn
pointer. The fn's static signature varies per call site, but the
trampoline only sees the dynamic `NextStep` payload. With typed
direct calls, the trampoline would need either:

1. **Per-arity dispatch** (`match arg_count { 0 => f0(...), 1 =>
   f1(c, a0), 2 => f2(c, a0, a1), ... }`) capping the maximum
   arity at compile-time and producing N transmute sites.
2. **Hand-rolled assembly** to push `arg_count` u64 values onto
   the calling-convention argument registers/stack and dispatch.
   Non-portable across `x86_64-unknown-linux-gnu` /
   `aarch64-apple-darwin`.
3. **A per-fn thunk** emitted by codegen that reads args from a
   buffer and tail-calls the typed body. Same total cost as the
   uniform convention but with extra indirection.

Option (1) caps effects to a fixed maximum arity and inflates
code size with N variants. (2) is per-platform unsafe code. (3)
is functionally equivalent to the uniform convention chosen
here, just with a syntactic difference. The uniform convention
keeps the trampoline portable, lets codegen emit a single CPS
prologue shape per fn, and eliminates the arity dispatch
problem entirely.

**Cost:** every CPS-color fn pays a small per-call cost reading
its args from the buffer (typically 1–3 64-bit loads). Cranelift
inlines the load chain into the prologue; on benchmark
workloads the overhead is dominated by the trampoline dispatch
itself, not the unpack. Native-color fns (the common case for
non-effect arithmetic like `fib`) keep the existing direct
calling convention with no change.

**Implementing commit:** 9c6213e
**Closure point:** Task 55 ships the codegen prologue. If a
performance-floor breach traces to the unpack overhead, the
fallback is option (3) (per-fn thunk) which keeps the
trampoline-side ABI unchanged.

---

## 2026-04-26 — [DEVIATION Task 56] HandlerFrame reuses TAG_CLOSURE; Boehm-only GC tracking

**Plan body** (Stage 6 Task 56) defines `HandlerFrame` with a
specific shape but leaves the heap layout / object-tag question
implicit.

**Implemented form:** HandlerFrame heap objects are allocated
via `sigil_alloc` with `TAG_CLOSURE` reused as the object tag.
The 32-bit GC pointer bitmap explicitly marks the
`return_closure`, `prev`, and per-arm `closure_ptr` slots so
Boehm's mark phase walks them correctly. Function pointers
(`return_fn`, `arms[i].fn_ptr`) are NOT marked — they reference
`.text` not the GC heap.

**Reason for tag reuse:** introducing `TAG_HANDLER_FRAME` would
require extending `sigil-header-constants` (the workspace crate
that owns the canonical 8-byte object header), which is shared
across compiler and runtime and outside Task 56's scope. Boehm
only consumes the pointer bitmap, not the tag, so the overload
is functionally inert today. A future GC walker that
introspects tags can add `TAG_HANDLER_FRAME` in a single line
without touching this allocation site.

**Capacity bound:** `MAX_HANDLER_ARMS = 13`. Bounded by the
32-bit pointer bitmap: arm `i`'s closure_ptr lives at payload
word `5 + 2*i`, so bit 31 corresponds to arm 13. v1 effects
ship with 1–3 ops; the cap is comfortably above realistic v1
needs. A future relaxation requires widening the bitmap field
in the Sigil object header (out of scope for Plan B).

**Implementing commit:** 9c6213e
**Closure point:** open — TAG_HANDLER_FRAME slot can land in a
future plan if a tag-aware GC walker arrives. The layout
otherwise stable.

---

## 2026-04-26 — [DEVIATION Task 56] Runtime TLS roots: register/unregister via Boehm `GC_add_roots`

**Plan body** (Stage 6 Task 56) describes `HandlerFrame` and the
trampoline arena without saying how either is reachable from Boehm's
mark phase. PR #21 review (boldfield, 2026-04-26) flagged the gap as
Critical #1 / M1: TLS storage holding the `HANDLER_STACK` head and the
arena's `Vec<u64>` payload sits outside Boehm's automatic
stack/data-segment scan, so a `HandlerFrame` reachable only through
`HANDLER_STACK` (or a closure pointer reachable only through an arena
slot) would be reclaimed mid-iteration on any nontrivial GC pressure.

**Implemented form:** in `sigil_gc_init` after `GC_init`, the calling
thread's `HANDLER_STACK` cell and `ARENA` storage range are registered
with `GC_add_roots`, idempotent per thread via per-thread
`HANDLER_STACK_ROOTED` and `ARENA_ROOTED` flags. The arena's
`Vec<u64>` is non-reallocating after the first `try_reserve_exact` so
the registered range stays valid for the thread's lifetime.

**Test-mode caveat:** `cargo test` spawns a fresh thread per test;
auto-registering each test thread's TLS would leak stale ranges in
Boehm's root list when the thread exits, which segfaults the next
collection. The auto-registration is therefore `cfg(not(test))`-only;
test code opts in via `GcThreadEnrolment::acquire` (in
`test_support`), an RAII guard that registers AND unregisters
symmetrically on Drop (using `GC_remove_roots` + `GC_unregister_my_thread`).

**Conservative-scan tradeoff:** Boehm scans the registered arena range
`[start, start + capacity*8)` byte-by-byte and follows any
pointer-shaped 8-byte value. The arena holds non-pointer u64 args
alongside genuine `closure_ptr` slots; values that happen to alias
Boehm-heap addresses pin those blocks until the next reset. The
pinning is bounded by one trampoline iteration (every reset clears
`len` to 0 AND zeros the just-cleared bytes so subsequent scans see
only the active iteration's writes). Acceptable for v1; v2 may
revisit by tracking precise pointer slots per `NextStep`.

**Verification:** PR #21 ships three GC stress tests
(`handler_frame_survives_forced_gc_while_pushed`,
`closure_in_handler_arm_slot_survives_gc`,
`closure_in_next_step_survives_gc_via_arena_root`) that allocate
sentinel-bearing closures, push handlers, force `GC_gcollect`, and
verify the survival contract. The tests run by default under `cargo
test`. Each one's outer `#[test]` body re-execs the test binary with
`--exact handlers::tests::<name> --nocapture` and a
`SIGIL_GC_STRESS_INNER=1` env var; the inner subprocess runs only that
single test, drops its `GcThreadEnrolment`, and exits. The OS reclaims
all per-process state, so the Boehm thread enrolment / re-enrolment
issue (cargo test thread teardown + Boehm thread re-registration on
the next test in the same process) cannot accumulate stale ranges
across tests.

The subprocess pattern is the test-only counterpart to v1's
single-threaded production model: every stress scenario runs on its
own pristine thread that owns its registration for its lifetime. v2's
multi-threaded trampoline will need a precise per-thread root
lifecycle anyway; the production rooting contract is unchanged by
this test-harness adaptation.

**Implementing commit:** [HEAD]
**Closure point:** closed at PR #21 merge.

---

## 2026-04-26 — [DEVIATION Task 56] `MAX_INLINE_ARGS = 32` cap with bound-check at perform site

**Plan body** doesn't specify a maximum effect arity. PR #21 review
M5 / Important #7 / Important #8 flagged that `sigil_perform`'s
trampoline-side check at `MAX_INLINE_ARGS = 32` (a) named the wrong
layer in the error message (perform was the source, run_loop was the
abort site), (b) wasted the arena allocation before discovering the
overflow, and (c) hid a magic number Task 55's codegen will need to
respect.

**Implemented form:** `pub const MAX_INLINE_ARGS: u32 = 32` exported
at the `handlers` module top, sized to comfortably exceed v1's effect
arities (Raise, State, Choose all use 0–2 user args; the cap covers
arbitrary one-shot effects with a hefty safety margin). Bound check
moved up the stack:

1. `sigil_perform` checks `args_len.saturating_add(2) > MAX_INLINE_ARGS`
   first thing, naming the offending `effect_id` / `op_id` in the
   abort message. The `+2` covers the implicit `(k_closure, k_fn)`
   pair the runtime appends to the dispatched arg vector.
2. `sigil_next_step_call` performs the same check on its own
   `arg_count` argument so codegen sites that bypass `sigil_perform`
   (direct CPS-color calls in Task 55) also fail fast.
3. `sigil_run_loop` keeps the trampoline-side check as
   defense-in-depth with a "bypassed perform/next_step_call?" message
   so a future regression that constructs NextSteps without going
   through the helpers is still caught.

**Codegen impact for Task 55:** any user effect operation with more
than 30 user args (`MAX_INLINE_ARGS - 2`) requires boxing — codegen
must emit a heap-allocated args record with a single pointer in
`args_buf` rather than packing the args inline. v1 has no such
operations; the cap is a forward-compat boundary, not a current
constraint.

**Implementing commit:** [HEAD]
**Closure point:** Task 55 (when codegen produces the first
`sigil_perform` call site that needs to consult `MAX_INLINE_ARGS`).
The cap can be raised in a future plan by changing one constant and
re-checking call sites; v1 lacks any concrete need.

---

## 2026-04-26 — [DEVIATION Task 56] `Vec::reserve` panic-on-OOM does NOT cross the FFI boundary

**Plan body** doesn't specify allocation-failure behavior for the
arena. PR #21 review Critical #3 flagged that the original
implementation called `Vec::reserve`, which panics on OOM; panic
unwinding across an `extern "C"` boundary (the surrounding
`sigil_arena_alloc`) is undefined behavior under the workspace's
default `panic = "unwind"` profile.

**Implemented form:** the arena's first-time reserve uses
`try_reserve_exact(INITIAL_CAPACITY_WORDS)`. On `Err`, the code
prints a diagnostic to stderr and aborts via `std::process::abort()`
— matching the existing abort-on-overflow pattern at the bump path.
No panic, no unwind, no FFI-boundary UB.

**Implementing commit:** [HEAD]
**Closure point:** closed.

---

## 2026-04-26 — [DEVIATION Task 56] Arena alignment via `Vec<u64>` backing storage

**Plan body** says NextStep records hold word-aligned u64 fields. PR
#21 review Important #5 flagged that the original `Vec<u8>` backing
relied on the system allocator returning ≥8-byte-aligned blocks,
which is true on every platform Sigil targets but is not a Rust
guarantee.

**Implemented form:** the arena's backing storage is `Vec<u64>`
instead of `Vec<u8>`. `u64`'s natural alignment guarantees the
`Vec`'s allocation base is 8-byte aligned regardless of allocator
behavior. Byte-level pointer arithmetic on the returned `*mut u8`
preserves alignment because every allocation rounds the byte size up
to a multiple of `ALIGN = 8` and the underlying word count advances
in u64 units. A test
(`alloc_round_trips_and_aligns_to_eight`) asserts both
relative offsets AND absolute alignment of every returned pointer.

**Implementing commit:** [HEAD]
**Closure point:** closed.
