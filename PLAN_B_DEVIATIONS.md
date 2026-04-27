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

4. **Slice D — surrounding-lambda closure captures into arm bodies (item 7).** Phase 4d MVP's `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending` `#[ignore]`'d test pins the gap. Phase 4e captures+ extends the typecheck-side `handle_arm_captures` side-table with a per-arm "lambda-frame source" annotation indicating whether each capture comes from the immediate surrounding fn's locals (today's path) or from an enclosing lambda's closure record (new path). Codegen's arm-closure-record allocation site reads from the lambda's `closure_ptr` (already in scope at the arm-body's lowering) for the latter, instead of from `Lowerer.env`. The walker's `Expr::ClosureEnvLoad` rejection in arm bodies lifts. The `#[ignore]`'d test inverts to a positive test.

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
