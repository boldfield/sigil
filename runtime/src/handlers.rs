//! Algebraic-effects handler stack + CPS trampoline — Plan B Task 56.
//!
//! Provides the runtime scaffolding for `handle ... with { ... }` blocks
//! and `perform` sites. Plan B Task 55 wires codegen against this surface.
//!
//! # CPS-color calling convention
//!
//! Every CPS-color function shares a uniform signature so the trampoline
//! can dispatch any fn against any argument vector without static type
//! knowledge:
//!
//! ```text
//! extern "C" fn cps_fn(
//!     closure_ptr: *mut u8,    // captured environment (or null for top-level fns)
//!     args_ptr:    *const u64, // packed argument buffer
//!     args_len:    u32,        // number of u64-widened user args
//! ) -> *mut NextStep
//! ```
//!
//! Each user argument is widened to `u64` before being placed in the
//! buffer. The fn prologue (emitted by Task 55's codegen) reads the
//! args from `args_ptr` according to the fn's known signature. The
//! return is a pointer to an arena-allocated `NextStep` describing what
//! the trampoline should do next:
//!
//! - `NEXT_STEP_TAG_DONE` — terminal. The trampoline returns
//!   `next.value` to its caller and exits.
//! - `NEXT_STEP_TAG_CALL` — invoke the carried closure with the carried
//!   args, then dispatch on the result.
//!
//! # HandlerFrame layout
//!
//! HandlerFrames are heap-allocated (via `sigil_alloc`) so they survive
//! arena resets across trampoline iterations. The thread-local handler
//! stack keeps a head pointer; frames link via `prev`.
//!
//! Layout (after the 8-byte Sigil object header):
//!
//! ```text
//! offset 8:  effect_id: u32
//! offset 12: arm_count: u32         (number of op_arms)
//! offset 16: return_fn:      *mut u8 (CPS-color fn for the `return(v) =>` arm)
//! offset 24: return_closure: *mut u8 (closure_ptr for the return arm)
//! offset 32: prev: *mut HandlerFrame (next-deeper frame, or null)
//! offset 40: arms: [(fn_ptr: *mut u8, closure_ptr: *mut u8); arm_count]
//! ```
//!
//! GC pointer-bitmap (see `Header::new`):
//! - Bit 2 — return_closure (payload word 2)
//! - Bit 3 — prev           (payload word 3)
//! - Bits 5, 7, 9, … — arm[i].closure_ptr at payload word `5 + 2*i`
//!
//! The function pointers (return_fn, arms[i].fn_ptr) are NOT scanned —
//! they reference `.text` not the GC heap.
//!
//! # Counters
//!
//! - `SIGIL_COUNTER_HANDLER_WALK_COUNT` — incremented per
//!   `sigil_perform` **attempt**, regardless of whether a matching
//!   frame was found. Counts perform sites reached, not successful
//!   dispatches; a deliberately-unhandled effect aborts but still
//!   shows up in this counter for debugging visibility.
//! - `SIGIL_COUNTER_HANDLER_WALK_DEPTH_SUM` — accumulates the walk
//!   depth, defined as the number of frames inspected up to and
//!   including the matching frame on a hit, or the full stack depth
//!   on an unhandled-effect abort. Average walk depth is
//!   `WALK_DEPTH_SUM / WALK_COUNT`.
//! - `SIGIL_COUNTER_TRAMPOLINE_DISPATCH_COUNT` — incremented per
//!   `sigil_run_loop` iteration.
//!
//! # GC reachability
//!
//! `HANDLER_STACK` is a thread-local `Cell<*mut HandlerFrame>`. Boehm's
//! automatic stack/data-segment scan does not enumerate `thread_local!`
//! storage in any portable way, so a `HandlerFrame` reachable only
//! through `HANDLER_STACK` would be reclaimed if a GC fires while
//! pushed. Plan B Task 56 fixes this by registering the cell's TLS
//! address as a Boehm root via `register_handler_stack_root_for_calling_thread`,
//! triggered from `sigil_gc_init`.
//!
//! Each subsequent `prev` pointer in the chain is reachable through
//! the previous frame's payload; Boehm scans those conservatively
//! because `sigil_alloc` allocates HandlerFrames via `GC_malloc` (the
//! per-bit precision of the pointer bitmap is v2-forward-compat
//! metadata; v1 Boehm consumes it as a binary signal selecting between
//! `GC_malloc` and `GC_malloc_atomic`). The `arms[i].closure_ptr`
//! slots and `return_closure` slot become reachable through the
//! HandlerFrame allocation and are scanned conservatively along with
//! the rest of the block.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;

use sigil_abi::effect::{NEXT_STEP_TAG_CALL, NEXT_STEP_TAG_DONE};

use crate::counters::{self, CounterId};
use crate::header::{Header, TAG_CLOSURE};

/// CPS-color calling convention (see module-level docs).
type CpsFn = unsafe extern "C" fn(
    closure_ptr: *mut u8,
    args_ptr: *const u64,
    args_len: u32,
) -> *mut NextStep;

/// Maximum op-arms a single handler frame can carry. Bounded by the
/// 32-bit GC pointer-bitmap: arm `i`'s closure pointer lives at
/// payload word `5 + 2*i`, so the highest reachable bit is `5 + 2*13 = 31`
/// at `i = 13`. With `MAX_HANDLER_ARMS = 14` (i.e. `i ∈ [0, 13]`) the
/// bitmap is fully utilised; one less and bit 31 stays empty. v1
/// effects ship with 1–3 ops; the cap is comfortably above realistic
/// v1 needs.
pub const MAX_HANDLER_ARMS: u32 = 14;

/// Maximum user-arg count `sigil_perform` can carry through to a
/// handler arm (plus the implicit `(k_closure_ptr, k_fn_ptr)` pair the
/// runtime appends, so the trampoline-side cap is `MAX_INLINE_ARGS + 2`
/// total). Re-exported from `sigil_abi::effect` (Plan B Task 55 Phase 4b)
/// so codegen and runtime read from the same source. Codegen (Task 55
/// Phase 4b) must box arities exceeding this — flagged in
/// `PLAN_B_DEVIATIONS.md`.
pub use sigil_abi::effect::MAX_INLINE_ARGS;

/// Discriminated `NextStep` record. Arena-allocated; pointer is invalid
/// after the next `sigil_arena_reset`. The trampoline reads the
/// discriminant + payload into stack locals before resetting.
///
/// `#[repr(C)]` so codegen (Task 55) can store fields at known offsets.
#[repr(C)]
pub struct NextStep {
    /// `NEXT_STEP_TAG_DONE` or `NEXT_STEP_TAG_CALL`.
    pub tag: u32,
    /// Number of u64 args that follow this struct in arena memory
    /// (valid when `tag == NEXT_STEP_TAG_CALL`).
    pub arg_count: u32,
    /// `closure_ptr` for the callee (valid when tag == CALL).
    pub closure_ptr: *mut u8,
    /// `fn_ptr` for the callee (valid when tag == CALL); cast to
    /// `CpsFn` at dispatch.
    pub fn_ptr: *mut u8,
    /// Result value (valid when tag == DONE).
    pub value: u64,
    // When tag == CALL, `arg_count` u64 args follow this struct in
    // arena memory at offset `size_of::<NextStep>()`. Codegen reaches
    // them via pointer arithmetic; the trampoline uses
    // `next_step_args_ptr` below.
}

/// HandlerFrame heap object (see module-level docs for the layout).
///
/// `#[repr(C)]` to pin the field offsets that codegen (Task 55) writes.
/// The `arms` array is variable-length and lives immediately after this
/// struct in the same allocation; the fixed-size struct only covers the
/// header. Use the helper accessors below to read or write arm slots.
#[repr(C)]
pub struct HandlerFrame {
    pub effect_id: u32,
    pub arm_count: u32,
    pub return_fn: *mut u8,
    pub return_closure: *mut u8,
    pub prev: *mut HandlerFrame,
    // arms follow: [(fn_ptr: *mut u8, closure_ptr: *mut u8); arm_count]
}

// Thread-local handler stack head. Frames link via `prev`. Null = no
// active handlers (top-level user code).
//
// v1 is single-threaded but the `thread_local!` keeps the API
// forward-compatible: a multi-threaded v2 trampoline can add
// inter-thread effect dispatch on top of this without ABI churn.
//
// Boehm GC rooting: the cell's TLS address is registered as a Boehm
// root via `register_handler_stack_root_for_calling_thread` (called
// from `sigil_gc_init`). See module-level "GC reachability" docs.
thread_local! {
    static HANDLER_STACK: Cell<*mut HandlerFrame> = const { Cell::new(ptr::null_mut()) };
    /// Per-thread flag: has this thread's `HANDLER_STACK` cell been
    /// registered with Boehm? Set by
    /// `register_handler_stack_root_for_calling_thread`, idempotent
    /// per thread.
    static HANDLER_STACK_ROOTED: Cell<bool> = const { Cell::new(false) };
}

/// Register the calling thread's `HANDLER_STACK` TLS cell as a Boehm
/// GC root. Idempotent per thread.
///
/// Returns the `[start, end)` range that was registered (or the
/// already-registered range from a prior call on this thread). Test
/// infrastructure uses the returned range to symmetrically
/// `GC_remove_roots` on thread exit.
///
/// Must be called by every thread that will push HandlerFrames. v1 is
/// single-threaded (only `main` enters the trampoline in production);
/// test threads opt in via `GcThreadEnrolment::acquire` so the
/// registration is paired with a teardown on Drop.
pub(crate) fn register_handler_stack_root_for_calling_thread() -> (*mut c_void, *mut c_void) {
    HANDLER_STACK.with(|cell| {
        let start = cell as *const Cell<*mut HandlerFrame> as *mut c_void;
        // Cell<*mut HandlerFrame> is one machine word (8 bytes on
        // 64-bit hosts). Compute end via byte arithmetic so the
        // cast type doesn't have to match the underlying repr.
        // SAFETY: not an interior pointer (the result feeds an FFI
        // call that takes [start, end) as a half-open range, never
        // retained; the cell's TLS storage lives for the thread's
        // lifetime).
        let end = unsafe {
            (start as *mut u8).add(core::mem::size_of::<Cell<*mut HandlerFrame>>()) as *mut c_void
        };
        let already_registered = HANDLER_STACK_ROOTED.with(|rooted| {
            let r = rooted.get();
            rooted.set(true);
            r
        });
        if !already_registered {
            unsafe {
                crate::gc::GC_add_roots(start, end);
            }
        }
        (start, end)
    })
}

/// Inverse of `register_handler_stack_root_for_calling_thread`. Used
/// by `GcThreadEnrolment::drop` in tests to unregister the range
/// before the thread exits, preventing stale-range leaks across test
/// thread teardowns.
#[cfg(test)]
pub(crate) fn unregister_handler_stack_root_for_calling_thread(
    start: *mut c_void,
    end: *mut c_void,
) {
    HANDLER_STACK_ROOTED.with(|rooted| rooted.set(false));
    unsafe {
        crate::gc::GC_remove_roots(start, end);
    }
}

// ---------------------------------------------------------------------
// HandlerFrame allocation
// ---------------------------------------------------------------------

/// Allocate a `HandlerFrame` for `arm_count` op-arms. All arm slots and
/// the return arm are zero-initialised; codegen subsequently calls
/// `sigil_handler_frame_set_arm` and `sigil_handler_frame_set_return`
/// to populate them.
///
/// # Aborts
///
/// Aborts if `arm_count > MAX_HANDLER_ARMS` (14). The cap is set by the
/// 32-bit GC pointer bitmap; a future relaxation requires a wider
/// bitmap field in the Sigil object header.
///
/// # Safety
///
/// Safe to call. The returned pointer is valid until the GC reclaims it
/// (which only happens once it is no longer reachable from any live
/// closure or the handler-stack head).
#[no_mangle]
pub unsafe extern "C" fn sigil_handler_frame_new(
    effect_id: u32,
    arm_count: u32,
) -> *mut HandlerFrame {
    if arm_count > MAX_HANDLER_ARMS {
        eprintln!(
            "sigil_handler_frame_new: arm_count {arm_count} exceeds MAX_HANDLER_ARMS ({})",
            MAX_HANDLER_ARMS
        );
        std::process::abort();
    }
    let payload_bytes: usize = handler_frame_payload_bytes(arm_count as usize);
    let payload_words: u8 = (payload_bytes / 8).try_into().unwrap_or_else(|_| {
        // Unreachable under MAX_HANDLER_ARMS = 14: payload_bytes peaks
        // at 32 + 16*14 = 256, payload_words at 32 → fits u8. Defensive
        // for future cap revisions.
        eprintln!(
            "sigil_handler_frame_new: payload_words {} exceeds u8 range \
             (arm_count={arm_count}, payload_bytes={payload_bytes})",
            payload_bytes / 8
        );
        std::process::abort();
    });
    let bitmap = handler_frame_pointer_bitmap(arm_count as usize);

    // INVARIANT: Boehm consumes only the pointer bitmap (binary signal
    // selecting GC_malloc vs GC_malloc_atomic), not the type tag. Reusing
    // TAG_CLOSURE as "heap object with closure-shaped pointer fields" is
    // functionally inert today. If a v2 type-aware GC walker is
    // introduced, add TAG_HANDLER_FRAME alongside in
    // `sigil-header-constants` and revise this site.
    let header = Header::new(TAG_CLOSURE, payload_words, bitmap);
    let obj = crate::gc::sigil_alloc(header.raw(), payload_bytes);

    // Frame fields begin at offset 8 (past the Sigil object header).
    //
    // SAFETY: not an interior pointer (the cast is to a single
    // local-scope read/write target reflecting the documented layout;
    // the pointer is not stored or returned beyond this initialisation).
    let frame_ptr = obj.add(8) as *mut HandlerFrame;
    (*frame_ptr).effect_id = effect_id;
    (*frame_ptr).arm_count = arm_count;
    (*frame_ptr).return_fn = ptr::null_mut();
    (*frame_ptr).return_closure = ptr::null_mut();
    (*frame_ptr).prev = ptr::null_mut();

    // Explicitly zero-init the variable-length arms region rather than
    // depending on the Boehm allocator-zeroing contract. `GC_malloc` /
    // `GC_malloc_atomic` zero today, but that's a libgc-version
    // contract, not a Rust contract. Future Boehm flag flips (e.g. a
    // switch to `GC_malloc_atomic_uncollectable`) would silently flip
    // arm-slot reads from null to garbage. The cost is one
    // `write_bytes` over ≤ 224 bytes (`16 * 14` for the arms region).
    let arms_region_start = (frame_ptr as *mut u8).add(core::mem::size_of::<HandlerFrame>());
    let arms_region_bytes = (arm_count as usize) * 16;
    // SAFETY: not an interior pointer (the destination pointer addresses
    // a freshly-allocated, exclusively-owned payload region; the
    // write_bytes call zeros bytes via a single memset, not pointer
    // retention).
    ptr::write_bytes(arms_region_start, 0, arms_region_bytes);

    frame_ptr
}

/// Set the `(fn_ptr, closure_ptr)` for op-arm `op_id` on a previously
/// allocated frame. Codegen calls this once per op-arm at the top of a
/// `handle ... with` block before pushing the frame.
///
/// # Aborts
///
/// Aborts if `op_id >= frame.arm_count` or `frame` is null.
///
/// # Safety
///
/// `frame` must be a pointer previously returned by
/// `sigil_handler_frame_new`. `fn_ptr` and `closure_ptr` must remain
/// valid for the frame's lifetime.
#[no_mangle]
pub unsafe extern "C" fn sigil_handler_frame_set_arm(
    frame: *mut HandlerFrame,
    op_id: u32,
    fn_ptr: *mut u8,
    closure_ptr: *mut u8,
) {
    if frame.is_null() {
        eprintln!("sigil_handler_frame_set_arm: null frame");
        std::process::abort();
    }
    let arm_count = (*frame).arm_count;
    if op_id >= arm_count {
        eprintln!(
            "sigil_handler_frame_set_arm: op_id {op_id} out of range (arm_count={arm_count})"
        );
        std::process::abort();
    }
    let arms_base = arms_base_ptr(frame);
    // SAFETY: not an interior pointer (the offset is computed solely to
    // perform two local stores against pre-allocated, properly-aligned
    // payload memory; pointers are not retained).
    let slot = arms_base.add(op_id as usize * 2);
    slot.write(fn_ptr);
    slot.add(1).write(closure_ptr);
}

/// Set the return-arm `(fn_ptr, closure_ptr)` on a previously allocated
/// frame. Codegen calls this once per `handle` site that has a
/// `return(v) =>` arm; sites without a return arm leave the slots null
/// (the trampoline's return-arm path treats null as "use the default
/// identity return: forward `value` to the surrounding continuation").
///
/// # Safety
///
/// `frame` must be a pointer previously returned by
/// `sigil_handler_frame_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_handler_frame_set_return(
    frame: *mut HandlerFrame,
    fn_ptr: *mut u8,
    closure_ptr: *mut u8,
) {
    if frame.is_null() {
        eprintln!("sigil_handler_frame_set_return: null frame");
        std::process::abort();
    }
    (*frame).return_fn = fn_ptr;
    (*frame).return_closure = closure_ptr;
}

/// Push `frame` onto the thread-local handler stack. Codegen emits a
/// call to this at the top of every `handle ... with` block, after the
/// frame has been allocated and populated.
///
/// # Safety
///
/// `frame` must be a non-null pointer previously returned by
/// `sigil_handler_frame_new`.
#[no_mangle]
pub unsafe extern "C" fn sigil_handle_push(frame: *mut HandlerFrame) {
    if frame.is_null() {
        eprintln!("sigil_handle_push: null frame");
        std::process::abort();
    }
    // Defensive against codegen bugs that double-push the same frame:
    // a non-null `prev` at push time would silently overwrite the prior
    // chain link. The check is debug-only because a release build on a
    // verified codegen never trips it; if it ever does, the panic
    // localises the bug to the push site rather than a later traversal.
    debug_assert!(
        (*frame).prev.is_null(),
        "sigil_handle_push: frame already linked (double-push?)"
    );
    HANDLER_STACK.with(|cell| {
        let head = cell.get();
        (*frame).prev = head;
        cell.set(frame);
    });
}

/// Pop the top of the thread-local handler stack and return the popped
/// frame pointer. Codegen emits a call to this when the body of a
/// `handle ... with` block reaches its end (and after the return arm,
/// if one is invoked).
///
/// # Aborts
///
/// Aborts on stack underflow (no frames to pop). The codegen
/// invariant is that pushes and pops balance lexically; an underflow
/// indicates a compiler bug.
///
/// # Safety
///
/// Marked `unsafe` for FFI-surface uniformity: the function returns a
/// raw pointer whose validity is the caller's responsibility (the
/// returned frame remains valid only as long as some other live
/// reference — captured continuation closure or surrounding handler
/// chain — keeps it reachable).
#[no_mangle]
pub unsafe extern "C" fn sigil_handle_pop() -> *mut HandlerFrame {
    HANDLER_STACK.with(|cell| {
        let head = cell.get();
        if head.is_null() {
            eprintln!("sigil_handle_pop: handler stack underflow");
            std::process::abort();
        }
        // SAFETY: head is non-null per the underflow check; reading
        // `prev` against the documented HandlerFrame layout is sound.
        let prev = (*head).prev;
        // Clear the popped frame's `prev` link so a subsequent push of
        // the same frame (legitimate use case: re-entering a `handle`
        // in a loop) doesn't trip the no-double-push debug_assert at
        // `sigil_handle_push`.
        (*head).prev = ptr::null_mut();
        cell.set(prev);
        head
    })
}

/// Read the current handler-stack head without popping. Used by tests;
/// codegen has no need (it pushes/pops symmetrically).
#[doc(hidden)]
pub fn handler_stack_head() -> *mut HandlerFrame {
    HANDLER_STACK.with(|cell| cell.get())
}

// ---------------------------------------------------------------------
// NextStep allocation helpers
// ---------------------------------------------------------------------

/// Allocate a `NEXT_STEP_TAG_DONE` record from the per-dispatch arena
/// holding `value`.
///
/// # Safety
///
/// Safe to call. Returned pointer is valid until the next
/// `sigil_arena_reset`.
#[no_mangle]
pub unsafe extern "C" fn sigil_next_step_done(value: u64) -> *mut NextStep {
    let raw = crate::arena::sigil_arena_alloc(core::mem::size_of::<NextStep>());
    let ns = raw as *mut NextStep;
    (*ns).tag = NEXT_STEP_TAG_DONE;
    (*ns).arg_count = 0;
    (*ns).closure_ptr = ptr::null_mut();
    (*ns).fn_ptr = ptr::null_mut();
    (*ns).value = value;
    ns
}

/// Allocate a `NEXT_STEP_TAG_CALL` record from the per-dispatch arena
/// describing a call to `(fn_ptr, closure_ptr)` with `arg_count` args.
/// The args themselves live immediately after the struct in arena
/// memory; the caller (codegen) writes them via the pointer returned by
/// `sigil_next_step_args_ptr`.
///
/// # Safety
///
/// Safe to call. Returned pointer is valid until the next
/// `sigil_arena_reset`.
#[no_mangle]
pub unsafe extern "C" fn sigil_next_step_call(
    closure_ptr: *mut u8,
    fn_ptr: *mut u8,
    arg_count: u32,
) -> *mut NextStep {
    if arg_count > MAX_INLINE_ARGS {
        eprintln!(
            "sigil_next_step_call: arg_count {arg_count} exceeds MAX_INLINE_ARGS ({MAX_INLINE_ARGS})"
        );
        std::process::abort();
    }
    let header_size = core::mem::size_of::<NextStep>();
    let args_size = (arg_count as usize) * 8;
    let raw = crate::arena::sigil_arena_alloc(header_size + args_size);
    let ns = raw as *mut NextStep;
    (*ns).tag = NEXT_STEP_TAG_CALL;
    (*ns).arg_count = arg_count;
    (*ns).closure_ptr = closure_ptr;
    (*ns).fn_ptr = fn_ptr;
    (*ns).value = 0;
    ns
}

/// Pointer to the args buffer attached to a `NEXT_STEP_TAG_CALL`
/// record. Returns null for `DONE` records (or records with arg_count
/// 0). Codegen writes args here after `sigil_next_step_call`.
///
/// # Safety
///
/// `ns` must be a valid pointer returned by `sigil_next_step_call`
/// (or `sigil_next_step_done`, which yields a degenerate null).
#[no_mangle]
pub unsafe extern "C" fn sigil_next_step_args_ptr(ns: *mut NextStep) -> *mut u64 {
    if ns.is_null() || (*ns).tag != NEXT_STEP_TAG_CALL || (*ns).arg_count == 0 {
        return ptr::null_mut();
    }
    // SAFETY: not an interior pointer (the result is a transient
    // arena-buffer address used by the caller to write packed args; the
    // arena is not GC-managed and arena pointers are always
    // non-interior in the GC sense).
    (ns as *mut u8).add(core::mem::size_of::<NextStep>()) as *mut u64
}

/// Plan B Task 55 (Phase 4d) — identity continuation intrinsic.
///
/// Codegen emits the address of this function as the `k_fn_ptr` arg
/// to every non-IO `sigil_perform` site (with `k_closure_ptr` set to
/// null) for performs that don't have a helper synth-cont in scope —
/// e.g., when the perform is in tail position of the handle body (no
/// CPS-color helper wrapping it). When a synthetic CPS arm fn invokes
/// its captured `k(value)` in tail position, codegen lowers the call
/// as `sigil_next_step_call(loaded_k_closure, loaded_k_fn,
/// /*arg_count=*/3)` followed by stores of `[value,
/// post_arm_k_closure, post_arm_k_fn]` at offsets 0/8/16 of the args
/// buffer (Phase 4e captures+ Slice A trailing-pair convention). The
/// returned `NextStep::Call` is the arm fn's return value. The
/// trampoline (`sigil_run_loop`) dispatches the `Call`, invoking
/// `sigil_continuation_identity(null, args_ptr, args_len)`. Identity
/// reads only `args_ptr[0]` and returns `NextStep::Done(value)` —
/// the trailing post-arm-k slots are intentionally ignored at the
/// identity dispatch point. They matter when the runtime dispatches
/// into a *helper synth-cont* k_fn (the captures+ Slice B+ paths)
/// where the synth-cont DOES forward its result through the post-
/// arm-k pair; identity is the terminal case where there is no
/// further chaining.
///
/// The shape produces algebraic-correct results when:
///   - `k(arg)` is invoked in tail position of the arm body, AND
///   - the perform site is in tail position of the handle body (or
///     anywhere within the handle body, since the surrounding native
///     fn synchronously blocks on `sigil_run_loop` and feeds the
///     result back to the perform site).
///
/// Both conditions are enforced by the `unsupported_handle_construct`
/// codegen-entry walker for the Phase 4d MVP path. The captures+
/// Slice A foundation refactor extends the trailing-pair convention
/// to all tail-`k` arm-fn emissions (args_len=3); identity tolerates
/// `args_len >= 1` and reads only the first slot. Non-tail `k` use,
/// multi-shot `k` use, and surrounding-lambda captures into arm
/// bodies remain rejected by the walker until captures+ Slices B/C/D.
/// See `[DEVIATION Task 55] Phase 4e captures+` in
/// `PLAN_B_DEVIATIONS.md` and the "Verification limits (in-flight)"
/// section in `README.md`.
///
/// # Safety
///
/// `args_ptr` must point to at least one readable u64 (`args_len >= 1`).
/// `closure_ptr` is unused (this intrinsic is closure-less). The
/// trampoline guarantees both invariants when dispatching from a
/// `NextStep::Call` produced by codegen's tail-`k` lowering.
#[no_mangle]
pub unsafe extern "C" fn sigil_continuation_identity(
    _closure_ptr: *const u8,
    args_ptr: *const u64,
    args_len: u32,
) -> *mut NextStep {
    // Plan B Task 55, Phase 4e captures+ Slice A — identity tolerates
    // `args_len >= 1`. Tail-`k` arm-fn emissions now uniformly pack
    // `[arg, post_arm_k_closure, post_arm_k_fn]` at args_len=3 (the
    // trailing-pair convention). When the arm dispatches into a
    // helper synth-cont k_fn, the synth-cont reads the post-arm-k
    // pair from args_ptr[1..3] and forwards its result to it; when
    // the arm dispatches into identity directly (perform in tail
    // position of handle body, no helper synth-cont), the trailing
    // pair is irrelevant — identity reads only args_ptr[0] and
    // returns terminal Done.
    debug_assert!(
        args_len >= 1,
        "sigil_continuation_identity: args_len must be >= 1 (codegen \
         emits arg_count=1 from synth-cont's `Call(post_arm_k_*, [result])` \
         dispatch and arg_count=3 from arm-fn tail-`k` direct emit per \
         the Phase 4e captures+ Slice A trailing-pair convention)"
    );
    debug_assert!(
        !args_ptr.is_null(),
        "sigil_continuation_identity: args_ptr must be non-null when args_len >= 1"
    );
    // SAFETY: caller (codegen tail-k lowering or synth-cont post-arm-k
    // dispatch) guarantees args_ptr points to >= 1 readable u64
    // holding the captured arg at slot 0. Trailing slots, if any
    // (Slice A's post-arm-k pair at slots 1..3), are ignored —
    // identity is the terminal continuation.
    let value = *args_ptr;
    sigil_next_step_done(value)
}

// ---------------------------------------------------------------------
// `sigil_perform` and `sigil_run_loop`
// ---------------------------------------------------------------------

/// Resolve a `perform Effect.op(args...)` site. Walks the handler stack
/// looking for the topmost frame whose `effect_id` matches; on a hit,
/// returns a `NextStep::Call` to that frame's `arms[op_id]` with the
/// caller-supplied args followed by the captured continuation `k`.
///
/// `args_ptr` is a packed `[u64; args_len]` buffer (caller-owned; can
/// be arena-allocated). `k_closure_ptr` and `k_fn_ptr` describe the
/// continuation closure that resumes the post-`perform` computation
/// when the arm calls `k(value)`. The continuation is passed as the
/// last user argument to the arm under the convention
/// `(arg0, ..., argN-1, k_closure_ptr, k_fn_ptr)`.
///
/// # Aborts
///
/// Aborts on unhandled effect (no matching frame) or out-of-range
/// `op_id`. v1 considers both compiler bugs — the typechecker rejects
/// programs whose effect rows don't match an enclosing handler.
///
/// # Counters
///
/// Increments `SIGIL_COUNTER_HANDLER_WALK_COUNT` by 1 and
/// `SIGIL_COUNTER_HANDLER_WALK_DEPTH_SUM` by the number of frames
/// inspected (including the matching frame).
///
/// # Safety
///
/// `args_ptr` must point to at least `args_len` readable u64s, or be
/// null when `args_len == 0`. `k_closure_ptr` and `k_fn_ptr` must
/// describe a CPS-color closure that satisfies the calling convention.
#[no_mangle]
pub unsafe extern "C" fn sigil_perform(
    effect_id: u32,
    op_id: u32,
    args_ptr: *const u64,
    args_len: u32,
    k_closure_ptr: *mut u8,
    k_fn_ptr: *mut u8,
) -> *mut NextStep {
    // Bound-check at the perform site so the abort message names the
    // offending effect/op (a deeper check at `sigil_next_step_call` or
    // in the trampoline obscures the source). The arm receives `args
    // + (k_closure, k_fn)`, so the dispatched arg_count is `args_len + 2`
    // — that's what must fit MAX_INLINE_ARGS, not args_len alone.
    if args_len.saturating_add(2) > MAX_INLINE_ARGS {
        eprintln!(
            "sigil_perform: args_len {args_len} + 2 (continuation) exceeds \
             MAX_INLINE_ARGS ({MAX_INLINE_ARGS}) at effect_id={effect_id} op_id={op_id}"
        );
        std::process::abort();
    }
    counters::incr(CounterId::HandlerWalkCount);

    let mut depth: u64 = 0;
    let mut frame = HANDLER_STACK.with(|cell| cell.get());
    while !frame.is_null() {
        depth += 1;
        if (*frame).effect_id == effect_id {
            counters::add(CounterId::HandlerWalkDepthSum, depth);
            if op_id >= (*frame).arm_count {
                eprintln!(
                    "sigil_perform: op_id {op_id} out of range for effect_id {effect_id} \
                     (arm_count={})",
                    (*frame).arm_count
                );
                std::process::abort();
            }
            let arms_base = arms_base_ptr(frame);
            let slot = arms_base.add(op_id as usize * 2) as *const *mut u8;
            let arm_fn = slot.read();
            let arm_closure = slot.add(1).read();
            if arm_fn.is_null() {
                eprintln!(
                    "sigil_perform: matched frame has null fn for effect_id={effect_id} op_id={op_id}"
                );
                std::process::abort();
            }
            // Build a NextStep::Call to the arm with the args followed
            // by (k_closure_ptr, k_fn_ptr) packed as two u64s. The arm
            // prologue (Task 55 codegen) reads the trailing two slots
            // to reconstruct the continuation closure.
            let total_args = args_len + 2;
            let ns = sigil_next_step_call(arm_closure, arm_fn, total_args);
            let ns_args = sigil_next_step_args_ptr(ns);
            // Copy user args. ns_args points into the non-GC arena;
            // args_ptr is a caller-owned u64 buffer. The offsets drive
            // value-copying loads/stores, not retained pointers.
            for i in 0..(args_len as usize) {
                // SAFETY: not an interior pointer (see comment above).
                ns_args.add(i).write(*args_ptr.add(i));
            }
            // Append k_closure_ptr, k_fn_ptr.
            ns_args.add(args_len as usize).write(k_closure_ptr as u64);
            ns_args.add(args_len as usize + 1).write(k_fn_ptr as u64);
            return ns;
        }
        frame = (*frame).prev;
    }
    counters::add(CounterId::HandlerWalkDepthSum, depth);
    eprintln!(
        "sigil_perform: unhandled effect_id {effect_id} (op_id {op_id}); handler stack empty"
    );
    std::process::abort();
}

/// Drive the CPS trampoline starting from `initial_step`. Each
/// iteration:
///
/// 1. Reset the per-dispatch arena.
/// 2. Read the current step's discriminant.
/// 3. If `DONE`, return `value` to the caller.
/// 4. If `CALL`, copy the dispatch info into stack locals (the arena
///    reset on the next iteration would otherwise clobber it), invoke
///    the carried fn with the carried args, and continue with the
///    returned `NextStep`.
///
/// Every iteration increments `SIGIL_COUNTER_TRAMPOLINE_DISPATCH_COUNT`.
///
/// # Safety
///
/// `initial_step` must be a valid `*mut NextStep` produced by
/// `sigil_next_step_done` or `sigil_next_step_call`. The fns referenced
/// by any `CALL` step must satisfy the CPS calling convention.
#[no_mangle]
pub unsafe extern "C" fn sigil_run_loop(initial_step: *mut NextStep) -> u64 {
    let mut current = initial_step;
    loop {
        counters::incr(CounterId::TrampolineDispatchCount);

        if current.is_null() {
            eprintln!("sigil_run_loop: null NextStep pointer");
            std::process::abort();
        }

        let tag = (*current).tag;
        match tag {
            NEXT_STEP_TAG_DONE => {
                let v = (*current).value;
                // Reset the arena before returning so the next
                // top-level entry starts with a clean slate.
                crate::arena::sigil_arena_reset();
                return v;
            }
            NEXT_STEP_TAG_CALL => {
                // Copy dispatch info into stack locals before resetting
                // the arena. The args buffer is also in the arena, so
                // we copy those out too.
                let closure_ptr = (*current).closure_ptr;
                let fn_ptr = (*current).fn_ptr;
                let arg_count = (*current).arg_count;
                // Stack-allocated args buffer sized to the module-level
                // `MAX_INLINE_ARGS` const. `sigil_perform` and
                // `sigil_next_step_call` already pre-check this bound at
                // their respective entry points; the trampoline-side
                // check is defense-in-depth against a future call site
                // that constructs a NextStep without going through those
                // helpers.
                if arg_count > MAX_INLINE_ARGS {
                    eprintln!(
                        "sigil_run_loop: arg_count {arg_count} exceeds MAX_INLINE_ARGS \
                         ({MAX_INLINE_ARGS}) — bypassed perform/next_step_call bound check?"
                    );
                    std::process::abort();
                }
                let mut args_buf = [0u64; MAX_INLINE_ARGS as usize];
                if arg_count > 0 {
                    let src = sigil_next_step_args_ptr(current);
                    for (i, slot) in args_buf.iter_mut().enumerate().take(arg_count as usize) {
                        *slot = src.add(i).read();
                    }
                }
                // Reset the arena now that we've extracted the
                // dispatch info. Any in-arena pointer the caller might
                // have stashed elsewhere is invalidated by this reset
                // — that's the contract codegen relies on.
                crate::arena::sigil_arena_reset();

                // SAFETY: fn_ptr came from a NextStep::Call constructed
                // by `sigil_next_step_call` and thus reflects a CPS-color
                // fn pointer per the documented calling convention.
                let f: CpsFn = core::mem::transmute(fn_ptr);
                // args_buf is a stack local; the callee reads value-bytes from the pointer.
                // SAFETY: not an interior pointer (args_buf is a stack local, no GC retention).
                current = f(closure_ptr, args_buf.as_ptr(), arg_count);
            }
            _ => {
                eprintln!("sigil_run_loop: unknown NextStep tag {tag}");
                std::process::abort();
            }
        }
    }
}

// ---------------------------------------------------------------------
// HandlerFrame internal helpers
// ---------------------------------------------------------------------

#[inline]
fn handler_frame_payload_bytes(arm_count: usize) -> usize {
    // 32 bytes of fixed header + 16 bytes per arm.
    32 + 16 * arm_count
}

fn handler_frame_pointer_bitmap(arm_count: usize) -> u32 {
    // Word 0: (effect_id, arm_count) — not a pointer.
    // Word 1: return_fn — function pointer, NOT GC-tracked.
    // Word 2: return_closure — GC pointer.
    // Word 3: prev — GC pointer (to another HandlerFrame, also GC-allocated).
    // Words 4+: arms — even slots are fn_ptrs (skip), odd slots are closure_ptrs (track).
    let mut bitmap: u32 = 0;
    bitmap |= 1 << 2;
    bitmap |= 1 << 3;
    for i in 0..arm_count {
        let closure_word_idx = 5 + 2 * i;
        // The MAX_HANDLER_ARMS cap (14) keeps `closure_word_idx` ≤ 31:
        // i ranges over [0, arm_count) ⊆ [0, 14), so max idx = 5 + 2*13 = 31.
        debug_assert!(closure_word_idx < 32);
        bitmap |= 1u32 << closure_word_idx;
    }
    bitmap
}

/// Pointer to the start of the variable-length arms array on `frame`.
/// Each arm slot is two pointers: `(fn_ptr, closure_ptr)`.
///
/// # Safety
///
/// `frame` must be non-null and point to a properly-allocated
/// HandlerFrame.
#[inline]
unsafe fn arms_base_ptr(frame: *mut HandlerFrame) -> *mut *mut u8 {
    // SAFETY: not an interior pointer (the result is computed once per
    // accessor invocation against the documented variable-length
    // layout; not stored beyond the immediate slot read/write).
    (frame as *mut u8).add(core::mem::size_of::<HandlerFrame>()) as *mut *mut u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reset both the arena and the handler stack so tests start from a
    /// known state. Each test that touches GC/heap also holds the
    /// shared `gc_test_lock`.
    fn reset_state() {
        // SAFETY: tests hold gc_test_lock or otherwise serialise; no
        // live arena pointers outlive the call.
        unsafe {
            crate::arena::sigil_arena_reset();
        }
        HANDLER_STACK.with(|cell| cell.set(ptr::null_mut()));
    }

    fn ensure_gc() {
        crate::gc::sigil_gc_init();
    }

    // Test CPS-color fn: takes one arg, returns Done(arg + 1).
    unsafe extern "C" fn cps_done_plus_one(
        _closure: *mut u8,
        args_ptr: *const u64,
        args_len: u32,
    ) -> *mut NextStep {
        assert_eq!(args_len, 1);
        let v = *args_ptr;
        sigil_next_step_done(v + 1)
    }

    // Test CPS-color fn: takes one arg, returns Call(cps_done_plus_one, arg + 10).
    unsafe extern "C" fn cps_call_then_plus_one(
        _closure: *mut u8,
        args_ptr: *const u64,
        args_len: u32,
    ) -> *mut NextStep {
        assert_eq!(args_len, 1);
        let v = *args_ptr;
        let ns = sigil_next_step_call(ptr::null_mut(), cps_done_plus_one as *mut u8, 1);
        let args = sigil_next_step_args_ptr(ns);
        args.write(v + 10);
        ns
    }

    #[test]
    fn next_step_done_round_trips() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_done(42) };
        unsafe {
            assert_eq!((*ns).tag, NEXT_STEP_TAG_DONE);
            assert_eq!((*ns).value, 42);
            assert_eq!((*ns).arg_count, 0);
        }
        reset_state();
    }

    #[test]
    fn next_step_call_packs_args() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_call(0xDEAD as *mut u8, 0xBEEF as *mut u8, 3) };
        unsafe {
            assert_eq!((*ns).tag, NEXT_STEP_TAG_CALL);
            assert_eq!((*ns).arg_count, 3);
            assert_eq!((*ns).closure_ptr as usize, 0xDEAD);
            assert_eq!((*ns).fn_ptr as usize, 0xBEEF);
            let args = sigil_next_step_args_ptr(ns);
            args.write(11);
            args.add(1).write(22);
            args.add(2).write(33);
            assert_eq!(args.read(), 11);
            assert_eq!(args.add(1).read(), 22);
            assert_eq!(args.add(2).read(), 33);
        }
        reset_state();
    }

    #[test]
    fn next_step_args_ptr_is_null_for_done() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_done(7) };
        let args = unsafe { sigil_next_step_args_ptr(ns) };
        assert!(args.is_null());
        reset_state();
    }

    #[test]
    fn run_loop_done_returns_value_directly() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_done(99) };
        let dispatches_before = counters::read(CounterId::TrampolineDispatchCount);
        let v = unsafe { sigil_run_loop(ns) };
        let dispatches_after = counters::read(CounterId::TrampolineDispatchCount);
        assert_eq!(v, 99);
        assert_eq!(dispatches_after - dispatches_before, 1);
        reset_state();
    }

    #[test]
    fn run_loop_dispatches_call_then_done() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_call(ptr::null_mut(), cps_done_plus_one as *mut u8, 1) };
        let args = unsafe { sigil_next_step_args_ptr(ns) };
        unsafe { args.write(41) };
        let dispatches_before = counters::read(CounterId::TrampolineDispatchCount);
        let v = unsafe { sigil_run_loop(ns) };
        let dispatches_after = counters::read(CounterId::TrampolineDispatchCount);
        assert_eq!(v, 42);
        assert_eq!(dispatches_after - dispatches_before, 2);
        reset_state();
    }

    #[test]
    fn run_loop_chains_multiple_call_steps() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns =
            unsafe { sigil_next_step_call(ptr::null_mut(), cps_call_then_plus_one as *mut u8, 1) };
        let args = unsafe { sigil_next_step_args_ptr(ns) };
        unsafe { args.write(5) };
        let dispatches_before = counters::read(CounterId::TrampolineDispatchCount);
        let v = unsafe { sigil_run_loop(ns) };
        let dispatches_after = counters::read(CounterId::TrampolineDispatchCount);
        // 5 -> Call(cps_call_then_plus_one, 5)
        // -> Call(cps_done_plus_one, 5+10=15)
        // -> Done(15+1=16)
        assert_eq!(v, 16);
        assert_eq!(dispatches_after - dispatches_before, 3);
        reset_state();
    }

    #[test]
    fn continuation_identity_returns_done_with_args_ptr_value() {
        // Plan B Task 55 (Phase 4d) — direct invariant check on the
        // identity continuation. Calling it with a single u64 in the
        // args buffer must produce a `NextStep::Done(value)` from
        // the arena. This is the unit invariant codegen's tail-`k`
        // lowering depends on; the round-trip-through-run_loop test
        // below exercises the integration path.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let known: u64 = 0xFEEDFACE_DEADBEEF;
        let args: [u64; 1] = [known];
        // SAFETY: not an interior pointer (stack array, non-GC, outlives the call).
        let ns = unsafe { sigil_continuation_identity(ptr::null(), args.as_ptr(), 1) };
        unsafe {
            assert_eq!((*ns).tag, NEXT_STEP_TAG_DONE);
            assert_eq!((*ns).value, known);
            assert_eq!((*ns).arg_count, 0);
            assert!((*ns).closure_ptr.is_null());
            assert!((*ns).fn_ptr.is_null());
        }
        reset_state();
    }

    #[test]
    fn continuation_identity_round_trips_through_run_loop() {
        // Plan B Task 55 (Phase 4d) — integration check matching the
        // shape codegen's tail-`k` lowering produces:
        //   NextStep::Call(closure_ptr=null, fn=identity, args=[42])
        //     → run_loop dispatches identity → Done(42)
        //     → run_loop returns 42 to native caller.
        // This is the exact path the synth-pass arm-fn body traces
        // when it lowers `k(42)` in tail position. A regression here
        // would surface as a wrong perform-site value at the
        // surrounding fn's `lower_perform_non_io_to_value` site.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe {
            sigil_next_step_call(ptr::null_mut(), sigil_continuation_identity as *mut u8, 1)
        };
        let args = unsafe { sigil_next_step_args_ptr(ns) };
        unsafe { args.write(42) };
        let dispatches_before = counters::read(CounterId::TrampolineDispatchCount);
        let v = unsafe { sigil_run_loop(ns) };
        let dispatches_after = counters::read(CounterId::TrampolineDispatchCount);
        assert_eq!(v, 42);
        // 2 dispatches: one for the Call (loop dispatches identity,
        // which returns Done), one more iteration to observe the
        // Done tag and return — the counter increments at the top
        // of every loop iteration including the terminal Done check.
        // Matches `run_loop_dispatches_call_then_done` above.
        assert_eq!(dispatches_after - dispatches_before, 2);
        reset_state();
    }

    #[test]
    fn continuation_identity_tolerates_args_len_3_trailing_pair_convention() {
        // Plan B Task 55, Phase 4e captures+ Slice A — trailing-pair
        // convention. Tail-`k` arm-fn emissions now uniformly pack
        // `[arg, post_arm_k_closure, post_arm_k_fn]` at `args_len=3`.
        // When the arm dispatches into identity directly (perform in
        // tail position of the handle body, no helper synth-cont in
        // scope), identity sees args_len=3 — the trailing pair is
        // irrelevant since identity is the terminal continuation.
        //
        // Identity must read only `args_ptr[0]` and produce
        // `NextStep::Done(args_ptr[0])`, ignoring the trailing slots.
        //
        // Bisecting hint: a regression in the existing
        // `arm_uses_k_in_tail_position_returns_continuation_value`
        // e2e test after Slice A would surface as identity's
        // arity-1 invariant firing here. This unit test pins the
        // contract directly so the failure attribution is unambiguous.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let known: u64 = 0xFEEDFACE_DEADBEEF;
        // [arg, post_arm_k_closure (null), post_arm_k_fn (irrelevant)]
        let args: [u64; 3] = [known, 0xCAFE, 0xBABE];
        // SAFETY: not an interior pointer (stack array, non-GC, outlives the call).
        let ns = unsafe { sigil_continuation_identity(ptr::null(), args.as_ptr(), 3) };
        unsafe {
            assert_eq!((*ns).tag, NEXT_STEP_TAG_DONE);
            assert_eq!((*ns).value, known);
            assert_eq!((*ns).arg_count, 0);
            assert!((*ns).closure_ptr.is_null());
            assert!((*ns).fn_ptr.is_null());
        }
        reset_state();
    }

    #[test]
    fn handler_frame_new_initialises_zero_arms_and_pointers() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe { sigil_handler_frame_new(7, 0) };
        unsafe {
            assert_eq!((*frame).effect_id, 7);
            assert_eq!((*frame).arm_count, 0);
            assert!((*frame).return_fn.is_null());
            assert!((*frame).return_closure.is_null());
            assert!((*frame).prev.is_null());
        }
        reset_state();
    }

    #[test]
    fn handler_frame_new_with_arms_zero_initialises_arm_slots() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe { sigil_handler_frame_new(11, 3) };
        unsafe {
            assert_eq!((*frame).arm_count, 3);
            let arms = arms_base_ptr(frame);
            for i in 0..3 {
                let slot = arms.add(i * 2) as *const *mut u8;
                assert!(slot.read().is_null());
                assert!(slot.add(1).read().is_null());
            }
        }
        reset_state();
    }

    #[test]
    fn set_arm_writes_at_the_documented_offset() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe { sigil_handler_frame_new(3, 2) };
        unsafe {
            sigil_handler_frame_set_arm(frame, 0, 0xAA00 as *mut u8, 0xAA01 as *mut u8);
            sigil_handler_frame_set_arm(frame, 1, 0xBB00 as *mut u8, 0xBB01 as *mut u8);
            let arms = arms_base_ptr(frame);
            assert_eq!((arms.add(0).read() as usize), 0xAA00);
            assert_eq!((arms.add(1).read() as usize), 0xAA01);
            assert_eq!((arms.add(2).read() as usize), 0xBB00);
            assert_eq!((arms.add(3).read() as usize), 0xBB01);
        }
        reset_state();
    }

    #[test]
    fn set_return_writes_return_arm_slots() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe { sigil_handler_frame_new(0, 0) };
        unsafe {
            sigil_handler_frame_set_return(frame, 0xFEED as *mut u8, 0xFACE as *mut u8);
            assert_eq!((*frame).return_fn as usize, 0xFEED);
            assert_eq!((*frame).return_closure as usize, 0xFACE);
        }
        reset_state();
    }

    #[test]
    fn push_pop_round_trip_with_prev_chain() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let outer = unsafe { sigil_handler_frame_new(1, 0) };
        let inner = unsafe { sigil_handler_frame_new(2, 0) };
        assert!(handler_stack_head().is_null());
        unsafe { sigil_handle_push(outer) };
        assert_eq!(handler_stack_head(), outer);
        unsafe { sigil_handle_push(inner) };
        assert_eq!(handler_stack_head(), inner);
        unsafe {
            assert_eq!((*inner).prev, outer);
        }
        unsafe {
            assert!((*outer).prev.is_null());
        }
        let popped_inner = unsafe { sigil_handle_pop() };
        assert_eq!(popped_inner, inner);
        assert_eq!(handler_stack_head(), outer);
        let popped_outer = unsafe { sigil_handle_pop() };
        assert_eq!(popped_outer, outer);
        assert!(handler_stack_head().is_null());
        reset_state();
    }

    // Test arm: takes (raised_value: u64, k_closure: u64, k_fn: u64),
    // returns Done(raised_value * 100). Effectively "catch and replace
    // the result with raised * 100", ignoring the continuation.
    unsafe extern "C" fn arm_done_times_100(
        _closure: *mut u8,
        args_ptr: *const u64,
        args_len: u32,
    ) -> *mut NextStep {
        assert_eq!(args_len, 3); // raised_value, k_closure, k_fn
        let raised = *args_ptr;
        sigil_next_step_done(raised * 100)
    }

    #[test]
    fn perform_dispatches_to_matching_arm() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe {
            sigil_handler_frame_new(/* effect_id */ 5, /* arm_count */ 1)
        };
        unsafe {
            sigil_handler_frame_set_arm(
                frame,
                /* op_id */ 0,
                arm_done_times_100 as *mut u8,
                ptr::null_mut(),
            );
            sigil_handle_push(frame);
            let walk_count_before = counters::read(CounterId::HandlerWalkCount);
            let depth_sum_before = counters::read(CounterId::HandlerWalkDepthSum);
            let arg = 7u64;
            let ns = sigil_perform(
                /* effect_id */ 5,
                /* op_id */ 0,
                &arg as *const u64,
                /* args_len */ 1,
                /* k_closure_ptr */ 0xDEAD as *mut u8,
                /* k_fn_ptr */ 0xBEEF as *mut u8,
            );
            let walk_count_after = counters::read(CounterId::HandlerWalkCount);
            let depth_sum_after = counters::read(CounterId::HandlerWalkDepthSum);
            assert_eq!(walk_count_after - walk_count_before, 1);
            assert_eq!(depth_sum_after - depth_sum_before, 1);

            // Dispatch the resulting NextStep through the trampoline.
            let result = sigil_run_loop(ns);
            assert_eq!(result, 700);

            // Pop the frame to leave a clean handler stack.
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    #[test]
    fn perform_walks_past_unrelated_outer_frame() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let outer = unsafe { sigil_handler_frame_new(99, 0) };
        let target = unsafe { sigil_handler_frame_new(5, 1) };
        unsafe {
            sigil_handler_frame_set_arm(target, 0, arm_done_times_100 as *mut u8, ptr::null_mut());
            sigil_handle_push(target);
            sigil_handle_push(outer);
            let depth_before = counters::read(CounterId::HandlerWalkDepthSum);
            let arg = 3u64;
            let ns = sigil_perform(
                5,
                0,
                &arg as *const u64,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let depth_after = counters::read(CounterId::HandlerWalkDepthSum);
            // Outer is on top, target is one below; walk depth = 2.
            assert_eq!(depth_after - depth_before, 2);
            let result = sigil_run_loop(ns);
            assert_eq!(result, 300);
            let _ = sigil_handle_pop();
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    #[test]
    fn perform_packs_continuation_after_user_args() {
        // Validate the args layout: user args first, then k_closure, k_fn.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();

        // A custom arm that asserts the exact layout.
        unsafe extern "C" fn arm_layout_check(
            _closure: *mut u8,
            args_ptr: *const u64,
            args_len: u32,
        ) -> *mut NextStep {
            assert_eq!(args_len, 4); // 2 user args + k_closure + k_fn
                                     // SAFETY: not an interior pointer (args_ptr points at a
                                     // non-GC arena buffer; reads are value loads, no GC retention).
            assert_eq!(*args_ptr, 100);
            // SAFETY: not an interior pointer (same as above).
            assert_eq!(*args_ptr.add(1), 200);
            // SAFETY: not an interior pointer (same as above).
            assert_eq!(*args_ptr.add(2) as usize, 0xCC);
            // SAFETY: not an interior pointer (same as above).
            assert_eq!(*args_ptr.add(3) as usize, 0xDD);
            sigil_next_step_done(0)
        }

        let frame = unsafe { sigil_handler_frame_new(7, 1) };
        unsafe {
            sigil_handler_frame_set_arm(frame, 0, arm_layout_check as *mut u8, ptr::null_mut());
            sigil_handle_push(frame);
            let user_args = [100u64, 200u64];
            // user_args is a stack local; the runtime copies bytes via the pointer.
            // SAFETY: not an interior pointer (user_args is a stack local).
            let user_args_ptr = user_args.as_ptr();
            let ns = sigil_perform(7, 0, user_args_ptr, 2, 0xCC as *mut u8, 0xDD as *mut u8);
            let _ = sigil_run_loop(ns);
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    #[test]
    fn handler_frame_pointer_bitmap_marks_correct_words() {
        // 0 arms → just return_closure (bit 2) and prev (bit 3).
        assert_eq!(handler_frame_pointer_bitmap(0), 0b0000_1100);
        // 1 arm → adds bit 5 (arm 0's closure_ptr at payload word 5).
        assert_eq!(handler_frame_pointer_bitmap(1), 0b0010_1100);
        // 3 arms → adds bits 5, 7, 9.
        assert_eq!(handler_frame_pointer_bitmap(3), 0b10_1010_1100);
        // Max (13) arms → bits 5, 7, 9, ..., 31.
        let max = handler_frame_pointer_bitmap(MAX_HANDLER_ARMS as usize);
        // bits 2, 3 plus odd bits from 5 through 31.
        assert!(max & (1 << 2) != 0);
        assert!(max & (1 << 3) != 0);
        for i in 0..(MAX_HANDLER_ARMS as usize) {
            let bit = 5 + 2 * i;
            assert!(max & (1u32 << bit) != 0, "missing bit {bit} for arm {i}");
        }
    }

    #[test]
    fn handler_frame_payload_bytes_matches_layout() {
        // 32 fixed + 16 per arm.
        assert_eq!(handler_frame_payload_bytes(0), 32);
        assert_eq!(handler_frame_payload_bytes(1), 48);
        assert_eq!(
            handler_frame_payload_bytes(MAX_HANDLER_ARMS as usize),
            32 + 16 * MAX_HANDLER_ARMS as usize
        );
    }

    // ----------------------------------------------------------------
    // 3+ deep prev chain (review item #10)
    // ----------------------------------------------------------------

    #[test]
    fn perform_walks_three_deep_prev_chain_to_match() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let target = unsafe {
            sigil_handler_frame_new(/* effect_id */ 100, 1)
        };
        let middle = unsafe {
            sigil_handler_frame_new(/* effect_id */ 200, 0)
        };
        let outer = unsafe {
            sigil_handler_frame_new(/* effect_id */ 300, 0)
        };
        unsafe {
            sigil_handler_frame_set_arm(target, 0, arm_done_times_100 as *mut u8, ptr::null_mut());
            sigil_handle_push(target);
            sigil_handle_push(middle);
            sigil_handle_push(outer);
            // Stack now (top → bottom): outer (300), middle (200), target (100).
            // perform(100, ...) walks past outer and middle to reach target.
            let depth_before = counters::read(CounterId::HandlerWalkDepthSum);
            let arg = 4u64;
            let user_args_ptr = &arg as *const u64;
            let ns = sigil_perform(100, 0, user_args_ptr, 1, ptr::null_mut(), ptr::null_mut());
            let depth_after = counters::read(CounterId::HandlerWalkDepthSum);
            assert_eq!(
                depth_after - depth_before,
                3,
                "expected walk depth 3 (outer + middle + target)"
            );
            let result = sigil_run_loop(ns);
            assert_eq!(result, 400);
            let _ = sigil_handle_pop();
            let _ = sigil_handle_pop();
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    // ----------------------------------------------------------------
    // Boundary-arity (review M4): MAX_HANDLER_ARMS allocation +
    // dispatch end-to-end. The pure-fn bitmap test only exercises
    // the bitmap helper; this test exercises the alloc + push +
    // perform path against the cap.
    // ----------------------------------------------------------------

    #[test]
    fn handler_frame_dispatch_at_max_arm_count() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let frame = unsafe { sigil_handler_frame_new(7, MAX_HANDLER_ARMS) };
        unsafe {
            // Set every arm to the same test fn; only arm 0 is
            // actually invoked, but the alloc must successfully size
            // for all MAX_HANDLER_ARMS slots and zero-init them.
            for op in 0..MAX_HANDLER_ARMS {
                sigil_handler_frame_set_arm(
                    frame,
                    op,
                    arm_done_times_100 as *mut u8,
                    ptr::null_mut(),
                );
            }
            sigil_handle_push(frame);
            // Dispatch the LAST arm (op = MAX_HANDLER_ARMS - 1) so the
            // arm-index arithmetic and GC pointer-bitmap span the full
            // arms region.
            let arg = 11u64;
            let arg_ptr = &arg as *const u64;
            let ns = sigil_perform(
                7,
                MAX_HANDLER_ARMS - 1,
                arg_ptr,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let result = sigil_run_loop(ns);
            assert_eq!(result, 1100);
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    // Note: the `arm_count > MAX_HANDLER_ARMS` abort path cannot be
    // tested directly from cargo test (unwinding across abort is
    // undefined and a child-process driver is heavier than this PR
    // warrants). The abort message is exercised manually; the
    // `arm_count > MAX_HANDLER_ARMS` branch has a dedicated test of
    // the `handler_frame_pointer_bitmap` helper proving the cap is
    // self-consistent (`handler_frame_pointer_bitmap_marks_correct_words`).

    // ----------------------------------------------------------------
    // GC stress (review M2): verify the rooting fixes hold under
    // forced collection. These tests load-bear the
    // `register_handler_stack_root_for_calling_thread` and
    // `register_arena_root_for_calling_thread` fixes; without them,
    // the affected reads after GC would see freed memory.
    //
    // Each test re-execs the test binary in a subprocess so the
    // stress scenario runs against a fresh Boehm state. Sidesteps
    // the original "Boehm thread enrolment composes poorly with
    // cargo test's per-test thread teardowns" issue: only ONE test
    // runs in the subprocess, drops its `GcThreadEnrolment`, and
    // the process exits. The OS reclaims everything; no stale
    // ranges leak into Boehm's root list across tests.
    //
    // Outer mode (no env var): spawn the subprocess, wait for it,
    // assert success. Inner mode (env var set): run the actual body.
    // ----------------------------------------------------------------

    /// Env-var marker that switches a stress test into "inner" mode
    /// (run the actual body) instead of "outer" mode (spawn a child
    /// subprocess that runs only this one test).
    const STRESS_INNER_VAR: &str = "SIGIL_GC_STRESS_INNER";

    fn in_stress_subprocess() -> bool {
        std::env::var(STRESS_INNER_VAR).is_ok()
    }

    /// Outer-mode helper: re-exec this test binary, filtered to
    /// `handlers::tests::<test_name>` with `--exact`, with the
    /// inner-mode env var set. Asserts the child exited zero. Errors
    /// surface via the project's eprintln+abort convention; the test
    /// process aborts (visible as failure to the harness) rather than
    /// panicking, matching the rest of the runtime crate's error style
    /// (clippy disallows `unwrap`/`expect`/`panic!`).
    fn run_stress_in_subprocess(test_name: &str) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("run_stress_in_subprocess: current_exe failed: {e}");
                std::process::abort();
            }
        };
        let full_name = format!("handlers::tests::{test_name}");
        let status = match std::process::Command::new(&exe)
            .args(["--exact", &full_name, "--nocapture"])
            .env(STRESS_INNER_VAR, "1")
            .status()
        {
            Ok(s) => s,
            Err(e) => {
                eprintln!("run_stress_in_subprocess: spawn for `{full_name}` failed: {e}");
                std::process::abort();
            }
        };
        assert!(
            status.success(),
            "GC-stress subprocess for `{full_name}` failed: {status}"
        );
    }

    #[test]
    fn handler_frame_survives_forced_gc_while_pushed() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("handler_frame_survives_forced_gc_while_pushed");
            return;
        }
        // Frame is reachable through HANDLER_STACK only after we drop
        // the local. Without the HANDLER_STACK root registration,
        // GC_gcollect would reclaim it; perform would then walk into
        // freed memory or hit an unhandled-effect abort.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_state();
        unsafe {
            let frame = sigil_handler_frame_new(4242, 1);
            sigil_handler_frame_set_arm(frame, 0, arm_done_times_100 as *mut u8, ptr::null_mut());
            sigil_handle_push(frame);
            // Allocate-spam to overwrite the test thread's stack slot
            // that may still hold the `frame` local.
            for _ in 0..32 {
                let h = crate::header::Header::new(crate::header::TAG_INT64, 1, 0);
                let _ = crate::gc::sigil_alloc(h.raw(), 8);
            }
            // Force a full collection.
            crate::gc::GC_gcollect();
            // perform succeeds iff the frame is still reachable.
            let arg = 9u64;
            let arg_ptr = &arg as *const u64;
            let ns = sigil_perform(4242, 0, arg_ptr, 1, ptr::null_mut(), ptr::null_mut());
            let result = sigil_run_loop(ns);
            assert_eq!(result, 900);
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    /// Sentinel bytes a test "closure" carries in its payload word.
    const STRESS_CLOSURE_SENTINEL: u64 = 0x5A5A_F00D_BEEF_1234;

    // Test arm: reads sentinel from its own closure pointer (arg 0
    // to the arm) and returns it via Done. Validates that the
    // closure pointer the arm receives still dereferences correctly
    // post-GC.
    unsafe extern "C" fn arm_read_closure_sentinel(
        closure: *mut u8,
        _args_ptr: *const u64,
        _args_len: u32,
    ) -> *mut NextStep {
        // Read the sentinel word at offset 8 (past the Sigil object header).
        let payload = closure.add(8) as *const u64;
        let v = payload.read();
        sigil_next_step_done(v)
    }

    #[test]
    fn closure_in_handler_arm_slot_survives_gc() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("closure_in_handler_arm_slot_survives_gc");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_state();
        unsafe {
            // Allocate the "closure" (1 payload word holding the sentinel).
            let h = crate::header::Header::new(crate::header::TAG_INT64, 1, 0);
            let closure = crate::gc::sigil_alloc(h.raw(), 8);
            let payload = closure.add(8) as *mut u64;
            payload.write(STRESS_CLOSURE_SENTINEL);

            // Allocate handler frame and stash the closure in arm 0's
            // closure_ptr slot. The frame's GC pointer bitmap marks
            // bit 5 (arm 0's closure_ptr at payload word 5), so Boehm
            // walks from the frame to the closure during mark phase.
            let frame = sigil_handler_frame_new(7777, 1);
            sigil_handler_frame_set_arm(frame, 0, arm_read_closure_sentinel as *mut u8, closure);
            sigil_handle_push(frame);
            // Allocate-spam to overwrite stack-side aliases of `closure`.
            for _ in 0..32 {
                let h = crate::header::Header::new(crate::header::TAG_INT64, 1, 0);
                let _ = crate::gc::sigil_alloc(h.raw(), 8);
            }
            crate::gc::GC_gcollect();

            // Dispatch through the arm. The trampoline invokes
            // arm_read_closure_sentinel with closure_ptr = the original
            // closure; it reads the sentinel and returns it.
            let ns = sigil_perform(7777, 0, ptr::null(), 0, ptr::null_mut(), ptr::null_mut());
            let result = sigil_run_loop(ns);
            assert_eq!(result, STRESS_CLOSURE_SENTINEL);
            let _ = sigil_handle_pop();
        }
        reset_state();
    }

    // CPS fn: allocates a closure, builds NextStep::Call to the
    // verifier carrying the closure as closure_ptr, forces GC just
    // before returning. Validates that the closure pointer stored in
    // the arena's NextStep::Call survives the collection — the
    // arena's storage range is registered as a Boehm root, so the
    // conservative scan finds the closure pointer in an arena slot.
    unsafe extern "C" fn cps_alloc_then_gc(
        _closure: *mut u8,
        _args_ptr: *const u64,
        _args_len: u32,
    ) -> *mut NextStep {
        let h = crate::header::Header::new(crate::header::TAG_INT64, 1, 0);
        let target_closure = crate::gc::sigil_alloc(h.raw(), 8);
        let payload = target_closure.add(8) as *mut u64;
        payload.write(STRESS_CLOSURE_SENTINEL);
        let ns = sigil_next_step_call(target_closure, arm_read_closure_sentinel as *mut u8, 0);
        // Force GC before returning. With the arena registered as a
        // Boehm root, the closure pointer we just stored in the arena
        // (via sigil_next_step_call → ns.closure_ptr field) keeps the
        // closure alive across the collection.
        crate::gc::GC_gcollect();
        ns
    }

    #[test]
    fn closure_in_next_step_survives_gc_via_arena_root() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("closure_in_next_step_survives_gc_via_arena_root");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_state();
        unsafe {
            // Initial NextStep::Call drives cps_alloc_then_gc, which
            // returns a NextStep::Call to arm_read_closure_sentinel.
            // The trampoline reads the closure_ptr into a stack local,
            // resets the arena, and dispatches; if the closure was
            // collected during cps_alloc_then_gc's GC, the verifier
            // would read freed bytes (could be anything; sentinel
            // mismatch would fire the assert).
            let initial = sigil_next_step_call(ptr::null_mut(), cps_alloc_then_gc as *mut u8, 0);
            let result = sigil_run_loop(initial);
            assert_eq!(result, STRESS_CLOSURE_SENTINEL);
        }
        reset_state();
    }
}
