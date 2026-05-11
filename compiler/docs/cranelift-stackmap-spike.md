# Cranelift 0.131.0 stack-map API — spike findings

## Status: spike complete, Plan E2 Phase 1 Task 1

This document pins the Cranelift API surface Plan E2 (precise GC +
real stack-maps) will use. The repro lives at
`compiler/tests/cranelift_stackmap_spike.rs` and runs on every push
via `cargo test`. Two tests cover both API paths Phase 1 Task 2 will
use:

- `value_variant_flag_filters_live_set_at_safepoint` — the
  single-Value path. Two i64 values are live across the safepoint;
  only one is flagged. The test asserts `entries.len() == 1`, proving
  `declare_value_needs_stack_map` is the filter (not "every live
  value gets a stack-map entry").
- `var_variant_emits_stackmap_for_phi_confluence` — the Variable
  path (one `declare_var_needs_stack_map` + two `def_var`s in
  separate predecessor blocks + one `use_var` past the safepoint in
  the join block). This is the canonical phi-confluence shape the
  Task 2 sweep will need. The Variable path is the documented
  frontend-supported route for values that flow through a phi; we
  have NOT validated per-Value flagging across phi confluences
  (would require flagging each predecessor's `iconst` separately and
  observing whether the frontend's SSA-propagation produces an entry
  at the merge-block safepoint). Use the Variable variant because
  that's what's verified, not because per-Value provably fails.

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
byte emission inside `ObjectModule::define_function`). Concretely:
sigil currently calls `module.define_function(entry.func_id, &mut
ctx)` at `compiler/src/codegen.rs:11024`, `:12085`, `:12289`,
`:12700` (and other sites — `grep -n "\.define_function(" compiler/src/codegen.rs`
for the full list). Each is the integration point Task 4 amends:
**after** `define_function` returns and **before** `ctx` is reused
for the next function, read `ctx.compiled_code().unwrap().buffer.
user_stack_maps()` (immutable borrow — sufficient for v1 emission)
or `.take_user_stack_maps()` (mutable; needed only if you keep the
data past `ctx.clear()`). Inside `define_function`, the
`cranelift-object` backend already does its own immutable
`buffer.relocs()` + `buffer.data()` walk to emit bytes, so the
`take_*` mutable borrow is only safe **after** that call completes
— which is also when we can first observe the buffer at all.

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

Value-variant test passes against `maps.len() == 1` *and*
`entries.len() == 1`, with two i64 values live across the safepoint
and only one flagged. The structural shape — one entry, type i64 —
is what gets asserted; the absolute slot offset isn't, since
spill-slot layout is allowed to change across Cranelift updates.

Variable-variant test passes against `maps.len() == 1`,
`entries.len() == 1`, one entry of type `i64` at the call site in
`merge_blk`. The phi result allocated by the frontend is whichever
SSA value carries `gc_ref` at the safepoint;
`declare_var_needs_stack_map` flags whichever it picks.

Both tests dump the post-`finalize()` IR via `Function::display` to
stderr *before* the asserts run, so any failed assertion (empty map
list, wrong entry count, wrong entry type) lands with the IR
already visible — saves a debug session next time the spike fails
after an upstream Cranelift change.

The combined evidence is the minimum proof that:

- `declare_value_needs_stack_map` actually *filters* the live set
  (only the flagged value gets a stack-map entry — the second live
  i64 in the value variant test does NOT appear in the map).
- `declare_var_needs_stack_map` causes a spill + map entry for the
  Variable's value at the safepoint.
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
  - phi confluences whose inputs are heap pointers — use the
    Variable path. `declare_var_needs_stack_map(var)` is the
    documented frontend-supported route: the safepoint pass walks
    `func_ctx.stack_map_vars` in `FunctionBuilder::finalize` and
    propagates the needs-stack-map flag to every SSA value the SSA
    builder produced for that variable via
    `func_ctx.ssa.values_for_var`. The integration test verifies
    this path; the per-Value path on a phi-result value has not been
    validated and is not the documented contract.

- **Task 3 (annotate safepoints).** No-op against the API. The task
  becomes: *audit that every call site Plan E2 cares about uses
  `call` / `call_indirect` (not `return_call*`)*. Cranelift treats
  every non-tail `call` / `call_indirect` as an automatic safepoint;
  `return_call` and `return_call_indirect` are *not* safepoints —
  the caller's frame is released by the time the callee runs, so
  any live GC refs flowing across become roots in the callee's
  fresh frame, not roots at the tail-call instruction itself.

  **Audit performed 2026-05-11.** Correct multi-line-safe scan:

  ```
  grep -nE 'return_call(_indirect)?\(' compiler/src/codegen.rs | grep -v '//'
  ```

  returns **two** sites:

  | # | Line | Site | First-arg shape |
  |---|---|---|---|
  | 1 | 19987 | `lower_call_in_tail_pos`, Sync direct branch — emits `return_call` | `null_closure` (constant `iconst(pointer_ty, 0)` — NOT a heap pointer) |
  | 2 | 20428 | `lower_call_in_tail_pos`'s indirect branch (PR #108) — emits `return_call_indirect` | `closure_value` — **the actual closure heap pointer being tail-invoked** |

  At both sites: `arg_vals` (user-fn args, may be heap pointers)
  and `terminal_out` (caller's terminal-out pointer param — a
  pointer to caller-stack, not a heap ref) follow.

  The non-annotation conclusion is the same for both sites but the
  *reason* differs subtly:

  - **Site 1 (direct).** The first arg is statically null; user
    args + terminal_out are the only ones with non-trivial liveness,
    and they become block-params in the callee's frame.
  - **Site 2 (indirect).** The first arg `closure_value` IS a live
    heap pointer — but the reason no stack-map entry is needed at
    the `return_call_indirect` instruction is **not** that the
    value is non-heap. It's that Cranelift's tail-call IR is
    explicitly not a safepoint: at the moment the tail-call
    executes, the caller's stack frame is released, and any live
    heap refs (including `closure_value`) flow into the callee's
    block-params via the call's ABI shape. The callee's safepoint
    pass attaches stack-map entries to those block-params if they
    are flagged needs-stack-map.

  *No annotation is needed at either `return_call*` site itself.*
  Marking the callee's received fn-entry block-params as
  needs-stack-map is a separate concern, handled by **Task 2b**'s
  block-arg sweep (categories 2-3 of Task 2 in the plan body). For
  Plan E2's correctness, Task 2b's contract here is tighter than the
  plan body's wording: *every fn-entry block-param of pointer type
  for any fn callable via `return_call` / `return_call_indirect`*
  must be flagged. **Re-audit this section after Task 2b lands** —
  Task 3's closure depends on Task 2b's coverage of fn-entry
  block-params for tail-callable fns.

  Task 3's plan-body acceptance test ("stackmap section is
  non-empty after a small program compile") is covered transitively:
  PR #151's spike integration tests (`value_variant_flag_filters_
  live_set_at_safepoint`, `var_variant_emits_stackmap_for_phi_
  confluence`) verify Cranelift's automatic safepoint at each
  non-tail `call`, and PR #156's Task 2a markings ensure
  `code.buffer.user_stack_maps()` has real entries for any compiled
  function with at least one alloc-bearing call. The stackmap
  *section* (v0 placeholder today) is bumped to v1 with real
  entries by **Task 4**, which is also where G1's end-to-end
  verification test lands.

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
- The API is documented and tested in 0.131.0; the user-stack-maps
  terminology has been present for several recent minor versions, but
  we have NOT audited exact rename history across the 0.1x line
  (`declare_value_needs_stack_map` specifically moved around as the
  user-stack-maps work landed in stages). Risk of an API rename
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
