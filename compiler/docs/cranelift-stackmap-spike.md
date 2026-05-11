# Cranelift 0.131.0 stack-map API — spike findings

## Status: spike complete, Plan E2 Phase 1 Task 1

This document pins the Cranelift API surface Plan E2 (precise GC +
real stack-maps) will use. The repro lives at
`compiler/tests/cranelift_stackmap_spike.rs` and runs on every push
via `cargo test`. Two tests cover both API paths Phase 1 Task 2 will
use:

- `value_variant_emits_stackmap_at_call_safepoint` — the single-Value
  path (one `declare_value_needs_stack_map` + one call safepoint + one
  expected entry).
- `var_variant_emits_stackmap_for_phi_confluence` — the Variable
  path (one `declare_var_needs_stack_map` + two `def_var`s in
  separate predecessor blocks + one `use_var` past the safepoint in
  the join block). This is the canonical phi-confluence shape the
  Task 2 sweep will need; per-Value flagging at each phi input edge
  is fragile, the Variable variant is not.

## Plan E2 hypothesis vs reality

The plan body listed three open questions about the API surface:

| Plan hypothesis | Reality in 0.131.0 |
|---|---|
| `ir::Value::set_gc_ref()` flags a GC reference. | No such method. The frontend exposes `FunctionBuilder::declare_value_needs_stack_map(Value)` / `declare_var_needs_stack_map(Variable)`. The frontend handles the spill/reload around each safepoint and records the stack slot in the per-call `UserStackMap`. |
| Safepoint annotation is a per-instruction flag, possibly via `MemFlags` or a separate `safepoint_*` instruction. | No annotation needed. *Every non-tail `call` / `call_indirect` is automatically a safepoint.* The 0.131 docs explicitly call out the conservatism: skipping safepoints at calls known not to GC, or moving them to e.g. volatile loads at GC-trigger pages, is "future work." For Sigil this conservatism matches what we want anyway. |
| Stack-map retrieval candidates: `MachBufferFinalized::stack_maps()`, `compile()` return value, separate emit pass. | `Context::compile(isa, ctrl_plane)` returns a `&CompiledCode`. `code.buffer.user_stack_maps()` returns `&[(CodeOffset, u32, ir::UserStackMap)]` — `(pc_offset_within_fn, frame_size_bytes, map)`. The `UserStackMap` yields `(ir::Type, sp_offset)` entries via `.entries()`. |

The big shape change vs the plan: **Cranelift handles spilling and
slot allocation for us** — the producer's only job is (a) flag values
that are GC refs, and (b) attach stack-map entries at safepoints that
the frontend creates. With `declare_value_needs_stack_map`, even step
(b) is handled: the frontend walks the function in `finalize()`,
computes live-across-safepoint sets, allocates spill slots, and
attaches `UserStackMapEntry { ty, slot, offset }` for each live GC ref
at each call site.

## API surface (Plan E2 will use this verbatim)

### Marking GC references

```rust
// At the IR-builder level — after defining the value that holds a heap pointer:
let v = builder.ins().call(alloc_ref, &[...]);
let cp = builder.inst_results(v)[0];          // returned heap pointer
builder.declare_value_needs_stack_map(cp);    // flag as GC ref
```

Constraints (asserted inside `declare_value_needs_stack_map`):

- Value type's size must be `<= 16` bytes.
- Size must be a power of two.

Sigil's pointer values are `pointer_ty` (i64 on x86_64 / aarch64), so
the constraint is trivially satisfied for every category in Phase 1
Task 2 (alloc returns, heap-pointer loads, phi confluences).

### Safepoints

No explicit marking required. From `cranelift-codegen`'s
`ir/user_stack_maps.rs` module-level comment:

> A **safepoint** is a program point (i.e. CLIF instruction) where it
> must be safe to run GC. Currently all non-tail call instructions are
> considered safepoints.

This affects Phase 1 Task 3 ("Annotate safepoints") materially: the
task as written is a no-op. The Plan-E2 follow-up is to *evaluate
whether Cranelift's automatic safepoint coverage is sufficient* —
which we believe it is, since Sigil's only safepoints are exactly
call sites that may allocate, and all such sites are calls.

Tail calls (`return_call`, used by Sigil's Sync CallConv user-fn
TCO) are *not* safepoints. This is correct for our needs — the
callee owns the next safepoint after the transfer of control. Plan
E2 should not need to revisit TCO sites.

### Finalizing and compiling

```rust
builder.finalize();                                // runs the safepoint pass
let code = ctx.compile(&*isa, &mut ControlPlane::default())?;
```

`FunctionBuilder::finalize()` triggers the safepoint pass in
`cranelift_frontend::frontend::safepoints::run` when
`stack_map_values` is non-empty. The pass:

1. Walks the function CFG and computes the live set at each safepoint.
2. Allocates sized stack slots per (type, slot-size) bucket.
3. Spills live GC refs at each safepoint, reloads immediately after.
4. Calls `dfg.append_user_stack_map_entry(inst, UserStackMapEntry { ty, slot, offset })`
   for each live ref at each safepoint.

The pass runs whether or not `Module::define_function` is used:
`define_function` calls `ctx.compile(...)` internally, so the
identical pipeline applies for sigil's `ObjectModule` integration.

### Reading the stack maps post-compile

```rust
let maps: &[(CodeOffset, u32, ir::UserStackMap)] =
    code.buffer.user_stack_maps();
for (pc_off_in_fn, frame_size_bytes, sm) in maps {
    for (ty, sp_offset) in sm.entries() {
        // pc_off_in_fn is the offset from the function's first byte.
        // sp_offset is from the safepoint's SP, growing toward higher
        // addresses (so the pointer to the live ref is `sp + sp_offset`).
    }
}
```

`code.buffer` is a `MachBufferFinalized`. `user_stack_maps()` returns
a borrow; `take_user_stack_maps()` moves the data out if you need to
hold it past the compile boundary (Phase 1 Task 4 will move; the
section writer owns the data). Ownership note for Task 4:
`take_user_stack_maps` takes `&mut self` on the buffer, which means
the section writer must grab the data before anything else borrows
the buffer immutably (e.g. before `buffer.data()` is called for the
relocations/byte emission). Sigil's existing `ObjectModule::
define_function_with_control_plane` call sequence reads
`ctx.compiled_code().unwrap().buffer` immutably for `relocs()` and
`data()`; Task 4 should take the stack maps before that step (or
clone the borrow), not after.

Within Sigil's `ObjectModule` integration, the offset is *within the
function*. The Task 4 section writer needs to translate this to a
section-relative or text-relative offset before emitting. The
`Module::define_function` path already exposes `ctx.compiled_code()`
after the call returns; we read `buffer.user_stack_maps()` there and
add the function's section base offset (tracked by `ObjectModule`).

## The minimal repro

`compiler/tests/cranelift_stackmap_spike.rs` is an integration test
exercising both API paths on every push. Run locally with:

```
cargo test --test cranelift_stackmap_spike -p sigil-compiler
```

### Value-variant test

Builds the function:

```text
fn entry() -> i64:
  v0 = iconst.i64 42      ; declared needs-stack-map
  call tickle(v0)         ; implicit safepoint
  return v0
```

`tickle` is an unresolved external (`UserExternalName { namespace: 0,
index: 1 }`). The call site is the safepoint; `v0` is live across it
(it's also the return value), and it's flagged as a GC ref, so
Cranelift spills it.

### Variable-variant test (phi confluence)

Builds the function:

```text
fn entry(selector: i64) -> i64:
  entry:
    brif selector, then, else
  then:
    def_var gc_ref, 111
    jump merge
  else:
    def_var gc_ref, 222
    jump merge
  merge:
    live = use_var gc_ref
    call tickle(live)       ; implicit safepoint
    return use_var gc_ref
```

`gc_ref` is a frontend `Variable` declared via
`declare_var_needs_stack_map`. The two `def_var`s converge at `merge`;
the safepoint pass propagates needs-stack-map to whichever SSA value
the frontend chooses as the phi result. One expected entry,
regardless of which predecessor flowed.

### Verified output (run 2026-05-11, linux-x86_64, host CallConv)

Value-variant test passes against `maps.len() == 1`, one entry of
type `i64`. The first observed run (linux-x86_64, host CallConv) put
the call site at PC offset `0x20` with a 16-byte frame and the spill
slot at `sp+0x0`. The test asserts the *structural* shape — entry
count and type — not the absolute offsets, since spill-slot layout is
allowed to change across Cranelift updates.

Variable-variant test passes against `maps.len() == 1`, one entry of
type `i64` at the call site in `merge_blk`. The phi result allocated
by the frontend is whichever SSA value carries `gc_ref` at the safepoint;
`declare_var_needs_stack_map` flags whichever it picks.

Both tests dump the post-`finalize()` IR via `Function::display` when
the stack-map list is empty — saves a debug session next time the
spike fails after an upstream Cranelift change.

The combined evidence is the minimum proof that:

- `declare_value_needs_stack_map` and `declare_var_needs_stack_map`
  both actually cause spills + map entries.
- The `call` instruction is treated as a safepoint without explicit
  annotation.
- `code.buffer.user_stack_maps()` returns post-regalloc data with a
  real PC offset, not a placeholder IR-handle.
- The Variable path handles phi confluence — Phase 1 Task 2 can
  rely on it for heap-pointer-bearing locals defined in multiple
  predecessor blocks.

## Implications for Phase 1's downstream tasks

- **Task 2 (mark GC refs in codegen).** Walk `codegen.rs` and call
  `builder.declare_value_needs_stack_map(v)` at:
  - every `let cp = builder.inst_results(alloc_call)[0]` after a
    `sigil_alloc` (constructors, closure records, Ref cells,
    NextStep allocations);
  - every heap-pointer load (record-field reads, closure-env loads);
  - phi confluences whose inputs are heap pointers — needs care:
    `declare_value_needs_stack_map` is per-value, so each predecessor
    side has to declare. The frontend's safepoint pass already
    propagates needs-stack-map across SSA values via
    `func_ctx.ssa.values_for_var`, so the cleanest pattern is to use
    `Variable`s for GC-ref locals and call
    `declare_var_needs_stack_map(var)` — every value that flows into
    that variable is automatically flagged.

- **Task 3 (annotate safepoints).** No-op against the API. The task
  becomes: *audit that every call site Plan E2 cares about uses
  `call` / `call_indirect` (not `return_call`)*. Sigil's
  `lower_call_in_tail_pos` for the Sync CallConv uses `return_call`,
  which is *not* a safepoint — but it also can't allocate (the callee
  takes over). Cranelift's automatic safepoint coverage is sufficient.

- **Task 4 (v1 section writer).** Translate each `(pc_off_in_fn,
  frame_size, UserStackMap)` to the v1 record shape declared in
  `sigil-abi::stackmap`. Open question for Task 4: do we want to
  expand the v1 record to carry the type alongside each offset, or
  treat all entries as opaque "live pointer" slots? Cranelift gives us
  the type (`ir::Type`) for free; carrying it through is one extra u8
  per entry and lets Phase 2's precise marker validate the bitmap
  shape against the typecheck-derived expectation. Recommend: include
  it. Phase 2 acceptance gates on bitmap-vs-typecheck cross-check,
  which costs nothing here.

- **Task 5 (runtime reader + cross-check).** The reader walks
  `[(pc_off, frame_size, entries)]` records; the cross-check compares
  the entries Cranelift emitted against Sigil's typecheck-derived
  expected root set. The cross-check is "precise ⊆ conservative" per
  the design doc — note that with frontend-driven spills, the precise
  set may *exactly equal* the typecheck expectation (no over-
  approximation), so the inclusion check passes trivially. Real
  divergence would indicate a bug in Phase 1 Task 2's `declare_*`
  coverage.

## Stability

- 0.131.0 ships the user-stack-map API as `pub`-stable.
  `declare_value_needs_stack_map` is documented and tested in
  `cranelift-frontend`; `MachBufferFinalized::user_stack_maps` is
  documented in `cranelift-codegen::machinst::buffer`.
- The same API has been present and stable since 0.115 (introduction
  of the "user stack maps" terminology). Risk of an API rename
  across a patch-version bump is low; risk across a minor-version
  bump is real and warrants the existing `=0.131.0` exact pin staying
  in place until Plan E2 lands.
- No 0.131.0 escalation needed: every capability the design requires
  is present and exposed.

## What this spike does NOT decide

- Whether to extend the v1 record format with type info (Task 4
  question; see "Implications" above for the recommendation).
- How to thread the per-function PC offset into a section-relative
  one. This is mechanical (`ObjectProduct::object` carries the text
  section's per-function offsets via `function_offset` once available
  on the finalized object), but the exact integration point is a
  Task 4 detail.
- Whether to keep `STACKMAP_VERSION_PLACEHOLDER` records alongside v1
  during the transition. The design doc says no (v1 replaces v0
  entirely); Task 4 confirms by deleting the v0 placeholder writer in
  the same commit that lands v1.
