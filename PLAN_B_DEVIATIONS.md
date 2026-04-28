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

**Rationale:** The all-or-nothing alternative (single commit landing every piece together) would require holding the entire ~2000-LOC change in working state across multiple sessions before any pod-verify or CI checkpoint. Splitting at the asymmetric-gate boundary gives a clean intermediate state where (1) effect-only programs gain a real codegen path immediately, (2) handle-using programs continue to surface E0134 with a clear "in-progress" message, and (3) each follow-up commit can be pod-verified independently. The **single-PR convention** from Tasks 49 / 53 / 54 / 56 still holds *for the foundation → Phase 4a chunk* — that chunk landed as a single PR (#22). The post-Phase-4a Phase 4 sub-tasks (4b, 4c, 4d, 4e, 4f) each land as their own focused PR — see the cadence-pivot addendum below.

**Implementing commit(s):** `b3af204` (foundation phase: E0133 lift + entry walker), `2d69b52` (Phase 2 minimum: E0134 lift + handle body-pass-through + effect/op IDs + e2e tests), `ef4be8d` (Phase 3a), `d0aa4c4` + `2e7c0de` (Phase 3b), `adcb897` (Phase 4a), `54b4a60` + `95472ec` + `6057373` + `ca32659` (review-fixup batch). All squash-merged via PR #22 as `2f56e87` on 2026-04-26. Phase 4b lands separately on `plan-b-task-55-phase-4b` via PR #23 — see the cadence pivot below.

**Closure point:** PR #22 squash-merge for the foundation → Phase 4a chunk (closed at `2f56e87`). For the rest of Task 55 (Phases 4b–4f), the closure point is "all six Phase 4 PRs squash-merged" — currently 1/5 (Phase 4b at PR #23, in flight at the time of this addendum).

**Cadence pivot (added 2026-04-26 in Phase 4b):** the original Deviation text claimed *"only one PR opens for Task 55"* but **that turned out wrong in practice**. After PR #22 landed, the remaining Phase 4 sub-tasks (4b–4f) pivoted to one PR per phase against `main`, each on its own branch (`plan-b-task-55-phase-4b`, etc.) — not commits on a continuing `plan-b-task-55` branch. The pivot reasoning:

- **Reviewability:** PR #22 was already 2,373/-272 lines (8 files) and required two review rounds with 4 fixup commits; bundling 4b–4f on the same branch would have produced a ~6,000+ line PR with even more review surface.
- **Independent CI cycles:** each Phase 4 sub-task is an independent restriction-lift; its CI failure is unlikely to be related to a sibling phase's CI failure. Per-phase PRs let CI fail isolated, not as a single bisect-unfriendly red.
- **Rollback granularity:** if Phase 4d turns out wrong (e.g., the colorer's handler-discharge refinement isn't ready), reverting one PR is cleaner than reverting one commit out of a bundle.
- **Review checkpoint cadence:** Plan B says *"every change in this plan should be reviewed by a human before merging."* Per-phase PRs honor that more naturally — each phase gets its own review pass instead of bundling restriction-lifts that the reviewer has to mentally split apart.

The original Deviation text's "single-PR convention" claim is preserved unchanged above for historical accuracy, with this addendum to record what actually happened. Future agents resuming Task 55 work after this point should expect the per-phase-PR cadence: branch `plan-b-task-55-phase-{4c,4d,4e,4f}` against `main`, one focused PR per phase.

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

**Rationale:** The simplest meaningful test program — `handle (perform Raise.fail()) with { Raise.fail(k) => 42 }` — exercises the entire FFI surface end-to-end (`frame_new → set_arm → push → sigil_perform → sigil_run_loop dispatch → arm fn → next_step_done → run_loop returns u64 → pop`) without committing to the much larger CPS calling convention infrastructure. The Phase 3b fixup commit (`2e7c0de`) inserted the `sigil_run_loop` step between `sigil_perform` and the value extraction; codegen no longer reads the `NextStep` layout directly (it just consumes `run_loop`'s `u64` return). The simplifying restrictions can be lifted one at a time, each as its own focused commit. The single-shot one-shot-arm path is also the most common handler shape in practice (Raise-style early-exit), so it's not just a stepping stone — it covers a real use case.

**Implementing commit(s):** `d0aa4c4` (Phase 3b initial), `2e7c0de` (Phase 3b fixup: route perform's NextStep::Call through `sigil_run_loop` instead of reading `(*ns).value` directly), `adcb897` (Phase 4a: multi-arm single-effect handlers), `[HEAD]` (Phase 4b: args-buffer packing on perform side).

**Closure point** (per-restriction):
- *Single arm* — closed in Phase 4a (`adcb897`).
- *IntLit-only arm body* — closed in Phase 4c (`[HEAD]`). Synthetic CPS arm fn now lowers its body through a real `Lowerer` instance with op-args bound from `args_ptr` at fn entry; bodies can use any expression over op-args + globals (top-level fns, ctors, builtins). The Lowerer-driven path handles arithmetic, conditionals, calls, and IO performs naturally; the perform side mirror-narrows `sigil_run_loop`'s I64 result to the op's declared return type (`ireduce` for narrower types) so callers see the right Cranelift type. Walker now rejects only k-usage, outer-scope captures, and nested Lambda/ClosureRecord shapes; non-IntLit bodies are otherwise fine.
- *Zero-arg ops* — closed in Phase 4b (`[HEAD]`). Perform side packs user args into a stack-allocated `[u64; N]` buffer; the runtime copies them into the dispatched `NextStep::Call`'s args slots before invoking the arm fn.
- *No `k` use* — pending Phase 4d (continuation reification + lambda-lifting of perform's continuation). Phase 4c walker still rejects with a `references continuation` diagnostic until Phase 4d ships.
- *No outer-scope captures in arm bodies* — pending Phase 4d-or-later (lifts alongside k-using arms; both need the synthetic CPS arm fn's `closure_ptr` to point at a real closure record). Phase 4c walker rejects with a `captures outer-scope binding` diagnostic.
- *Single effect per handle* — pending Phase 4e (frame-per-effect).
- *No return arm* — pending Phase 4f (synthetic return-fn registered via `sigil_handler_frame_set_return`).

## 2026-04-26 — [DEVIATION Task 55] Phase 4b — args-buffer packing on perform side; stack-slot allocation; arm side still IntLit-only

**Context:** Phase 3b (`d0aa4c4` + `2e7c0de`) and Phase 4a (`adcb897`) shipped the handler-arm dispatch path under five restrictions: single arm (closed in 4a), zero user op-args, IntLit-only arm body, no `k` use, single effect, no return arm. Phase 4b targets the second restriction: lifting the "non-IO perform must have zero user args" gate so handler-discharged effects can carry data.

**Deviation:** The user-arg buffer is **stack-allocated** in the calling fn's frame via `FunctionBuilder::create_sized_stack_slot`, not arena-allocated through `sigil_arena_alloc`. The plan body specifies arena allocation only for `NextStep` records (Task 56's existing arena), not for the per-perform args buffer. The runtime's `sigil_perform` documentation (Task 56's `args_ptr` safety contract) treats the args buffer as caller-owned and copies values into the dispatched `NextStep::Call`'s slots before returning, so the buffer's lifetime only needs to outlive `sigil_perform`'s call site. A stack slot in the perform's enclosing function frame trivially satisfies that — no escape risk, no arena traffic on the hot path, no GC interaction (Boehm doesn't scan stack slots; the runtime conservatively scans the arena).

The arm fn side stays untouched in Phase 4b: arm bodies remain `Expr::IntLit` (Phase 4c lifts that), so the synthetic arm fn that runs under the trampoline ignores the `args_ptr` parameter it receives. The end-to-end FFI plumbing now carries packed args through `lower_perform_non_io_to_value` → `sigil_perform` → `sigil_run_loop` → arm fn invocation, but the arm fn doesn't read from args_ptr until Phase 4c wires arm-body lowering. This split is deliberate: Phase 4b's risk is exclusively in the perform-side buffer-layout / widening / overflow path; bundling Phase 4c's arm-side reads would conflate the two failure modes.

**Per-arg widening:** Cranelift values are widened to `u64` before the slot store. `I64` (Sigil `Int`) and `pointer_ty` (`String`, user-type heap pointers — pointer_ty is `I64` on every supported target: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`) store directly. `I8` (Bool, Byte, Unit) and `I32` (Char) are zero-extended via `uextend` because Sigil's surface integer types treat narrower payloads as unsigned and `sigil_perform` reads slots as `u64`. Cranelift's load-store width invariants would otherwise reject the slot store; alignment is `align_shift = 3` (8-byte) matching `sigil_perform`'s `args_ptr.add(i)` u64-stride read.

**Bound check:** A defensive `debug_assert!` in `lower_perform_non_io_to_value` rejects `args.len() + 2 > MAX_INLINE_ARGS` in dev builds, mirroring (but deliberately weaker than) the runtime's `sigil_perform` overflow check. The runtime's check is the source of truth — it aborts with a named `effect_id` / `op_id` message that's strictly better than the codegen-side "internal compiler error" panic; the compiler-side `debug_assert!` exists to catch the bug pre-link in dev builds before the runtime guard fires. The `+2` accounts for the implicit `(k_closure, k_fn)` slots the runtime appends to every dispatched arg vector. v1's effect arities (Raise / State / Choose, all 0–2 user args) sit well below the cap. The constant lives in `sigil_abi::effect::MAX_INLINE_ARGS` (moved from `sigil_runtime::handlers` in this commit so compiler and runtime share one source); a future operation needing > 30 user args would require boxing — flagged in the Task 56 MAX_INLINE_ARGS deviation entry, not in scope for Plan B.

**Float / 32-bit-target safety net:** The widening fallthrough (after the I64 / I8 / I32 branches) uses an `assert!` (not `debug_assert!`) so a future F32 / F64 surface type or a 32-bit target port that smuggles a non-pointer-width value through the args-buffer path panics in *both* dev and release builds rather than silently storing the bit-pattern as if it were a pointer-sized value. Cheap insurance until v2 either adds floats with an explicit branch or this assertion fires and forces the question.

**Rationale:** Stack-slot allocation is the smallest meaningful change to the perform-side codegen path that lifts the zero-arg gate. It avoids arena-traffic regressions on a path that already touches the arena once per perform (via `sigil_next_step_call` → `sigil_arena_alloc` for the `NextStep::Call` record). It also keeps the per-perform allocation cost amortised into the frame-allocation Cranelift already performs at fn entry (no per-perform `malloc`-equivalent). Arm-side reads are deferred to Phase 4c so the perform-side change can be reviewed in isolation; bundling both would double the diff and conflate two distinct failure modes (perform-side packing vs arm-side unpacking).

**Implementing commit(s):** `[HEAD]`.

**Closure point:** the closure-point references in the original Phase-3b restrictions entry (above) update in lockstep — *Zero-arg ops* moves to closed at this commit; *IntLit-only arm body* gains the dependency note that Phase 4c only needs to wire the arm-side reads (the FFI plumbing is already end-to-end). Stack-slot allocation here ships under the synchronous `lower_perform_non_io_to_value` → `sigil_perform` → `sigil_run_loop` call pattern; **Phase 4d** migrates the args buffer from stack to arena (`sigil_arena_alloc`) when perform sites convert to returning `NextStep::Call` to the caller's trampoline rather than synchronously calling `sigil_run_loop` — at that point the stack slot dies before the trampoline reads it on the next dispatch. The dependency is also documented inline at the `create_sized_stack_slot` call site (`compiler/src/codegen.rs::lower_perform_non_io_to_value`) and cross-references the `[DEVIATION Task 55] Native callers drive sigil_run_loop synchronously` entry.

**Acceptance precondition for Phase 4c — args-content verification (PR #23 review MF1).** The three e2e tests landed by Phase 4b (`handle_with_int_arg_op_packs_args_buffer`, `handle_with_three_int_args_packs_buffer`, `handle_with_mixed_type_args_widens_correctly`) only verify that the FFI plumbing compiles and runs without crashing — they do **not** verify that the widened values delivered to the runtime match the source values. The arm bodies are still `Expr::IntLit` (Phase 4c restriction), so the arm fn ignores `args_ptr` and returns its literal regardless of the buffer's contents. Off-by-one slot offsets, wrong-direction widening (`uextend` vs `sextend`), wrong endianness, and off-by-one `args_len` would all land green under Phase 4b's coverage.

Phase 4c's PR is therefore **required to ship arg-content-verification e2e tests** as a precondition of merging — at minimum:

1. **Int arg readback** — arm body returns the bound op-arg (or arithmetic on it); `perform Effect.op(N)` with a known `N` must observe `N` arrives at the arm.
2. **Bool / Char arg readback** — exercises the `uextend` widening path. `perform Effect.op(true)` returning the bool from the arm must round-trip; same for a `Char` value via `int_to_string` of its codepoint.
3. **String arg readback** — exercises the pointer-store path. `perform Effect.op("hi")` returning the bound name and printing it via `IO.println` must produce the source string.
4. **Multi-arg readback in declared order** — `perform Effect.op(10, 20, 30)` with an arm that returns one of the three (e.g. `(a, b, c, k) => b`) must observe `20`, pinning that the offset arithmetic on the perform side matches the runtime's `args_ptr.add(i)` u64-stride read.

These tests are **deferred from Phase 4b** because the arm side cannot read bound names until Phase 4c lifts the IntLit-only restriction; deferring them is the cheapest of the three options the reviewer offered (vs adding a runtime `#[cfg(test)]` verification path or a codegen-side direct-FFI test that reads back the stack slot). The trade-off: a Phase 4b regression in widening / offset arithmetic would land green now and only surface when Phase 4c lands. A bisecting agent investigating a wrong-args-value bug after Phase 4c lands should treat this entry as a pointer back to Phase 4b (`[HEAD]`) as the suspect — the args-packing path was added there.

## 2026-04-26 — [DEVIATION Task 55] Phase 4c — richer arm bodies via Lowerer; arm-side arg unpacking; perform-side return-type narrowing

**Context:** Phase 4b (`2114235` on `main`) shipped args-buffer packing on the perform side; arm fns received `args_ptr` but ignored it because the walker still rejected non-`Expr::IntLit` arm bodies. Phase 4c lifts that restriction: arm bodies are now lowered through the regular `Lowerer` with op-args bound from `args_ptr` at fn entry. This is the matching arm-side consumer of Phase 4b's perform-side packing, and the closure point of the Phase 4b `[DEVIATION Task 55] Phase 4b — args-buffer packing on perform side` entry's "Acceptance precondition for Phase 4c — args-content verification (PR #23 review MF1)" section.

**Deviation:** Three implementation choices worth recording.

1. **Arm body lowering via shared `Lowerer`, not a dedicated CPS-aware lowerer.** The Phase 3b deviation entry described future "richer bodies needing a CPS-aware lowerer that handles op-arg unpacking from `args_ptr`, `k` usage via `sigil_next_step_call`, and outer-scope captures via a closure record." Phase 4c implements only the first piece (op-arg unpacking); `k` usage and closure captures are deferred to Phase 4d via the walker's `arm_body_phase_4c_violations` gate. This lets Phase 4c reuse the existing `Lowerer` machinery — arithmetic, conditionals, calls, IO performs, all work — without growing a parallel codegen path. The arm fn's setup duplicates the per-fn FFI ref / `lit_gv` / `user_fn_refs` / `handler_arm_refs` declarations from the user-fn loop (necessary because Cranelift's `declare_func_in_func` returns FuncRefs scoped to a single fn body); a future cleanup could DRY this into a helper.

2. **Per-arg widen / narrow discipline mirrored across the FFI boundary.** Codegen widens narrower op-arg types (`I8` Bool/Byte/Unit, `I32` Char) to `u64` before storing into the perform-side stack slot (Phase 4b); the runtime copies the u64 verbatim into the dispatched `NextStep::Call`'s args slot; Phase 4c's arm-fn entry `ireduce`'s back to the declared Cranelift type before binding in env. The body's lowered Cranelift `Value` is widened to I64 before `sigil_next_step_done` (matching its FFI signature); `lower_perform_non_io_to_value` mirror-narrows on the perform side via a new lookup against `effects[effect].ops[op].return_type → cranelift_ty_for_type_expr` so callers see the right Cranelift type. Without the narrow, an op declared `() -> Bool` would expose an I64 Cranelift `Value` where `type_of_expr` predicts I8 — Cranelift's verifier would reject the next operation that expected I8 input. The `assert_eq!(return_ty, pointer_ty)` fallthrough panics in dev and release for unexpected types (mirroring Phase 4b's widening-fallthrough hardening).

3. **Walker's free-var capture check is scope-tracking, not just syntactic.** The new `arm_body_phase_4c_violations` helper walks the arm body with a stack of `BTreeSet<String>` scope frames: op-args at the bottom, then push/pop for `let`-introduced names, match-arm pattern bindings, and nested-handle inner arms. An Ident is a "capture" only if it's not in any active scope, not in `globals` (top-level fns + ctors + `int_to_string` builtin), and not the `k_name`. Without scope tracking, a benign `let y = x + 1; y` inside an arm body would flag `y` as a capture; with scope tracking the let-binding adds `y` to the active scope before the tail expression walks. Phase 4c's walker also rejects nested `Expr::Lambda` and `Expr::ClosureRecord` in arm bodies — these would need closure-record allocation that arrives alongside k reification (same closure point as the capture gate).

**Rationale:** Reusing the existing `Lowerer` instead of building a dedicated CPS-aware lowerer cuts Phase 4c's surface area dramatically — the arm fn's body inherits every expression form the user-fn lowerer supports (arithmetic, conditionals, calls, ctor allocations, IO performs, recursive non-IO performs that walk to outer handler stacks) for free. The cost: the Lowerer can lower constructs that aren't safe in the synthetic arm fn context (e.g., closure captures via `ClosureEnvLoad` against a null `closure_ptr`). The walker's gates plug those holes by rejecting at codegen entry; the trade is "small lowerer + sharp walker" vs "duplicated lowerer + permissive walker." The former is cheaper to maintain and review.

The widen/narrow mirror discipline matches Plan B's "raw Int internally, tag at C-ABI boundary" decision (Task 4.5.5 deviation entry) for the analogous case at the perform/arm FFI boundary: u64 is the lingua franca across the FFI boundary, narrower types are restored on each side. The deviation entry's `args.len() + 2 <= MAX_INLINE_ARGS` check is replicated symmetrically.

The scope-tracking walker is the smallest correct check that avoids false positives. A purely syntactic Ident-collection check would either flag legitimate let-bindings (over-restrictive) or miss real captures by treating any matching name as bound (under-restrictive). Walking with scope frames is ~80 LOC of tightly-scoped helper code; less invasive than threading closure-convert's existing free-var analysis through.

**Implementing commit(s):** `[HEAD]`.

**Closure point:** the Phase 3b restrictions entry's *IntLit-only arm body* item moves to closed at this commit. The Phase 4b deviation entry's "Acceptance precondition for Phase 4c — args-content verification (PR #23 review MF1)" closure: this commit ships the 4 required tests (`arm_reads_int_arg_returns_it`, `arm_reads_bool_arg_branches_on_it`, `arm_reads_string_arg_prints_via_io_println`, `arm_reads_multi_args_in_declared_order`) plus 3 bonus tests (`arm_body_does_arithmetic_on_op_args`, `arm_uses_k_is_rejected_at_codegen`, `arm_captures_outer_scope_is_rejected_at_codegen`). Future Phase 4d work picks up the `k`-usage gate + the closure-capture gate together; the arm fn's `closure_ptr` becomes load-bearing (currently null) once captures are supported, which means stackmap entries at every arena-allocating call inside the arm body need the closure_ptr threaded as a live root (today the placeholder stackmap is sufficient because closure_ptr is null and the only roots are pointer-typed op-args).

## 2026-04-26 — [DEVIATION Task 55] `unsupported_handle_construct` walker does not follow call edges

**Context:** Phase 3b's codegen-entry guard `unsupported_handle_construct` walks every handle expression's body looking for direct `Expr::Perform` sites. It does NOT follow `Expr::Call` edges into called fns. A handle whose body is `helper()` where `helper` itself performs the handled effect therefore slips past the guard's "non-IO perform with args" check.

**Deviation:** The walker stays syntactic and shallow. Once MF1's `Stmt::Perform` dispatch fix landed (review-fixup commit `54b4a60`), this is **no longer a soundness concern** — there is no longer any `unreachable!` or IO-only assertion that a transitive perform would crash into. A non-IO perform reached through a call edge now lowers correctly via `sigil_perform` → `sigil_run_loop` against whatever handler frame is on the runtime stack at call time. The walker's role is reduced to a *Phase-4 ergonomic gate*: it surfaces a friendly "this shape isn't supported yet" diagnostic for shapes Phase 3b/4a's codegen can't handle. Transitive performs through call edges are not in that set today; they work because the called fn's own Stmt/Expr Perform sites lower through the runtime ABI like any other.

**Rationale:** Following call edges in the codegen-entry walker would require fixed-point analysis over the call graph (intentional, since a fn could call another that performs) and would duplicate work the colorer already does. Leaving the walker syntactic keeps it simple and CI-fast. The standing precondition that programs reach codegen with all `Item::Effect` registered + all op IDs assigned + every reachable handle visible to the per-arm CPS-fn synthesis pre-pass holds regardless of transitive analysis — those passes walk every fn body, so a perform reached via a call edge gets its `effect_id` / `op_id` looked up correctly.

**Implementing commit(s):** original walker `2d69b52`, recursion fix into nested handle bodies `54b4a60`, op-arg gate lifted at `[HEAD]` (Phase 4b).

**Closure point:** Phase 4b (`[HEAD]`) lifted the "non-IO perform with args" gate. The walker's `arm.params.is_empty()` check and the `body_contains_non_io_perform_with_args` traversal were both removed; the now-dead `body_contains_non_io_perform_filtered` / `block_contains_non_io_perform_filtered` helpers were deleted with them. The walker still rejects multi-effect arms (Phase 4e), non-IntLit arm bodies (Phase 4c), and return arms (Phase 4f) — those gates remain syntactic + shallow + Phase-4 ergonomic. Phase 4c/4d may need a colorer-driven check (per-monomorph color variance under handler-context — the PR #18 reviewer's open ask) to decide which monomorphs sit at the native↔CPS boundary; that check is NOT a property of the walker, it's a property of color inference, and it lives in `compiler/src/color.rs`.

## 2026-04-26 — [DEVIATION Task 55] Handler frame leaks on body unwind; depends on Task 57 to surface

**Context:** `Expr::Handle` codegen lowers to `frame_new → set_arm* → push → body → pop`. If the body aborts mid-execution, the frame stays on the runtime handler stack until the process dies. Today the only way to abort mid-body is `sigil_panic_arith_error` (div/mod by zero, integer overflow), which kills the process — so the leak never matters because there is no surviving program state to observe it.

**Deviation:** No frame-pop-on-unwind path is wired in Phase 3b/4a. The codegen sequence is straight-line; there is no scope-guard / drop-glue / unwind-resumption mechanism for the handler frame. Programs that compile under Phase 3b/4a today never observe this leak because no recoverable abort path exists.

**Rationale:** Adding scope-guard machinery now requires designing the unwind contract before there's a recoverable abort path to test it against. The contract would have to be revised when Task 57 replaces `sigil_panic_arith_error` with `Raise[ArithError]` (the ArithError handler would itself pop frames). Building the contract once, against the real ArithError path, is cheaper than building it twice.

**Implementing commit(s):** `d0aa4c4` (Phase 3b — straight-line frame_new/push/pop, no unwind path).

**Closure point:** **Task 57.** When `sigil_panic_arith_error` is replaced with `perform ArithError.divide_by_zero(...)`, the recovery path (the surrounding `handle ArithError` arm) becomes a real observation point for any leaked frames. Task 57's PR must either (a) add a scope guard at every `Expr::Handle` codegen site that pops the frame on every exit edge from the body (success or unwind), or (b) make `sigil_handle_pop` idempotent + tear down the leaked frames on the unwind path before the next `sigil_perform` walks the stack. The arithmetic-overflow recoverable-abort case is what makes this load-bearing; until Task 57 lands, the dependency is documented but the bug is not user-observable.

## 2026-04-26 — [DEVIATION Task 55] Native callers drive `sigil_run_loop` synchronously; tail-call discipline lifts in Phase 4d

**Context:** Phase 3b's `lower_perform_non_io_to_value` lowers a non-IO `perform Effect.op(...)` site as `sigil_perform(...) → sigil_run_loop(call_ns)`. The native caller blocks on `sigil_run_loop` until it returns a terminal `Done(value)`. This works for Phase 3b/4a's restricted shape (synchronous, single-shot, `IntLit` arm body, no `k` use) because `run_loop` completes in one or two trampoline dispatches and returns promptly.

**Deviation:** Native callers issue `sigil_run_loop` calls synchronously rather than handing the perform's `NextStep::Call` to a containing trampoline as a tail position. For Phase 3b/4a this is fine — there's no enclosing trampoline yet because `main` and helper fns are both Native-color. The synchronous-blocking shape would defeat the trampoline's stack-amortisation guarantee if a deep chain of CPS calls landed on it (each one pushing a native stack frame), but Phase 3b's restrictions cap chain depth at 1–2 dispatches.

**Rationale:** Wiring a real native↔CPS interop boundary (color-driven CC, native fns that detect they're inside a handler-discharge context and emit `NextStep::Call` instead of synchronous `run_loop`) requires the colorer to refine handler-discharge — which is exactly the MF3 / PR #18-reviewer Stage-6 ask. Phase 3b/4a ship the synchronous shape because it's correct under their narrow restrictions and because designing the interop boundary alongside the color refinement gives us one design pass instead of two.

**Implementing commit(s):** `d0aa4c4` (Phase 3b — synchronous `sigil_perform` + `sigil_run_loop` from `lower_perform_non_io_to_value`).

**Closure point:** **Phase 4d** (k-using arms via continuation reification). Phase 4d makes arm bodies that invoke `k(value)` work end-to-end, which means the body's "rest of computation after the perform" gets lambda-lifted into a CPS-color synthetic closure. At that point native-color fns that contain a perform site under a handle MUST be recoloured CPS (or wrapped in a per-call trampoline) so the perform's `NextStep::Call` goes to the enclosing trampoline rather than to a synchronous `sigil_run_loop` invocation. The colorer's handler-discharge refinement (the PR #18 reviewer's ask, `compiler/src/color.rs::find_non_io_perform_in_expr` Phase 4d closure point) is what enables this — a fn whose only non-IO performs are discharged by enclosing handlers stays Native, but a fn whose performs reach a `k`-reifying handler arm becomes CPS. Until Phase 4d, the synchronous-blocking shape is the correct lowering.

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

**Capacity bound:** `MAX_HANDLER_ARMS = 14`. Bounded by the
32-bit pointer bitmap: arm `i`'s closure_ptr lives at payload
word `5 + 2*i`, so bit 31 corresponds to arm `i = 13`, giving 14
total arms (indices 0..=13). v1 effects
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
**Closure point:** closed in Task 55 Phase 4b (`[HEAD]`). Codegen's
`lower_perform_non_io_to_value` now packs user args into a stack-
allocated `[u64; N]` buffer and reads `MAX_INLINE_ARGS` from
`sigil_abi::effect` (moved from `sigil_runtime::handlers` so both the
compiler and runtime read from one source); a defensive
`debug_assert` at the perform-site catches any operation that exceeds
`MAX_INLINE_ARGS - 2` user args before the runtime's matching check
fires. v1's effect arities (Raise / State / Choose, all 0–2 user
args) are well below the cap; the constant can be raised in a future
plan by editing `sigil_abi::effect::MAX_INLINE_ARGS` and re-checking
the runtime's matching constants in `sigil_run_loop`'s stack-resident
args buffer.

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

## 2026-04-26 — [DEVIATION Task 55] Phase 4d — closure-capture support + tail-position k via identity continuation; non-tail k + colorer refinement + discard-k correctness deferred to Phase 4e

**Context:** Phase 4c (`e3ed53a` on `main`) lifted the IntLit-only arm-body restriction. The walker (`arm_body_phase_4c_violations`) still enforces three Phase-4-pointing gates: (1) **`k`-name references** (rejected with a Phase-4d-pointing diagnostic), (2) **outer-scope captures** in arm bodies (rejected with the same closure point), and (3) **nested `Lambda` / `ClosureRecord`** in arm bodies (rejected with the same closure point). The synthetic CPS arm fn's `closure_ptr` (passed by `sigil_handler_frame_set_arm`) is null at every Phase 4c arm slot — Phase 4c arm bodies never read it because gates (2) and (3) ensure the env is bounded by op-args.

The `[DEVIATION Task 55] Native callers drive sigil_run_loop synchronously` entry above identified Phase 4d as the closure point for both gate (1) (k-using arms) and the colorer's handler-discharge refinement (PR #18 reviewer's open Stage-6 ask). The entry framed Phase 4d as a single design pass covering: continuation reification + lambda-lifting of the perform's "rest of computation" + colorer recoloring of fns whose performs reach a `k`-reifying arm into CPS-color + native↔CPS interop wrappers + closure-capture support.

**Deviation:** Phase 4d ships as an **MVP** rather than the unified design pass that prior entries described. Three pieces land at this commit:

1. **Closure-capture support (full).** Codegen at `Expr::Handle` site now allocates a closure record per arm (TAG_CLOSURE; pointer bitmap covering pointer-typed captures) when the arm body references outer-scope bindings, and passes the record's pointer as `closure_ptr` to `sigil_handler_frame_set_arm` (the 4th arg, present since Task 56). The synthetic arm fn loads captured values via the existing `lower_closure_env_load` lowering (`closure_ptr + 16 + 8*i`, narrow-cast per slot kind). The walker's capture rejection and `ClosureEnvLoad` rejection are lifted; `closure_convert::rewrite_expr`'s existing `Expr::Handle` arm (which already rewrites captured-name `Ident`s in arm bodies into `ClosureEnvLoad` slots so a handle inside a lambda is captured by closure conversion's existing free-var analysis) becomes load-bearing.

2. **Tail-position `k(arg)` via `sigil_continuation_identity` runtime intrinsic.** A new runtime intrinsic `extern "C" fn sigil_continuation_identity(closure_ptr: *const u8, args_ptr: *const u64, args_len: u32) -> *mut NextStep` returns `sigil_next_step_done(*args_ptr)` — it packages its single u64 arg as a terminal `NextStep::Done`. At each non-IO perform site, codegen emits the address of this intrinsic as `k_fn` (and keeps `k_closure` null). The runtime appends `(k_closure, k_fn)` to the dispatched arm's args buffer (positions `args_ptr[N]`, `args_ptr[N+1]` where `N = user_args.len()`), so the arm fn loads them at fn entry alongside user op-args. When an arm body's tail expression is `k(x)`, codegen lowers it to `sigil_next_step_call(k_closure_loaded, k_fn_loaded, 1)` + write `x` to the new NextStep's args slot 0 + return that NextStep pointer. The synchronous `sigil_run_loop` chain dispatches: arm fn → `Call(k_fn=identity, [x])` → identity → `Done(x)` → run_loop returns `x` to the perform site. The walker's k-use rejection is split: tail-position `k(arg)` is now accepted; non-tail uses (`k(x) + 1`, `k(x) * 2`, `let r = k(x); ...`) are rejected with a new Phase-4e-pointing diagnostic.

3. **No colorer change. No native-fn → CPS-color recoloring. No native↔CPS interop wrappers.** The synchronous `lower_perform_non_io_to_value` → `sigil_perform` → `sigil_run_loop` shape is preserved. The MVP relies on the algebraic identity that, when `k(arg)` is invoked in tail position of an arm body, `k_fn = identity` produces the same observable result as a real continuation — the perform site receives `arg` from `run_loop` and the rest-of-handle-body is encoded in the native caller's lowered code AFTER the perform call.

The walker's nested `Lambda` / `ClosureRecord` rejection is preserved (closure point: future phase — needs free-var analysis distinct from arm-body-level captures; closure-conv runs before codegen and would already have rewritten any genuine free-vars into `ClosureEnvLoad`, so reaching codegen with a residual `Lambda`/`ClosureRecord` in an arm body is a sentinel for some case the lowering pipeline doesn't yet handle).

**Phase 4d MVP does NOT lift these (closure point: Phase 4e):**

- **Non-tail `k` use.** `Op(x, k) => k(x) + 1` — arm wants to invoke k synchronously, get back a value, continue computing. Under the synchronous shape, the arm fn cannot yield to the trampoline mid-body and resume. Lifting requires CPS-transforming the arm body itself, which in turn forces the surrounding native fn to be CPS-color so the arm-body's continuation can return `NextStep::Call` to it.
- **Multi-shot k use.** Arms for `effect E resumes: many` that invoke `k` more than once. Multi-shot requires k to be a heap-allocated, persistent, re-invokable closure rather than a stack-encoded synchronous-return. The runtime's `HandlerFrame.arms[i].closure_ptr` and the arm's `(k_closure, k_fn)` pair are already pointer-shaped to support this; the missing piece is codegen producing a real continuation closure (rather than `&sigil_continuation_identity`) when the arm's effect is `resumes: many`.
- **Discard-k correctness across function-call boundaries.** This is the load-bearing semantic gap. Algebraic semantics says: when an arm discards `k` (zero uses; e.g., `Raise.fail(k) => 42`), the handle's overall value is the arm's value — early-exit. The rest-of-handle-body is **never run**. Under the Phase 4d MVP synchronous shape, when the perform reaches the arm via a function-call boundary (e.g., the handle body calls `helper()` and `helper`'s body performs the effect), `sigil_run_loop` returns the arm value to the perform site **inside `helper`**. `helper` continues executing whatever follows the perform, returns its value to the handle body's caller, the handle body continues with `helper`'s return value, and the handle's overall is whatever the body's tail produces — **not** the arm value. The bug is structural: the synchronous shape conflates "arm value" with "perform result" and there is no way to early-exit through native call frames without unwinding. Lifting requires either (a) full CPS transform of native fns reachable under a handler-discharge context (the colorer refinement), or (b) a tagged-return discriminator + per-call check at every native call edge under a handler scope. (a) is what the closure point references below.

**Walker gate restructure at this commit:**

- *capture rejection* → **lifted**; arm-body Idents that resolve to `closure_convert`-rewritten `ClosureEnvLoad` slots load from the arm's closure record at runtime.
- *`ClosureEnvLoad` rejection* → **lifted**; same path.
- *`k`-use rejection* → **split**:
  - tail-position `k(arg)` → **accepted**; lowers via `sigil_continuation_identity`.
  - non-tail `k(arg)` (anything where the result of `k(arg)` feeds into another expression) → **still rejected** with a Phase-4e-pointing diagnostic.
- *nested `Lambda` / `ClosureRecord` rejection* → **preserved** (sentinel for unhandled lowering shapes; not Phase 4e's scope).

**Rationale:** Three reasons for the MVP-vs-full-Phase-4d split.

First, **review tractability**. The full Phase 4d as described in the synchronous-`run_loop` deviation entry is a 2,000–3,000 LOC change touching color inference globally, calling-convention bifurcation, native↔CPS interop, and closure capture. Past PRs in this stage have shown that single PRs over ~1,500 LOC accumulate review fatigue (PR #22 had two review rounds + four fixup commits at 2,373/-272). The colorer's handler-discharge refinement specifically is the kind of design decision that benefits from being its own focused PR — it's the PR #18 reviewer's open Stage-6 ask, and bundling it with closure plumbing makes the diff hard to read. Splitting at the "synchronous identity continuation works for tail-k" boundary gives a Phase 4d sized comparably to Phases 4b/4c, plus a focused Phase 4e dedicated to the calling-convention shift.

Second, **the MVP lifts every user-visible Phase 4c gate**. All three currently-rejected e2e tests (`arm_uses_k_is_rejected_at_codegen` for tail-position k use, `arm_captures_outer_scope_is_rejected_at_codegen` for outer captures, `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen` for captures inside lambdas) convert to positive tests under the MVP. The MVP shape supports the *common* algebraic-effects patterns: `Raise`-style discard-k handlers (when the perform is in tail position of the handle body — every existing Phase 4c test follows this shape), `State`-style tail-call k handlers, and the `with-default` pattern. The remaining gaps (non-tail k, multi-shot, cross-function-call discard-k) are real but narrower than "the entire k-using-arms feature."

Third, **the discard-k correctness gap is bounded and documented, not silent**. Two safeguards land in the same commit:

- **README "Verification limits" section** with explicit user-facing call-out: discard-continuation handlers (Raise-style early-exit) do not yet propagate through function-call boundaries; behavior matches Phase 4c semantics where the arm's return value flows to the perform site, not the handle expression. Programs depending on this work as expected. (User-facing because Stage 9 spec validation will produce LLM-authored programs that assume standard algebraic semantics; if the limit isn't surfaced in the README, the failure mode is silent miscompilation rather than a documented restriction.)
- **`#[ignore]`'d e2e test** `discard_k_handler_does_not_abort_helper_phase_4e_pending` pinning the current broken behavior with comments naming Phase 4e as the closure point. Inverts to a normal test (without `#[ignore]`) when Phase 4e lands. Makes the bug grep-findable and impossible to forget.

Plus the structural safeguards:

- `PLAN_B_PROGRESS.md` Task 55 phase enumeration is renumbered: **Phase 4e** (this entry's closure point) becomes the colorer-refinement / non-tail-k / discard-k-correctness phase; existing Phase 4e (multi-effect handles) is renumbered to **Phase 4f**; existing Phase 4f (return arms) is renumbered to **Phase 4g**. The renumber reflects priority: Phase 4e closes the algebraic-semantics gap that gates Stage 9 spec validation, which makes it a higher-priority remaining piece than feature breadth (multi-effect, return arms).
- `PLAN_B_PROGRESS.md` gains an explicit **"Phase 4e is a Stage 9 prerequisite"** hard-gate entry. Stage 9's `scripts/validate-spec.sh` cannot run until Phase 4e ships; the validation prompts (Plan A/B-bank P6 `Raise[String]`-based safe parser, P19 `State[Int]` counter, P20 multi-shot `Choose`) all exercise patterns that need Phase 4e correctness.

**`sigil_continuation_identity` design notes.**

- Signature matches the uniform CPS arm-fn ABI: `extern "C" fn(closure_ptr: *const u8, args_ptr: *const u64, args_len: u32) -> *mut NextStep`.
- Body is one-line: `sigil_next_step_done(*args_ptr)`. `args_ptr` is u64-stride; the single u64 arg is the value the arm passed to `k(...)`.
- `closure_ptr` and `args_len` are unused (closure-less, fixed-arity-1).
- `args_len == 1` is invariant — codegen always emits `sigil_next_step_call(k_closure, k_fn, /*arg_count=*/1)` for tail-position k(arg). A `debug_assert_eq!(args_len, 1)` in the runtime catches a future codegen regression that emits the wrong arity.
- Lives in `runtime/src/handlers.rs` next to the other `sigil_next_step_*` intrinsics. The address is exposed to codegen via a new FFI declaration (`continuation_identity_ref` field on `Lowerer`, mirroring `next_step_done_ref`).
- The intrinsic is the runtime side of "identity continuation". Codegen could have computed `Done` directly at the perform site and bypassed the FFI, but routing through `sigil_run_loop`'s normal Call→Done dispatch keeps the perform-site code path uniform with what Phase 4e will replace it with (a real lambda-lifted continuation fn whose body returns either Done or another Call). That uniformity matters: Phase 4e's diff will be "swap the k_fn from identity to a real continuation" rather than "rewrite the perform-site lowering."

**Closure-record allocation site.** Codegen at `Expr::Handle` builds the closure record at handler-frame setup time (before `sigil_handle_push`) using the existing `lower_lambda_alloc` machinery — the same TAG_CLOSURE-tagged record that user-level closures use. Pointer bitmap is computed per arm based on the arm's captured-name slot kinds (closure_convert's `EnvSlotKind::{Closure, String, User}` → pointer bit set; `Int, Bool, Byte, Unit, Char` → cleared). The record is rooted by the handler frame's `arms[i].closure_ptr` slot (Task 56's `HandlerFrame` precise-bitmap covers this — the bitmap's odd bits mark each `arms[i].closure_ptr` as a tracked pointer). On `sigil_handle_pop`, the frame is popped and the closure records become unreachable, so Boehm reclaims them on the next collection.

**Implementing commit(s):** [HEAD] (this commit lands the deviation entry only — implementing commits follow on the same branch `plan-b-task-55-phase-4d`).

**Closure point:** **Phase 4e** (NEW phase, inserted at PROGRESS.md task list before existing 4e/4f which renumber to 4f/4g). Phase 4e closes:

- *Non-tail k use.* Arm bodies whose tail isn't a single `k(arg)` call. Implementation requires CPS-transforming the arm body (lambda-lifting the post-`k` rest into a synthetic continuation fn) AND CPS-transforming surrounding native fns whose performs reach the arm.
- *Discard-k correctness across function-call boundaries.* The structural fix is the colorer's handler-discharge refinement (`compiler/src/color.rs::find_non_io_perform_in_expr` Phase-4e-pointing comment): a fn whose performs reach a discard-k arm reclassifies as CPS-color so the perform's `NextStep::Call` returns to the enclosing trampoline rather than to a synchronous `sigil_run_loop` invocation. The `#[ignore]`'d test `discard_k_handler_does_not_abort_helper_phase_4e_pending` inverts to a normal test when this lands.
- *Synchronous `lower_perform_non_io_to_value` → `sigil_run_loop`.* Replaced with: native-color performs continue to call `sigil_run_loop` synchronously; CPS-color performs return `NextStep::Call` to the caller's trampoline. The `[DEVIATION Task 55] Native callers drive sigil_run_loop synchronously` entry above closes here.
- *Args-buffer arena migration.* The `[DEVIATION Task 55] Phase 4b — args-buffer packing on perform side` entry's "Phase 4d migrates the args buffer from stack to arena" line: reread as "Phase 4e" (the actual closure-of-the-stack-slot is the calling-convention shift, not the closure-capture support shipping in 4d).

**Bisecting hint pattern (mirroring the Phase 4b entry's "args-content verification" pattern):** A bisecting agent investigating a wrong-handle-result bug after Phase 4e lands should treat the Phase 4d MVP as a candidate for the regression source if the failure mode is one of:

- *Discard-k arm fires but rest-of-helper-body still runs* — Phase 4d MVP shape (synchronous `run_loop`); the `#[ignore]`'d pinning test shows the exact shape. Phase 4e is supposed to fix this; if the test inversion failed silently, the bisect target is whichever Phase 4e commit made the colorer change.
- *Tail-position `k(arg)` returns wrong value* — Phase 4d MVP wires `sigil_continuation_identity` at every perform site; if Phase 4e replaced this with a real continuation incorrectly, the regression source is the perform-site k_fn change in Phase 4e.
- *Captured outer-scope binding reads zero / wrong-typed value at arm runtime* — Phase 4d MVP wires the closure-record allocation + `lower_closure_env_load` lowering; if Phase 4e refactored either, the regression source is the env-record bitmap or the slot-narrow code.

The bisecting hint is also embedded inline at the four code change sites (closure-record alloc in `Expr::Handle` codegen, `sigil_continuation_identity` declaration, walker's split k-use gate, perform-site `k_fn` constant) so a `git blame` lands on the deviation entry without a documentation cross-reference.

## 2026-04-26 — [DEVIATION Task 55] Phase 4e — comprehensive: CPS-color user-fn calling convention; real CPS transform; native↔CPS interop; non-tail k; multi-shot k; surrounding-lambda captures

**Context:** Phase 4d MVP (`e6c29f2` on `main`) ships closure-capture + tail-position `k(arg)` via `sigil_continuation_identity`. The Phase 4d entry above enumerates four specific gaps deferred to Phase 4e: (1) non-tail k use; (2) multi-shot k use; (3) discard-k correctness across function-call boundaries; (4) surrounding-lambda closure captures into arm bodies. The Phase 4d entry also names the structural fix for (3) — the colorer's handler-discharge refinement (PR #18 reviewer's open Stage-6 ask) — and the closure-point follow-up for the synchronous-`run_loop` deviation entry (line 164 in this file). Phase 4e is the **Stage 9 prerequisite HARD GATE** per `PLAN_B_PROGRESS.md`: the spec-validation prompt bank's algebraic-effects entries (P6 Raise-based parser, P19 State counter, P20 multi-shot Choose) all exercise patterns that require Phase 4e correctness before `scripts/validate-spec.sh` can land.

**Deviation:** Phase 4e ships **comprehensively** in a single PR — all four lifts plus the architectural pieces they share (real CPS transform, CPS-color user-fn calling convention, native↔CPS interop) — rather than as a 4-PR sub-sequence (4e1: colorer refinement, 4e2: non-tail k, 4e3: multi-shot, 4e4: surrounding-lambda captures). The user explicitly chose this scope after presented with all three options (MVP / MVP+multi-shot / comprehensive) and the multi-week single-PR tradeoff. The PR is expected to be ~3,000–5,000 LOC across compiler + runtime + tests, with multiple review rounds; per-phase-PR cadence (the Phase 4b–4d standing convention) is suspended for this single phase. Within the PR, commits are organised by architectural layer — the deviation + PROGRESS + README updates land first (foundation commit), then the real CPS transform replaces the `cps.rs` stub, then codegen consumes color info, then each lift layers on with its own commit. Each commit is independently pod-verified before the next layer begins.

**Comprehensive scope — what lifts at this phase:**

1. **CPS-aware lowering in `codegen.rs`, generalising the Phase 4d MVP synthetic-arm-fn machinery** (Option B; revised from the original Option A framing in this entry's foundation commit `a8b9dbd`, then revised again at the `CpsProgram` deletion commit). The actual CPS conversion happens at codegen-pass time when `emit_object` iterates user fns; CPS-color user fns get the uniform CPS calling convention `extern "C" fn(closure_ptr, args_ptr, args_len) -> *mut NextStep` (matching the synthetic-arm-fn signature shipped in Phase 4d). Their bodies are lowered through a CPS-aware Lowerer path that recognises perform sites, calls to CPS-color callees, and non-tail expressions whose continuation reaches such yield points; at each yield, the post-yield rest of the surrounding fn is lambda-lifted into a synthetic continuation closure. Pure parts of the body emit `sigil_next_step_done(value)` directly. Native-color fns pass through the existing native lowering unchanged. The choice of Option B over Option A (separate IR pass with `Expr::CpsTailCall`/`CpsDone`/`CpsContinue` variants and rewrite-pass machinery) was made deliberately: Option A would require ~1500 LOC of new IR + rewrite logic plus matches at every `Expr` use site in the compiler, while Option B reuses Phase 4d's already-tested per-arm closure-record allocation (`alloc_arm_closure_record`) and `HandlerArmSynth` pre-pass machinery. The leading-choice rationale is documented in `a756bd3`'s commit message; the subsequent `CpsProgram` deletion (with accessors moved to [`ColoredProgram`]) is documented in the `[Task 55] Phase 4e: delete CpsProgram wrapper` commit. Option B introduces one structural cost not present in Option A: the lambda-lifted continuation closures are synthesised at codegen-pass time (after `closure_convert` has already run), so they need their own side-table / FuncId pre-pass extension to the existing `HandlerArmSynth` pattern — see the *Codegen consumes color* and *Non-tail k lift* commits in the roadmap below for the closure-convert side-table extension that surfaces this cost.

   **CpsProgram deleted as a transitional artifact.** The original Option A framing was implemented by introducing a `CpsProgram` wrapper around [`ColoredProgram`] in `compiler/src/cps.rs`; `a756bd3` added accessor methods (`needs_cps_transform`, `cps_color_user_fns`) on this wrapper. After confirming Option B as the architectural direction, the wrapper carried no CPS-form-specific metadata: the synthetic continuation closure pre-pass + FuncId allocations live in `codegen.rs` (per Option B's "inline lowering" choice), not in any post-color IR pass. The `CpsProgram` wrapper was therefore deleted; the accessors moved to [`ColoredProgram`] directly. The pipeline is now `lex → parse → resolve → typecheck → elaborate → monomorphize → infer_colors → closure_convert → emit_object` — one fewer typed pipeline checkpoint, and one less architectural hedge. The mid-flight reviewer's concern (`a756bd3`'s review) that the wrapper was an architectural fiction under Option B is closed by the deletion. If a future Phase 4e commit discovers a need for post-color CPS-form metadata after all, it lives directly on [`ColoredProgram`] (or a fresh wrapper introduced at the time, with the metadata it actually carries).

2. **CPS-color user fn calling convention.** Codegen emits CPS-color user fns with the uniform CPS ABI `extern "C" fn(closure_ptr: *const u8, args_ptr: *const u64, args_len: u32) -> *mut NextStep` (the same shape as Task 56's synthetic CPS arm fn ABI). The fn body is the CPS-transformed expression tree from (1). Args are unpacked from `args_ptr` at fn entry into the Lowerer's env (parallel to how synthetic arm fns load op-args in Phase 4c). Native-color user fns continue to use their declared Cranelift signatures unchanged; the colorer is the source of truth for which calling convention applies.

3. **Native↔CPS interop wrappers at call sites of CPS-color fns.** When a native-color caller invokes a CPS-color callee, codegen emits the wrapper inline at the call site: pack args into a stack-allocated `[u64; N]` buffer (the existing Phase 4b machinery, generalised), call the CPS fn → get `*mut NextStep`, hand off to `sigil_run_loop` → get u64 value, narrow back to the callee's declared return type. The wrapper is the same structural shape as today's `lower_perform_non_io_to_value`; this generalises that helper into `lower_cps_call_from_native(callee_fn_ref, args, return_ty)` and wires it at every native-call-of-CPS-fn edge. CPS-color callers of CPS-color callees emit a tail `NextStep::Call` instead — no `sigil_run_loop` invocation, the enclosing trampoline handles it.

4. **Colorer's handler-discharge classification — verified correct, pinned via regression tests; no code change to `color.rs`.** This sub-section was originally framed (in the foundation commit `a8b9dbd`) as a planned refinement to `find_non_io_perform_in_expr` to make handler-discharge more precise. **Investigation in commit `5cc3a58` confirmed the colorer is already correct for the load-bearing scenarios** — the asymmetry is intentional and right: `find_non_io_perform_in_expr` skips handle bodies (those performs are discharged so don't taint the surrounding fn's *intrinsic* color), but `collect_calls_in_expr` for `Expr::Handle` *does* descend into the body and record call-graph edges, so a fn whose handle body calls a CPS-color helper is already classified CPS via the existing SCC bridge propagation (Phase 4d's machinery, untouched here). The PR #18 reviewer's open Stage-6 ask — handler-context color variance — is therefore about codegen consuming the (already-correct) color information for user fns rather than about the colorer producing different classifications. The actual Phase 4e change is in items 2 and 3 above (CPS-color user fn calling convention + native↔CPS interop), not in `color.rs`. Three regression tests landed in `5cc3a58` pin the colorer's existing behavior so a future refactor that drops call-graph edges out of `Expr::Handle` bodies (or special-cases handle-discharged effects to suppress the edges) is caught before it can silently regress discard-k cross-boundary correctness: (a) `handle_body_calling_cps_helper_makes_caller_cps_via_bridge` pins the discharge-via-call-graph SCC bridge, (b) `handle_body_with_only_direct_perform_keeps_caller_native_under_existing_local_rule` pins the local-walk skip-the-handle-body rule (perf regression guard — keeps Native fns Native when the synchronous-run-loop shape is correct), (c) `handle_arm_body_performing_undischarged_effect_taints_caller_intrinsically` pins that arm bodies still contribute to intrinsic-CPS analysis. The "colorer refinement" commit referenced in the commit-organisation roadmap below pivoted to "tests-only, no code change to `color.rs`"; it has effectively already landed at `5cc3a58`.

5. **Non-tail `k` use via CPS-transformed arm body.** Phase 4d's `arm_body_phase_4c_violations` rejects arm bodies whose k-use isn't in tail position. Phase 4e lifts the rejection: the arm body is run through the CPS transform from (1), which lambda-lifts the post-`k` rest of the body into a synthetic continuation fn. The arm body emits `NextStep::Call(k_closure, k_fn, [arg, post_k_closure, post_k_fn])` where `(post_k_closure, post_k_fn)` is the lambda-lifted continuation. The trampoline dispatches: invoke `k_fn(k_closure, [arg, post_k_closure, post_k_fn], 3)`. The k_fn is no longer the `sigil_continuation_identity` constant — it's a real continuation that, when invoked, executes the post-`k` rest of the arm body and either returns Done or chains into another Call. The walker's k-use rejection (split into tail / non-tail in Phase 4d) is fully lifted; the synth-pass's tail-k detector still recognises tail-position uses for the simpler lowering (identity continuation, no lambda-lift needed), but non-tail uses now go through the CPS path.

6. **Multi-shot `k` via heap-reified continuation.** For arms of `effect E resumes: many`, the continuation closure is heap-allocated (TAG_CLOSURE; pointer bitmap covering captured slots) rather than encoded as `(null, &sigil_continuation_identity)` or a stack-bound continuation. Calling `k(arg1)` returns `NextStep::Call(k_closure, k_fn, [arg1, ...])` where `k_closure` is a Boehm-rooted reusable record; calling `k(arg2)` with the same closure returns a fresh Call with `arg2`. The runtime side already supports this — `HandlerFrame.arms[i].closure_ptr` is pointer-shaped and the precise GC bitmap covers it. Codegen detects `resumes_many` from the EffectDecl registry at arm synthesis time and routes the continuation reification through the heap-allocator path instead of the stack/identity path. The one-shot linearity check (E0220, Task 54) remains in place for `resumes: one` (default) effects; multi-shot effects skip it (already implemented in typecheck).

7. **Surrounding-lambda closure captures into arm bodies.** Phase 4d MVP only captures from the surrounding fn's locals (let-bindings, fn-params); when a `handle` is inside a `lambda`, captured names from outside the lambda are not visible to the arm body because closure_convert's `Expr::Handle` rewrite doesn't extend through to arm-body capture analysis. Phase 4e extends the typecheck-side `handle_arm_captures` side-table with a per-arm "lambda-frame source" annotation indicating whether each capture comes from the immediate surrounding fn's locals or from an enclosing lambda's closure record. Codegen's `alloc_arm_closure_record` reads from the lambda's `closure_ptr` (already in scope at the arm body's lowering) for the latter, instead of from `Lowerer.env`. The walker's `Expr::ClosureEnvLoad` rejection in arm bodies (re-aimed at Phase 4e in Phase 4d) is lifted.

**What this phase does NOT lift (deferred to Phases 4f / 4g / future):**

- *Multi-effect handles.* `handle expr with { E1.op1(k) => ..., E2.op2(k) => ..., ... }` where the arms target different effects. Currently rejected by codegen-entry walker. **Phase 4f** (renumbered from prior 4e in the Phase 4d MVP entry).
- *Return arms.* `handle expr with { return(v) => arm, ... }`. Currently rejected by codegen-entry walker; typechecked but not codegenned. **Phase 4g** (renumbered from prior 4f).
- *Whole-program optimisation of CPS-back-to-native.* A CPS-color fn whose only CPS source was a since-removed perform stays CPS even though it could safely be re-classified Native. The colorer is monotonic at this phase; refinement-after-lowering is a future v2 optimisation.
- *Selective CPS via tail-call return.* The Plan B body says "Native `return_call` codegen is a v2 optimization; do not pursue it here." — explicitly out-of-scope.

**Architectural choices and rationale:**

a) **Single PR rather than 4e1/4e2/4e3/4e4 sub-PRs.** The user's explicit choice. Tradeoff: review burden trades against architectural-coherence gains. Bundled, the calling-convention shift, the CPS transform, and the colorer refinement land together — a reviewer can verify that the pieces compose correctly without having to mentally merge across PRs. Split, each piece is reviewed in isolation, but the calling-convention shift in 4e1 would have to be designed against a hypothetical 4e2/4e3/4e4 future, which past phase histories show frequently produces back-tracking (e.g., the Phase 4b "stack slot" comment that now must migrate to arena under Phase 4e). The single-PR approach pays the upfront review cost in exchange for one design pass.

b) **CPS transform produces an extended `Expr` IR rather than a fresh `CpsExpr` AST.** Closure conversion runs after CPS transform; reusing the existing `Expr` enum means closure_convert's recursion rules apply unchanged to lambda-lifted continuation bodies. The CPS-form-specific shapes (`Expr::CpsTailCall { fn_ref, closure_ref, args }`, `Expr::CpsDone { value }`, `Expr::CpsContinue { post_closure, post_fn, value }`) become new variants on `Expr` with codegen lowering rules. The alternative — a separate `CpsExpr` with its own closure_convert pass — duplicates closure-conversion machinery and forces the typed-IR-preservation discipline (Plan B Task 49 entry) to be re-enforced on a parallel IR.

c) **Continuation closures are TAG_CLOSURE-tagged, sharing the user-level lambda closure machinery.** Same rationale as Phase 4d's arm-closure-record allocation site: Boehm precise GC tracks them via the standard 8-byte object header bitmap. No new runtime allocator; no new tag bit; closure_convert's existing `lower_closure_record` machinery handles them.

d) **Native↔CPS interop wrapper inlined at call sites, not synthesized as a separate fn.** Per call-site inlining costs roughly 4 Cranelift instructions (`stack_addr` + `iconst` + `call` + `call sigil_run_loop`). A synthesized wrapper fn would add a function-call frame and a pointer indirection, plus complicate the `--dump-color` output (synthesized wrappers have no source location and would need a synthetic name convention). Inlining keeps the codegen output dense and the diff localised to `lower_call`.

e) **Colorer behaviour relies on the existing `call_site_instantiations` table** (Task 50 / Plan B). Per item 4 above, no `color.rs` code change is needed: `infer_colors` already records call-graph edges via `collect_calls_in_expr`'s recursion into `Expr::Handle::body`, and the existing SCC bridge propagation already taints fns whose handle bodies call CPS-color helpers. The deviation entry's earlier framing of "Colorer refinement uses the existing `call_site_instantiations` table to drive an inter-procedural reachability check" was misleading — the reachability check already exists and works correctly; Phase 4e's contribution is making codegen *consume* the resulting classification (items 2 and 3) and pinning the existing behaviour via regression tests in `5cc3a58`. Past PRs (#16 / #17) wired the call-graph edges through; Phase 4d (#25) untouched them; Phase 4e leaves them as-is.

f) **`sigil_continuation_identity` retained for tail-k cases.** Phase 4e doesn't replace identity-continuation-at-tail-k with a real continuation across the board; arms whose body is just `k(arg)` (literal) still emit `(null, &sigil_continuation_identity)` because the lambda-lifted continuation would be a no-op identity fn anyway. Phase 4e adds the real-continuation path alongside the identity path; the tail-k detector chooses between them based on whether there's any post-`k` body. This preserves the Phase 4d test surface (tests that pin tail-k semantics keep working) while adding the new path for non-tail uses.

**Phased commit organisation within the single PR:**

- *Foundation* (this commit): deviation entry + `PLAN_B_PROGRESS.md` Phase 4e status update + README "Verification limits" expansion noting the comprehensive scope. No source code changes; just documentation that records the design and the deferred-work cross-references. PR opens after this commit lands so subsequent commits accumulate against the open PR.
- *Accessor helpers on `ColoredProgram`* (landed at `a756bd3` on `CpsProgram`, then relocated to `ColoredProgram` at the `CpsProgram` deletion commit per the `06c3459`-review architectural pushback). `needs_cps_transform(name)` and `cps_color_user_fns()` accessor methods that the codegen-consumes-color commit consumes for per-fn ABI selection. 6 unit tests in `color::tests` pin native main, unknown-fn returns false, statement-form classification, single-hop bridge ordering, multi-hop transitive ordering, and SCC-collapse-with-mutual-recursion ordering. **No `Expr::CpsTailCall`/`CpsDone`/`CpsContinue` variants land** — Option B does the CPS conversion inline in codegen, not via a separate IR pass. **No `CpsProgram` wrapper either** — the wrapper was a transitional artifact superseded by Option B's commitment; the pipeline is `lex → parse → resolve → typecheck → elaborate → monomorphize → infer_colors → closure_convert → emit_object`. Closure_convert side-table extension for synthetic continuation closure records is broken out as its own roadmap entry below.
- *Codegen consumes color* — the **big diff**. CPS-color user fns get the uniform CPS ABI declared at the user-fn pre-pass loop in `emit_object`. Their bodies are lowered through a CPS-aware Lowerer path (perform sites emit `sigil_perform(...)` returning `*mut NextStep` to the caller's trampoline; calls to CPS callees emit `NextStep::Call(callee_fn, callee_closure, args_with_appended_continuation)`; pure tail expressions wrap in `sigil_next_step_done(value)`). Native callers of CPS callees emit the inlined wrapper (`pack_args + call CPS fn → drive sigil_run_loop → narrow result`). The C-ABI shim for `main` checks `main`'s color and either calls `sigil_user_main` directly (Native) or wraps it with `sigil_run_loop` (CPS). Inverts the e2e tests `discard_k_handler_does_not_abort_helper_phase_4e_pending` and `statement_form_non_io_perform_inside_handle_compiles_and_runs` per hard condition #2.
- *Closure-convert side-table extension for synthetic continuation closures* — broken out from the codegen-consumes-color commit as a sibling because it surfaces a non-trivial pre-pass + side-table extension. Phase 4d MVP's `arm_body_unsupported_construct` walker rejects nested `Lambda` / `ClosureRecord` in arm bodies precisely because closure_convert ran before codegen. Phase 4e's lambda-lifted continuation closures are synthesised at codegen-pass time, after closure_convert; they bypass that pipeline and need their own FuncId pre-pass + closure-record allocation site. Generalises the existing `HandlerArmSynth` precedent to cover synthesised continuations as well as synthesised arm fns. Pre-pass walks every CPS-color user fn body collecting yield points (perform sites, CPS calls in non-tail position) + their post-yield continuations + their captured free vars; allocates a `FuncId` per synthetic continuation; codegen lowers each one as a standalone CPS-ABI fn with body = the post-yield rest. The walker's nested-Lambda/ClosureRecord rejection in arm bodies stays in place at this commit (lifts at the surrounding-lambda-captures commit later in this PR). **Synthetic continuations bypass the walker entirely**: they are generated post-walker, post-closure-convert at codegen-pass time as `HandlerArmSynth`-style pre-pass output, so the walker's nested-Lambda/ClosureRecord rejection (which inspects user-level AST `Expr::Lambda` / `Expr::ClosureRecord` nodes) does not fire on them — the rejection's surface is user-authored arm-body lambdas, not codegen-synthesised continuations. This is why synthetic continuations can land alongside the walker rejection still being in place; the two surfaces are disjoint.
- *Colorer regression-pinning* (landed at `5cc3a58` — pivoted from "refinement" to "tests-only" per item 4 above). Three regression tests cover the discharge-via-call-graph SCC bridge (helper call inside handle body), the local-walk skip-the-handle-body rule (perf regression guard), and the arm-body intrinsic-CPS contribution. **No code change to `color.rs`**; the pivot is documented in commit `5cc3a58`'s message and in item 4's rewritten body. This roadmap entry is preserved (rather than deleted) so future bisecting agents tracking the original commit-organisation can see the pivot in place.
- *Non-tail k lift* — walker's split k-use gate fully lifted. CPS transform's lambda-lifting handles the post-`k` rest. New e2e tests pinning `let r = k(x); r + 1` and similar shapes.
- *Multi-shot k lift* — codegen detects `resumes_many` and routes to heap-allocated continuation. Runtime tests pin invariance of multi-shot k under repeated invocation. New e2e tests pinning `Choose`-style usage.
- *Surrounding-lambda captures* — typecheck side-table extension; codegen reads from lambda's `closure_ptr` for those captures. Walker's lambda-capture rejection in arm bodies fully lifted. The Phase 4d MVP `#[ignore]`'d test `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending` inverts to a positive test.
- *Test inversion + cleanup* — `discard_k_handler_does_not_abort_helper_phase_4e_pending` un-`#[ignore]`'d, asserting stdout `42`. Walker's Phase-4e-pointing diagnostics removed (the gates they pointed at are all lifted). README "Verification limits" updated to reflect what closes here vs what remains in Phase 4f/4g.
- *PROGRESS.md final update* — Task 55 status flips to `done-pending-ci` after CI passes; commit list extended through HEAD.

**User's hard conditions for the comprehensive Phase 4e PR (mirroring the Phase 4d MVP entry's pattern):**

1. README "Verification limits" lands in the same PR — explicit, user-facing call-out of what closes here AND what remains for Phase 4f/4g (multi-effect, return arms).
2. **Two e2e tests** invert at the codegen-consumes-color commit (per `df251fc`'s discovery — both pin the same algebraic-semantics gap from different angles). The Test inversion + cleanup commit organisation step covers both:
   - (a) `discard_k_handler_does_not_abort_helper_phase_4e_pending` — un-`#[ignore]`'d, stdout assertion flipped from `142` to `42`. The Phase 4d MVP entry's `#[ignore]`'d pinning test.
   - (b) `statement_form_non_io_perform_inside_handle_compiles_and_runs` (`compiler/tests/e2e.rs` around line 1054) — already a passing test today; stdout assertion flips from `42` (the Phase 4d MVP synchronous-shape behaviour where helper's stmt-form perform of `E.op()` returns the arm value to be discarded by the Stmt) to `99` (algebraic semantics — discard-`k` arm fires, helper aborts, the handle's overall is the arm value). The colorer pinning test added in `df251fc` (`statement_form_non_io_perform_inside_handle_classifies_main_cps_via_bridge`) attributes a regression here to codegen, not to the colorer.

   The test inversion commit's message names this entry by title and explicitly calls out **both** inversions so a reviewer at the test-inversion checkpoint cannot miss either one. A reviewer who checks only (a) and not (b) would miss that the existing positive test also needs to flip its assertion.
3. `PLAN_B_PROGRESS.md` Phase 4e entry calls out the Stage 9 unblock explicitly — the existing "Phase 4e is a Stage 9 prerequisite (HARD GATE)" line stays, with a new "(closes at this PR)" suffix added when the PR squash-merges.
4. Bisecting-hint pattern: failure-mode catalogue across the three architectural surfaces — calling-convention shift / CPS transform correctness / colorer refinement — so a future bisecting agent can attribute regressions to specific commits within this PR.

**Stage 9 unblock.** When this PR squash-merges, Stage 9's `scripts/validate-spec.sh` (Plan items 85–87 of `2026-04-21-sigil-finish.md`) becomes runnable. The validation prompt bank's algebraic-effects entries (Plan A/B-bank P6 `Raise[String]`-based safe parser, P19 `State[Int]` counter, P20 multi-shot `Choose` for all-pairs-summing-to-N) all exercise patterns that the comprehensive Phase 4e closes. Stage 9's pass-rate thresholds (70% first-compile / 90% after-one-edit on Opus AND Sonnet) become measurable for the first time.

**Carry-forward closure points** — these prior-entry closure points all close at this PR:

- The `[DEVIATION Task 55] Native callers drive sigil_run_loop synchronously` entry (line 164): synchronous `lower_perform_non_io_to_value` is replaced with the colorer-driven dispatch — CPS-color performs return `NextStep::Call`, native-color performs (none expected after Phase 4e) would still call `sigil_run_loop`.
- The `[DEVIATION Task 55] Phase 4b — args-buffer packing on perform side` entry (line 87): the "Phase 4d migrates the args buffer from stack to arena" line, re-aimed at Phase 4e in the Phase 4d MVP entry, closes here. CPS-color performs allocate args via `sigil_arena_alloc` instead of `create_sized_stack_slot` because the perform-site stack frame dies before the trampoline reads them.
- The Phase 4d MVP entry's four enumerated "Phase 4d MVP does NOT lift these (closure point: Phase 4e)" gaps all close here.

**Implementing commit(s):** [HEAD] (this commit lands the deviation entry, PROGRESS update, and README update only — implementing commits follow on the same branch `plan-b-task-55-phase-4e`).

**Closure point:** The next big closure point is **Phase 4f** (multi-effect handles, was original 4e) — that's the remaining feature-breadth work after Phase 4e closes the algebraic-semantics gap. Phase 4g (return arms, was original 4f) follows. Tasks 57–61 then complete Stage 6.

**Bisecting hint pattern:** A bisecting agent investigating a regression after Phase 4e lands should attribute the failure mode to specific commits within this PR:

- *Native fn miscompiled / wrong calling convention emitted* — codegen-consumes-color commit. Verify the colorer correctly classifies the bisect-target fn (run with `--dump-color`) and that codegen branches correctly on the classification.
- *CPS-color fn body produces wrong sequence of NextSteps at runtime* — CPS transform commit. The lambda-lifted continuations are the prime suspect; mismatch between perform-site continuation construction and arm-body continuation invocation would manifest here.
- *Discard-k arm fires but rest-of-helper-body still runs (i.e., Phase 4d MVP behavior persists)* — codegen-consumes-color commit (per item 4's pivot, not a separate "colorer refinement" commit). The colorer **already** classifies the helper fn as CPS via SCC bridge propagation (regression-pinned at `5cc3a58`); if the bug persists, codegen is still emitting the helper with native calling convention despite the CPS classification. Verify codegen's per-fn ABI selection consults `ColoredProgram.colors`. If the bug is that the colorer has dropped the call-graph edge, run the three regression tests in `color::tests` (handle_body_calling_cps_helper_makes_caller_cps_via_bridge etc.) — at least one should fail, naming the regressed rule.
- *Non-tail k(x) returns wrong value* — non-tail-k lift commit. The lambda-lifted continuation's body or the `NextStep::Call(k_closure, k_fn, [arg, post_k_closure, post_k_fn])` construction is the prime suspect.
- *Multi-shot k second invocation produces same value as first / corrupts first invocation's result* — multi-shot lift commit. The heap-reified continuation's environment is the prime suspect; verify the closure record is read-only after construction (re-invocation must not mutate).
- *Arm body inside lambda crashes / reads wrong capture* — surrounding-lambda captures commit. The typecheck side-table's lambda-frame source annotation or codegen's read-from-lambda-`closure_ptr` lowering is the prime suspect.

The bisecting hint is also embedded inline at the prime-suspect code change sites for each failure mode, so a `git blame` lands on the deviation entry without a documentation cross-reference.

**Cadence pivot (added 2026-04-27 mid-Phase-4e, mirroring the foundation entry's "Cadence pivot" pattern):** the original Deviation text claimed *"Phase 4e ships **comprehensively** in a single PR — all four lifts plus the architectural pieces they share"* but **that turned out wrong in practice**. PR #26 (`plan-b-task-55-phase-4e`) reached 27 commits at HEAD `adf0e23` with three of the four lifts shipped — codegen consumes color (CPS-color user-fn calling convention + native↔CPS interop wrappers), colorer regression-pinning (item 4), and lambda-lifting first slices for the helper's non-tail body shape (`ConstantDone` constant-tail synth-cont at `b818fc3`; captures-free `LetBindThenTail` at `2a9958b`; captures-bearing `LetBindThenTail` at `a5ee4c6`). Hard condition #2 closed (both e2e test inversions landed at `b818fc3` / `2a9958b`). The remaining three lifts — non-tail `k` use in **arm bodies** (item 5), multi-shot `k` (item 6), surrounding-lambda captures (item 7) — split off into a follow-up PR.

The pivot reasoning:

- **Reviewability — single-PR LOC budget.** PR #26 net diff sits around +5,000/-300 lines across 25 substantive commits + foundation/test-coverage commits, with two concurrent reviewers (boldfield contextual + boldfield no-context) cycling roughly every 1–2 commits. Bundling the remaining three lifts on the same branch projects to ~8,000–10,000 lines and another 15+ commits, which exceeds the threshold where the reviewer pair can verify a single-pass merge. The foundation entry's PR-#22 size (2,373/-272 with 4 fixup commits) was already at the upper bound; PR #26's current size is double that.
- **Architectural slice independence.** The three remaining lifts share machinery (lambda-lifted continuations passed via the trailing `(post_k_closure, post_k_fn)` pair) but each is structurally independent: the arm-body walker rejection lifts in-isolation per slice; the synth-cont signature shift to "always read trailing post-k pair" can land as the foundation refactor of the captures+ PR; multi-shot's `resumes_many` gating is orthogonal to the trailing-pair convention. Splitting at the helper-side / arm-side boundary keeps the reviewer's diff per PR within the verifiable range while preserving the architectural integrity of each slice.
- **Charter-invariant re-confirmation.** Mid-Phase-4e investigation surfaced an alternative arm-body-lowering shape ("synchronous drive in arm body" — arm fn drives `sigil_run_loop` internally per `k(arg)` call) that violates Plan B's stack-bounded trampoline charter (`run_loop` must remain the unique stack-bounded driver; nesting it inside arm-body lowering grows the C stack proportionally to handle depth). The lambda-lifting framing in sections 5 and 6 of this entry IS the right architecture; this entry's commitment to that framing is preserved. The follow-up PR explicitly does not revise sections 5/6 — it implements them as written.
- **Stage 9 hard-gate timing.** The Stage 9 unblock condition is "comprehensive Phase 4e correctness", which requires items 5/6 to land. Stage 9's `scripts/validate-spec.sh` does not become runnable until the captures+ PR squash-merges. The "(closes at this PR)" suffix tracking the Stage 9 hard gate moves to the captures+ PR's foundation commit. The `PLAN_B_PROGRESS.md` Phase 4e entry's HARD GATE annotation gains a "split across PR #26 + captures+ PR" note.

**What ships at PR #26 (closing here):**

- All architectural foundation: CPS-color user-fn calling convention; native↔CPS interop wrappers; codegen-consumes-color per-fn ABI selection; colorer regression-pinning.
- Lambda-lifting first slices for helper-side non-tail bodies: ConstantDone (constant-tail), LetBindThenTail captures-free, LetBindThenTail captures-bearing.
- Hard condition #2 (both test inversions) — `discard_k_handler_does_not_abort_helper` and `statement_form_non_io_perform_inside_handle_compiles_and_runs`.
- FFI-ref dedup (`prepare_per_fn_refs`) at `73c7e53`.
- Pattern-walker dedup + Char capture e2e test + doc-comment precision at `adf0e23`.

**What's deferred to the `plan-b-task-55-phase-4e-captures` follow-up PR (Phase 4e captures+):**

- Item 5: non-tail `k` use in arm bodies — walker rejection lifts; arm-body lambda-lifting via post-arm-k synth fn; trailing-pair convention `[arg, post_arm_k_closure, post_arm_k_fn]` at the arm's `Call` emission; helper's synth-cont signature shifts to uniformly read trailing post-arm-k pair (slice A foundation refactor).
- Item 6: multi-shot `k` use — `resumes_many` gating in pre-pass; heap-reified k_closure for `resumes: many` arms; `Choose`-style e2e test pinning multi-invocation invariance.
- Item 7: surrounding-lambda closure captures into arm bodies — typecheck side-table extension; codegen reads from lambda's `closure_ptr` for outer-scope captures; the `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending` `#[ignore]`'d test inverts.
- Test inversion + cleanup (walker's Phase-4e-pointing diagnostics removed) — at the captures+ PR's closeout commit.
- README "Verification limits" section's full Phase-4e-closes-here statement — moves to the captures+ PR's closeout.
- `PLAN_B_PROGRESS.md` Phase 4e final status flip to `done-pending-ci` — moves to the captures+ PR's closeout.

The original Deviation text's "comprehensive single-PR" claim is preserved unchanged above for historical accuracy. Future agents resuming Phase 4e work after this point should expect: (1) PR #26 closes with this addendum, (2) `plan-b-task-55-phase-4e-captures` branch opens against `main` (NOT against `plan-b-task-55-phase-4e`) — captures+ inherits the architectural foundation through `main` after PR #26 squash-merges, (3) the captures+ PR's foundation commit lands the helper-synth-cont signature shift (slice A) so subsequent slices have a consistent base to layer on.

**Implementing commit(s) for this addendum:** the closeout commit on `plan-b-task-55-phase-4e` (this addendum + README/PROGRESS updates noting the deferral). PR #26 description refresh names the addendum location for reviewers landing after the split.

## 2026-04-27 — [DEVIATION Task 55] Phase 4e captures+ — non-tail `k` in arm bodies + multi-shot `k` + surrounding-lambda captures + closeout

**Context:** PR #26 (`plan-b-task-55-phase-4e`, squash-merged at `2a3cb25` on `main`) shipped the architectural foundation of comprehensive Phase 4e — CPS-color user-fn calling convention, native↔CPS interop wrappers, codegen-consumes-color per-fn ABI selection, colorer regression-pinning, helper-side lambda-lifting first slices (ConstantDone constant-tail synth-cont, captures-free LetBindThenTail, captures-bearing LetBindThenTail), FFI-ref dedup, hard-condition #2 closure (both discard-`k` test inversions). The cadence-pivot addendum at the end of the prior `[DEVIATION Task 55] Phase 4e — comprehensive` entry documents the split. This entry's PR — `plan-b-task-55-phase-4e-captures` — closes the residual three lifts (items 5/6/7 of the comprehensive entry) plus the closeout cleanup.

**Deviation:** This is the second half of the comprehensive Phase 4e effort, branched against `main` post-PR-#26-squash-merge. Per the prior entry's cadence pivot, sections 5 and 6 of the comprehensive entry are NOT revised — captures+ implements them as written. The path-1 lambda-lifting + trailing-pair architecture remains the standing commitment; the alternative path-2 "synchronous drive in arm body" was investigated mid-PR-#26 and explicitly rejected because nesting `sigil_run_loop` inside arm-body lowering violates Plan B's stack-bounded trampoline charter (`run_loop` must remain the unique stack-bounded driver; nesting it grows the C stack proportionally to handle depth, even when v1's test bounds don't surface unbounded recursion).

**Scope — three lifts + closeout, layered as four roadmap commits:**

1. **Slice A — helper-synth-cont signature shift (foundation refactor).** The PR #26 helper synth-cont (lambda-lifted post-perform body) takes `args_ptr=[arg]`, `args_len=1`, returns `Done(result)`. To support arm bodies that compute around a `k(arg)` call, the synth-cont's signature shifts uniformly: it now reads `[arg, post_arm_k_closure, post_arm_k_fn]` from `args_ptr` (`args_len=3`), and returns `Call(post_arm_k_closure, post_arm_k_fn, [result])` instead of `Done(result)`. Tail-`k` arms (which today emit `Call(k_closure, k_fn, [arg])`) shift to emit `Call(k_closure, k_fn, [arg, null, &sigil_continuation_identity])` — the identity continuation receives the synth-cont's result as its single arg and produces terminal `Done(result)`, preserving existing observable behaviour with one extra trampoline hop. Slice A is the **invariant-preserving foundation refactor** that subsequent slices layer on. The PR #26 captures-bearing-LetBindThenTail e2e tests (`captures_bearing_synth_cont_arity_n_helper_use_k`, `captures_bearing_synth_cont_arity_n_helper_discard_k`, multi-capture variants, Bool/Char widen-narrow tests) all continue to pass through Slice A — any regression here means the trailing-pair convention is wrong.

2. **Slice B — non-tail `k` in arm bodies (item 5).** With Slice A's trailing-pair convention in place, the walker's "Phase 4d MVP supports `k(arg)` only as the tail expression of an arm body" rejection at `arm_body_walk` lifts for specific shapes. The arm body's post-`k` rest gets lambda-lifted into a **post-arm-k synth fn** (the arm-side analogue of helper's synth-cont). The arm fn emits `Call(k_closure, k_fn, [arg, post_arm_k_closure, post_arm_k_fn])` where `(post_arm_k_closure, post_arm_k_fn)` is the lambda-lifted continuation. The trampoline dispatches: invoke `k_fn(k_closure, [arg, post_arm_k_closure, post_arm_k_fn], 3)`. The k_fn — Slice A's helper-synth-cont — produces the post-perform result, then dispatches to `post_arm_k_fn` with that result, threading control back into the arm-body's post-`k` computation. New e2e tests pin `let r = k(x); r + 1`-shaped arm bodies plus the captures-bearing variant.

   **Grammar reversal at Slice B's parser surface.** The existing arm-body grammar — pre-Slice-B — accepts only single expressions after `=>`. PR #25's CI fixup #2 explicitly noted: *"Sigil v1's parser doesn't accept `{ stmt; expr }` as an expression — Block only appears in fn bodies / if then-else branches via parse_block."* Slice B requires `{ let r = k(arg); pure_tail }` arm bodies — the let-binding cannot be expressed without a Block expression at arm-body parse position. The minimum-viable fix is to extend `parse_primary` to accept `TokenKind::LBrace` as a primary expression returning `Expr::Block(parse_block())`. The change is orthogonal — block expressions parse cleanly in any expression position (let-binding RHS, match-arm body, lambda body) and existing Block-context parsers (`parse_fn_decl`, `parse_if_expr`) continue to call `parse_block` directly via dedicated parsers, unaffected. This IS a grammar reversal of PR #25's CI fixup #2 framing; the reversal is documented here so future readers can grep for the pivot. The 2 parser unit tests added in Slice B's polish (`block_parses_as_primary_expression_at_let_binding_rhs` and `block_parses_as_primary_expression_at_arm_body`) pin the surface so a future grammar change that silently regresses block-as-expression in other positions fires precisely.

3. **Slice C — multi-shot `k` via heap-reified continuation (item 6).** Codegen detects `resumes_many` from the EffectDecl registry at arm synthesis time. For arms of `effect E resumes: many`, the continuation closure is heap-allocated (TAG_CLOSURE; pointer bitmap covering captured slots) so that calling `k(arg1)` and `k(arg2)` with the same closure produces fresh `NextStep::Call`s that drive the helper synth-cont independently. The runtime side already supports this — `HandlerFrame.arms[i].closure_ptr` is pointer-shaped and the precise GC bitmap covers it. The one-shot linearity check (E0220, Task 54) remains in place for `resumes: one` (default) effects; multi-shot effects skip it (already implemented in typecheck). New e2e tests pin `Choose`-style usage; runtime tests pin invariance of multi-shot k under repeated invocation.

   **Slice C v1 first-commit shape: 2-let chained lambda-lift.** The minimum source surface that exercises multi-shot `k` is the explicit two-let arm body `{ let r1: T1 = k(arg1); let r2: T2 = k(arg2); pure_tail }` (recognised by `arm_body_multi_let_then_pure_tail_shape`). The pre-pass allocates TWO post-arm-k synth fns per matching arm: `post_arm_k_1` (handles the post-`k(arg1)` rest = `let r2 = k(arg2); pure_tail`) and `post_arm_k_2` (handles the post-`k(arg2)` rest = `pure_tail`). The arm-fn body emit lowers `arg1`, allocates a heap TAG_CLOSURE record capturing `(k_closure, k_fn)` (the helper synth-cont's closure + fn pointer), and emits `Call(k_closure, k_fn, [arg1, post_arm_k_1_closure, post_arm_k_1_fn])` per the Slice A trailing-pair convention. `post_arm_k_1`'s body reads `r1` from `args_ptr[0]`, reads `(k_closure, k_fn)` from its own closure_ptr at offsets 16/24, allocates `post_arm_k_2`'s TAG_CLOSURE record capturing `r1`, lowers `arg2_expr`, and emits `Call(k_closure, k_fn, [arg2, post_arm_k_2_closure, post_arm_k_2_fn])` — re-using the SAME `k_closure` (the helper synth-cont's heap-allocated closure record from PR #26's captures-bearing slice). `post_arm_k_2`'s body reads `r2` from `args_ptr[0]`, reads `r1` from its closure_ptr at offset 16, lowers `pure_tail` with both `r1` and `r2` in env, widens, returns `Done(widened)`. Multi-shot semantics emerge from the trampoline dispatching into the helper synth-cont k_fn TWICE with different args: once with `arg1` (returning helper's tail with `b=arg1`), then again with `arg2` (returning helper's tail with `b=arg2`).

   **Slice C first-commit restrictions:**
   - Effect declared `resumes: many` (one-shot effects continue to be rejected — the typecheck E0220 linearity gate already fires for one-shot multi-`k` paths but codegen mirrors the gate at the walker for completeness).
   - Both `arg1` and `arg2` must satisfy `expr_is_pure`.
   - `pure_tail` must satisfy `expr_is_pure` and reference only `r1`, `r2`, plus globals (the same free-var restriction Slice B's first commit applies, with the allowed set extended to two binding names).
   - Walker rejects multi-let with `resumes: one` effect with a Slice-suffix-pointing diagnostic (would have been caught by E0220 at typecheck, but the codegen-side gate surfaces it explicitly).

   **Deferred from Slice C v1 (future captures-bearing extension):**
   - More than 2 `k` invocations (3+ requires generalising the chain to N — straightforward but layered; v1 commits to the minimum that demonstrates multi-shot).
   - Captures from outer scope into the chain (paralleling Slice B's deferred captures-bearing extension; the closure record's bitmap encoding pattern is the same).
   - Binary-of-`k`-calls source surface (`k(arg1) + k(arg2)` directly) — would require a different shape detector that lifts the LHS k-call into a let, then hits the same chain. Multi-let is the more explicit and reviewable v1 surface.

   **Stage 9 P20 readiness analysis.** P20's canonical "all-pairs summing to N" idiom uses `Choose: () -> Bool` plus helper recursion (a `pick_int(low, high)` fn that recurses with `if perform Choose.flip() then low else pick_int(low+1, high)`). The arm body handles each Bool `Choose.flip()` invocation with the 2-let pattern `let r1 = k(true); let r2 = k(false); concat(r1, r2)` — exactly Slice C v1's accepted surface. **Slice C v1 is sufficient for P20 IF P20 uses the canonical Bool-Choose + helper-recursion idiom.** N-let chains for arbitrary option counts are deferred (would need the captures-bearing extension that generalises the chain to N). Concrete verification — write the actual P20 prompt's expected program, compile + run it through the captures+ branch, confirm canonical-vs-N-let — should land at the captures+ closeout commit; if P20 produces a non-canonical pattern, the captures-bearing extension becomes a Stage-9 prerequisite. Until that verification lands, the PR's "Stage 9 unblock — closes here" claim stays accurate for the canonical idiom; readers should treat it as conditional on the post-merge P20 walk-through.

   **GC-stackmap audit deferral.** Slice C's three new sites (arm-fn body emit's `widened_arg1` + `post_arm_k_1_closure_ptr` lives across the next_step_call; `post_arm_k_1` body's `widened_arg2` lives across the closure-record alloc) carry the same forward concern as Slice B's `TODO(plan-b-task-55-phase-4e-captures/slice-b-stackmap-root)` marker: heap pointers live across arena allocations need GC root tracking. Today the Slice C String-typed e2e test (`slice_c_choose_multi_shot_with_string_chain_threads_pointer_through_closures`) exercises the bitmap-encoding + pointer-typed-slot read/write path, but the strings are static literals (`sigil_string_new` returns pooled refs) — fresh heap String allocations across the chain aren't exercised. **Deferred to the captures+ closeout commit:** either document the StackMapBuilder/Lowerer auto-tracking guarantee (does Boehm see the live SSA values across arena allocs?) OR add explicit root annotations at the four stackmap-root TODO sites (Slice B's load + Slice C's three alloc-and-call sites). End-to-end verification with fresh-heap-String allocations is gated on Stage 6 stdlib growth (when `String.concat` / `String.from_int` etc. ship — Sigil's stdlib currently doesn't expose runtime string allocation).

4. **Slice D — surrounding-lambda closure captures into arm bodies (item 7).** Phase 4d MVP's `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending` `#[ignore]`'d test pins the gap. Phase 4e captures+ extends the typecheck-side `handle_arm_captures` side-table with a per-arm "lambda-frame source" annotation indicating whether each capture comes from the immediate surrounding fn's locals (today's path) or from an enclosing lambda's closure record (new path). Codegen's arm-closure-record allocation site reads from the lambda's `closure_ptr` (already in scope at the arm-body's lowering) for the latter, instead of from `Lowerer.env`. The walker's `Expr::ClosureEnvLoad` rejection in arm bodies lifts. The `#[ignore]`'d test inverts to a positive test.

   **Slice D implementation note (deviation from the framing above).** The implementation does NOT extend the typecheck-side `handle_arm_captures` side-table with a "lambda-frame source" annotation. Instead, the codegen-side pre-pass (where the post-closure_convert arm body is available) scans the arm body for `Expr::ClosureEnvLoad` nodes per capture name and populates an `ArmCapture::lambda_source: Option<(usize, EnvSlotKind)>` field directly. closure_convert rewrites every reference to an outer-scope name uniformly within a lifted lambda's body, so the first matching `ClosureEnvLoad` node's `(index, kind)` is the lambda-slot info for that name; the scanner just picks one (`find_closure_env_load_lambda_source`).

   The codegen-side detection is preferred over the typecheck-side annotation because closure_convert is the source of truth for which names get rewritten — it runs after typecheck and has full visibility into the lambda-lift transformation. Wiring a side-table from typecheck would have required typecheck to mirror closure_convert's free-var analysis, which is duplicative work with subtle drift risk (typecheck and closure_convert could disagree on what counts as a "free var" of a lambda's body). The codegen scanner reads the truth directly from closure_convert's output.

   At handle codegen time, `alloc_arm_closure_record` branches on `ArmCapture::lambda_source`: `Some((idx, kind))` ⇒ emit `lower_closure_env_load(idx, kind)` against the surrounding fn's `closure_ptr` (which IS the lifted lambda's closure_ptr at this point); `None` ⇒ existing Phase 4d MVP path that reads from `self.env` by name. Walker's `Expr::ClosureEnvLoad` arm in `arm_body_walk` is now a no-op (returns None). Phase 4d MVP's `#[ignore]`'d test inverted to a positive test asserting stdout `7`.

   Five new unit tests pin `find_closure_env_load_lambda_source`'s match/no-match boundary (direct ClosureEnvLoad match; plain Ident no-match; Binary subtree match; different-name no-match; first-match-wins for multi-occurrence safety).

5. **Closeout commit.** Test inversion completion + walker's Phase-4e-pointing diagnostics removed (the gates they pointed at are all lifted) + README "Verification limits" section's full Phase-4e-closes-here statement (with explicit `Phase 4f` / `Phase 4g` framing for what remains) + `PLAN_B_PROGRESS.md` Phase 4e final status flip to `done-pending-ci`.

**What this PR does NOT lift (deferred to Phases 4f / 4g):**

- Multi-effect handles → **Phase 4f** (renumbered from prior 4e in the Phase 4d MVP entry).
- Return arms → **Phase 4g** (renumbered from prior 4f).

**Architectural choices and rationale:**

a) **Slice A as a separate commit, not bundled with Slice B.** The trailing-pair convention is an ABI shift to the helper synth-cont's signature: every existing tail-`k` arm needs to update its `Call` emission to pass `[arg, null, &identity]` instead of `[arg]`, and the synth-cont needs to update its body to read+forward the trailing pair. This is invariant-preserving — every PR #26 captures-bearing-LetBindThenTail e2e test continues to pass through Slice A. Bundling this with Slice B's walker-rejection lift would conflate two failure modes (trailing-pair-convention bugs vs walker-rejection-lift bugs) and make a regression bisect-unfriendly. Slice A as its own commit gives a clean checkpoint where the existing test surface continues to pass under the new ABI, before Slice B layers the new arm-body shape on top.

b) **Lambda-lifting + trailing-pair preserved over synchronous-drive.** Plan B's trampoline `sigil_run_loop` is the unique stack-bounded driver. Codegen lowering MUST NOT emit `run_loop` invocations inside contexts that themselves run under a `run_loop` dispatch. The path-2 alternative ("arm fn drives `sigil_run_loop` internally per `k(arg)` call") was investigated mid-PR-#26 because it has appealing structural simplicity — multi-shot `k` falls out for free as "each invocation is just another sub-call" — but it nests `run_loop` at a depth proportional to handler stack depth, which violates a load-bearing charter invariant. The lambda-lifting path is more synth-fn machinery but preserves stack boundedness.

c) **Helper-synth-cont uniform signature, not per-arm-shape branching.** Slice A could in principle make the helper synth-cont's signature depend on the arm's tail-vs-non-tail shape (1 arg for tail-`k` arms; 3 args for non-tail arms). That would be wrong — the helper synth-cont is one fn, and the runtime dispatches into it from `Call`s emitted by potentially many different arms (a multi-arm handler can target the same effect's same op only via E0140 violation, but other helpers in the program share the helper-synth-cont's `FuncId` alongside their own fns). A uniform signature is the only correct shape. The convention "tail-`k` arms pass `[arg, null, &identity]`" surfaces the trailing-pair commitment at the arm's `Call` emission site, where the cost is one extra trampoline hop per tail-`k` invocation — observable as a small perf regression but not a semantic difference.

d) **Multi-shot heap-reified k_closure follows Phase 4d's TAG_CLOSURE pattern.** No new tag bit, no new runtime allocator, no new pointer bitmap convention — the k_closure record uses the existing TAG_CLOSURE header + arm-local pointer bitmap from PR #26's captures-bearing slice. Boehm precise GC tracks them via the standard 8-byte object header bitmap. The runtime's `HandlerFrame.arms[i].closure_ptr` already points to a TAG_CLOSURE record in PR #26's captures-bearing path; the multi-shot extension just makes the record reusable across arm invocations.

e) **`resumes_many` gating in pre-pass, not in walker.** The arm-body walker rejects multi-`k`-invocation patterns today by virtue of rejecting non-tail `k` (which is required for any arm body that invokes `k` more than once). Slice B lifts non-tail-`k` rejection; Slice C's `resumes_many` gate is structurally separate — it determines whether the k_closure is heap-reified (multi-shot) or stack-bound (one-shot). The walker doesn't need to know; the arm-fn definition pass does. This keeps the walker's surface narrow.

**User's hard conditions for Phase 4e captures+ (mirroring the comprehensive entry's pattern, with the cadence-pivot deferrals applied):**

1. README "Verification limits" — final cleanup at the closeout commit. The entry transitions from "what closes at PR #26 vs what's deferred to captures+" prose (PR #26's README state) to clean "Phase 4f / Phase 4g" framing for the residual work (multi-effect, return arms). Hard condition #1 from the comprehensive entry, deferred via the cadence pivot, closes here.
2. Phase 4d MVP's `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending` `#[ignore]`'d test inverts to a positive test at the Slice D commit.
3. `PLAN_B_PROGRESS.md` Phase 4e entry's Stage 9 HARD-GATE annotation gains "(closes at this PR)" suffix at the closeout commit.
4. Bisecting hint pattern: failure-mode catalogue per slice (A/B/C/D) so a future bisecting agent can attribute regressions to specific commits.

**Stage 9 unblock — closes here.** The Phase 4e Stage-9 HARD GATE — gated on multi-shot `k` and discard-`k` correctness — closes when this PR squash-merges. PR #26 closed the discard-`k` correctness piece; captures+ closes the multi-shot `k` piece (P20). When this PR merges, `scripts/validate-spec.sh` (Stage 9 of `2026-04-21-sigil-finish.md`, plan items 85–87) becomes runnable. The validation prompt bank's algebraic-effects entries (Plan A/B-bank P6 `Raise[String]`-based safe parser, P19 `State[Int]` counter, P20 multi-shot `Choose`) all become measurable.

**Implementing commit(s):** [HEAD] (this commit lands the deviation entry + PROGRESS update only; subsequent commits implement Slices A → B → C → D → closeout).

**Closure point:** When the captures+ PR squash-merges, Phase 4e closes fully. The next big closure point is **Phase 4f** (multi-effect handles) — the remaining feature-breadth work after Phase 4e closes the algebraic-semantics gap. Phase 4g (return arms) follows. Tasks 57–61 then complete Stage 6.

**Bisecting hint pattern (per slice):**

- *PR #26 captures-bearing-LetBindThenTail tests regress after Slice A* — the trailing-pair convention is wrong. Verify the helper synth-cont's body reads `args_ptr[0..3]` correctly, returns `Call(post_arm_k_closure, post_arm_k_fn, [result])` shape, and that tail-`k` arms emit `[arg, null, &identity]` instead of `[arg]`. The identity continuation must observe the synth-cont's result and return `Done(result)`.
- *Non-tail `k(x)` returns wrong value* — Slice B. The post-arm-k synth fn's body or the `Call(k_closure, k_fn, [arg, post_arm_k_closure, post_arm_k_fn])` construction is the prime suspect. Verify post-arm-k synth fn signature matches CPS ABI and that the result threading via the trampoline is correct.
- *Multi-shot `k` second invocation produces same value as first / corrupts first invocation's result* — Slice C. The heap-reified k_closure record is the prime suspect; verify it's read-only after construction (re-invocation must not mutate). Also verify the runtime's `HandlerFrame.arms[i].closure_ptr` is preserved across arm dispatch and not freed prematurely.
- *Arm body inside lambda crashes / reads wrong capture* — Slice D. The typecheck side-table's lambda-frame source annotation or codegen's read-from-lambda-`closure_ptr` lowering is the prime suspect.

The bisecting hint is also embedded inline at the prime-suspect code change sites for each failure mode, mirroring the comprehensive entry's pattern.

## 2026-04-28 — [DEVIATION Task 55] Phase 4f — multi-effect handlers via push-N-frames

**Context:** Phase 4e (closed at `3affced` via PR #27 squash-merge on 2026-04-28) shipped the algebraic-semantics correctness gate: discard-`k` across function-call boundaries, non-tail `k`, multi-shot `k`, and surrounding-lambda closure captures into arm bodies. Phase 4f is the residual feature-breadth work for **multi-effect handlers** — `handle expr with { E1.op1(k) => …, E2.op2(k) => …, … }` where arms target more than one declared effect. Currently rejected at codegen entry by `unsupported_handle_construct` at `compiler/src/codegen.rs:744` ("`handle` expression at … has arms targeting different effects (`{}` and `{}`) — multi-effect handlers are not yet supported in codegen (Plan B Task 55, in progress; arrives in Phase 4e via frame-per-effect)" — note the stale "Phase 4e" reference, addressed by hard condition #4 below). Typecheck already accepts multi-effect handles (Task 54's `check_handle` registry dispatch enumerates arms across effects without restriction); the gap is purely codegen-side.

**Deviation:** Phase 4f ships as one focused PR off `main` per the per-phase-PR cadence (`plan-b-task-55-phase-4f` branch). Architecture is **Option A — push N frames at handle entry** (one `HandlerFrame` per distinct effect, sharing arm-fn allocation pattern; `sigil_handler_frame_new(effect_id, arm_count)` called once per effect; pushed in BTreeMap-stable iteration order; popped in reverse at handle exit). The runtime ABI (`HandlerFrame`, `sigil_perform`, the GC bitmap derivation, `MAX_HANDLER_ARMS = 14`, `sigil-abi`) stays unchanged. Only `compiler/src/codegen.rs` and the codegen-entry walker change at the user-visible level; `runtime/src/handlers.rs` is touched only for any new debug-assert or test additions, not for layout.

**Why Option A over Option B (extend `HandlerFrame` to carry multiple `effect_id`s):**

The lead reason is **architectural reversibility**. Option A keeps the runtime ABI stable while Phase 4f / 4g / Plan C work continues. If Phase 4f's actual usage data reveals A is wrong — walk-depth inflation matters for Stage 9 demo perf, return-arm coordination becomes painful, the diagnostic enumerates N frames in a way that confuses LLM authors — **B can be layered on top as a Phase 4f-2 commit** with a clean ABI version bump. The ABI bump is cheaper to do *later* with concrete motivation than *now* on speculation. With B, the ABI bump ships unconditionally; reversing to A means a second ABI bump (and the cross-crate ripple of a second ABI version constant in `sigil-abi`). Phase 4f is itself an architectural exploration — there is no production data on what multi-effect handlers actually look like in practice yet. A is the conservative learning choice; B is a pre-commitment to a more invasive change before that data exists.

Supporting reasons (de-emphasized — soundness rather than load-bearing):
- *Zero ABI surface change.* The `HandlerFrame` 32-byte header (`effect_id` u32 + `arm_count` u32 + `return_fn` ptr + `return_closure` ptr + `prev` ptr) stays untouched. The precise GC bitmap derivation (Task 56's `handler_frame_pointer_bitmap`) stays untouched. The `MAX_HANDLER_ARMS = 14` cap derivation (set by the 32-bit bitmap) stays untouched.
- *Composes with Phase 4g's return-arm work.* Return arm registers on a single per-handle frame; Phase 4f's frame-grouping decision (concern #2 below) pins which one — the first-pushed (bottom-of-handle-group) frame.
- *Only observable inflation is the (non-budget-pinned) walk-depth counter.* Verified concretely below as concern #4.

**Phase 4f-2 escape valve:** Option A is a deliberate choice, not the only choice considered. If Phase 4g's return-arm coordination or walk-depth diagnostic complexity surfaces unexpected friction, Phase-4f-2 layering of B on top is the documented escape valve — future designers should know this was negotiated at scope time, not stumbled into.

**Approach:**

At every `Expr::Handle` codegen site:

- Group `op_arms` by `arm.effect` using `BTreeMap<String, Vec<&HandleOpArm>>` (BTreeMap-stable iteration order — preserves the existing determinism discipline that drives all of Plan B).
- For each distinct effect (in the BTreeMap's iteration order — the *push order*):
  - Allocate one `HandlerFrame` via the existing `sigil_handler_frame_new(effect_id, arm_count)` FFI. `arm_count` matches the existing single-effect convention exactly (no semantic change — the convention is what it is).
  - Populate that frame's arms via `set_arm` for every arm in this effect's group. Arm-fn `FuncRef` lookup uses the existing `handler_arm_refs_per_handle` side-table; the per-arm `closure_ptr` is computed via the existing `alloc_arm_closure_record` Phase 4d machinery (untouched).
  - Push via `sigil_handle_push(frame)`.
- Body lowering: unchanged.
- At handle exit: pop N frames via N `sigil_handle_pop()` calls in reverse push order (LIFO discipline matches the runtime stack contract).
- Walker (`unsupported_handle_construct` at `compiler/src/codegen.rs:744`): drop the "all arms reference the same effect" rejection; replace the rejection with a positive path. The walker still validates `op_arms.len() > 0` and the per-(effect, op) duplicate-arm rejection (typecheck E0140 already covers the latter; codegen guards defensively).

**`arm_count` semantic clarification (per-frame vs per-handle):** today, `frame.arm_count` and "total arms in this handle" coincide because the single-effect rule forces 1:1 frame-to-handle correspondence. Under Option A those quantities **diverge**: `arm_count` becomes strictly per-frame; the per-handle aggregate is `Σ frames[i].arm_count`. **Audit done at this commit:** every consumer of `arm_count` in `runtime/src/handlers.rs` and `compiler/src/codegen.rs` reads it as a per-frame quantity (`HandlerFrame.arm_count` field; `sigil_handler_frame_new(effect_id, arm_count)` parameter; bounds check `if op_id >= (*frame).arm_count` in `set_arm` and the perform walk; `handler_frame_payload_bytes(arm_count)` and `handler_frame_pointer_bitmap(arm_count)` derivations). No consumer reads it as a per-handle quantity. The `MAX_HANDLER_ARMS = 14` cap therefore applies **per-frame**, not per-handle — a multi-effect handle can collectively carry up to `MAX_HANDLER_ARMS × N_effects` arms.

The single-effect path becomes a special case of the multi-effect path (one BTreeMap entry → one frame). The codegen lowering is structured so the single-effect path **behaves the same** as today (one alloc-set-push sequence); whether it emits identical Cranelift IR (no extra BB / phi / stack slot from the BTreeMap-iteration scaffolding) is a polish-round verification target rather than a foundation-level guarantee — the deviation entry doesn't pre-commit to byte-for-byte IR equivalence on the single-effect path. If a polish-round IR diff shows new BBs / phi-nodes / stack slots introduced by the iteration scaffolding, that's a polish-round fixup target, not a regression against this entry.

**Pre-registered concerns (1–5):**

1. **Frame stack discipline at multi-effect handle exit.** Single-frame discipline has a structural "one push = one pop" guarantee; Option A relaxes to "N pushes = N pops" and any miscount corrupts stack state. PR #21's frame-leak-on-body-unwind concern (the [DEVIATION Task 55] entry titled "Handler frame leaks on body unwind; depends on Task 57 to surface" earlier in this file) gets amplified proportionally — today's leak is 1 frame per unwound handle; under Option A it's N frames per unwound multi-effect handle. Same Task 57 closure point; the magnitude grows linearly with effect count. **Mitigation in Phase 4f, no new FFI required — pointer-comparison mechanism:** the discipline check uses the **frame_1 pointer snapshot already required by concern #2's return-arm contract**. After `sigil_handler_frame_new` for the first effect, codegen captures the returned frame ptr into a Cranelift stack local — call this `frame_1_ptr_snapshot`. After pop_N (the last pop, which under LIFO discipline must return frame_1 since it was first-pushed), codegen `#[debug_assert_eq!]`'s the value returned by `sigil_handle_pop()` against `frame_1_ptr_snapshot`. If they disagree, an under-pop happened during body execution (last pop returned an inner frame). Over-pop is caught by the runtime's existing underflow abort at `runtime/src/handlers.rs:442`. **Why this works and the prior `frame.prev` mechanism didn't:** `sigil_handle_pop()` deliberately zeros `(*head).prev = ptr::null_mut()` at `runtime/src/handlers.rs:451` so a legitimate handle-in-loop re-entry doesn't trip the no-double-push debug_assert at `sigil_handle_push`. Reading `frame_1.prev` after pop_N always reads null regardless of discipline state, so a `prev`-comparison check would have misfired 100% of the time on the happy path. The pointer-comparison check above doesn't rely on any field of the frame — it compares the snapshot of frame_1's allocation address against the runtime's last-popped-frame return value, which is exactly what LIFO N-pops-of-N-pushes guarantees. Pitch-preserving: zero ABI surface change, no new FFI declarations, single snapshot serves both concern #1 (discipline check) and concern #2 (return-arm registration).

2. **Return arm coordination (Phase 4g) — frame_1 pointer snapshot at push time.** Pin the rule **now** so Phase 4g implements against a documented contract rather than a hand-wave: register the synth return fn on the **first-pushed (bottom-of-handle-group) frame**, since that is the natural "outermost" frame for the handle. Decline "register on every frame and last-matched-wins" — semantically ambiguous and harder to reason about. Decline "register on the last-pushed frame" — the bottom-of-group frame is the one that survives across the body's entire scope, so it's the durable anchor for return-on-body-Done semantics. Phase 4g's PR will reference this contract by anchor: "per the [DEVIATION Task 55] Phase 4f entry's concern #2, return-arm registers on the first-pushed frame." **Mechanical implication for Phase 4f's pop-loop shape (forward-compatibility for Phase 4g):** the first-pushed frame is the *bottom* of the group — i.e., the *last-popped* under LIFO. To apply `return_fn` from frame_1, Phase 4g needs the frame_1 pointer in scope at handle exit; the natural mechanism is to **snapshot frame_1 pointer in a Cranelift stack local at push time** (immediately after `sigil_handler_frame_new` for the first effect). The frame stays Boehm-rooted via the stack-local hold throughout the body, so Phase 4g reads `return_fn` / `return_closure` off the snapshot at handle exit (before, during, or after the pop loop — pop just updates the runtime head, not the frame's data). **Phase 4f emits this snapshot** so the pop-loop shape is forward-compatible; otherwise Phase 4g pays for a re-emit. Side benefit: the same snapshot serves concern #1's discipline check directly — concern #1 compares `sigil_handle_pop()`'s last return value against this snapshot ptr (see concern #1's pointer-comparison mechanism above). One snapshot, both purposes.

3. **`sigil_perform` walk diagnostics for unhandled ops — deferred to Phase 4f-cleanup.** Today's diagnostic ("effect E not handled by any frame on the stack") works perfectly under 1:1 frame-to-handle correspondence. Under Option A, an unhandled op fired inside a multi-effect handle scope walks past N frames belonging to the same handle on the way to an outer handler — the diagnostic enumerates them, which is imperfect but not catastrophic. A clean fix requires a per-frame `scope_id` (or `handle_span` u32 hash) field so the diagnostic can attribute "effect E not handled by the surrounding handle at <span>" rather than enumerate frames. **`scope_id` cannot be added without an ABI change** — the `HandlerFrame` 32-byte header (`effect_id` u32 + `arm_count` u32 + `return_fn` ptr + `return_closure` ptr + `prev` ptr) is fully packed; adding `scope_id` would either grow the struct (shifts arms array offset → changes per-arm GC bitmap derivation → real ABI change), require packed encoding (low 16 bits `arm_count` + high 16 bits `scope_id`, since `MAX_HANDLER_ARMS = 14` fits in 4 bits), or replace an existing field's encoding. All three contradict Option A's "zero ABI surface change" pitch. **Phase 4f-cleanup ships `scope_id` when concrete motivation surfaces** (Stage 9 prompt produces a confusing diagnostic, etc.) — same reversibility logic that drove A over B at the parent level.

4. **`HandlerWalkDepthSum` counter inflation — verified non-blocking.** Concrete check before shipping A: Task 60's perf floors are `fib(20)` native <50ms, `fib(20)` CPS-forced <500ms, multi-shot Choose (N=1000) <5s, arena escape ≤1%. None pin walk depth. `HandlerWalkCount` and `_DepthSum` are tracked counters surfaced via `--print-runtime-stats` but not perf-budget-gated. Option A's depth inflation (linear in effect count per multi-effect handle) is therefore observable but not regression-causing.

5. **`scope_id` deferred to Phase 4f-cleanup** (see concern #3 for full reasoning). Pre-registered explicitly here so a future implementer / reviewer / bisecting agent understands the scope_id absence is deliberate rather than an oversight. Today's diagnostic enumerates N frames of the same handle under multi-effect; quality-of-life improvement requires an ABI consideration (packed encoding or struct grow); deferred until concrete motivation surfaces.

**User's hard conditions for Phase 4f (mirroring the Phase 4d/4e entries' pattern):**

1. The walker rejection-test `handle_with_mixed_effect_arms_is_rejected_at_codegen` (referenced by fn name to avoid line-number rot) inverts to a positive `handle_with_mixed_effect_arms_dispatches_correct_arm_per_effect` test asserting that a `perform` of an op from each effect dispatches the correct arm. Test inversion lands at the codegen-lift commit.
2. `README.md` "Verification limits" section closes the multi-effect row and reframes with **Phase 4g (return arms) as the remaining feature-breadth work**. The closeout commit lands the README change in the same PR.
3. `PLAN_B_PROGRESS.md` Phase 4f entry — added at the foundation commit (this commit; status `in-progress`), filled with implementing-commit list at the closeout commit (`done-pending-ci`), updated with squash-hash post-merge.
4. **Walker / codegen stale-reference sweep — pre-enumerated.** The multi-effect-related stale `Phase 4e` references identified at this commit's grep audit (mirrors PR #27's `0b61935` closeout pattern):
   - `compiler/src/codegen.rs:715-716` — comment `// - all arms reference the same effect (Phase 4e lifts via frame-per-effect)` — referencing now-closed (here-lifted) work; rewrites or removes the forward-pointing language.
   - `compiler/src/codegen.rs:746` — comment `// lands in Phase 4e` — rewrites to reflect that this is what Phase 4f is doing right now.
   - `compiler/src/codegen.rs:755` — diagnostic-string fragment `"arrives in Phase 4e via frame-per-effect"` — entire diagnostic disappears when the rejection lifts; no rewrite needed.
   - `compiler/tests/e2e.rs` — `handle_with_mixed_effect_arms_is_rejected_at_codegen` test docstring + comments + body (inversion to positive replaces the whole content).

   **Other Phase-4e references in the codebase are NOT in this sweep's scope.** Phase 4e captures+ shipped substantial code with `Phase 4e captures+ Slice {A,B,C,D}` documentation comments — those reference work that is closed (not deferred), and only their forward-pointing language (if any) becomes stale, not their factual content. Multi-branch tail-`k` rejections (e.g., `arm_uses_k_inside_if_branch_is_rejected_pointing_at_phase_4e` at `compiler/tests/e2e.rs`) reference rejection paths that are still active for orthogonal-to-multi-effect reasons; rewriting them to "Phase 4f" would be wrong (Phase 4f doesn't lift them). The line 1610 fence is sharp: only references whose subject is multi-effect handlers get rewritten / removed at this PR.

   **Known renumber-victim acknowledged but out-of-scope under the sharp fence:** `compiler/src/codegen.rs:712-714` reads `// no return arm (Phase 4f lifts via a synthetic return-fn registered via sigil_handler_frame_set_return)`. Post-Phase-4d-MVP renumbering, return arms are **Phase 4g**, not Phase 4f. This is a renumber-victim that escaped both the Phase 4d MVP commit's renumber and PR #27's `0b61935` closeout sweep. Subject is return arms (Phase 4g), not multi-effect (Phase 4f), so it falls outside this PR's sharp fence. **Flagged for a separate follow-up sweep task** so the next reviewer touching that area doesn't re-discover the same context. Phase 4g's PR is the natural place to fix it (cleaning up renumber-victims as the work referenced by the comment actually lands).

**Tests not pre-committed as hard conditions but expected in the polish round** (per the recent PR cadence's "comprehensive coverage in polish round, then no-context reviewer flags gaps" discipline):

- Classifier + walker unit tests at the diagnostic boundary: BTreeMap grouping invariants (deterministic order across runs given the same input; correct per-effect arm counts in the grouped output; empty-effect and single-arm-per-effect edge cases).
- **BTreeMap source-order independence test.** Reorder arms in source position (e.g., `E2.b → E1.a → E2.a → E1.b` vs `E1.a → E1.b → E2.a → E2.b`) and confirm the BTreeMap-grouping-by-effect produces the same iteration order regardless of source order. This pins frame-push order to effect-id-lex-order, not to source-position-of-first-arm — the actual property the BTreeMap is doing the work of.
- Multi-effect dispatch e2e with at least two effects × two arms each (single-arm-per-effect doesn't exercise per-frame arm-slot population fully).
- Per-effect arm-count edge case (positive): one effect with `MAX_HANDLER_ARMS` arms in a multi-effect handle; verifies the cap applies per-frame, not per-handle (i.e., a multi-effect handle can collectively have up to `MAX_HANDLER_ARMS × N_effects` arms, not just `MAX_HANDLER_ARMS` total).
- **Per-effect arm-count edge case (negative): one effect with `MAX_HANDLER_ARMS + 1` arms inside a multi-effect handle gets rejected.** The cap applies in both directions — per-frame, not per-handle — and the rejection points at the same per-frame structural limit that drives the runtime's bitmap derivation. Verifies that lifting the multi-effect rejection didn't accidentally allow a per-frame cap violation through.

**Latent op_id/arm_count constraint surfaced during the codegen-lift mid-flight review (commit `65727c2`)** — pre-existing, not Phase 4f-introduced, but Phase 4f expands the user-reachable surface and so the constraint is documented here for future hardening. Op_ids are assigned alphabetically per-effect at `compiler/src/typecheck.rs:792-808`, globally over the effect's full declared op set. Codegen sizes `arm_count` to the handle's arm count for that effect (the BTreeMap group size under Phase 4f; `op_arms.len()` pre-Phase-4f). The runtime bounds check at `runtime/src/handlers.rs:349` requires `op_id < arm_count` — satisfied **only when the handled arms cover op_ids `[0, k)` contiguously** (a prefix subset). A partial handler that matches a non-prefix subset (e.g., `effect Choose { left, right }; handle ... with { Choose.right(k) => ... }` — op_id=1, arm_count=1) trips the runtime bounds-check abort with `sigil_handler_frame_set_arm: op_id 1 out of range (arm_count=1)`. **No existing test surfaces this** — every multi-op effect test in `compiler/tests/e2e.rs` happens to handle every op of every effect it touches. **Why Phase 4f matters:** pre-Phase-4f, the multi-effect rejection narrowed the user-reachable surface (programs combining multiple effects with partial-handlers-of-multi-op-effects bounced off the multi-effect rejection first); post-Phase-4f, those programs compile and runtime-abort. **Two resolution options for a separate post-Phase-4f task:**

- **Option 1 (convention fix):** size `arm_count` to the effect's declared op count (`effects[arm.effect].ops.len()`) instead of the handle's arm count for that effect. Unhandled slots stay null (the `sigil_handler_frame_new` zero-init from Task 56). `sigil_perform`'s null-arm-slot path at `runtime/src/handlers.rs:706-712` already aborts cleanly with a clear "op X has no arm in this handler frame" diagnostic when a perform reaches an unhandled op. Small codegen-only change; runtime-comment update; no typecheck change.
- **Option 2 (typecheck fix, E0142):** require handles to be exhaustive over the matched effect's ops, mirroring `match` expression exhaustiveness (E0066 / E0120). Stricter, language-design call — forecloses partial handlers as a feature surface.

Pinned in CI by the `#[ignore]`'d e2e test `partial_handler_of_multi_op_effect_aborts_at_runtime_pending_resolution` (added at the codegen-lift review-fixup commit). Test asserts the future-correct option-1 behaviour (`stdout = "20\n"`); a future fix-PR un-ignores it. Option 2's resolution rewrites the test to a compile-fail E0142 assertion. The `#[ignore]` mirrors the Phase 4d MVP precedent (`discard_k_handler_does_not_abort_helper_phase_4e_pending`) — pinning the broken-but-known case so it stays grep-findable through the eventual fix.

This entry's Phase 4f scope **does not include the resolution.** Phase 4f-cleanup (sibling to scope_id-cleanup at concern #5) is the natural anchor; or a dedicated post-Phase-4f task if the resolution path needs broader discussion.

**Phased commit organisation within the PR:**

- *Foundation* (this commit): deviation entry (this entry) + `PLAN_B_PROGRESS.md` Phase 4e captures+ status cleanup (the merged PR #27 stale "(awaiting review + squash-merge)" suffix becomes the squash-hash) + new Phase 4f entry (`status: in-progress`, references Phase 4g/return-arm contract from concern #2). No source code changes.
- *Codegen lift* — drop the `op_arms[0].effect` consistency check in `unsupported_handle_construct`; rewrite `Expr::Handle` lowering to group arms by effect and emit alloc-set-push per effect. Inverts the rejection test per hard condition #1. Adds the `#[debug_assert]` from concern #1 at handle exit.
- *Codegen-lift review fixup* — addresses mid-flight reviews of the codegen-lift commit. Misleading "today's callers satisfy" comment at the new lowering site rewritten with the accurate constraint description. Adds the latent op_id/arm_count constraint sub-section above (this paragraph) + the `#[ignore]`'d pinning test.
- *Walker stale-reference sweep* — per hard condition #4. Codegen comments at `:744` and `arm_body_unsupported_construct`'s Phase-4e references rewritten or removed.
- *Polish round* — classifier + walker unit tests, multi-effect dispatch e2e (2×2), per-effect arm-count edge case (one effect with `MAX_HANDLER_ARMS` arms in a multi-effect handle).
- *Closeout* — README "Verification limits" closes multi-effect row, reframes with Phase 4g remaining; PROGRESS Phase 4f flips to `done-pending-ci`.

**Bisecting hint pattern** (three Phase 4f failure modes a future bisecting agent should attribute to this PR vs Phase 4e):

- *Multi-effect handle dispatches the wrong effect's arm at runtime* — frame-grouping bug; surfaces Phase 4f, not Phase 4e. The BTreeMap-grouping loop's per-frame `set_arm` calls are the prime suspect; verify each frame's arms array contains only ops belonging to that frame's `effect_id`.
- *Handler stack head pointer mismatched at handle exit (debug-assert fires)* — N-pop discipline regression (concern #1). The handle-exit pop-loop count or order is the prime suspect; verify it pops exactly `BTreeMap::len()` times in reverse-push order.
- *Unhandled-op diagnostic enumerates N frames of the same handle* — expected behaviour under Option A pre-Phase-4f-cleanup (concern #5); not a regression. The fix (scope_id) is deferred. A bisecting agent investigating this should land on this entry's concern #5 and conclude "deferred, not regressed."

The bisecting hint is also embedded inline at the prime-suspect code change sites for each failure mode, mirroring the precedent set by the comprehensive Phase 4e entry.

**Implementing commit(s):** [HEAD] (this commit lands the deviation entry + PROGRESS update only; subsequent commits implement codegen lift → walker sweep → polish → closeout).

**Closure point:** When the Phase 4f PR squash-merges, multi-effect handlers close. The remaining Plan B Task 55 work is **Phase 4g** (return arms) — see concern #2 above for the return-arm-on-first-pushed-frame contract Phase 4g implements against. After Phase 4g, Tasks 57–61 close Stage 6.

---

## [DEVIATION Task 55] Phase 4g — return arms via synthetic return fn registered on first-pushed frame; codegen-driven dispatch at handle exit (2026-04-28)

**Plan §:** Stage 6 / Task 55 / Phase 4g.

**Status:** done-pending-ci on `plan-b-task-55-phase-4g`. Three commits shipped: foundation (`5b30601`, deviation entry + PROGRESS update only), codegen lift (`eabef59`, walker lift + synth return fn + handle-exit dispatch + 8 e2e tests), closeout (this commit, README + PROGRESS final flip + this status update). CI verdict pending; squash-merge after green + review.

**Context — Phase 4f-shipped substrate.** PR #28 (`08d002a`) closed multi-effect handlers via Option A push-N-frames; the `Expr::Handle` lowering snapshots `frame_1_ptr_snapshot: Option<Value>` immediately after the first effect's `sigil_handler_frame_new` call (`compiler/src/codegen.rs:6878`). That snapshot serves two purposes per the Phase 4f entry's concerns #1 and #2: (1) the debug-only pop-discipline check at handle exit; (2) the durable hold that Phase 4g's return-arm registration reads against. Phase 4g's codegen lift implements directly against this contract — no additional Phase-4f work to redo, no ABI surface change.

**Scope of this PR (Phase 4g MVP).** Lift the codegen-entry walker's return-arm rejection at `compiler/src/codegen.rs:733-740`. Synthesize a CPS-color return arm fn (uniform `extern "C" fn(closure_ptr, args_ptr, args_len) -> *mut NextStep` calling convention; one `FuncId` per return arm allocated by the pre-pass). Register it on the first-pushed frame via `sigil_handler_frame_set_return(frame_1, fn_ptr, closure_ptr)` immediately after that frame's `sigil_handler_frame_set_arm` calls. At handle exit (after the body lowers, after the reverse-pop loop), if the handle has a return arm, dispatch through the return arm: build `NextStep::Call(return_closure, return_fn, [body_val_widened, null_post_handle_k_closure, sigil_continuation_identity])` via `sigil_next_step_call` + `sigil_next_step_args_ptr` writes; drive the trampoline via `sigil_run_loop`; narrow the result back to `handler_overall` (the return arm body's declared Cranelift type). When no return arm is present, codegen emits `body_val` directly as today (no behaviour change for handles without return arms).

**Architectural choice — codegen-driven dispatch (no new FFI).** The runtime exposes `sigil_handler_frame_set_return` as a setter (`runtime/src/handlers.rs:377-388`) but does NOT invoke `return_fn` anywhere on its own — `sigil_handle_pop` only unlinks the head frame; `sigil_run_loop` only dispatches `NEXT_STEP_TAG_DONE` and `NEXT_STEP_TAG_CALL` records. Two architectural alternatives existed:

- **Option A (codegen-driven, this PR):** codegen reads `return_fn` and `return_closure` directly off `frame_1_ptr_snapshot` at handle exit (using fixed `HandlerFrame` field offsets from `sigil_abi::effect`), builds the dispatch `NextStep::Call`, drives `sigil_run_loop` synchronously the same way `lower_perform_non_io_to_value` already does. **Zero ABI surface change.** Phase 4f's `set_return` setter already exists; Phase 4g consumes it in codegen. The frame_1 snapshot stays Boehm-rooted via the surrounding fn's stack hold, so post-pop reads against the snapshot ptr remain valid until the surrounding fn returns.
- **Option B (runtime-driven):** extend `sigil_handle_pop` (or introduce a new `sigil_handle_pop_with_return(value) -> *mut NextStep` FFI) so the runtime invokes `return_fn` on pop and the trampoline dispatches the resulting Call. Adds new FFI surface; arguably cleaner separation (the return arm fires "automatically" on pop), but moves dispatch logic out of codegen, where the value flow is most naturally expressed.

**A is chosen.** Concern #2 of the Phase 4f deviation entry explicitly anticipated this: "no new FFI required." Option A fits the same idiom as `lower_perform_non_io_to_value` (codegen builds a `NextStep::Call` and drives `run_loop`) and keeps the codegen↔runtime boundary stable. Option B's separation argument doesn't apply once the trampoline charter is taken into account: `sigil_run_loop` must remain stack-bounded (per the trampoline charter and Phase 4e's lambda-lifting discipline), so the return-arm dispatch should be expressible without nesting trampolines from the runtime. Both options support multi-effect handles via the first-pushed-frame contract; Option A keeps that contract as a codegen-internal invariant rather than a runtime one.

**HandlerFrame field offsets.** Phase 4g's codegen reads from frame_1_ptr_snapshot at offsets pinned by `runtime/src/handlers.rs:158-166`:

```
HandlerFrame layout (pinned by #[repr(C)]):
  offset  0: effect_id        (u32, 4 bytes)
  offset  4: arm_count        (u32, 4 bytes)
  offset  8: return_fn        (*mut u8, 8 bytes)
  offset 16: return_closure   (*mut u8, 8 bytes)
  offset 24: prev             (*mut HandlerFrame, 8 bytes)
  offset 32: arms[]           (variable-length, [(fn_ptr, closure_ptr); arm_count])
```

Phase 4g loads `return_fn` from offset 8 and `return_closure` from offset 16 directly off `frame_1_ptr_snapshot`. **No new FFI declaration is required** for the load (Cranelift `load.i64` with `MemFlags::trusted()` against the snapshot ptr suffices; the snapshot value is the same `*mut HandlerFrame` returned by `sigil_handler_frame_new`). If the runtime ever changes `HandlerFrame`'s layout, Phase 4g's reads break alongside the runtime's struct accessors — same fragility class as the per-arm `arms_base_ptr` arithmetic in `runtime/src/handlers.rs:357-363`. To mitigate, Phase 4g introduces a pair of `HANDLER_FRAME_RETURN_FN_OFF = 8` / `HANDLER_FRAME_RETURN_CLOSURE_OFF = 16` constants in `sigil_abi::effect` mirroring the existing `MAX_HANDLER_ARMS` / `MAX_INLINE_ARGS` cross-crate offset-discipline pattern. Codegen reads use these constants; runtime accessors continue to read via field-projection (which the constants document but don't replace). A `compile_assertions` test in `sigil-abi`'s `effect` module pins each constant to `offset_of!(HandlerFrame, return_fn)` etc. so a future struct reorder breaks at the abi-crate test rather than silently in codegen — verified by adding offset round-trip tests in `runtime/src/handlers.rs`.

**Synthetic return fn signature (mirrors arm fns + helper synth-conts).** The return arm fn uses the uniform CPS calling convention from `runtime/src/handlers.rs:101-105` (`extern "C" fn(closure_ptr, args_ptr, args_len) -> *mut NextStep`). At runtime its `args_ptr` carries `[v_widened, post_handle_k_closure, post_handle_k_fn]` — Phase 4e captures+ Slice A's trailing-pair convention applied uniformly. The single user arg `v` (per the parser's `return(v) => body` shape) lives at `args_ptr[0]`; `post_handle_k_closure` at `args_ptr[1]`; `post_handle_k_fn` at `args_ptr[2]`. The dispatch from the surrounding fn at handle exit always passes `(null, &sigil_continuation_identity)` as the trailing pair (Phase 4g's MVP doesn't lambda-lift surrounding-fn computation past the handle expression — that's `lower_perform_non_io_to_value`'s pattern, where the surrounding fn drives `sigil_run_loop` synchronously and unwraps the terminal `Done(value)` rather than passing a non-trivial post-handle continuation). Identity's args_len assertion was relaxed to `{1, 3}` in PR #27 Slice A foundation; the trailing-pair convention works uniformly here.

**Synth return fn body lowering.** Mirrors the arm fn body emit pass (`compiler/src/codegen.rs:4576-5132`) with three differences:

1. **Single user arg `v` instead of N op-args.** `arg_names = [binding_name]`, `arg_types = [body_ty]`. Loaded from `args_ptr[0]` and narrowed per `body_ty` (truncate I64→I8 for Bool/Byte/Unit, I64→I32 for Char, pass-through for I64 and pointer types). Bound in the Lowerer's env under `binding_name`.
2. **No `k_name` / no tail-`k` detection.** Return arms have no continuation binding; the walker rejects `k`-style references in return arm bodies (no continuation in scope at typecheck time either — `check_handle`'s return-arm walk binds only `v`, never `k`).
3. **Post-handle-k trailing pair always trampolines through identity.** The synth return fn's tail emits `Call(post_handle_k_closure_loaded, post_handle_k_fn_loaded, [tail_value_widened, null, identity_fn_addr])` — same shape as the tail-`k` arm body path. Since the surrounding fn dispatches with `(null, &identity)`, the trampoline calls identity, identity returns `Done(tail_value_widened)`, and `sigil_run_loop` returns the value to the surrounding fn. The trailing-pair-into-identity composition is uniformly applied so the synth return fn's body emit doesn't need a special-cased terminal `Done` path; the same shape works for any future caller that wants to compose post-handle continuations.

**Captures supported (mirrors Phase 4d arm-body captures + Phase 4e Slice D lambda-source captures).** Return arm bodies may reference outer-fn locals, fn params, and (when the handle is inside a closure_convert-lifted lambda) the surrounding lambda's closure environment slots. Phase 4g extends the typecheck-side `handle_arm_captures` discipline with a parallel `handle_return_arm_captures: BTreeMap<Span, Vec<(String, Ty)>>` side-table populated during `check_handle`'s return-arm walk. The codegen pre-pass mirrors the op-arm path: rewrite the return arm body's captured-name `Expr::Ident` / `Expr::ClosureEnvLoad` nodes into return-arm-local-indexed `Expr::ClosureEnvLoad` references; allocate a closure record at `Expr::Handle` codegen time via `alloc_arm_closure_record(captures)`; pass that record's pointer as `closure_ptr` to `sigil_handler_frame_set_return`. Empty captures vec ⇒ pass null `closure_ptr` (no allocation needed), matching the op-arm null-closure shape.

**Walker restrictions for Phase 4g.** Return arm bodies must satisfy:

- **No `k` references.** Return arms have no continuation binding; the walker enforces this even though typecheck wouldn't bind `k` in a return arm body. Defensive symmetry with op arms.
- **No nested `Lambda` / `ClosureRecord`.** Same restriction as op-arm bodies (Phase 4d). Closure-record allocation for arbitrary nested lambdas requires the same closure-convert side-table extension Phase 4d MVP deferred for op arms.
- **Nested `Expr::Handle` ALLOWED.** The pre-pass walker already recurses into return arm bodies (parallel to op arm body recursion); the synth return fn body is lowered through the regular `Lowerer::lower_expr` which routes `Expr::Handle` through Phase 4f's machinery (allocate frames, push, lower body, pop, return-arm dispatch). Nested handles inside return arm bodies "just work" — no additional codegen surface required. The walker's existing nested-handle recursion pattern (per-handle `expr_unsupported_handle` recursion) extends to return arm bodies via the same shape.

The walker's existing `arm_body_walk` machinery is reused with `k_name = ""` (return arms have no continuation binding so the k-related branches of the walker are inert) and a single scope frame containing the `v` binding name. The walker's nested-`Expr::Handle` arm walks the inner shapes' bodies / arm bodies / return arm bodies recursively under their own scopes (mirrors the op-arm path).

**Bisecting hint pattern (three Phase 4g failure modes a future bisecting agent should attribute to this PR vs Phase 4f or earlier):**

- *Return arm fires when no return arm declared, OR doesn't fire when one is declared* — `set_return` registration bug; verify the surrounding-fn codegen calls `set_return` against `frame_1_ptr_snapshot` exactly when `return_arm.is_some()` and that the `sigil_handler_frame_set_return` FFI declaration is wired correctly.
- *Return arm fires with the wrong `v` value* — `args_ptr[0]` write/read offset mismatch; verify the surrounding-fn dispatch packs `body_val_widened` into slot 0 (offset 0) and the synth return fn unpacks from the same slot.
- *Return arm body's tail value is correct but the surrounding fn returns the wrong type* — narrow-back bug at handle exit; verify the surrounding-fn `sigil_run_loop` result is narrowed per `handler_overall_ty` (the return arm body's declared Cranelift type, NOT `body_ty`). Same shape as `lower_perform_non_io_to_value`'s narrow-back at `compiler/src/codegen.rs:6632-6655`.

**Pre-registered concerns for Phase 4g (mirrors Phase 4f's concerns 1–5 pattern, scoped to return arm semantics):**

1. **Frame-rooting across body lowering.** `frame_1_ptr_snapshot` is a Cranelift SSA Value held through the body via the regalloc; Cranelift's SSA pinning suffices today (Phase 4f already relies on it for the discipline check). The frame's Boehm reachability is independent — the frame is rooted by the handler-stack TLS cell during the body, then by codegen's hold on `frame_1_ptr_snapshot` after the pop. The post-pop window between `sigil_handle_pop` and the return-arm dispatch is the load-bearing hazard: the runtime stack head no longer roots the frame, but `frame_1_ptr_snapshot` does (codegen still has the SSA Value, which keeps the frame's allocation Boehm-reachable through the live-out / stackmap). **Mitigation:** the existing per-call `stackmap.push_placeholder` discipline naturally covers this; the snapshot Value is live across the run_loop call so its stackmap entry roots the frame allocation.

2. **`closure_ptr` GC-rooting through the body.** When the return arm has captures, codegen allocates a closure record at handle entry (before the body lowers) and stores the pointer as `frame_1.return_closure`. The runtime's `HandlerFrame` GC bitmap covers `return_closure` (`runtime/src/handlers.rs:840-849`), so the closure record is Boehm-rooted via the frame chain. **Mitigation:** existing infrastructure; no new work required.

3. **Multi-effect handle return arm semantics — first-pushed-frame contract pinned.** Per the Phase 4f deviation entry's concern #2: the return arm registers on the first-pushed (bottom-of-handle-group) frame regardless of how many effects the handle discharges. The dispatch at handle exit reads from `frame_1_ptr_snapshot` (the same frame), so the `return_fn` / `return_closure` slots round-trip through the bottom-of-group frame whether the handle has 1 effect or N. Verified by an e2e test with a multi-effect handle + return arm.

4. **Return arm body row vs caller row.** Typecheck (`check_handle` lines 3030-3050) walks the return arm body under the **caller's row** (the discharged effects are NOT in scope inside the return arm body). Codegen mirrors this: the return arm synth fn body lowers as if it lived in caller scope (the surrounding fn's effect row drives the colorer, not the discharged row). E0042 / E0043 / IO performs in return arm bodies all behave identically to the surrounding fn's row — no special handling needed at codegen.

5. **Nested handle in return arm body — supported as a freebie.** Phase 4f's machinery (push-N-frames + first-pushed-frame contract) extends transparently to return arm bodies via `Lowerer::lower_expr`'s recursive `Expr::Handle` arm. The pre-pass already recurses into return arm bodies for FuncId allocation, so nested handles inside return arm bodies have their synth fn FuncIds allocated correctly. No additional codegen surface required; concrete coverage lands as part of the closeout test sweep (e.g., a multi-shot Choose handle whose return arm body wraps the result in another handle).

**User's hard conditions for Phase 4g (mirroring Phase 4d/4e/4f patterns):**

1. Walker rejection at `compiler/src/codegen.rs:733-740` is lifted in the codegen-lift commit; the existing inverse test `nested_handle_in_outer_body_propagates_inner_unsupported_diagnostic` (which uses inner-handle-with-return-arm as its walker-recursion sentinel post-Phase-4f) inverts to a positive test asserting the inner handle's return arm dispatches correctly. The walker-recursion-coverage intent is preserved by adding a new still-rejected sentinel covering nested-handle-inside-return-arm-body.
2. `README.md` "Verification limits" section closes the return arms row and reframes with **Tasks 57–61 as the remaining Stage 6 work**. The closeout commit lands the README change in the same PR.
3. `PLAN_B_PROGRESS.md` Phase 4g entry — added at the foundation commit (this commit; status `in-progress`), filled with implementing-commit list at the closeout commit (`done-pending-ci`), updated with squash-hash post-merge.
4. Bisecting-hint pattern in this deviation entry (above) names three Phase 4g failure modes a future bisecting agent should attribute to this PR vs Phase 4e/4f.

**Codebase reference sweep — Phase 4g rewrites these:**

- `compiler/src/codegen.rs:712-717` — comment about `return_arm.is_some()` rejection rewrites to past tense.
- `compiler/src/codegen.rs:733-740` — the rejection block itself, deleted.
- The `nested_handle_in_outer_body_propagates_inner_unsupported_diagnostic` e2e test inverts as described above.
- Renumber-victim sweep: any remaining `Phase 4f lifts` / `Phase 4g lifts` comment references to return arms specifically (not multi-effect) are flipped to "closed at Phase 4g (`[HEAD]`)".

**Implementation commit roadmap:**

- *Foundation* (this commit): deviation entry (this entry) + `PLAN_B_PROGRESS.md` Phase 4f post-merge hash flip (`done-pending-ci` → `done` at `08d002a`) + new Phase 4g `in-progress` entry. No source code changes.
- *Codegen lift*: walker rejection block dropped; `HandlerReturnArmSynth` struct + pre-pass FuncId allocation; new `handler_return_arm_synth: Vec<HandlerReturnArmSynth>` parallel to `handler_arm_synth`; new `handle_return_arm_captures` typecheck side-table; synth-fn definition pass (mirror of arm-fn body emit, simplified for single-arg + no tail-k); `Expr::Handle` lowering extended with `set_return` call after first-pushed frame's `set_arm` calls and handle-exit dispatch (NextStep::Call → run_loop → narrow). New `sigil_abi::effect::HANDLER_FRAME_RETURN_FN_OFF` / `_CLOSURE_OFF` constants with `compile_assertions` test pinning offsets to `offset_of!`. New `set_return` FFI declaration in codegen's per-fn refs. Walker `arm_body_walk` extended with `is_return_arm: bool` parameter. Tests: positive happy-path return arm; return arm with capture; return arm with body-type ≠ handler-overall-type narrowing; return arm performing IO; multi-effect handle + return arm; nested-handle-in-return-arm-body still rejected; existing nested-handle test inverted to positive.
- *Closeout*: README "Verification limits" return arms row flipped to "Closed at PR #29" with prose pointing at this deviation entry; PROGRESS Phase 4g entry filled with implementing-commit list (`done-pending-ci`); reference-sweep cleanups.
- *CI fix* (`dd10379`): two test-expectation corrections aligning to standard Koka/Effekt semantics ("the return clause runs over whatever value flows out of the body, including non-resuming op-arm tail values"); one latent Phase 4c body_ty bug fixed via `dfg.value_type` (mirrors Phase 4e Slice C's pattern at `:5260-5285`). Pre-existing Phase-4c bug, not Phase 4g-introduced; surfaced by Phase 4g's first body-vs-handler-overall mismatched test.
- *Review-fix* (`[HEAD]`): addresses three blocking + one should-fix items from PR #29 mid-flight reviews:
  - **#2** `Lowerer::type_of_expr` for `Expr::Handle` with return arm now self-injects `v: body_ty` into a forked preview before recursing into `ra.body`. Prior shape passed the caller's preview through unchanged — callers that don't pre-bind `v` (e.g., `lower_match`'s arm-body type predictor when the arm body contains a handle expression with a return arm using `v`) tripped the `unreachable!` ident-lookup path. New e2e test `handle_with_return_arm_inside_match_arm_compiles` pins the previously-broken path.
  - **#3** binding_ty hardcoded I64: pinned via `#[ignore]`'d test `handle_with_bool_body_and_return_arm_uses_v_pending_proper_binding_ty` mirroring the `discard_k_handler_does_not_abort_helper_phase_4e_pending` precedent. Two resolution options enumerated in the test docstring (Option 1: thread body_ty from dispatch site via mutable side-table; Option 2: typecheck side-table `handle_body_ty: BTreeMap<Span, Ty>`). Un-ignored at the resolution PR.
  - **#4** Phase 4c body_ty fix coverage: examined more carefully. The bug class ("op return type ≠ actual arm body Cranelift type") **only manifests when there's a return arm setting `handler_overall` ≠ op return type** — without a return arm, typecheck unifies body type with handler_overall, and for the simplest body shape `body_type = op_return_type` (the body is just `perform Op()` whose type is op return type). So a "no-return-arm" test for the bug class is structurally impossible. The existing `handle_with_return_arm_body_type_differs_from_body_type` test already exercises the body_ty fix on the op-arm path: `Raise.fail`'s arm body lowers to I8 (Bool, matching handler_overall) while op return type stays Int (I64); the synth arm fn body emit's widen logic must read `dfg.value_type(body_value)` (= I8) not the pre-stored `synth.body_ty` (= I64) to pass Cranelift's verifier — even though Raise.fail's arm body is never executed at runtime in that test, the verifier rejects the IR at codegen time. Reviewer's #4 was effectively asking for redundant coverage; the existing test is sufficient. (Initial review-fix added `op_arm_body_type_at_handler_overall_compiles_cleanly` but it tripped E0044 at typecheck since the body's type could not differ from op return type without a return arm; deleted as misconceived.)
  - **#5** GC-rooting audit comment expanded at the post-pop `snap` reads — under Boehm conservative scan, `snap` (a Cranelift Value held in register/spill) is rooted via thread-stack scan; future precise-GC pass would need stackmap entries at every call site live across the loads, not at the loads themselves. No change to runtime behavior.
  - **#6** Synth return fn docstring framing rewritten to reflect that the `(null, identity)` outbound trailing pair is hard-coded by Phase 4g MVP; a future caller wanting to compose a real post-handle continuation would need to thread its trailing pair through `args_ptr[1..3]` (the synth fn does NOT today), not re-emit the synth fn.
  - **#9** Defensive `debug_assert!(args_len == 3)` at synth return fn entry (gated behind `cfg!(debug_assertions)`); release builds elide. Catches future codegen regressions that miscount the trailing-pair packing.
  - **Forward observation from review #1** addressed: `handle_with_nested_handle_in_return_arm_body_compiles` is the positive test demonstrating the freebie nested-handle support (deviation entry concern #5 had claimed but no test exercised it).

**Closure point:** Phase 4g PR #29 squash-merged at `a777748` on 2026-04-28; return arms closed. **All Phase 4 sub-work for Task 55 is complete** — the only remaining Plan B work is Tasks 57–61 (IO refactor + `Raise[ArithError]`, multi-shot rigor, catch/state/choose examples, perf floor, P18–P20 prompts) and the Stage 6 review checkpoint. Plan B moves to `done/` only after Stage 6 review checkpoint passes.

## 2026-04-28 — [DEVIATION Task 57] Builtin-effect injection (vs. full stdlib loading) for `IO` and `ArithError`

**Context:** Plan B Task 57 says "**Refactor Stage 1's IO shortcut.** `IO` is now a normal effect, not special-cased. `perform IO.println(s)` goes through `sigil_perform`. Provide the top-level `IO` handler at program entry: a thin shim that calls `sigil_println` for `IO.println`." It also says "**Replace Plan A2's arithmetic-error panic into a proper `Raise[ArithError]` effect.** Declare `effect ArithError { div_by_zero: () -> Never }` in the stdlib."

The plan body uses the word "stdlib" for both effect declarations. Today, `std/io.sigil` exists and declares `effect IO { println: (String) -> Unit, }` literally — but per the Task 57 survey, **`Item::Import(_)` is a no-op everywhere** (`compiler/src/typecheck.rs:644, 742`, `compiler/src/codegen.rs:323, 568`, `compiler/src/closure_convert.rs:107`, `compiler/src/monomorphize.rs:162, 348, 415, 1643`); `stdlib_embed::get` is called only by its own self-test. No path actually feeds `std/*.sigil` content into `tc.effects`. Wiring up real import resolution + parse-and-merge is its own engineering task.

**Deviation:** For Task 57, **synthetic builtin `EffectDecl`s are injected into `tc.effects` at typecheck pre-pass start**, before walking `program.items`. The two builtins:

```
effect IO { println: (String) -> Unit }
effect ArithError {
  div_by_zero: () -> Int,
  mod_by_zero: () -> Int,
}
```

are constructed in code (no stdlib parse), pre-populated into `tc.effects` (so user-program shape `effect IO { … }` triggers the existing E0136 duplicate-effect path), and **assigned reserved low effect_ids regardless of user-effect count**: `ArithError = 0`, `IO = 1`. User effects start at id 2 in alphabetical order. Implementation: the effect_id assignment loop runs in two phases — phase 1 walks the builtin set in declaration order assigning ids 0 and 1; phase 2 walks user-declared effects in alphabetical order starting at id 2.

ArithError carries **two ops**: `div_by_zero` (op_id 0) and `mod_by_zero` (op_id 1), per `[DEVIATION Task 57] Foundation review fixups (review-2 issue #1)` — preserves Plan A2's distinct `"division by zero"` vs. `"remainder by zero"` stderr messages by giving the codegen a way to dispatch the two arithmetic-error sites to different default arm fns. Op_ids assigned alphabetically per-effect: `div_by_zero` < `mod_by_zero` lexicographically gives `div_by_zero = 0` and `mod_by_zero = 1`. The shim's `set_arm` calls in `main` are hardcoded against `(ArithError = 0, div_by_zero = 0, mod_by_zero = 1, IO = 1, println = 0)` — these are stable per the reserved-id convention.

The `std/io.sigil` source file stays in the tree as documentation but is not loaded by typecheck; a follow-up "stdlib loading" task can later swap the synthetic injection for a real import-resolution pipeline without touching codegen or runtime.

**Rationale:** Plan B's hard rule "**Do not implement Stage 7+ features (stdlib beyond the effect declarations) — those are Plan C**" forecloses the path to a real stdlib loader inside Task 57. Synthetic injection delivers Task 57's required behavior — `perform IO.println(s)` flows through `sigil_perform` against a registered effect; `perform ArithError.div_by_zero()` flows through the same registry — without expanding scope.

The trade-off: a future agent reading `tc.effects` and seeing entries that did not come from `program.items` may be briefly confused. Mitigation: the synthetic `EffectDecl`s carry a synthetic span (e.g., `Span::synthetic_builtin_effect`) so error messages distinguish them, and `Tc::new` (or a named `inject_builtin_effects` helper) is the single source of truth for what's pre-populated. The `std/io.sigil` file gains a comment header noting that the actual registration is via builtin injection in `typecheck.rs` (the file remains as forward-compat documentation for a future stdlib-loading task).

**Implementing commit(s):** Foundation (`d2828fa`) + foundation review fixups (`8311723`; added `mod_by_zero` op + reserved-low-id pin) + Slice 1 (`b98c08a`; both builtins injected, IO consumers wired).

**Closure point:** Synthetic injection is permanent for Plan B. A future Plan C task may replace it with real import resolution; the v1-to-v2 swap is local to typecheck pre-pass and does not require codegen / runtime changes (effect_ids stay alphabetical; the only observable difference is whether `effect IO` declarations in user code shadow or duplicate the builtin).

## 2026-04-28 — [DEVIATION Task 57] ArithError op return type — `Int` (v1 simplification) instead of `Never`

**Context:** Plan B Task 57 says: "Declare `effect ArithError { div_by_zero: () -> Never }` in the stdlib." The literal type is `Never` — a bottom type that unifies with anything (allowing `perform ArithError.div_by_zero()` to flow into any expression position, including `let q: Int = a / b` where the divide site lowers to `if b == 0 { perform ArithError.div_by_zero() } else { sdiv … }`).

The Task 57 survey confirmed `Ty::Never` does not exist in the typechecker today. Adding it requires: a new `Ty` variant; bottom-type subsumption in the unifier (`unify_ty(Never, T) = Ok(T)` for all T); occurs-check care; principal-type preservation across HM unification; instantiation rules at perform sites; codegen emit for "perform of Never-returning op" producing a `trap` after the call (since the return value is structurally vacuous). That is exactly the class of unifier change with the highest risk of subtle regressions visible only to a PL expert reading the whole pipeline — the structural correctness gap Plan B already worries about.

**Deviation:** v1 declares both ops with return type `Int`:

```
effect ArithError {
  div_by_zero: () -> Int,
  mod_by_zero: () -> Int,
}
```

Each op's declared return type is `Int` — the same type as the surrounding divide / modulo site's result. The default top-level handler installed in the `main` shim never returns (each arm fn calls `exit(2)` after writing the Plan A2 banner to stderr; per-op arm fns preserve the distinct `"division by zero"` vs. `"remainder by zero"` messages); user handlers may resume `k(recovery_int)` to substitute any Int as the recovery value at the operation site. The elaborate-time rewrite (per `[DEVIATION Task 57] BinOp::Div and BinOp::Mod elaborate to perform-bearing form` below) produces `if rhs == 0 { perform ArithError.div_by_zero() } else { __intrinsic_sdiv(lhs, rhs) }` for `/` and the parallel form with `mod_by_zero` for `%`; the two branches join into the surrounding expression normally — no trap, no special handling.

**Rationale:** Three reasons:

1. **Structural simplicity.** Op return type is `Int`. The unifier rules are unchanged. The handler ABI (already shipped through Phase 4) handles `() -> Int` natively. The post-elaborate AST joins `perform`-result and `__intrinsic_sdiv`-result into a single SSA `Value` of type `Int` — exactly the shape today's code joins `sigil_panic_arith_error`-trap and `sdiv`-result, just with the trap replaced by a `perform` whose result feeds the join block. Cranelift's verifier passes by construction.

2. **Semantic cost is zero for v1.** The only emit sites for `ArithError.div_by_zero` and `ArithError.mod_by_zero` are `BinOp::Div` and `BinOp::Mod` respectively, where the surrounding expected type is always `Int`. The handler-returns-Int recovery model composes cleanly. v1 admits one extra class of programs versus the `Never` design — a handler that resumes `k(0)` to substitute 0 as the divide result and continue execution — but div-by-zero (or mod-by-zero) recovery via continuing-the-operation-as-zero is dubious semantics anyway (it's a footgun, not a feature). Programs that want algebraic recovery use `handle (a / b) with { ArithError.div_by_zero(k) => 999 }` (discarding `k`), not `=> k(0)`. The v1-vs-`Never` distinction is invisible for the canonical use case.

3. **v2 path is clean.** When `Ty::Never` lands (separate Plan C task or post-Plan-B chore), the swap is a one-line edit per op: `() -> Int` becomes `() -> Never` in the builtin-effect injection. Existing handlers like `ArithError.div_by_zero(k) => 0` keep typechecking because the arm body's type is unified with handler-overall (the surrounding expression's expected type, `Int`), not with op-return-type. **Only programs that actually call `k(recovery_int)` break at v2** — a tightening that disallows the dubious-recovery pattern. Acceptable.

**v1 partial-handler contract** (per closeout-review issue #2): `effect ArithError` declares **two ops** (`div_by_zero` / `mod_by_zero`). Under the Phase 4f latent op_id/arm_count constraint (pinned by `#[ignore]`'d e2e `partial_handler_of_multi_op_effect_aborts_at_runtime_pending_resolution`), user handlers MUST register both arms even when the handled expression only triggers one — a handle with `only` `div_by_zero(k) => …` runtime-aborts when `mod_by_zero` fires. `examples/div_recover.sigil` registers both arms (the `mod_by_zero` arm is unreachable in that program but keeps the `arm_count` matched to the effect's declared op count). Users copying the `div_recover` pattern should preserve the both-arms shape until the Phase 4f constraint resolves (option 1: convention fix sizing `arm_count` to `effects[arm.effect].ops.len()`; option 2: typecheck E0142 exhaustiveness rejection). This v1 contract is a temporary footgun, not a permanent design choice — same closure point as the latent constraint itself.

**Implementing commit(s):** Slice 2 (`28721af`) lands the two-op effect declaration + the elaborate-time rewrite producing the appropriate per-operator perform; Slice 2 review fixup (closeout follow-up) updates `examples/div_recover.sigil` to register both arms and adds this v1-contract paragraph.

**Closure point:** v1 ships with `() -> Int`. v2 task (post-Plan-B; tracked via `[v2]` deferral note in design doc's effects section) introduces `Ty::Never` and flips the op return type. The `examples/div_recover.sigil` test pattern (`ArithError.div_by_zero(k) => 999`) survives the v1-to-v2 swap unchanged. The partial-handler footgun closes alongside the Phase 4f latent op_id/arm_count constraint resolution (separate post-Phase-4f task; not Task 57's responsibility).

## 2026-04-28 — [DEVIATION Task 57] Top-level handler installation in `main` shim (push at startup, pop at exit)

**Context:** Plan B Task 57 says "**Provide the top-level `IO` handler at program entry**" and "**The top-level runner installs a default handler that prints to stderr and exits 2**" for ArithError. Both refactors require pushing handler frames before user `main` runs. Today the `main` shim at `compiler/src/codegen.rs:5010-5046` is generated as: `call sigil_gc_init(); call sigil_user_main(null_closure); reduce-tagged-Int-to-i32; return`. No handler frames exist outside `Expr::Handle` lowering — the survey confirmed every `sigil_handler_frame_new` / `sigil_handle_push` / `sigil_handle_pop` call site lives inside `Expr::Handle`'s codegen arm.

**Deviation:** The `main` shim is extended to push two top-level handler frames between `sigil_gc_init` and `call sigil_user_main`, and pop them in reverse order before reducing the return value. The IO frame holds 1 arm (`println`); the ArithError frame holds 2 arms (`div_by_zero` at op_id 0; `mod_by_zero` at op_id 1) per the foundation review fixup:

```
call sigil_gc_init()

// IO handler:
io_frame = sigil_handler_frame_new(/*effect_id=*/1 /* IO */, /*arm_count=*/1)
sigil_handler_frame_set_arm(io_frame, /*op_id=*/0, &sigil_io_println_arm, /*closure_ptr=*/null)
sigil_handle_push(io_frame)
io_first_frame_snapshot = io_frame   // for shim discipline check (debug builds only)

// ArithError handler:
arith_frame = sigil_handler_frame_new(/*effect_id=*/0 /* ArithError */, /*arm_count=*/2)
sigil_handler_frame_set_arm(arith_frame, /*op_id=*/0, &sigil_arith_error_div_by_zero_arm, null)
sigil_handler_frame_set_arm(arith_frame, /*op_id=*/1, &sigil_arith_error_mod_by_zero_arm, null)
sigil_handle_push(arith_frame)

call sigil_user_main(null_closure_ptr)

popped_arith = sigil_handle_pop()  // ArithError frame
popped_io    = sigil_handle_pop()  // IO frame

// Shim discipline check (cfg!(debug_assertions) only):
debug_assert(popped_io == io_first_frame_snapshot, TRAP_HANDLE_DISCIPLINE_VIOLATION)

reduce + return
```

The effect_ids `0` (ArithError) and `1` (IO) are stable per the reserved-low-id convention pinned in the Builtin-effect-injection entry above; user effects start at id 2.

All three arm fns are runtime-side C functions in `runtime/src/handlers.rs` (or a new `runtime/src/builtin_arms.rs`) conforming to the CPS arm fn ABI `extern "C" fn(closure_ptr, in_args: *const u64, args_len: u32) -> *mut NextStep` already pinned by Phase 4 (Slice A's trailing-pair convention: `in_args = [user_args..., k_closure, k_fn]`). The two distinct buffers — *inbound* `in_args` read by the arm fn vs. *outbound* args slots written into a freshly-built `NextStep::Call` — get distinct names below to avoid the `args_ptr[0]` overload that confused the foundation's first draft:

- **`sigil_io_println_arm`**: reads `in_args[0]` as the heap-string pointer the user passed to `IO.println`, calls `sigil_println(heap_ptr)`, then builds `NextStep::Call(k_closure=in_args[1], k_fn=in_args[2], arg_count=1)`. Writes the unit value (`i64 0`) into the outbound NextStep's arg slot 0 via `sigil_next_step_args_ptr(ns)[0] = unit_value`. The trampoline dispatches to `k`, which (under default IO usage) is `sigil_continuation_identity` — `Done(unit)` then unwinds to `lower_perform_to_value`'s `sigil_run_loop` callsite, which narrows the `i64` back to `Unit`. User code receives unit, identical to the old synchronous `sigil_println` path semantically.

- **`sigil_arith_error_div_by_zero_arm`**: writes `"sigil: arithmetic error: division by zero\n"` to stderr, then calls `std::process::exit(2)`. Function never returns — the `*mut NextStep` return type is structurally unreachable. Reads no fields from `in_args` (op takes no user args; trailing-pair `(k_closure, k_fn)` at `in_args[0..2]` is irrelevant since exit never resumes).

- **`sigil_arith_error_mod_by_zero_arm`**: identical shape to `sigil_arith_error_div_by_zero_arm` except the message is `"sigil: arithmetic error: remainder by zero\n"`. The two arms share an internal helper writing the banner-and-exit-2 sequence parameterised on the message string, but ship as two separate `extern "C"` symbols since the runtime ABI passes no op_id to arm fns (per the Phase 4 ABI constraint; arm dispatch is keyed by the `set_arm` slot, not by op_id read at fn entry).

Together the two arm fns preserve Plan A2's `examples/div_by_zero.sigil` and the parallel `mod_by_zero` e2e test verbatim — same stderr banner per operator, same exit code 2.

**Shim discipline check parity (debug-only, per foundation review concern #3):** Phase 4f added a `cfg!(debug_assertions)`-gated `TRAP_HANDLE_DISCIPLINE_VIOLATION = 0x42` icmp+brif at every `Expr::Handle` exit, snapshotting the first-pushed frame's pointer at push and comparing against the last `sigil_handle_pop()`'s return at handle exit. The shim's two-frame push/pop is a bespoke codegen path, not an `Expr::Handle`, so the `lower_expr` Handle-arm machinery does not apply. The shim adds the same shape: snapshot `io_frame` (first-pushed) into a Cranelift stack local, compare against `popped_io` (the second `sigil_handle_pop` return) at shim exit, trap on mismatch. The check costs four instructions and is debug-only; the consistency win — same discipline applied uniformly across `Expr::Handle` and the shim — is non-zero. Mismatch in the shim implies a codegen bug in the shim itself (push/pop count drift) or runtime corruption of the handler stack head Cell, both of which warrant a hard fail.

User `main` is unchanged at the source level. User-installed handlers via `handle … with { IO.println(s, k) => …, ArithError.div_by_zero(k) => … }` walk the handler stack inward-first, finding the user's frame before the top-level default — so user override works for free.

**Rationale:** Two reasons:

1. **The shim is the only context where "before main" exists.** Effect rows on user `main` would normally require declaring `![IO, ArithError]`, which the existing examples do (with at least `![IO]`). The shim is conceptually the "discharge environment" for those effects: it installs handlers that catch them with default behavior. The row check is satisfied because the user's `main` body declares `![IO]` (and post-Task-57, `![ArithError]` if it does division — see Q1 in the planning round); the discharge is implicit at the shim level via the pushed frames.

2. **Symmetry with `Expr::Handle`.** Today `Expr::Handle` codegen is the only consumer of `sigil_handler_frame_new` + `set_arm` + `push`. Reusing the exact same FFI surface from the shim — same arm fn ABI, same closure_ptr-null convention for closure-less arms, same `set_arm` offsets — keeps the runtime-side mental model uniform. The shim is simply a `Expr::Handle` whose body is `sigil_user_main` and whose arms are two C-function-pointer literals, lifted out of Sigil source and into codegen-emitted `main`.

**Implementing commit(s):** Slice 1 (`b98c08a`; IO frame + `sigil_io_println_arm`) + Slice 2 (`28721af`; ArithError frame + the two `sigil_arith_error_{div,mod}_by_zero_arm` runtime fns + snapshot rename per `[DEVIATION Task 57] Slice 2 shim-snapshot rename`).

**Closure point:** Both top-level handlers ship with Task 57. Future tasks may add additional builtin effects (e.g., a hypothetical `Net` or `FS`) by extending the shim's push/pop pair count and adding corresponding runtime-side arm fns; the pattern is mechanical.

## 2026-04-28 — [DEVIATION Task 57] `BinOp::Div` and `BinOp::Mod` elaborate to perform-bearing form (per foundation review forward concern #1; supersedes codegen-time synthesis sketch)

**Context:** The foundation commit's first draft proposed synthesizing `perform ArithError.div_by_zero()` at **codegen time** — i.e., the existing `trap_on_zero` helper at `compiler/src/codegen.rs:8750-8781` would be rewritten to emit a `sigil_perform` call instead of a `sigil_panic_arith_error` call. Foundation review concern #1 raised a structural objection: the colorer (`compiler/src/color.rs::find_non_io_perform_in_perform`) walks the AST/elaborated IR, not Cranelift IR. A codegen-time-synthesized perform is invisible to the colorer; a fn doing `/` would not get reclassified as CPS-color even though it performs at runtime. That contradicts Phase 4e's discard-`k` correctness gate.

**Deviation:** Synthesize the perform at **elaborate time**, not codegen time. The elaboration pass rewrites every `Expr::Binary { op: Div | Mod, lhs, rhs, .. }` into the if-perform-else AST shape:

```
let lhs_eval = <lhs>;
let rhs_eval = <rhs>;
if rhs_eval == 0 {
  perform ArithError.div_by_zero()    // for Div; ArithError.mod_by_zero for Mod
} else {
  __intrinsic_sdiv(lhs_eval, rhs_eval)  // for Div; __intrinsic_srem for Mod
}
```

The `__intrinsic_sdiv` / `__intrinsic_srem` shape is a new codegen-internal `BinOp` variant (`BinOp::SdivUnchecked` / `BinOp::SremUnchecked`) — produced only by elaborate's rewrite, never by surface syntax — that codegen lowers as a plain `sdiv` / `srem` without the zero check. The existing `BinOp::Div` / `BinOp::Mod` codegen arms are unreachable post-elaborate (they `unreachable!()` in `lower_expr` and `type_of_expr` since elaborate handles all surface-level Div/Mod cases).

**Rationale:** Three reasons (this is the architectural reformulation of the original tracked-effect-vs-unchecked-effect entry):

1. **Colorer sees the perform structurally.** Once elaborate produces an `Expr::Perform(ArithError, div_by_zero, [])` in the AST, every downstream pass (monomorphize, color, closure_convert, codegen) handles it via the existing perform machinery. `find_non_io_perform_in_perform` already returns `Some(("ArithError", "div_by_zero"))` for that AST shape. Fns doing `/` or `%` get classified as CPS-color automatically, with `--dump-color` emitting `cps: performs ArithError`. No new colorer special-case for BinOp::Div / Mod is needed — and crucially, no risk of the colorer drifting from codegen's actual emit behavior.

2. **Typecheck row introduction is mechanical.** Typecheck runs *before* elaborate in the pipeline (`lex → parse → resolve → typecheck → elaborate → ...`), so it sees the original `BinOp::Div` / `BinOp::Mod` and cannot rely on elaborate's rewrite to introduce the row. **Typecheck still needs a one-line check_binop addition** that calls a small `register_effect_use("ArithError")` helper (also called by `check_perform`) when op is `Div` or `Mod` — adding `ArithError` to the surrounding fn's row, triggering closed-row rejection (E0042) for fns declared `![]` or `![IO]`. The helper unifies the row-introduction logic across `check_perform` and `check_binop`'s arithmetic arm. This is structurally cleaner than the original sketch's "introduce ArithError into the row at the BinOp::Div typecheck site" because the helper is the single source of truth — it cannot drift from the elaborate rewrite (the rewrite produces a perform; the helper handles the row introduction; both register the same effect_use).

3. **Codegen drops the `trap_on_zero` helper entirely.** Post-elaborate, no Div/Mod codegen arm exists (handled via the if-perform-else AST). The `panic_arith_ref` / `div_zero_msg_id` / `mod_zero_msg_id` / `trap_on_zero` machinery in `compiler/src/codegen.rs` deletes wholesale. The `sigil_panic_arith_error` runtime fn deletes from `runtime/src/arith.rs`. The Slice 2 diff is largely deletions on the codegen side, additions on the elaborate side — easier to review.

**Tracked-vs-unchecked decision (preserved from the original entry):** the choice remains option (a) tracked effect over option (b) unchecked. Sigil's "fight-the-priors" doctrine treats arithmetic-fallibility as a real effect that the type system should make legible. Programs doing division declare `![ArithError]` (or include it in a row variable's residual). Option (b) would require the typechecker to special-case `/` and `%` as "performs an effect but doesn't track it" — exactly the meet-the-priors hack the language is designed to fight. Once option (b)'s door is opened, future authors reach for the same hack for the next "tracked but not really tracked" effect, eroding the row system's value.

**Quantified breakage** (per foundation review issue #3, verified against `main` at `a777748`):

- `examples/arith.sigil` (3 sites: `n / 2`, `n % 10`, `n % 2 == 0`) — row `![]` → `![ArithError]`.
- `examples/div_by_zero.sigil` (1 site: `a / b`) — row `![]` → `![ArithError]`.
- `compiler/tests/e2e.rs` line 446-448 (1 inline source: `let r: Int = a % b;` in the existing `mod_by_zero_traps` test) — row `![]` → `![ArithError]`.

**Total: 3 source files, 5 BinOp::Div/Mod sites, 3 effect-row updates.** No other `examples/*.sigil` files use `/` or `%`. No other inline e2e Sigil sources use `/` or `%`. If Slice 2's diff requires more than 3 source-row edits, the typecheck check_binop change has bug — surface this for review.

**Implementing commit(s):** Slice 2 (`28721af`); lands the typecheck check_binop helper extension via `register_effect_use`, the elaborate `Expr::Binary { op: Div | Mod, .. }` rewrite, the new `BinOp::SdivUnchecked` / `SremUnchecked` AST variants, the codegen drops (`trap_on_zero` + `TRAP_ARITH_ABORT`), and the row updates on the 3 source files (`examples/arith.sigil`, `examples/div_by_zero.sigil`, `compiler/tests/e2e.rs:446-448`).

**Closure point:** Permanent for v1. Programs that do division declare `ArithError` in their row from Task 57 onward. The `examples/div_recover.sigil` example (new in Slice 2) declares `![ArithError]` on the inner fn that does division and `![]` on the outer fn that handles the effect via `handle … with { ArithError.div_by_zero(k) => … }`, demonstrating effect discharge at the type level.

## 2026-04-28 — [DEVIATION Task 57] Single PR with two commit slices (IO refactor → ArithError refactor → closeout)

**Context:** Plan B Task 57 groups two refactors under one task ID. Per-phase-PR cadence (established for Phases 4b–4g of Task 55) suggests one PR per task. The two refactors are independent — the IO refactor touches `lower_perform`, the four IO-special-case sites in codegen, and the typecheck `check_perform` IO branch; the ArithError refactor touches `BinOp::Div` / `Mod`, `trap_on_zero`, the example file rows, and the runtime arith module. Splitting into two PRs would let each get its own review cycle.

**Deviation:** Single PR (`plan-b-task-57` against `main`) with **two commit slices**:

- *Foundation* (`d2828fa`) — original deviation entries + PROGRESS Phase 4g squash-hash flip + Task 57 in-progress entry. No source code changes.
- *Foundation review fixups* (this commit) — addresses 7 items from the foundation mid-flight reviews: ArithError gains `mod_by_zero` op (review-2 issue #1; preserves Plan A2's distinct `%`-vs-`/` stderr message); reserved-low-id convention pinned (review-2 issue #2; `ArithError = 0`, `IO = 1` regardless of user-effect count); elaborate-time synthesis adopted over codegen-time (review-1 forward concern #1; colorer sees the perform structurally); e2e row-update count quantified (review-2 issue #3; 3 source files); shim discipline check parity adopted (review-1 forward concern #3); inbound-vs-outbound `args_ptr` references renamed (review-2 issue #5); inherited `MAX_HANDLER_ARMS = 13` → 14 drift fixed in PROGRESS + DEVIATIONS (review-2 issue #6). One stale-line-number claim from the review (review-2 issue #4) examined and rejected: `:644, :742` and `:2144` match current `main` at `a777748`; the reviewer's claim of `:624, :722, :2114` was incorrect for the current file state. Numbers stay; will be refreshed if Slice 1 touches typecheck.rs lines around the citations.
- *Slice 1: IO refactor* — synthetic builtin `effect IO` injection in typecheck pre-pass + 4 codegen IO-special-case sites + typecheck IO hard-wire deletion + `main` shim IO frame push/pop + `sigil_io_println_arm` runtime fn + `Stmt::Perform` and `Expr::Perform` IO branch deletion + comment cleanup at codegen `:3798`, typecheck `:2144`, `std/io.sigil`. New e2e tests pinning that `perform IO.println(s)` flows through `sigil_perform` (via `--print-runtime-stats` or behavior verification). **Atomic application:** Slice 1 lands as one commit; intermediate WIP states between the typecheck-pre-pass injection and the codegen-special-case deletions are not CI-checked. CI runs against the slice's HEAD only (per foundation review forward concern #4 — process discipline note).
- *Slice 2: ArithError refactor* — synthetic builtin `effect ArithError { div_by_zero, mod_by_zero }` injection + typecheck `check_binop` row introduction via shared `register_effect_use` helper + elaborate `Expr::Binary { op: Div | Mod, .. }` rewrite to if-perform-else AST shape + new codegen-internal `BinOp::SdivUnchecked` / `SremUnchecked` AST variants for the post-elaborate sdiv/srem paths + codegen `BinOp::Div` / `Mod` arms become `unreachable!()` post-elaborate + `trap_on_zero` / `panic_arith_ref` / `div_zero_msg_id` / `mod_zero_msg_id` deletion + `sigil_panic_arith_error` runtime fn deletion + `main` shim ArithError frame push/pop with 2-arm count + `sigil_arith_error_div_by_zero_arm` and `sigil_arith_error_mod_by_zero_arm` runtime fns (each preserves Plan A2 banner verbatim for its operator) + 3 source-file row updates (`examples/arith.sigil`, `examples/div_by_zero.sigil`, `compiler/tests/e2e.rs:446-448`) to `![ArithError]` + new `examples/div_recover.sigil` + e2e test for div_recover (positive: handler returns 999) + e2e test for div_by_zero (preserved: stderr banner + exit 2) + e2e test for mod_by_zero (preserved: distinct stderr banner + exit 2). **Atomic application:** Slice 2 lands as one commit for the same reason as Slice 1.
- *Closeout* — README "Verification limits" Plan A1 IO comment row flipped to "Closed at PR #30"; PROGRESS Task 57 entry filled with implementing-commit list (`done-pending-ci`); deviation entry implementing-commit lines back-filled.

**Rationale:** Three reasons:

1. **Plan B groups them.** Task 57 in `2026-04-21-sigil-effects.md` is one task with two paragraphs. Splitting deviates from the plan structure without an obvious payoff — the two refactors share the same builtin-effect-injection mechanism, the same shim push/pop machinery, and the same architectural rationale. Reviewing them as one bundle preserves that context.

2. **Cadence consistency.** Per-phase-PR cadence has been one PR per phase number; Task 57 is one task. Splitting into two PRs deviates without an obvious reviewability win.

3. **Bisect granularity is preserved by commit hygiene, not PR-splitting.** Two clean commit slices (IO, then ArithError), each pod-verified independently, give a bisecting agent the same granularity as two separate PRs would. The mitigation against bundle-masking-bugs is small focused commits, not separate PRs.

**Bisecting hints (failure-mode-and-architectural-surface attribution):**

- *IO `perform` synchronously crashes* in a program that compiles: the IO refactor's `lower_perform` deletion or `main` shim IO frame push is wrong. Look at Slice 1.
- *Division by zero produces unexpected stderr or exit code* (Plan A2 regression for `/`): the ArithError refactor's `sigil_arith_error_div_by_zero_arm` runtime fn or shim ArithError frame push (or its 2-arm count) is wrong. Look at Slice 2.
- *Modulo by zero produces "division by zero" stderr instead of "remainder by zero"* (review-2 issue #1 regression): the shim's op_id 1 set_arm wiring or `sigil_arith_error_mod_by_zero_arm` is wrong, or elaborate's BinOp::Mod rewrite produced a `div_by_zero` perform instead of `mod_by_zero`. Look at Slice 2.
- *`examples/arith.sigil` fails to typecheck with E0042 "ArithError not in row"*: either the row update on the example file is missing, or the typecheck `check_binop` row introduction (`register_effect_use("ArithError")` for Div/Mod) is wrong. Look at Slice 2.
- *`examples/div_recover.sigil` doesn't return 999*: the user-installed handler's frame push or arm fn dispatch through the Phase 4d/4e CPS pipeline is wrong, OR elaborate's BinOp::Div rewrite produced a malformed if-perform-else AST. Look at Slice 2 (start with elaborate output via `--dump-elaborate` if it exists, else inspect post-elaborate AST in a unit test).
- *Color analysis classifies a fn doing `/` as Native instead of CPS*: post-elaborate-time-synthesis the `Expr::Perform(ArithError, div_by_zero, [])` should be visible in the AST and `find_non_io_perform_in_perform` should classify the fn as CPS. Confirm via `--dump-color` on a fn doing division — should be `cps: performs ArithError`. If still Native, the elaborate rewrite is producing the perform under a different AST shape than the colorer recognizes; look at elaborate.rs's emit and color.rs's walker.
- *Cranelift verifier rejects the post-elaborate `BinOp::SdivUnchecked` arm*: the new codegen-internal AST variants need a `lower_expr` arm + `type_of_expr` arm. If they reach codegen as `unreachable!()` and panic, elaborate's rewrite is missing them entirely. Look at Slice 2 (codegen + elaborate).

**Implementing commit(s):** Foundation (`d2828fa`) + foundation review fixups (`8311723`) + Slice 1 (`b98c08a`) + CI fix #1 (`29b2b5e`; stackmap test count) + Slice 1 review fixups (`2e12eaa`) + Slice 2 (`28721af`) + Closeout (`[HEAD]`). All seven commits squash-merged via PR #30.

**Closure point:** PR #30 squash-merge closes Task 57. Tasks 58–61 + Stage 6 review checkpoint remain. After Task 57, `IO` is a normal effect routing through `sigil_perform`, `ArithError` is a real algebraic effect with default + override-able handlers, no `sigil_panic_arith_error` call sites remain in codegen, and the Plan A2 user-visible behavior of `examples/div_by_zero.sigil` (and the parallel `mod_by_zero` e2e test) is preserved verbatim. The five Task 57 architectural choices in deviation entries above all closed.

## 2026-04-28 — [DEVIATION Task 57] IO color filter retention (perf-preserving choice; residual discard-`k` gap)

**Context:** Task 57's IO refactor moves `IO` from a synchronous-shortcut special case to a registry-driven effect at typecheck and codegen — every `perform IO.println(s)` now flows through `sigil_perform` → `sigil_io_println_arm` → `sigil_println` → trampoline → narrow-back to Unit, identical to the runtime path of any non-IO effect.

The Slice 1 implementation **does NOT** lift the colorer's IO-skip filter at `compiler/src/color.rs::NATIVE_EFFECT` and the three parallel codegen-classifier filters (`is_simple_tail_perform_with_pure_args_body`, `is_simple_yield_then_constant_tail_body`, `is_simple_let_yield_then_pure_tail_body`). The filters keep IO out of the CPS-color classification — fns whose only effect is `IO` stay Native-color and pay no trampoline overhead per println.

The original foundation deviation entry's framing of "drop the filters" was aspirational; the Slice 1 commit (`b98c08a`)'s decision to retain them — and the commit message's "without correctness loss" claim — is more nuanced than the foundation entry anticipated. This entry corrects the framing and pins the residual gap.

**Deviation:** Retain IO-skip filters in the colorer + 3 codegen classifiers. The choice is **perf-preserving with a documented residual correctness gap**, not "without correctness loss":

- *Perf preservation:* lifting `NATIVE_EFFECT` would force every fn doing `perform IO.println(...)` to become CPS-color. Trampoline overhead would propagate up the call graph (Native callers of an IO-using callee themselves become CPS-color via the colorer's transitive rule), making every print-bearing fn pay the trampoline cost on every invocation.

- *Residual correctness gap:* a user-installed discard-`k` IO handler does **not** unwind a Native-color helper fn. Concrete failure case:

  ```sigil
  fn helper() ![IO] {
    perform IO.println("a");          // discard-k handler in caller
                                       // would normally unwind here
    let x = expensive_computation();   // ... but this still runs
    perform IO.println("b");          // ... and so does this
  }

  fn main() ![IO] {
    handle helper() with {
      IO.println(s, k) => Unit       // discards k
    };
  }
  ```

  Under standard algebraic-effects semantics, after the first perform fires inside `helper`, the discharging arm in `main` runs, returns Unit, and `helper` does NOT continue past the perform. Phase 4e closed exactly this gap for non-IO performs by reclassifying helpers whose performs reach a discharging handler as CPS-color, so the discard propagates as a `NextStep::Done` that unwinds through the trampoline. The IO filter retains the Native-color synchronous shape for `helper`: `lower_perform_to_value`'s `sigil_run_loop` returns Unit from the discard arm and `helper` continues to `expensive_computation` and the second perform, both of which run despite user intent.

  This is the **same structural hole** Phase 4e closed for non-IO; deliberately left open here for the perf-preserving reason above.

**Load-bearing invariant:** the top-level IO handler frame is always on the stack when user code runs (installed by `compiler/src/codegen.rs`'s `main` shim emit between `sigil_gc_init` and `call sigil_user_main`). If the shim regresses (wrong `effect_id`, missing `set_arm`, frame_new fails), Native-color fns doing `perform IO.println(...)` would `sigil_perform` against an unhandled effect and abort at runtime. `examples/hello.sigil`'s e2e test exercises this end-to-end on every CI run; any shim-wiring regression fails there immediately. The `builtin_effects_present_in_every_program` typecheck unit test pins `IO = 1` against the registry for the source-of-truth side; the shim's hardcoded `effect_id = 1` for the IO frame matches.

**Rationale:** Three reasons:

1. **Perf cost is real and viral.** Even programs with rare prints would pay the trampoline cost on every invocation of any fn in their call tree that prints. The trampoline overhead Phase 4e accepted for non-IO performs is justified by algebraic-correctness for user-installed handlers; for IO, the canonical use case is `println` flowing to stdout, not user-installed handlers. Preserving the synchronous shape for the canonical case is the right perf trade-off until concrete user demand appears for algebraic IO.

2. **Residual gap is bounded and documented.** The gap fires only when a user installs a discard-`k` IO handler and expects it to unwind a Native-color helper. Under v1, no such pattern is canonical — the IO effect is overwhelmingly used to call `println` on the default top-level handler. A user who genuinely wants algebraic IO can wrap their helpers in `![IO | e]` rows that include a non-IO effect to force CPS-color reclassification, or wait for the v2 path below.

3. **v2 path is clean.** Lifting `NATIVE_EFFECT = "IO"` (one constant; the three codegen classifier filters reference it) reclassifies every IO-using fn as CPS-color. Existing programs continue to work — IO performs route through the same `sigil_perform` machinery either way; the v1-vs-v2 difference is whether the surrounding fn synchronously calls `lower_perform_to_value` or returns `NextStep::Call` to its caller's trampoline. The pinning test (`user_discard_k_io_handler_does_not_unwind_native_color_helper_pending_color_filter_lift`) inverts to a positive test (asserting only "a" is printed) when `NATIVE_EFFECT` is removed and the codegen filters are dropped.

**Reference choice — `NATIVE_EFFECT` over `BUILTIN_EFFECT_NAMES`:** the three codegen classifier filters now reference `color::NATIVE_EFFECT` rather than the literal `"IO"` so a future builtin rename touches one source of truth. The filters' logic — "IO-color performs don't trigger CPS classification" — is semantically about Native-color status, not "is in the builtin set", so the colorer's name is the right reference (per review-2 issue #3).

**Implementing commit(s):** Slice 1 review fixups (`2e12eaa`). Slice 1's `b98c08a` framing of "without correctness loss" is corrected by this entry's clearer "perf-preserving with residual gap" phrasing.

**Pinning test:** `user_discard_k_io_handler_does_not_unwind_native_color_helper_pending_color_filter_lift` (`#[ignore]`'d e2e). Inverts to a positive test asserting only `"a"` is printed when the IO filters are lifted. Mirrors the `discard_k_handler_does_not_abort_helper_phase_4e_pending` precedent (Phase 4d MVP) and `partial_handler_of_multi_op_effect_aborts_at_runtime_pending_resolution` precedent (Phase 4f).

**Closure point:** v2 task (post-Plan-B; tracked via "v2 IO color filter lift" in the plan's deferred-items list when it lands, or as a separate follow-up PR if user demand surfaces during Plan C). Lifts `NATIVE_EFFECT = "IO"`, drops the three codegen classifier filters, un-ignores the pinning test. The change is local: typecheck + codegen + runtime are all already uniform.

## 2026-04-28 — [DEVIATION Task 57] Slice 2 shim-snapshot rename (pre-flag for review)

**Context:** Slice 1's `main` shim emit installs a single top-level IO handler frame (effect_id = 1) and snapshots `io_frame_ptr` for the discipline-check trap (debug-only, mirrors Phase 4f's `Expr::Handle`-exit check shape). The single pop verifies against the snapshot.

Slice 2 will add the ArithError handler frame **before** the IO frame in install order:

```
push arith_frame   ← first-pushed; gets the discipline trap
push io_frame
... user_main ...
pop → io_frame      (verify == io_frame_ptr — local sanity check)
pop → arith_frame   (verify == arith_frame_snapshot — discipline trap)
```

**Pre-flag:** Slice 2's shim emit MUST move the `frame_snapshot` capture from `io_frame_ptr` (Slice 1) to `arith_frame_ptr` (first-pushed in the LIFO order). The discipline trap fires at the **second** pop (after both frames have been removed from the stack). The first pop gets a local sanity check (`popped_io == io_frame_ptr`) since drift between push order and the LIFO assumption would surface even before the trap site fires.

**Why this matters:** under LIFO, the first-pushed frame is last-popped, and the discipline check's whole point is to catch a runtime corruption / shim emit regression where push/pop pairs drift. The check must verify the longest-lived frame's identity at unwind. Slice 2's review should specifically verify:

1. The `frame_snapshot` Cranelift `Value` captures `arith_frame_ptr` (the first `frame_new`'s result), not `io_frame_ptr`.
2. The trap site is after the second pop (the ArithError pop), not the first.
3. The IO pop has its own sanity check (`popped_io == io_frame_ptr`) for the LIFO assumption.

**Why pre-flag rather than land alongside Slice 2:** the failure mode (a partial Slice 2 implementation that retains the Slice 1 snapshot variable) is silent in release builds (the trap is debug-gated) and easy to miss at review. Pre-flagging in this entry forces the Slice 2 reviewer's attention to the rename without requiring them to remember it from Slice 1's shim deviation entry.

**Implementing commit(s):** Slice 2 (`28721af`). The actual rename + the ArithError frame push-before-IO + the two-pop discipline-check shape all land in that commit; the local sanity check on the IO pop and the discipline trap on the ArithError pop are both wired per the bullet list above.

**Closure point:** Closed at Slice 2 (`28721af`). The discipline check correctly verifies the ArithError frame's identity at the second pop; the IO pop has its own sanity check.

## 2026-04-28 — [DEVIATION Task 58] Multi-shot stress shape — sequential-handles workaround for Slice C v1's 2-let arm cap

**Context:** Plan B Task 58 calls for two example programs:

- `examples/choose_demo.sigil` using `effect Choose resumes: many { choose: (Int) -> Int }` — "a handler invokes `k` twice and collects both results".
- `examples/multishot_stress.sigil` — "resume a continuation 10+ times with different inputs; assert results are independent".

The first example matches the v1 multi-shot machinery exactly: Slice C's `arm_body_multi_let_then_pure_tail_shape` recognises `{ let r1 = k(arg1); let r2 = k(arg2); pure_tail }` and emits the 2-step lambda-lifted post-arm-k chain. The second example's literal "10+ resumes" wording would require a single arm body with 10+ `let r_i = k(arg_i)` bindings — a shape Slice C v1 explicitly does NOT accept. See `[DEVIATION Task 55] Phase 4e captures+` line 1503's *"Deferred from Slice C v1 (future captures-bearing extension)"* item:

> More than 2 `k` invocations (3+ requires generalising the chain to N — straightforward but layered; v1 commits to the minimum that demonstrates multi-shot).

The negative coverage test `slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` (compiler/tests/e2e.rs:3667) pins the cap at exactly 2 lets at the codegen-entry walker. Lifting the cap to N is mechanical (the post-arm-k synth-fn chain extends from 2 fns to N fns, each capturing the previous lets' results) but layered work that wasn't in Phase 4e's MVP scope.

**Deviation:** Task 58 v1 ships:

1. **`examples/choose_demo.sigil`** with the literal `effect Choose resumes: many { choose: (Int) -> Int }` declaration and the canonical 2-resume arm body shape `{ let r1: Int = k(arg1); let r2: Int = k(arg2); r1 + r2 }`. This matches the plan's first-example wording verbatim — "a handler invokes `k` twice and collects both results".

2. **`examples/multishot_stress.sigil`** with FIVE sequential `handle` expressions chained in a sum, each a 2-resume Choose handler invoked over an `(Int) -> Int` op with independent `arg` values. Each handle drives `k` twice with different inputs (`k(arg + 100)` and `k(arg + 200)`), so the program runs 10 multi-shot continuation invocations total across 5 fresh handler frames. Independence is assertable via the closed-form expected-output value: each handle returns `(seed + arg + 100) + (seed + arg + 200)` for its own `seed`; the final sum across 5 handles is a fixed integer the e2e test pins exactly.

The structural difference between the literal-shape Task 58 wants and the v1-shape Task 58 ships:

| Property | Plan literal "10+ in single arm" | Task 58 v1 ship |
|---|---|---|
| Multi-shot k invocations | 10+ in one arm | 10 across 5 arms (2 per arm) |
| Distinct handler frames | 1 | 5 |
| Distinct k_closure heap records | 1 (reused 10×) | 5 (reused 2× each) |
| Tests "k_closure stays valid across N reads" | Yes (single record reused 10×) | Partially (single record reused 2×) |
| Tests "fresh-frame multi-shot is independent across handles" | No (single frame) | Yes (5 fresh frames) |

The two property sets are complementary, not equivalent. The literal shape is the harder reuse-stress test for the heap-allocated k_closure record; the v1 shape is the harder fresh-frame-handler test for the runtime's frame allocator + GC bitmap discipline + push/pop sequencing across multiple handles in one program. Both are useful coverage. **Task 60's perf floor work** (`Multi-shot stress test (3-element Choose combinator, N=1000 iterations) in <5s on both hosts`) will exercise the literal-shape stress at much higher N via the canonical helper-recursion idiom (per `[DEVIATION Task 55] Phase 4e captures+` line 1507's "Stage 9 P20 readiness analysis": `pick_int(low, high)` recurses with `if perform Choose.flip() then low else pick_int(low+1, high)`); the recursion-driven N can reach 32+ leaves with N=5 levels of helper recursion, all using only the Slice C v1 2-let arm shape. Task 58's v1 ship does the *correctness rigor* (multi-shot semantics work end-to-end with `(Int) -> Int` ops, fresh-frame independence) at a representative scale; Task 60 does the *throughput stress* via the recursion idiom.

**Rationale:** Three options were considered:

1. **Land the Slice C N-chain extension as part of Task 58.** This is real codegen work — extending the post-arm-k synth-fn chain from 2 fns to N fns, each capturing prior `let r_i` results into its closure record. The work is mechanical but layered: the pre-pass needs to allocate N-1 synth-fn FuncIds per matching arm (vs. exactly 1 today for `LetBindThenTail`); each post-arm-k synth fn needs its own captures list; the closure-record allocation discipline at the arm-fn body emit site needs to thread (k_closure, k_fn) plus all prior `r_i` results into each step's record. Out of scope for Task 58 (which is a *test rigor* task, not a *codegen-feature* task).

2. **Land Task 58 with `examples/multishot_stress.sigil` deferred until the Slice C N-chain extension ships.** This option keeps the literal plan wording intact but pushes Task 58 closure to an unknown future date and leaves Stage 6's "multi-shot rigor" coverage incomplete in the interim. Task 60's perf-floor target depends on the multi-shot machinery being exercised at scale; without Task 58's stress example landing, Task 60's verification has nothing to point at as a representative correctness baseline.

3. **(Selected.)** Land Task 58 with the v1-permitted stress shape (5 sequential handles totalling 10 k invocations) and a deviation entry naming the literal-shape shipment as future Slice C N-chain extension territory. This matches the established Plan B precedent — Phase 4d MVP shipped under literal-Phase-4 wording with a deviation entry naming the captures+ extension closure point; Phase 4e captures+ shipped the captures+ extension under a separate deviation entry pinning the 2-let cap; Task 58 ships the *correctness rigor* under the same pattern, with the *literal-N stress* shape pinned for a future captures-bearing extension.

The selected option is consistent with the plan's "do not paper over correctness failures" hard rule (Task 58 v1 produces correct multi-shot semantics; the v1 shape just doesn't exercise the literal "10+ in single arm" stress) and with the "do not weaken to unrestricted continuations" rule (the v1 shape is canonical 2-let-arm Slice C; no semantic relaxation).

**Closure point:** future Slice C N-chain extension (a separate post-Task-58, post-Phase-4e chore with its own PR). The extension lifts the cap on `arm_body_multi_let_then_pure_tail_shape` from 2 lets to N lets; un-ignores `slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen` and inverts it to a positive test asserting the N-chain dispatches correctly; ships an `examples/multishot_stress_n.sigil` (or upgrades `multishot_stress.sigil`) with 10+ resumes in a single arm. Task 58 v1's `multishot_stress.sigil` continues to serve as the fresh-frame-independence coverage; the N-chain version adds the single-frame-reuse coverage. Both ship coexistent.

**Implementing commit(s):** Foundation `[HEAD]` (this entry + Task 57 squash-hash flip + PROGRESS Task 58 entry transition `todo` → `in-progress`); subsequent commits implement `examples/choose_demo.sigil` + `examples/multishot_stress.sigil` + e2e tests.
