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
//! the previous frame's payload. Plan E2 Task 8 switched `sigil_alloc`
//! to allocate HandlerFrames via `GC_malloc_explicitly_typed` with a
//! descriptor built from the Header's pointer bitmap, so Boehm's mark
//! phase scans only the payload words the bitmap names as GC pointers
//! (`prev`, `return_closure`, every `arms[i].closure_ptr`). Atomic
//! payloads — no GC pointers in any payload word — continue to use
//! `GC_malloc_atomic` (Boehm skips scanning entirely).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr;

use sigil_abi::effect::{NEXT_STEP_TAG_CALL, NEXT_STEP_TAG_DISCHARGED, NEXT_STEP_TAG_DONE};

use crate::counters::{self, CounterId};
use crate::header::{Header, TAG_CLOSURE};

/// CPS-color calling convention (see module-level docs). Plan D Task
/// 111c added the trailing `terminal_out: *mut TerminalResult` arg so
/// handle-exit terminal writes from inside Cps callees propagate up
/// the call chain via the caller-owned channel (replacing the TLS
/// path that 111d removes).
type CpsFn = unsafe extern "C" fn(
    closure_ptr: *mut u8,
    args_ptr: *const u64,
    args_len: u32,
    terminal_out: *mut TerminalResult,
) -> *mut NextStep;

/// Maximum op-arms a single handler frame can carry. Re-exported from
/// `sigil_abi::effect` (Plan B Task 55 Phase 4f polish round) so codegen
/// and runtime read from the same source. Codegen-walker rejects
/// per-effect arm counts above this at compile time
/// (`compiler/src/codegen.rs`'s `unsupported_handle_construct`); the
/// runtime's `sigil_handler_frame_new` retains its abort-on-overflow
/// check as defense-in-depth. Bounded by the 32-bit GC pointer-bitmap
/// on `HandlerFrame` (see `sigil_abi::effect::MAX_HANDLER_ARMS` for the
/// derivation).
pub use sigil_abi::effect::MAX_HANDLER_ARMS;

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
    /// Plotkin fix — `arm_count` lives in low 16 bits. The high
    /// bit (bit 31) encodes the effect's `resumes: many` flag so
    /// `wrap_continuation_with_outer_post_arm_k` can determine
    /// whether the crossed frame's effect is multi-shot at perform
    /// time. Cap-aware: arm_count caps at MAX_HANDLER_ARMS = 14
    /// (well under 16-bit range), so the high bit is free for the
    /// resumes_many flag without ABI churn. Codegen masks the
    /// flag in via the new
    /// `sigil_handler_frame_new_with_resumes_many` entry; legacy
    /// `sigil_handler_frame_new` keeps the flag clear (single-shot
    /// default).
    pub arm_count: u32,
    pub return_fn: *mut u8,
    pub return_closure: *mut u8,
    pub prev: *mut HandlerFrame,
    // arms follow: [(fn_ptr: *mut u8, closure_ptr: *mut u8); arm_count]
}

/// Mask for the arm_count's low bits (excluding the resumes_many flag
/// at bit 31).
const ARM_COUNT_MASK: u32 = 0x7FFF_FFFF;
const RESUMES_MANY_BIT: u32 = 1u32 << 31;

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

    /// Plan D Task 117 — push/pop relink discipline tracker.
    ///
    /// Each entry is `(frame_ptr, did_link)`: the frame whose
    /// push/pop pair this entry tracks, plus whether the push
    /// actually linked the frame (`true`) or skipped (`false`; the
    /// frame was already on top of the handler stack — the
    /// "skip-if-on-top" case introduced by Task 117's let-bound k
    /// dispatch path). The matching `sigil_handle_pop` reads the
    /// most recent entry, debug-asserts `entry.0 == HANDLER_STACK
    /// head` (defense-in-depth against codegen bugs that desync
    /// push/pop pairs across frames), then no-ops the unlink when
    /// `did_link == false`.
    ///
    /// Why frame-keyed (Plan D Task 117 review feedback): a bool-
    /// only counter caught COUNT-balanced unbalanced pairs (push X
    /// 2× / pop X 2× across two different frames) silently. Frame-
    /// keyed entries fail the debug_assert at the actual desync
    /// site rather than three handler levels later.
    ///
    /// Why a per-thread stack: lower_k_pair_call's push/pop pair
    /// is re-entrant — `run_loop` between push and pop drives
    /// arbitrary handler-stack activity (nested handles in the
    /// body's continuation), each of which push/pops RELINK_STACK
    /// in their own scope. By the time lower_k_pair_call's pop
    /// runs, intermediate entries have been balanced and the top
    /// of the stack is the matching push's entry.
    ///
    /// Initial state: empty. Pop on empty hard-`panic!`s in both
    /// debug and release builds — see the comment at
    /// `sigil_handle_pop`'s `unwrap_or_else` callsite for why this
    /// is the right tradeoff.
    ///
    /// **GC reasoning**: The Vec buffer holds raw `*mut
    /// HandlerFrame` pointers. The buffer itself is on Rust's
    /// global allocator (not Boehm's heap), so its contents are
    /// NOT conservatively scanned — frames are kept alive only via
    /// HANDLER_STACK's registered GC root (see
    /// `register_handler_stack_root_for_calling_thread`). This is
    /// sound by invariant: every frame in RELINK_STACK is also
    /// reachable from HANDLER_STACK because push/pop pairs balance
    /// in lockstep across both stacks. A balance violation would
    /// trip the `unwrap_or_else` panic or the debug_assert at the
    /// pop site before the GC could observe the desync.
    static RELINK_STACK: RefCell<Vec<(*mut HandlerFrame, bool)>> =
        const { RefCell::new(Vec::new()) };
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
        // SAFETY: gc-heap-ptr arithmetic (the result feeds an FFI
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
// Plan B' Stage 6.7 multi-shot composition fix — outer post_arm_k stack
//
// Background: when a multi-shot arm's `k(arg)` call dispatches into a
// multi-perform helper (B.2 chained-let-yield helper body), the
// helper's Middle step issues `sigil_perform` for the next perform.
// The outer arm's `post_arm_k` pair is in helper Middle's `args_ptr
// [1..3]` but Middle dispatches only via sigil_perform, dropping
// post_arm_k. The inner chain runs to `Done`; the trampoline observes
// Done and returns to the wrapper, dropping the outer arm's chain.
//
// Fix: helper Middle pushes its incoming post_arm_k pair onto a
// thread-local stack BEFORE issuing sigil_perform; the trampoline's
// Done branch checks the stack and routes Done's value through the
// popped post_arm_k chain instead of returning to the wrapper.
//
// Stack discipline: 1 push per outer-arm-k-into-helper-Middle, 1 pop
// per inner Done. Push/pop balance is maintained as long as helper
// Middle and the trampoline are correctly paired (no abort or skip
// in between). Stack depth is bounded by the deepest nested
// helper-perform-count; v1 uses a fixed-size TLS array (32 entries)
// — overflow aborts cleanly.
//
// GC rooting: the stack array sits in TLS; entries' `closure_ptr`
// fields point at heap-allocated closure records (the post_arm_k
// closure of the calling arm) and must be reachable from the GC root
// set across `sigil_perform` boundaries. Boehm scans the registered
// TLS range; the explicit `GC_add_roots` call in
// `register_outer_post_arm_k_stack_root_for_calling_thread` covers
// the array so closure pointers parked here aren't reclaimed
// mid-dispatch.
//
// `fn_ptr` fields point into the text segment (compiled code); Boehm
// treats them as non-pointer-like or as pointers to non-heap regions,
// either way they're benign.

// Plan State-Cell — bumped from 32 to 256. Cell-based State arms
// resume `k(arg)` (rather than discharging with a state-fn closure
// like Plotkin's encoding), so State's run_loop stays alive across
// State.get/set operations. Each chain Middle step pushes one
// OUTER_POST_ARM_K entry on perform; in deeply-recursive State-using
// fns (e.g., a recursive-descent JSON parser doing `let pos =
// perform State.get(); ... let _ = perform State.set(pos+N); ...`
// inside each recursive frame), pushes accumulate linearly with
// recursion depth and the prior cap of 32 overflowed on real
// programs. The Plotkin encoding worked under cap=32 because each
// State op terminated State's run_loop, draining the entries
// pushed during that loop's lifetime. Cell-based encoding can't
// terminate the run_loop without re-introducing the Sync-ABI gap
// the cell encoding fixes (foreign-discharge propagation through
// fn returns), so the resolution is the larger cap. v2 may revisit
// with a growable VecDeque or a chunked overflow region.
const OUTER_POST_ARM_K_STACK_SIZE: usize = 256;

#[repr(C)]
#[derive(Copy, Clone)]
struct OuterPostArmKEntry {
    closure_ptr: *mut u8,
    fn_ptr: *mut u8,
}

thread_local! {
    static OUTER_POST_ARM_K_STACK: RefCell<Vec<OuterPostArmKEntry>> = const {
        RefCell::new(Vec::new())
    };
    static OUTER_POST_ARM_K_DEPTH: Cell<usize> = const { Cell::new(0) };
    static OUTER_POST_ARM_K_STACK_ROOTED: Cell<bool> = const { Cell::new(false) };

    /// Last `OUTER_POST_ARM_K_STACK` extent registered with the precise
    /// GC for this thread, as `(start_addr, end_addr)`. `(0, 0)` means
    /// "nothing rooted yet". `rereg_outer_post_arm_k_root` reads this to
    /// no-op when the backing buffer pointer is unchanged, and writes it
    /// after every (un)register so the next re-root knows the live extent.
    static OUTER_POST_ARM_K_LAST_ROOTED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };

    /// Current `sigil_run_loop`'s `outer_post_arm_k_entry_depth`
    /// snapshot. `wrap_continuation_with_outer_post_arm_k` reads
    /// this to determine how many `OUTER_POST_ARM_K` entries the
    /// current run_loop owns — only those may be popped into the
    /// wrapper chain. Entries below this depth belong to enclosing
    /// run_loops and must not be moved (doing so would silently
    /// underflow the parent's `outer_post_arm_k_entry_depth`
    /// invariant and dereference garbage on the next pop).
    ///
    /// Single `Cell<usize>` rather than a TLS stack: nested
    /// run_loops save the prior value into the run_loop's local
    /// Rust stack frame at entry and restore it at every return
    /// path. The save/restore is naturally bounded by C-stack
    /// depth (no separate stack-overflow surface), and the read
    /// path from `wrap_continuation_with_outer_post_arm_k` cannot
    /// panic on a `RefCell` borrow.
    static RUN_LOOP_ENTRY_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Internal API for the outer post-arm-k continuation stack.
/// Encapsulates all direct access to OUTER_POST_ARM_K_STACK and
/// OUTER_POST_ARM_K_DEPTH thread-local state.
mod outer_post_arm_k_stack_api {
    use super::{
        trace_opak, OuterPostArmKEntry, OUTER_POST_ARM_K_DEPTH, OUTER_POST_ARM_K_LAST_ROOTED,
        OUTER_POST_ARM_K_STACK, OUTER_POST_ARM_K_STACK_SIZE,
    };
    use std::ffi::c_void;

    /// Push an entry onto the stack. Supports unbounded growth with automatic
    /// re-rooting when the backing buffer reallocates (for precise GC).
    pub(super) fn push(closure_ptr: *mut u8, fn_ptr: *mut u8) -> bool {
        OUTER_POST_ARM_K_DEPTH.with(|depth_cell| {
            let depth = depth_cell.get();

            // Capture reallocation info to call re-root after releasing the borrow
            let reroot_info: Option<(*mut c_void, *mut c_void, *mut c_void, *mut c_void)> =
                OUTER_POST_ARM_K_STACK.with(|stack_cell| {
                    let mut stack = stack_cell.borrow_mut();

                    // Capture old buffer state for reallocation detection
                    // SAFETY: gc-heap-ptr arithmetic (Vec data ptr; capacity*sizeof(Entry) bounded; thread lifetime).
                    let old_ptr = stack.as_ptr();
                    let old_capacity = stack.capacity();

                    // Ensure we have a buffer allocated
                    if stack.capacity() == 0 {
                        stack.reserve(OUTER_POST_ARM_K_STACK_SIZE);
                    }

                    // Resize to accommodate the entry
                    if depth >= stack.len() {
                        stack.resize(
                            depth + 1,
                            OuterPostArmKEntry {
                                closure_ptr: std::ptr::null_mut(),
                                fn_ptr: std::ptr::null_mut(),
                            },
                        );
                    }

                    // Write the entry
                    stack[depth] = OuterPostArmKEntry {
                        closure_ptr,
                        fn_ptr,
                    };

                    // Check for reallocation (buffer pointer changed or capacity increased)
                    // SAFETY: gc-heap-ptr arithmetic (Vec data ptr; capacity*sizeof(Entry) bounded; thread lifetime).
                    let new_ptr = stack.as_ptr();
                    let new_capacity = stack.capacity();

                    if old_capacity > 0 && (old_ptr != new_ptr || new_capacity != old_capacity) {
                        // Reallocation/growth occurred - compute old and new extents
                        let old_start = old_ptr as *mut c_void;
                        let old_end = unsafe {
                            (old_ptr as *mut u8)
                                .add(old_capacity * std::mem::size_of::<OuterPostArmKEntry>())
                                as *mut c_void
                        };
                        let new_start = new_ptr as *mut c_void;
                        let new_end = unsafe {
                            (new_ptr as *mut u8)
                                .add(new_capacity * std::mem::size_of::<OuterPostArmKEntry>())
                                as *mut c_void
                        };
                        Some((old_start, old_end, new_start, new_end))
                    } else {
                        None
                    }
                });

            // Call re-root helper if reallocation occurred
            if let Some((old_start, old_end, new_start, new_end)) = reroot_info {
                super::rereg_outer_post_arm_k_root(old_start, old_end, new_start, new_end);
            }

            depth_cell.set(depth + 1);
            if trace_opak() {
                eprintln!(
                    "[OPAK PUSH] depth {} -> {} (closure=0x{:x} fn=0x{:x})",
                    depth,
                    depth + 1,
                    closure_ptr as usize,
                    fn_ptr as usize
                );
            }
            true
        })
    }

    /// Pop the top entry from the stack. Returns None if the stack is empty.
    /// Overwrites the popped slot with nulls for stale-pointer hygiene.
    pub(super) fn pop() -> Option<OuterPostArmKEntry> {
        OUTER_POST_ARM_K_DEPTH.with(|depth_cell| {
            let depth = depth_cell.get();
            if depth == 0 {
                None
            } else {
                depth_cell.set(depth - 1);
                OUTER_POST_ARM_K_STACK.with(|stack_cell| {
                    let mut stack = stack_cell.borrow_mut();
                    let popped = stack[depth - 1];
                    // Clear the slot so a stale pointer doesn't survive
                    // in the TLS-rooted range across the next GC scan.
                    stack[depth - 1] = OuterPostArmKEntry {
                        closure_ptr: std::ptr::null_mut(),
                        fn_ptr: std::ptr::null_mut(),
                    };
                    if trace_opak() {
                        eprintln!(
                            "[OPAK POP] depth {} -> {} (closure=0x{:x} fn=0x{:x})",
                            depth,
                            depth - 1,
                            popped.closure_ptr as usize,
                            popped.fn_ptr as usize
                        );
                    }
                    Some(popped)
                })
            }
        })
    }

    /// Get the current depth of the stack.
    #[inline]
    pub(super) fn current_depth() -> usize {
        OUTER_POST_ARM_K_DEPTH.with(|c| c.get())
    }

    /// Set the current depth of the stack.
    #[inline]
    pub(super) fn set_depth(value: usize) {
        OUTER_POST_ARM_K_DEPTH.with(|c| c.set(value));
    }

    /// Get the base pointer and capacity extent of the stack for GC rooting.
    /// The returned pair is (start, end) where end points one past the end.
    /// This returns the full *capacity* extent, not the current depth,
    /// to match the array version's full-extent root.
    pub(super) fn root_extent() -> (*mut c_void, *mut c_void) {
        OUTER_POST_ARM_K_STACK.with(|cell| {
            let stack = cell.borrow();
            // SAFETY: gc-heap-ptr arithmetic (Vec data ptr; capacity*sizeof(Entry) bounded; thread lifetime).
            let start = stack.as_ptr() as *mut c_void;
            let capacity_bytes = stack.capacity() * core::mem::size_of::<OuterPostArmKEntry>();
            let end = unsafe { (start as *mut u8).add(capacity_bytes) as *mut c_void };
            (start, end)
        })
    }

    /// The extent most recently registered with the precise GC for this
    /// thread's stack, as `(start_addr, end_addr)`. `(0, 0)` before any
    /// registration. See `OUTER_POST_ARM_K_LAST_ROOTED`.
    #[allow(dead_code)]
    pub(super) fn last_rooted() -> (usize, usize) {
        OUTER_POST_ARM_K_LAST_ROOTED.with(|c| c.get())
    }

    /// Record the extent currently registered with the precise GC for
    /// this thread's stack. Pass `(null, null)` to mark "nothing rooted".
    #[allow(dead_code)]
    pub(super) fn set_last_rooted(start: *mut c_void, end: *mut c_void) {
        OUTER_POST_ARM_K_LAST_ROOTED.with(|c| c.set((start as usize, end as usize)));
    }

    /// Check if any entry in the range [min_depth, max_depth) has the given fn_ptr.
    pub(super) fn has_fn_ptr_in_range(fn_ptr: *mut u8, min_depth: usize, max_depth: usize) -> bool {
        OUTER_POST_ARM_K_STACK.with(|stack_cell| {
            let stack = stack_cell.borrow();
            let mut i = max_depth;
            while i > min_depth {
                i -= 1;
                if stack[i].fn_ptr == fn_ptr {
                    return true;
                }
            }
            false
        })
    }

    /// Drop (without dispatching) the top n entries from the stack.
    /// Overwrites dropped slots with nulls for stale-pointer hygiene.
    /// If n exceeds current depth, drops only what's available.
    pub(super) fn drop_top(n: u32) {
        if n == 0 {
            return;
        }
        if trace_opak() {
            let cur = current_depth();
            eprintln!(
                "[OPAK DROP] requesting drop of {} entries; depth={}",
                n, cur
            );
        }
        OUTER_POST_ARM_K_DEPTH.with(|depth_cell| {
            let depth = depth_cell.get();
            let drop_count = (n as usize).min(depth);
            if drop_count < (n as usize) {
                // Underflow: caller asked for more than available.
                // Don't abort — the runtime stays well-formed. Logged so
                // a future codegen-discipline regression surfaces.
                #[cfg(debug_assertions)]
                eprintln!(
                    "outer_post_arm_k_stack_api::drop_top: requested {} but only {} available; \
                     dropping what's available (codegen chain-push count mismatch?)",
                    n, depth
                );
            }
            if drop_count == 0 {
                return;
            }
            OUTER_POST_ARM_K_STACK.with(|stack_cell| {
                let mut stack = stack_cell.borrow_mut();
                for i in 0..drop_count {
                    stack[depth - 1 - i] = OuterPostArmKEntry {
                        closure_ptr: std::ptr::null_mut(),
                        fn_ptr: std::ptr::null_mut(),
                    };
                }
            });
            depth_cell.set(depth - drop_count);
        });
    }
}

#[inline]
fn run_loop_entry_depth_get() -> usize {
    RUN_LOOP_ENTRY_DEPTH.with(|c| c.get())
}

#[inline]
fn run_loop_entry_depth_set(value: usize) {
    RUN_LOOP_ENTRY_DEPTH.with(|c| c.set(value));
}

/// Plan D Task 117 (b) Phase 4 — snapshot the current
/// OUTER_POST_ARM_K depth so a caller (e.g.
/// `sigil_continuation_invoke`) can drain any pushes the captured
/// continuation performs during its run_loop drive. Mirror of the
/// snapshot `sigil_run_loop` does at entry; exposed as a free
/// function so the continuation-invoke helper can use the same
/// discipline without needing to drive run_loop transitively.
#[inline]
pub fn outer_post_arm_k_depth_snapshot() -> usize {
    outer_post_arm_k_stack_api::current_depth()
}

// Plan D Task 112e — env-var-gated runtime trace flags. Read once
// per thread via `OnceCell` so the hot trampoline paths pay only a
// single TLS load + branch per trace site (vs `std::env::var_os`'s
// process-wide env-table lock + linear scan that the original
// implementation accidentally compiled to). The trace flags are
// opt-in debugging aids; production runs leave them unset.
thread_local! {
    static TRACE_OPAK: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
    static TRACE_TERM: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
    static TRACE_CALL: std::cell::OnceCell<bool> = const { std::cell::OnceCell::new() };
}

#[inline]
fn trace_opak() -> bool {
    TRACE_OPAK.with(|c| *c.get_or_init(|| std::env::var_os("SIGIL_TRACE_OPAK").is_some()))
}

#[inline]
fn trace_term() -> bool {
    TRACE_TERM.with(|c| *c.get_or_init(|| std::env::var_os("SIGIL_TRACE_TERM").is_some()))
}

#[inline]
fn trace_call() -> bool {
    TRACE_CALL.with(|c| *c.get_or_init(|| std::env::var_os("SIGIL_TRACE_CALL").is_some()))
}

/// Plan D Task 117 (b) Phase 4 — restore OUTER_POST_ARM_K depth to
/// a previous snapshot. Used by `sigil_continuation_invoke` after
/// driving the captured continuation's run_loop to drain any
/// post-arm-k entries that the continuation's internal synth-conts
/// pushed but the trampoline's routing didn't pop (e.g., when our
/// codegen wraps the body value via a SECOND run_loop, the first
/// run_loop's DONE handler doesn't pop the entry because the
/// trampoline routes through the outer chain instead).
#[inline]
pub fn outer_post_arm_k_depth_restore(target: usize) {
    outer_post_arm_k_stack_api::set_depth(target);
}

// 2026-05-04 return-arm-via-args lift Stage 5 — the
// `BODY_RETURN_ARM_STACK` TLS (Task 78.5 G4 Approach 6 deep-redo)
// retired here. Its only consumer was the `fired` flag in the args-
// helper (closure_ptr / fn_ptr were already gone post-Stage 4). The
// flag is now a per-handle stack cell allocated at
// `lower_handle_body_direct_cps_call` and threaded through args_ptr +
// synth-cont closure records by the codegen layout extension; the
// helper gates dispatch on `*fired_ptr`.

// Nested-effect-forwarding fix: when sigil_perform crosses intervening
// handlers, record the crossed frame pointers so lower_k_pair_call can
// re-push them when resuming the continuation. Uses a dynamic Vec to
// support arbitrarily deep nesting (e.g., backtracking search over 81
// cells in the sudoku solver). The frame pointers stored here are
// already GC-rooted through the handler stack — this is only a record
// of which frames to re-push.
thread_local! {
    static CROSSED_FRAMES_STACK: RefCell<Vec<*mut HandlerFrame>> =
        const { RefCell::new(Vec::new()) };
}

/// Push crossed frame pointers recorded by `sigil_perform` for a given
/// target handler frame. Called by k-pair dispatch before driving run_loop.
///
/// Protocol: `sigil_perform` pushes N entries (the crossed frames, outermost
/// first). `sigil_repush_crossed_frames` re-pushes them onto the handler
/// stack in the SAME order sigil_perform recorded them (outermost first →
/// innermost last = on top), so the handler stack matches the original
/// push order. Returns the count N so the caller can pop them after run_loop.
///
/// # Safety
///
/// All frame pointers in the TLS stack must be valid, arena-allocated
/// `HandlerFrame`s that remain live for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn sigil_repush_crossed_frames(_target_frame: *mut HandlerFrame) -> u32 {
    CROSSED_FRAMES_STACK.with(|cell| {
        let stack = cell.borrow();
        if stack.is_empty() {
            return 0;
        }
        let mut count = 0u32;
        let mut i = stack.len();
        let mut found_sentinel = false;
        while i > 0 {
            i -= 1;
            if stack[i].is_null() {
                found_sentinel = true;
                break;
            }
            count += 1;
        }
        if !found_sentinel {
            eprintln!(
                "sigil_repush_crossed_frames: no null sentinel found in \
                 crossed-frames TLS stack (len={}) — stack corruption",
                stack.len()
            );
            std::process::abort();
        }
        let base = stack.len() - count as usize;
        for j in base..stack.len() {
            let frame = stack[j];
            if !frame.is_null() {
                sigil_handle_push(frame);
            }
        }
        count
    })
}

/// Pop N crossed frames that were re-pushed by `sigil_repush_crossed_frames`.
/// After popping the handler frames, applies each crossed handler's return
/// arm to the current terminal value (innermost first). This is necessary
/// because the continuation k resumed the body without the crossed handlers'
/// return arm entries on the BODY_RETURN_ARM_STACK — identity/synth-cont
/// terminal paths bypass that stack. Instead we drive run_loop for each
/// return arm here, updating terminal_out in place.
///
/// # Safety
///
/// `terminal_out` must point to a valid, caller-owned `TerminalResult`.
/// Frame pointers in the TLS stack must be valid, arena-allocated
/// `HandlerFrame`s that remain live for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn sigil_pop_crossed_frames(count: u32, terminal_out: *mut TerminalResult) {
    for _ in 0..count {
        sigil_handle_pop();
    }

    if count == 0 {
        return;
    }

    // Snapshot the entries we need before mutating the stack, since
    // driving run_loop for return arms below may trigger nested
    // performs that push/pop the same TLS stack.
    let entries: Vec<*mut HandlerFrame> = CROSSED_FRAMES_STACK.with(|cell| {
        let stack = cell.borrow();
        let mut n = 0usize;
        let mut i = stack.len();
        while i > 0 {
            i -= 1;
            if stack[i].is_null() {
                break;
            }
            n += 1;
        }
        let base = stack.len() - n;
        stack[base..stack.len()].to_vec()
    });

    // Apply return arms innermost first.
    for &frame in entries.iter().rev() {
        if frame.is_null() {
            continue;
        }
        let f = &*frame;
        if f.return_fn.is_null() {
            continue;
        }
        if (*terminal_out).tag != sigil_abi::effect::NEXT_STEP_TAG_DONE as u64 {
            break;
        }
        let value = (*terminal_out).value;
        let ns = sigil_next_step_call(f.return_closure, f.return_fn, 3);
        let args = sigil_next_step_args_ptr(ns);
        ptr::write(args.add(0), value);
        ptr::write(args.add(1), 0u64);
        ptr::write(
            args.add(2),
            sigil_continuation_identity as *const () as usize as u64,
        );
        sigil_run_loop(ns, terminal_out);
    }

    // Remove sentinel + entries from TLS stack.
    CROSSED_FRAMES_STACK.with(|cell| {
        let mut stack = cell.borrow_mut();
        let remove = count as usize + 1; // +1 for null sentinel
        if stack.len() < remove {
            eprintln!(
                "sigil_pop_crossed_frames: stack len ({}) < remove ({remove}); \
                 crossed-frames TLS stack is corrupted — sentinel/entry mismatch",
                stack.len()
            );
            std::process::abort();
        }
        let new_len = stack.len() - remove;
        stack.truncate(new_len);
    });
}

/// Register the calling thread's `OUTER_POST_ARM_K_STACK` TLS cell as
/// a Boehm GC root. Idempotent per thread. Mirrors
/// [`register_handler_stack_root_for_calling_thread`]'s discipline.
pub(crate) fn register_outer_post_arm_k_stack_root_for_calling_thread() -> (*mut c_void, *mut c_void)
{
    // Pre-reserve the stack to capacity 256 before registering with GC,
    // so root_extent() captures the real heap buffer, not a dangling pointer.
    OUTER_POST_ARM_K_STACK.with(|stack_cell| {
        let mut stack = stack_cell.borrow_mut();
        if stack.capacity() == 0 {
            stack.reserve(OUTER_POST_ARM_K_STACK_SIZE);
        }
    });
    let (start, end) = outer_post_arm_k_stack_api::root_extent();
    let already_registered = OUTER_POST_ARM_K_STACK_ROOTED.with(|rooted| {
        let r = rooted.get();
        rooted.set(true);
        r
    });
    if !already_registered {
        unsafe {
            crate::gc::GC_add_roots(start, end);
        }
    }
    // Seed the re-root tracker so `rereg_outer_post_arm_k_root` knows the
    // currently-live extent: a later re-root with the *same* buffer
    // no-ops, and a re-root after a move correctly drops *this* extent.
    outer_post_arm_k_stack_api::set_last_rooted(start, end);
    (start, end)
}

/// A precise-GC root-set mutation issued by
/// [`rereg_outer_post_arm_k_root`]. Under `cfg(test)` the helper records
/// these (see [`REROOT_GC_OP_LOG`]) instead of mutating Boehm's real root
/// set, so a unit test can assert the helper's add/remove decision —
/// "old removed, new present" — directly and without any GC-table risk.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RerootGcOp {
    /// Unregister `[start, end)` — fields are `(start_addr, end_addr)`.
    Remove(usize, usize),
    /// Register `[start, end)` — fields are `(start_addr, end_addr)`.
    Add(usize, usize),
}

#[cfg(test)]
thread_local! {
    /// In-test log of the GC root mutations `rereg_outer_post_arm_k_root`
    /// would issue. Real in-process `GC_add_roots`/`GC_remove_roots` on the
    /// synthetic extents a unit test supplies corrupt Boehm's dynamic root
    /// table (Boehm rounds/coalesces ranges) and SIGSEGV at the next
    /// collection — the failure mode every prior real-Boehm test hit. So
    /// under test the helper records its decision here; the production
    /// (`cfg(not(test))`) path calls Boehm directly. Drained by the test.
    static REROOT_GC_OP_LOG: RefCell<Vec<RerootGcOp>> = const { RefCell::new(Vec::new()) };
}

/// Empty the re-root GC-op log (test-only).
#[cfg(test)]
fn reroot_gc_clear_log() {
    REROOT_GC_OP_LOG.with(|log| log.borrow_mut().clear());
}

/// Take and clear the re-root GC-op log (test-only).
#[cfg(test)]
fn reroot_gc_take_log() -> Vec<RerootGcOp> {
    REROOT_GC_OP_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Unregister `[start, end)` from the precise GC as part of a re-root.
/// Production forwards to Boehm; under test it records the intent in
/// [`REROOT_GC_OP_LOG`] without touching Boehm's root set.
#[allow(dead_code)]
fn reroot_gc_remove(start: *mut c_void, end: *mut c_void) {
    #[cfg(test)]
    REROOT_GC_OP_LOG.with(|log| {
        log.borrow_mut()
            .push(RerootGcOp::Remove(start as usize, end as usize))
    });
    #[cfg(not(test))]
    unsafe {
        crate::gc::GC_remove_roots(start, end);
    }
}

/// Register `[start, end)` with the precise GC as part of a re-root.
/// Production forwards to Boehm; under test it records the intent in
/// [`REROOT_GC_OP_LOG`] without touching Boehm's root set.
#[allow(dead_code)]
fn reroot_gc_add(start: *mut c_void, end: *mut c_void) {
    #[cfg(test)]
    REROOT_GC_OP_LOG.with(|log| {
        log.borrow_mut()
            .push(RerootGcOp::Add(start as usize, end as usize))
    });
    #[cfg(not(test))]
    unsafe {
        crate::gc::GC_add_roots(start, end);
    }
}

/// Re-register the calling thread's `OUTER_POST_ARM_K_STACK` precise-GC
/// root after its backing buffer has moved (a `Vec` realloc): unregister
/// the previously-rooted `[old_start, old_end)` extent, register the new
/// `[new_start, new_end)` extent, and record the new extent as last-rooted.
/// Uses the same Boehm root API
/// ([`register_outer_post_arm_k_stack_root_for_calling_thread`]) that the
/// initial registration uses.
///
/// Adjusts the GC root extent when the continuation stack reallocates
/// (pointer moves) or grows (capacity increases). Unregisters the old extent,
/// registers the new extent. No-op when the buffer extent is unchanged — detected
/// by comparing the requested new extent against the tracked last-rooted extent
/// (`OUTER_POST_ARM_K_LAST_ROOTED`) — so a caller may invoke it unconditionally
/// after any operation that *might* reallocate or grow.
pub(crate) fn rereg_outer_post_arm_k_root(
    old_start: *mut c_void,
    old_end: *mut c_void,
    new_start: *mut c_void,
    new_end: *mut c_void,
) {
    let new_key = (new_start as usize, new_end as usize);
    // Buffer pointer unchanged: the new extent is already the live root,
    // so there is nothing to unregister or re-register.
    if outer_post_arm_k_stack_api::last_rooted() == new_key {
        return;
    }
    // Drop the stale root *before* adding the new one, so the GC never
    // holds two ranges for the same logical buffer and never scans the
    // moved-from extent. `old_start` is null only when nothing was rooted
    // yet (initial registration handles that path). The GC mutation goes
    // through `reroot_gc_remove`/`reroot_gc_add`, which call Boehm in
    // production and record into `REROOT_GC_OP_LOG` under test.
    if !old_start.is_null() {
        reroot_gc_remove(old_start, old_end);
    }
    reroot_gc_add(new_start, new_end);
    outer_post_arm_k_stack_api::set_last_rooted(new_start, new_end);
}

/// Inverse of [`register_outer_post_arm_k_stack_root_for_calling_thread`].
/// Used by `GcThreadEnrolment::drop` in tests to unregister the range
/// before the thread exits.
#[cfg(test)]
pub(crate) fn unregister_outer_post_arm_k_stack_root_for_calling_thread(
    start: *mut c_void,
    end: *mut c_void,
) {
    OUTER_POST_ARM_K_STACK_ROOTED.with(|rooted| rooted.set(false));
    outer_post_arm_k_stack_api::set_depth(0);
    // Clear the re-root tracker so a reused test thread doesn't believe a
    // freed extent is still live.
    outer_post_arm_k_stack_api::set_last_rooted(ptr::null_mut(), ptr::null_mut());
    unsafe {
        crate::gc::GC_remove_roots(start, end);
    }
}

/// Push an outer `post_arm_k` pair onto the thread-local stack.
/// Codegen emits a call to this fn from B.2 helper Middle's emit
/// before issuing `sigil_perform` for the next chain step. Supports
/// unbounded depth with automatic re-rooting on buffer growth (for precise GC).
///
/// # Push/pop balance discipline
///
/// Every push from helper Middle's emit is paired with exactly one
/// pop in the trampoline's `Done` branch (see `sigil_run_loop`).
/// The pairing holds across the perform → arm-dispatch → arm-body →
/// Done dispatch sequence: helper Middle issues `sigil_perform`,
/// the perform's arm runs, the arm body eventually returns Done,
/// the trampoline's Done branch pops the pushed entry and routes
/// the value through it.
///
/// **Abnormal exit (process-fatal-by-design).** If the program
/// aborts mid-perform (panic, unhandled effect, stack overflow,
/// runtime invariant violation), the push has no matching pop —
/// the stack is left with a stale entry. This is acceptable
/// because: (1) Sigil aborts the process on any of these conditions
/// (no unwinding to user code that would observe stale state); (2)
/// the TLS stack dies with the thread on process exit, so leaks
/// don't accumulate across runs; (3) recovering from such a state
/// is out of scope for v1. The discipline is "balanced under normal
/// flow; fatal under abnormal flow," matching how `HANDLER_STACK`
/// and the arena handle the same edge.
///
/// # Safety
///
/// `closure_ptr` must be either null or a valid heap-allocated
/// TAG_CLOSURE record. `fn_ptr` must point at a CPS-fn satisfying the
/// `(closure_ptr, args_ptr, args_len) -> *mut NextStep` calling
/// convention. The caller MUST ensure a corresponding pop happens —
/// the trampoline pops on `Done` observation; helper Middle's emit
/// pairs every push with a perform that eventually drives a Done.
#[no_mangle]
pub unsafe extern "C" fn sigil_outer_post_arm_k_push(closure_ptr: *mut u8, fn_ptr: *mut u8) {
    outer_post_arm_k_stack_api::push(closure_ptr, fn_ptr);
}

/// Pop the top of the outer `post_arm_k` stack. Called by the
/// trampoline's `Done` branch. Returns `None` when the stack is empty
/// (top-level Done — return to wrapper as before).
///
/// **Stale-pointer hygiene:** the popped slot is overwritten with
/// nulls so a future Boehm scan of the rooted TLS range doesn't
/// see a stale `closure_ptr` from the prior push as a live heap
/// reference. Without this, the slot would retain the pushed value
/// until overwritten by a future push at the same depth — Boehm
/// would treat it as a root and keep the closure record alive
/// past its useful lifetime, plus risk segfaults from interior-
/// pointer mis-classification when the test-mode pushed values are
/// non-heap-allocated synthetic pointers.
fn outer_post_arm_k_try_pop() -> Option<OuterPostArmKEntry> {
    outer_post_arm_k_stack_api::pop()
}

/// Drop (without dispatching) the top `n` entries of the outer
/// `post_arm_k` stack. Called by codegen's Cps→Cps tail-call branch
/// (PR #108 follow-up) to balance the chain-step transition pushes
/// (`sigil_outer_post_arm_k_push`) before tail-iterating without
/// going through the normal Done-observation pop path.
///
/// **Why a drop-without-dispatch.** The trampoline's Done-observation
/// pop loop dispatches each popped entry to continue the chain; that's
/// the discharge mechanism for normal chain completion. A tail-call-
/// out (NextStep::Call return from the chain's Final-step) bypasses
/// that mechanism — the next iteration's chain re-pushes its own
/// entries, and the previous iteration's accumulated entries are not
/// useful (the post-completion continuation never needs to fire on
/// the popped value because the recursion never terminates through
/// that path). Dropping without dispatching is the matching pop
/// discipline for the tail-call-out shape.
///
/// **Stale-pointer hygiene.** Mirrors `outer_post_arm_k_try_pop`'s
/// slot-clearing — overwrite each dropped slot with nulls so a
/// future Boehm scan of the rooted TLS range doesn't see a stale
/// `closure_ptr` from the dropped push as a live heap reference.
///
/// **Underflow.** If `n` exceeds the current depth, drops only what's
/// available (saturates at zero) and continues. Overflow is impossible
/// (depth is monotonically non-negative). Underflow indicates a
/// codegen bug — the caller's chain push count claim doesn't match
/// what the runtime actually accumulated. Logged via eprintln in
/// debug builds; no abort, since the underflow is benign at runtime
/// (the remaining stack stays well-formed).
///
/// # Safety
///
/// Safe to call. The dropped entries' `closure_ptr` / `fn_ptr` values
/// are heap-managed by the GC; dropping them from the stack only
/// removes them as roots — the GC will reclaim them when no other
/// reference remains.
#[no_mangle]
pub unsafe extern "C" fn sigil_outer_post_arm_k_drop(n: u32) {
    outer_post_arm_k_stack_api::drop_top(n);
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
    // Looks up the descriptor index for this `arm_count` via the
    // runtime's pre-registered handler-frame shape table. Codegen-
    // emitted call sites bypass this wrapper and call
    // `sigil_handler_frame_new_with_resumes_many` directly (passing
    // the index they computed during the shape pre-pass), so this
    // branch is only reached by direct runtime / test callers.
    let descriptor_index = if (arm_count as usize) < crate::gc::MAX_HANDLER_ARMS_INCLUSIVE {
        crate::gc::runtime_shape_indices().handler_frame[arm_count as usize]
    } else {
        // Out-of-range arm_count — pass `u32::MAX`; the
        // `arm_count > MAX_HANDLER_ARMS` check in the inner
        // entry will fire and abort before the descriptor
        // lookup runs anyway.
        u32::MAX
    };
    sigil_handler_frame_new_with_resumes_many(effect_id, arm_count, 0, descriptor_index)
}

/// Plotkin fix — variant of `sigil_handler_frame_new` that records
/// the effect's `resumes: many` flag in the frame header. Codegen
/// emits this entry instead of the legacy one when the effect
/// declaration has `resumes: many`. Read at perform time by
/// `wrap_continuation_with_outer_post_arm_k` to decide whether to
/// preserve the crossed-frame's chain entries (multi-shot semantics)
/// or let them flow through as normal Done routing (single-shot).
///
/// # Safety
///
/// `arm_count` must be `<= MAX_HANDLER_ARMS` (the function aborts
/// otherwise — checked, not a precondition). `resumes_many` is
/// treated as a boolean (zero or non-zero); any non-zero value
/// sets `RESUMES_MANY_BIT` on the frame's `arm_count` field.
/// The returned pointer is GC-managed (allocated via `sigil_alloc`)
/// and may be moved to a TLS-rooted slot before the caller
/// allocates again. Callers are responsible for installing the
/// frame onto the handler stack via `sigil_handle_push`.
#[no_mangle]
pub unsafe extern "C" fn sigil_handler_frame_new_with_resumes_many(
    effect_id: u32,
    arm_count: u32,
    resumes_many: u32,
    descriptor_index: u32,
) -> *mut HandlerFrame {
    if arm_count > MAX_HANDLER_ARMS {
        eprintln!(
            "sigil_handler_frame_new: arm_count {arm_count} exceeds MAX_HANDLER_ARMS ({})",
            MAX_HANDLER_ARMS
        );
        std::process::abort();
    }
    let payload_bytes: usize = handler_frame_payload_bytes(arm_count);
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
    let bitmap = handler_frame_pointer_bitmap(arm_count);

    // INVARIANT: Boehm consumes the pointer bitmap two ways post-
    // Plan E2 Task 8: (a) bitmap=0 selects `GC_malloc_atomic` (no
    // scan); (b) bitmap!=0 selects `GC_malloc_explicitly_typed` and
    // the per-bit pattern drives precise marking via the descriptor
    // cache. The type tag is still inert at the GC layer. Reusing
    // TAG_CLOSURE as "heap object with closure-shaped pointer fields"
    // is functionally fine; if a v2 type-aware GC walker is
    // introduced, add TAG_HANDLER_FRAME alongside in
    // `sigil-header-constants` and revise this site.
    let header = Header::new(TAG_CLOSURE, payload_words, bitmap);
    let obj = crate::gc::sigil_alloc(header.raw(), payload_bytes, descriptor_index);

    // Frame fields begin at offset 8 (past the Sigil object header).
    //
    // SAFETY: gc-heap-ptr arithmetic (the cast is to a single
    // local-scope read/write target reflecting the documented layout;
    // the pointer is not stored or returned beyond this initialisation).
    let frame_ptr = obj.add(8) as *mut HandlerFrame;
    (*frame_ptr).effect_id = effect_id;
    let arm_count_with_flag = (arm_count & ARM_COUNT_MASK)
        | (if resumes_many != 0 {
            RESUMES_MANY_BIT
        } else {
            0
        });
    (*frame_ptr).arm_count = arm_count_with_flag;
    (*frame_ptr).return_fn = ptr::null_mut();
    (*frame_ptr).return_closure = ptr::null_mut();
    (*frame_ptr).prev = ptr::null_mut();

    // Explicitly zero-init the variable-length arms region rather than
    // depending on the Boehm allocator-zeroing contract.
    // `GC_malloc_atomic` / `GC_malloc_explicitly_typed` both zero
    // today, but that's a libgc-version contract, not a Rust contract.
    // Future Boehm flag flips (e.g. a switch to
    // `GC_malloc_atomic_uncollectable`) would silently flip arm-slot
    // reads from null to garbage. The cost is one `write_bytes` over
    // ≤ 224 bytes (`16 * 14` for the arms region).
    let arms_region_start = (frame_ptr as *mut u8).add(core::mem::size_of::<HandlerFrame>());
    let arms_region_bytes = (arm_count as usize) * 16;
    // SAFETY: gc-heap-ptr arithmetic (the destination pointer addresses
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
    let arm_count = (*frame).arm_count & ARM_COUNT_MASK;
    if op_id >= arm_count {
        eprintln!(
            "sigil_handler_frame_set_arm: op_id {op_id} out of range (arm_count={arm_count})"
        );
        std::process::abort();
    }
    let arms_base = arms_base_ptr(frame);
    // SAFETY: gc-heap-ptr arithmetic (the offset is computed solely to
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
    HANDLER_STACK.with(|cell| {
        let head = cell.get();
        // Plan D Task 117 — skip-if-on-top. When `lower_k_pair_call`
        // dispatches a let-bound k from inside the originating handle's
        // arm body, the captured `frame_ptr` IS the current head; the
        // re-link push would trip the "frame already linked" panic
        // (`frame.prev` is non-null because the outer handle linked
        // it). Detect this and no-op the link, recording the
        // skip-decision into RELINK_STACK so the matching pop is
        // also a no-op.
        if head == frame {
            RELINK_STACK.with(|s| s.borrow_mut().push((frame, false)));
            return;
        }
        // Defensive against codegen bugs that double-push the same
        // frame from a NON-head position: a non-null `prev` at push
        // time would silently overwrite the prior chain link. The
        // check is debug-only because a release build on a verified
        // codegen never trips it; if it ever does, the panic
        // localises the bug to the push site rather than a later
        // traversal. Skip-if-on-top above already handles the
        // legitimate re-push-of-head case.
        debug_assert!(
            (*frame).prev.is_null(),
            "sigil_handle_push: frame already linked but not at head (double-push from below?)"
        );
        (*frame).prev = head;
        cell.set(frame);
        RELINK_STACK.with(|s| s.borrow_mut().push((frame, true)));
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
    // Plan D Task 117 — read the matching push's relink-decision
    // off RELINK_STACK. If the push was a no-op (frame was already
    // on top), the matching pop must also no-op. There are no
    // current bypass callers — every push/pop site goes through
    // `sigil_handle_push` / `sigil_handle_pop` — so an empty stack
    // at pop time means the stacks have desynced (codegen-emitted
    // pop without a matching push, panic unwind across a push, or
    // a future control-flow primitive intercepting pop). With
    // HANDLER_STACK non-empty (the underflow check below passes)
    // but RELINK_STACK empty, a default-`true` would silently
    // unlink a real frame with no record of why — exactly the
    // corruption the design wants to prevent. Hard-panic in both
    // debug and release builds so a desync surfaces where it
    // happens, not three handler levels later.
    let (recorded_frame, did_link) = RELINK_STACK.with(|s| match s.borrow_mut().pop() {
        Some(entry) => entry,
        None => {
            eprintln!(
                "sigil_handle_pop: RELINK_STACK underflow — every pop must follow a \
                 matching push from sigil_handle_push. A pop without a matching push \
                 indicates a codegen-emitted unbalanced pair, an unwind across a push, \
                 or a missing RELINK_STACK update from a control-flow primitive. \
                 Default-passthrough would silently unlink a real frame; aborting \
                 surfaces the desync at the actual unbalance."
            );
            std::process::abort();
        }
    });
    HANDLER_STACK.with(|cell| {
        let head = cell.get();
        if head.is_null() {
            eprintln!("sigil_handle_pop: handler stack underflow");
            std::process::abort();
        }
        // Plan D Task 117 — frame-keyed RELINK_STACK pairing
        // assert. Pop time HEAD must equal the push-time recorded
        // frame: invariant for both did_link branches (link case
        // pushed frame and set HEAD=frame; skip case left HEAD
        // alone but recorded frame == HEAD). A mismatch means
        // intermediate push/pop pairs targeted different frames
        // (a desync on the order of "pop X is matching pop Y" —
        // bool-only counter would silently let count-balanced
        // desyncs through). Debug-only; the production-codegen
        // contract is balanced pairs, so a release build on
        // verified codegen never trips this.
        debug_assert_eq!(
            recorded_frame, head,
            "sigil_handle_pop: RELINK_STACK frame mismatch — push-time frame {:p} != \
             pop-time HANDLER_STACK head {:p}. Push/pop pair desync; check codegen for \
             unbalanced pairs across nested handles.",
            recorded_frame, head
        );
        if !did_link {
            // Skip-if-on-top counterpart: matching push was a no-op,
            // so pop is too. Return the current head unchanged
            // (codegen may use the return value as a sentinel; the
            // existing contract is "popped frame ptr").
            return head;
        }
        // SAFETY: head is non-null per the underflow check; reading
        // `prev` against the documented HandlerFrame layout is sound.
        let prev = (*head).prev;
        // Clear the popped frame's `prev` link so a subsequent push of
        // the same frame (legitimate use case: re-entering a `handle`
        // in a loop) doesn't trip the not-at-head double-push assert
        // at `sigil_handle_push`.
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

// Plan D Task 111d — `sigil_last_terminal_tag`,
// `sigil_reset_last_terminal_tag`, `sigil_last_terminal_value`,
// `sigil_reset_last_terminal_value` are gone. Codegen reads / inits
// the caller-owned `TerminalResult` slot directly via load/store at
// `terminal_out_param`'s pointer. The four FFI helpers + their TLS
// statics shipped in Plan B (Phase 4f / Stage-6.8-followup Bug 1+2
// fixes) and dual-wrote alongside `*out` from 111a–111c; the slot
// became authoritative once the ABI threading completed and the TLS
// path is now removed entirely.

// 2026-05-04 return-arm-via-args lift Stage 5 — the four body-return-
// arm TLS push/pop/mask helpers (`sigil_body_return_arm_push`, `_pop`,
// `_push_mask_if_needed`, `_pop_if_flag`) retired. Codegen no longer
// emits calls to them; the per-handle `fired` cell is the structural
// replacement.

/// 2026-05-04 return-arm-via-args lift — natural-exit dispatch helper.
///
/// `return_arm_fired_ptr: *mut u64` points at a 1-u64 cell on the
/// handle expression's stack frame. The cell starts at 0 and is
/// mutated to 1 on first dispatch; chain-unwind synth-conts inheriting
/// the same `fired_ptr` (via forward-copy through closure records)
/// observe the mutation and short-circuit. The Stage-5 structural
/// replacement for the retired TLS `BODY_RETURN_ARM_STACK[depth-1].
/// fired` flag.
///
/// Semantics:
/// - Null `return_arm_fired_ptr` (sub-Cps-call boundary — no outer
///   handle in flight) → emit `Done(v)`.
/// - `*fired_ptr != 0` (already dispatched at the body's deepest
///   natural-exit) → emit `Done(v)`.
/// - Null `return_arm_fn` (no return arm even though a handle is in
///   flight) → emit `Done(v)`.
/// - Otherwise set `*fired_ptr = 1` and dispatch
///   `Call(return_arm_closure, return_arm_fn, [v, null, identity])`.
///
/// # Safety
///
/// `return_arm_fired_ptr` must be either null or a valid pointer to a
/// writable `u64` cell aligned to 8 bytes that lives for the duration
/// of the helper invocation. Codegen guarantees this by allocating the
/// cell on the handle expression's stack frame at the same site that
/// writes the cell's address into the args trailing slot.
#[no_mangle]
pub unsafe extern "C" fn sigil_done_or_dispatch_return_arm_via_args(
    v: u64,
    return_arm_closure: *mut u8,
    return_arm_fn: *mut u8,
    return_arm_fired_ptr: *mut u64,
) -> *mut NextStep {
    if return_arm_fired_ptr.is_null() {
        return sigil_next_step_done(v);
    }
    if ptr::read(return_arm_fired_ptr) != 0 {
        return sigil_next_step_done(v);
    }
    if return_arm_fn.is_null() {
        return sigil_next_step_done(v);
    }
    ptr::write(return_arm_fired_ptr, 1u64);
    let ns = sigil_next_step_call(return_arm_closure, return_arm_fn, 3);
    let args = sigil_next_step_args_ptr(ns);
    ptr::write(args.add(0), v);
    ptr::write(args.add(1), 0u64);
    ptr::write(
        args.add(2),
        sigil_continuation_identity as *const () as usize as u64,
    );
    ns
}

/// Allocate a `NEXT_STEP_TAG_DISCHARGED` record from the per-dispatch
/// arena holding `value`.
///
/// Emitted by op arm fn bodies on the discard-`k` tail path — the arm
/// produces a final value WITHOUT invoking `k`, so per algebraic-effects
/// semantics the value IS the handle's final value (not subject to the
/// return clause's wrapper). The trampoline propagates the value
/// identically to `Done` (including routing through the outer post_arm_k
/// stack for multi-shot composition); the distinction is recorded in
/// the caller-owned `TerminalResult` slot's `tag` field (Plan D Task
/// 111d; previously TLS) for the handle expression's outer codegen
/// logic to query via a load from `terminal_out_param + 8`.
///
/// See `[DEVIATION Stage-6.8-followup Bug 2] Return arm dispatch on
/// op-arm-discharge values violates algebraic-effects semantics` for
/// the bug analysis and Phase-4g `dd10379` rationale this corrects.
///
/// # Safety
///
/// Safe to call. Returned pointer is valid until the next
/// `sigil_arena_reset`.
#[no_mangle]
pub unsafe extern "C" fn sigil_next_step_discharged(value: u64) -> *mut NextStep {
    let raw = crate::arena::sigil_arena_alloc(core::mem::size_of::<NextStep>());
    let ns = raw as *mut NextStep;
    (*ns).tag = NEXT_STEP_TAG_DISCHARGED;
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
    // SAFETY: gc-heap-ptr arithmetic (the result is a transient
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
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    // Plan B Task 55, Phase 4e captures+ Slice A — identity has
    // exactly two legitimate calling sources, both pinned by unit
    // tests in this module:
    //   - `args_len == 1`: synth-cont's terminal `Call(post_arm_k_*,
    //     [result])` dispatch lands here when post_arm_k_fn is
    //     identity (`continuation_identity_returns_done_with_args_ptr_value`
    //     and `continuation_identity_round_trips_through_run_loop`).
    //   - `args_len == 3`: arm-fn tail-`k` direct emit lands here
    //     when the arm calls `k(arg)` and there's no helper synth-
    //     cont in scope (perform in tail position of the handle body)
    //     — the trailing pair `[null, &identity]` is irrelevant since
    //     identity is the terminal continuation
    //     (`continuation_identity_tolerates_args_len_3_trailing_pair_convention`).
    // No other args_len is reachable from current codegen. Asserting
    // exactly `{1, 3}` keeps a future codegen bug producing an
    // unexpected args_len shape catchable here instead of silently
    // absorbed by a permissive `>= 1` pass-through.
    debug_assert!(
        args_len == 1 || args_len == 3,
        "sigil_continuation_identity: args_len must be exactly 1 \
         (synth-cont's terminal `Call(post_arm_k_*, [result])` dispatch) \
         or 3 (arm-fn tail-`k` direct emit per the Phase 4e captures+ \
         Slice A trailing-pair convention `[arg, post_arm_k_closure, \
         post_arm_k_fn]`); got {args_len}"
    );
    debug_assert!(
        !args_ptr.is_null(),
        "sigil_continuation_identity: args_ptr must be non-null when args_len >= 1"
    );
    // SAFETY: caller (codegen tail-k lowering or synth-cont post-arm-k
    // dispatch) guarantees args_ptr points to >= 1 readable u64
    // holding the captured arg at slot 0.
    let value = *args_ptr;
    // Plotkin fix — when called as the k_fn for an arm's k-call with
    // a multi-shot post_arm_k chain trailing pair (args_len == 3 with
    // a non-null, non-identity fn at slot 2), dispatch through the
    // trailing pair so chain step_0 fires. This is necessary for
    // tail-perform body shapes (e.g., `body() => perform Effect.op()`)
    // where the body has no chain step that would otherwise push the
    // trailing pair onto OUTER_POST_ARM_K via the
    // `outer_post_arm_k_push_ref` call at codegen.rs:16014. Without
    // this dispatch, the arm's `k(arg)` lands here (the identity k_fn
    // selected by `lower_handle_body_direct_cps_call` / Sync-side
    // body call), and chain step_0 is silently dropped — the multi-
    // shot enumeration produces only the first branch's value.
    if args_len == 3 {
        // SAFETY: gc-heap-ptr arithmetic (caller-owned args buffer at args_len=3 reads slot 1 of trailing pair — identity-as-k_fn dispatch convention)
        let post_arm_k_closure = *args_ptr.add(1) as *mut u8;
        // SAFETY: gc-heap-ptr arithmetic (same args buffer, slot 2)
        let post_arm_k_fn = *args_ptr.add(2) as *mut u8;
        let self_addr = sigil_continuation_identity as *mut u8;
        if !post_arm_k_fn.is_null() && post_arm_k_fn != self_addr {
            let ns = sigil_next_step_call(post_arm_k_closure, post_arm_k_fn, 1);
            let ns_args = sigil_next_step_args_ptr(ns);
            ns_args.write(value);
            return ns;
        }
    }
    sigil_next_step_done(value)
}

// ---------------------------------------------------------------------
// Builtin top-level handler arm fns (Plan B Task 57)
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// Plotkin fix — continuation wrapper for OUTER_POST_ARM_K save
// ---------------------------------------------------------------------

/// CPS function that re-pushes a saved OUTER_POST_ARM_K entry and then
/// delegates to an inner continuation. Used by `sigil_perform` when a
/// perform crosses handlers AND OUTER_POST_ARM_K has entries — those
/// entries belong to the inner-handler chain that the perform crosses
/// through; consuming them at the outer-handler arm's Done would
/// dispatch the wrong value to the inner chain. Instead, perform pops
/// all entries and embeds them into a chain of wrappers around the
/// captured continuation. When the continuation is eventually invoked
/// (e.g., via the outer arm's lambda `fn(s) => k(s)(s)` body), the
/// wrappers re-push the entries in the original bottom-to-top order
/// before delegating to the original continuation. This preserves the
/// inner chain across the outer-handler termination boundary.
///
/// Closure layout (TAG_CLOSURE, count=5, bitmap=0b01010):
///   offset  8: code_ptr (null — never read; required by closure ABI)
///   offset 16: inner_closure (GC ptr — bitmap bit 1)
///   offset 24: inner_fn (code addr — bit 2 clear)
///   offset 32: saved_closure (GC ptr — bitmap bit 3)
///   offset 40: saved_fn (code addr — bit 4 clear)
///
/// # Safety
///
/// `closure_ptr` must point to a wrapper closure with the exact
/// layout documented above (allocated by
/// `wrap_continuation_with_outer_post_arm_k`). `args_ptr` and
/// `args_len` follow the standard CPS-call ABI: a non-null buffer of
/// `args_len` u64 slots that the caller (the trampoline) keeps
/// alive across this call. The returned `NextStep` is arena-
/// allocated and consumed by the trampoline before the next
/// dispatch (which resets the arena).
#[no_mangle]
pub unsafe extern "C" fn sigil_k_continuation_wrapper(
    closure_ptr: *mut u8,
    args_ptr: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    // SAFETY: gc-heap-ptr arithmetic (fixed-layout 5-slot wrapper closure: code_ptr@0, inner_closure@16, inner_fn@24, saved_closure@32, saved_fn@40)
    let inner_closure = *(closure_ptr.add(16) as *const *mut u8);
    // SAFETY: gc-heap-ptr arithmetic (same fixed wrapper-closure layout)
    let inner_fn = *(closure_ptr.add(24) as *const *mut u8);
    // SAFETY: gc-heap-ptr arithmetic (same fixed wrapper-closure layout)
    let saved_closure = *(closure_ptr.add(32) as *const *mut u8);
    // SAFETY: gc-heap-ptr arithmetic (same fixed wrapper-closure layout)
    let saved_fn = *(closure_ptr.add(40) as *const *mut u8);

    sigil_outer_post_arm_k_push(saved_closure, saved_fn);

    let ns = sigil_next_step_call(inner_closure, inner_fn, args_len);
    let ns_args = sigil_next_step_args_ptr(ns);
    for i in 0..args_len as usize {
        // SAFETY: gc-heap-ptr arithmetic (copying caller-provided u64 args into NextStep's args buffer; bounds enforced by args_len)
        ns_args.add(i).write(*args_ptr.add(i));
    }
    ns
}

/// Pop all OUTER_POST_ARM_K entries and build a chain of wrapper
/// closures around `(k_closure, k_fn)`. When the outermost wrapper is
/// invoked, it re-pushes entries in the original bottom-to-top order
/// before delegating to the original continuation. Returns
/// `(k_closure, k_fn)` unchanged when OUTER_POST_ARM_K is empty.
unsafe fn wrap_continuation_with_outer_post_arm_k(
    k_closure: *mut u8,
    k_fn: *mut u8,
) -> (*mut u8, *mut u8) {
    use crate::gc::sigil_alloc;

    let depth = outer_post_arm_k_stack_api::current_depth();
    if depth == 0 {
        return (k_closure, k_fn);
    }

    // Only pop entries owned by the current run_loop — entries
    // below the current entry_depth belong to enclosing run_loops
    // and would silently break their invariants if moved into the
    // wrapper chain (the parent's `outer_post_arm_k_entry_depth`
    // is set at entry; popping below it underflows the depth and
    // dereferences a null fn_ptr at the next pop).
    let entry_depth = run_loop_entry_depth_get();
    let owned = depth - entry_depth;
    if owned == 0 {
        return (k_closure, k_fn);
    }

    // CORRECTNESS — skip re-wrapping when ANY owned entry is
    // already a continuation wrapper. Not just an optimization:
    // a fresh wrap on top of an existing wrapper chain would
    // repackage the SAME saved outer-arm chain entries under a
    // new layer, so when the outer wrapper later invokes inner,
    // the inner wrapper would re-push entries already pushed by
    // the outer one — duplicating the chain on
    // OUTER_POST_ARM_K_STACK and routing the next Done through
    // the wrong (duplicated) chain step. The stack-overflow
    // symptom on json-shaped deep cross-handler recursion is the
    // visible failure mode; the silent invariant break (chain
    // step double-fire) is the load-bearing reason this guard
    // matters. The existing wrapper(s) already captured the
    // outer arm chain; let them drain naturally on their
    // run_loop's terminal.
    let wrapper_fn_addr_check = sigil_k_continuation_wrapper as *mut u8;
    if outer_post_arm_k_stack_api::has_fn_ptr_in_range(wrapper_fn_addr_check, entry_depth, depth) {
        return (k_closure, k_fn);
    }

    // Pop owned entries (top-first). entries[0] = top,
    // entries[owned-1] = bottom-of-owned (just above entry_depth).
    let mut entries: [OuterPostArmKEntry; OUTER_POST_ARM_K_STACK_SIZE] = [OuterPostArmKEntry {
        closure_ptr: ptr::null_mut(),
        fn_ptr: ptr::null_mut(),
    };
        OUTER_POST_ARM_K_STACK_SIZE];
    let mut count = 0usize;
    while count < owned {
        match outer_post_arm_k_try_pop() {
            Some(entry) => {
                entries[count] = entry;
                count += 1;
            }
            None => break,
        }
    }

    let wrapper_fn_addr = sigil_k_continuation_wrapper as *mut u8;
    let mut current_closure = k_closure;
    let mut current_fn = k_fn;

    // Iterate from top (i=0) to bottom (i=count-1). Each wrapper
    // pushes its saved entry then delegates to the previous wrapper
    // (or original k). Execution order: outermost (= bottom-saved)
    // pushes bottom entry first, innermost (= top-saved) pushes top
    // entry last → restored stack = [bottom..top] = original.
    for entry in entries.iter().take(count) {
        // bitmap=0b01010: bits 1 and 3 set (inner_closure at slot 1,
        // saved_closure at slot 3 are GC-managed pointers). Slot 0 is
        // code_ptr (null), slot 2 is inner_fn (code addr), slot 4 is
        // saved_fn (code addr).
        let h = Header::new(TAG_CLOSURE, 5, 0b01010);
        // Typed-malloc shape `(0b01010, 5)` — pre-registered as
        // `RuntimeShapeIndices::wrapper_continuation`.
        let descriptor_index = crate::gc::runtime_shape_indices().wrapper_continuation;
        let wrapper = sigil_alloc(h.raw(), 40, descriptor_index);
        *(wrapper.add(8) as *mut *mut u8) = ptr::null_mut();
        *(wrapper.add(16) as *mut *mut u8) = current_closure;
        *(wrapper.add(24) as *mut *mut u8) = current_fn;
        *(wrapper.add(32) as *mut *mut u8) = entry.closure_ptr;
        *(wrapper.add(40) as *mut *mut u8) = entry.fn_ptr;

        current_closure = wrapper;
        current_fn = wrapper_fn_addr;
    }

    (current_closure, current_fn)
}

/// Build the `NextStep::Call` that builtin handler arms (e.g.
/// `sigil_io_println_arm`) return to dispatch their continuation
/// `k(value)`.
///
/// **The 3-slot Slice A trailing-pair convention.** Continuations
/// dispatched from arm bodies follow the Phase 4e captures+ Slice A
/// shape: `[arg, post_arm_k_closure, post_arm_k_fn]`. The synth-cont
/// generated by codegen for a parent helper's `match { .. => { perform
/// e; tail } }` arm reads its `post_arm_k` pair from
/// `args_ptr+POST_ARM_K_CLOSURE_OFF` (= 8) and `args_ptr+POST_ARM_K_FN_OFF`
/// (= 16) — the same offsets user-defined arm fns load from after a
/// `lower_k_pair_call` dispatch. If the trailing slots aren't
/// initialised, the synth-cont reads garbage and the eventual
/// `Call(garbage_fn, ...)` segfaults.
///
/// **The trailing pair for builtin arms.** Builtin handlers
/// (`IO.println`, `IO.print`, `IO.read_line`, ...) sit at the top of
/// the handler stack. Every codegen path that ends up dispatched to a
/// builtin IO arm has the parent helper's own post-arm-k pair set to
/// `(null, identity)` (the Sync→Cps wrapper writes `(null, identity)`
/// at `k_closure_offset(N) / k_fn_offset(N)`; B.3 Cps→Cps direct
/// dispatch writes `(null, identity)` as the recursive callee's
/// trailing pair after pushing the surrounding chain onto
/// `OUTER_POST_ARM_K_STACK`). So the synth-cont's expected post-arm-k
/// pair is also `(null, identity)`. Identity reads `args_ptr[0]` and
/// returns `Done(value)`; the trampoline's Done branch routes
/// through `OUTER_POST_ARM_K_STACK` if any chain steps pushed entries.
///
/// Pre-fix, builtin arms allocated only one slot for `[arg]` and
/// the synth-cont read garbage at offsets 8/16 — manifested as
/// `match xs { Cons(h, _) => { perform IO.println(h); 0 } }` exiting
/// with SIGSEGV after the `IO.println` output reached stdout. The
/// natural arm-block-with-perform-then-literal-tail shape is one of
/// the most idiomatic in the language; silently violating the Slice
/// A convention here was a footgun for every recursive-print pattern
/// over `List[String]` / `List[T]` (see PR #109's review trace and
/// the Stage MOS deviation entry in `PLAN_C_DEVIATIONS.md`).
///
/// # Safety
///
/// `k_closure` and `k_fn` are caller-supplied `*mut u8` slots that
/// the trampoline will dispatch when this `NextStep::Call` is
/// returned. Both come from the arm's own `in_args` trailing slots,
/// which `sigil_perform` populated with the original perform site's
/// `(k_closure_ptr, k_fn_ptr)` arguments — non-null by codegen
/// invariant.
pub(crate) unsafe fn write_k_dispatch_value(
    k_closure: *mut u8,
    k_fn: *mut u8,
    value: u64,
) -> *mut NextStep {
    let ns = sigil_next_step_call(k_closure, k_fn, 3);
    // SAFETY: sigil_next_step_call returns a valid *mut NextStep with
    // 3 slots reserved; sigil_next_step_args_ptr returns a pointer to
    // slot 0.
    let out_args = sigil_next_step_args_ptr(ns);
    // Slot 0: the value passed to `k(arg)`.
    *out_args = value;
    // Slot 1: post_arm_k_closure = null (builtin arms sit at the top
    // of the handler stack — see the helper's docstring).
    *out_args.add(1) = 0;
    // Slot 2: post_arm_k_fn = sigil_continuation_identity. Identity
    // returns `Done(args_ptr[0])` ignoring slots 1/2 (its `args_len ==
    // 3` branch is explicitly tolerated).
    *out_args.add(2) = sigil_continuation_identity as *const () as usize as u64;
    ns
}

/// Convenience wrapper: dispatch `k(unit)` (Sigil's `Unit` is encoded
/// as `i64 0`).
#[allow(dead_code)]
pub(crate) unsafe fn write_k_dispatch_unit(k_closure: *mut u8, k_fn: *mut u8) -> *mut NextStep {
    write_k_dispatch_value(k_closure, k_fn, 0)
}

/// Plan B Task 57 — runtime-side default handler for `IO.println`.
///
/// Conforms to the Phase 4 CPS arm fn ABI:
/// `extern "C" fn(closure_ptr, in_args, args_len) -> *mut NextStep`
/// with the trailing-pair convention `in_args = [user_arg_0,
/// k_closure, k_fn]`. For `IO.println` the user arg is a heap-string
/// pointer (the same header-pointer form `sigil_string_new` produces);
/// the trailing pair carries the caller's continuation.
///
/// Behavior: read the heap-string pointer from `in_args[0]`, write
/// it to stdout via [`crate::io::sigil_println`], then build a
/// `NextStep::Call` to the trailing-pair continuation `(k_closure,
/// k_fn)` with the unit value (`i64 0`) as its single arg. The
/// trampoline dispatches to `k`, which (under default IO usage from
/// `lower_perform_to_value`) is `sigil_continuation_identity` —
/// `Done(unit)`. The `sigil_run_loop` invocation in the caller's
/// `lower_perform_to_value` then narrows the `u64` back to Unit
/// (`i8 0`) at the source-level `perform IO.println(s)` site.
///
/// The `main` shim installed at codegen-time pushes a top-level IO
/// handler frame whose op_id 0 (`println`) is set to this fn; user
/// programs that wrap IO with their own `handle ... with { IO.println
/// ... }` install a deeper frame that `sigil_perform`'s walk reaches
/// first. The default top-level frame is the safety net for programs
/// that never install their own IO handler.
///
/// # Safety
///
/// `in_args` must point to at least three readable u64 (`args_len ==
/// 3`). `in_args[0]` must be a non-null heap-string pointer
/// (returned by `sigil_string_new`); `in_args[1..3]` is the
/// trailing-pair continuation. The trampoline guarantees these
/// invariants when dispatching from a `NextStep::Call` produced by
/// codegen's perform lowering applied to `IO.println(s)`.
#[no_mangle]
pub unsafe extern "C" fn sigil_io_println_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    // 2026-05-04 return-arm-via-args lift Stage 3b — sigil_perform now
    // packs `args_len + 4` (k_closure, k_fn, return_arm_closure,
    // return_arm_fn) trailing slots, bumping this arm's args_len from
    // 3 to 5. This arm doesn't consume the return_arm pair (built-in
    // IO arms aren't part of a user handle expression); the trailing
    // two slots are read past by the user-arg unpacking.
    debug_assert!(
        args_len == 6,
        "sigil_io_println_arm: args_len must be exactly 6 (in_args = \
         [heap_string_ptr, k_closure, k_fn, return_arm_closure, return_arm_fn, return_arm_fired_ptr]); got {args_len}"
    );
    debug_assert!(
        !in_args.is_null(),
        "sigil_io_println_arm: in_args must be non-null when args_len == 6"
    );
    // SAFETY: caller (sigil_perform via the dispatched NextStep::Call)
    // guarantees in_args points to 5 readable u64. Slot 0 is the
    // heap-string pointer the user passed to `IO.println`; slots 1..3
    // are the trailing-pair continuation; slots 3..5 are the Stage-3b
    // return_arm pair (unused here).
    let heap_ptr = *in_args as *const u8;
    debug_assert!(
        !heap_ptr.is_null(),
        "sigil_io_println_arm: heap_ptr (in_args[0]) must be non-null \
         (caller's `lower_perform_to_value` lowered a String arg, which \
         flows through `sigil_string_new` returning a non-null heap header)"
    );
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;
    crate::io::sigil_println(heap_ptr);
    write_k_dispatch_unit(k_closure, k_fn)
}

/// Plan C Task 70 — runtime-side default handler for `IO.print`.
/// Companion to `sigil_io_println_arm`. Same arg shape: 1 user
/// arg (heap-string pointer) + trailing-pair k. Emits no newline.
///
/// # Safety
///
/// Same as `sigil_io_println_arm`.
#[no_mangle]
pub unsafe extern "C" fn sigil_io_print_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 6);
    debug_assert!(!in_args.is_null());
    let heap_ptr = *in_args as *const u8;
    debug_assert!(!heap_ptr.is_null());
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;
    crate::io::sigil_print(heap_ptr);
    write_k_dispatch_unit(k_closure, k_fn)
}

/// Plan C Task 70 — runtime-side default handler for `IO.read_line`.
/// Zero user args; reads a line from stdin, returns it as a fresh
/// Sigil String.
///
/// # Safety
///
/// `in_args` must satisfy the trailing-pair invariant
/// (`args_len == 2`).
#[no_mangle]
pub unsafe extern "C" fn sigil_io_read_line_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    debug_assert!(args_len == 5);
    debug_assert!(!in_args.is_null());
    let k_closure = *in_args as *mut u8;
    let k_fn = *in_args.add(1) as *mut u8;
    let line_ptr = crate::io::sigil_read_line();
    write_k_dispatch_value(k_closure, k_fn, line_ptr as u64)
}

// Plan C addendum (CLI external-system effects, EE1) —
// `sigil_io_read_file_arm` and `sigil_io_write_file_arm` were
// removed alongside their corresponding `IO.read_file` /
// `IO.write_file` ops. File operations migrate to the `Fs` effect's
// raw-shape ops, dispatched through codegen-synthesized arm fns
// that build `(Int, String)` tuples + map errors to `FsError`
// variants in stdlib `std/fs.sigil` wrappers.

/// Plan B Task 57 — runtime-side default handler for
/// `ArithError.div_by_zero`. Conforms to the Phase 4 CPS arm fn ABI.
///
/// Behavior: write `"sigil: arithmetic error: division by zero\n"`
/// to stderr, then call `std::process::exit(2)`. Function never
/// returns — the `*mut NextStep` return type is structurally
/// unreachable. Preserves Plan A2's `examples/div_by_zero.sigil`
/// user-visible behavior verbatim (same stderr banner, same exit
/// code 2). User programs that wrap arithmetic in `handle ... with
/// { ArithError.div_by_zero(k) => ... }` install a deeper frame on
/// the handler stack that intercepts the perform before it reaches
/// this default; programs that don't install their own handler
/// fall through to here.
///
/// `in_args` is unused (op takes no user args; the trailing-pair
/// `(k_closure, k_fn)` at `in_args[0..2]` is irrelevant since exit
/// never resumes). `args_len` is asserted to be 2 (the trailing
/// pair without user args) per the Phase 4 CPS arm fn ABI applied
/// to a zero-user-arg op.
///
/// # Safety
///
/// `in_args` must point to at least two readable u64 (`args_len ==
/// 2`) under the Phase 4 CPS arm fn ABI's trailing-pair convention
/// — even though this fn reads neither. The trampoline guarantees
/// the invariant when dispatching from a `NextStep::Call` produced
/// by `sigil_perform` against the top-level ArithError frame.
#[no_mangle]
pub unsafe extern "C" fn sigil_arith_error_div_by_zero_arm(
    _closure_ptr: *const u8,
    _in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    // `arith_error_default_arm` is `-> !` (`process::exit(2)`); the
    // never-type unifies with the `*mut NextStep` return type. No
    // terminator needed after the call.
    arith_error_default_arm("division by zero", args_len)
}

/// Plan B Task 57 — `ArithError.mod_by_zero` parallel of
/// `sigil_arith_error_div_by_zero_arm`. Same shape; banner reads
/// `"sigil: arithmetic error: remainder by zero\n"`. Exists as a
/// distinct symbol because the Phase 4 CPS arm fn ABI doesn't pass
/// op_id to arm fns — arm dispatch is keyed by the `set_arm` slot,
/// not by op_id read at fn entry. The two arm fns share an internal
/// helper parameterised on the message string.
///
/// # Safety
///
/// Same contract as `sigil_arith_error_div_by_zero_arm`.
#[no_mangle]
pub unsafe extern "C" fn sigil_arith_error_mod_by_zero_arm(
    _closure_ptr: *const u8,
    _in_args: *const u64,
    args_len: u32,
    _terminal_out: *mut TerminalResult,
) -> *mut NextStep {
    arith_error_default_arm("remainder by zero", args_len)
}

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
/// and exit-2 behaviour verbatim so `examples/div_by_zero.sigil`'s
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
    return_arm_closure_ptr: *mut u8,
    return_arm_fn_ptr: *mut u8,
    return_arm_fired_ptr: *mut u64,
) -> *mut NextStep {
    // Bound-check at the perform site so the abort message names the
    // offending effect/op. 2026-05-04 return-arm-via-args lift Stage
    // 3b/5 — the arm receives `args + (k_closure, k_fn, return_arm_-
    // closure, return_arm_fn, return_arm_fired_ptr)`, so the dispatched
    // arg_count is `args_len + 5`. The trailing triple carries the
    // active handle's return arm + fired_ptr forward into the arm fn's
    // args_ptr, so the post-arm-k synth-cont's closure-record
    // allocator (arm-fn body emit) can copy it into the synth-cont
    // closure record. The synth-cont's natural-exit emit (Slice B/C
    // post-arm-k Done, ConstantDone synth-cont dispatch) then loads
    // the triple from `closure_ptr` and passes it to the args-passing
    // helper variant — which gates dispatch on `*fired_ptr`.
    if args_len.saturating_add(5) > MAX_INLINE_ARGS {
        eprintln!(
            "sigil_perform: args_len {args_len} + 5 (continuation + return arm \
             triple) exceeds MAX_INLINE_ARGS ({MAX_INLINE_ARGS}) at \
             effect_id={effect_id} op_id={op_id}"
        );
        std::process::abort();
    }
    counters::incr(CounterId::HandlerWalkCount);

    let mut depth: u64 = 0;
    let top_frame = HANDLER_STACK.with(|cell| cell.get());
    let mut frame = top_frame;
    while !frame.is_null() {
        depth += 1;
        if (*frame).effect_id == effect_id {
            counters::add(CounterId::HandlerWalkDepthSum, depth);
            let crossed = frame != top_frame;
            if crossed {
                // Record the crossed frames in the TLS stack so the
                // k-pair dispatch can re-push them. Push a null sentinel
                // first, then the crossed frames outermost-first.
                CROSSED_FRAMES_STACK.with(|cell| {
                    let mut stack = cell.borrow_mut();
                    stack.push(ptr::null_mut());
                    let mut cf = top_frame;
                    while cf != frame && !cf.is_null() {
                        stack.push(cf);
                        cf = (*cf).prev;
                    }
                });
            }
            if op_id >= ((*frame).arm_count & ARM_COUNT_MASK) {
                eprintln!(
                    "sigil_perform: op_id {op_id} out of range for effect_id {effect_id} \
                     (arm_count={})",
                    (*frame).arm_count & ARM_COUNT_MASK
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
            // Plotkin fix — when this perform crosses handlers AND
            // OUTER_POST_ARM_K has entries, the entries belong to the
            // inner-handler chain that this perform crosses through.
            // Consuming them at the outer-handler arm's Done would
            // dispatch the wrong value (e.g., State arm's lambda
            // closure) to the inner arm chain (which expects body's
            // natural value). Wrap the continuation: pop all entries
            // and embed them into a chain of wrapper closures. When
            // the continuation is later invoked (by the outer arm
            // calling k(s)), the wrappers re-push the entries, then
            // the inner chain runs correctly.
            // Plotkin fix — wrap_continuation only fires when:
            //   (1) the perform crosses one or more handler frames,
            //   (2) OUTER_POST_ARM_K has entries (an inner-arm chain
            //       might be in progress that needs preservation), AND
            //   (3) at least one of the crossed frames belongs to a
            //       `resumes: many` handler.
            //
            // (3) discriminates the case that needs wrap (Plotkin's
            //   State perform crossing Amb's multi-shot frame, where
            //   Amb's chain step entries must survive the outer-arm
            //   discharge and be replayed when the captured k is
            //   invoked) from cases that don't (json's State perform
            //   crossing catch's single-shot Raise frame, where the
            //   chain entries belong to body's own chain steps and
            //   wrap would just accumulate wrappers per perform until
            //   OUTER_POST_ARM_K_STACK overflows on deep recursion).
            let crossed_is_multi_shot = if crossed {
                let mut cf = top_frame;
                let mut found = false;
                while cf != frame && !cf.is_null() {
                    if (*cf).arm_count & RESUMES_MANY_BIT != 0 {
                        found = true;
                        break;
                    }
                    cf = (*cf).prev;
                }
                found
            } else {
                false
            };
            let (actual_k_closure, actual_k_fn) =
                if crossed_is_multi_shot && outer_post_arm_k_stack_api::current_depth() > 0 {
                    wrap_continuation_with_outer_post_arm_k(k_closure_ptr, k_fn_ptr)
                } else {
                    (k_closure_ptr, k_fn_ptr)
                };

            // Build a NextStep::Call to the arm with the args followed
            // by (k_closure_ptr, k_fn_ptr, return_arm_closure_ptr,
            // return_arm_fn_ptr, return_arm_fired_ptr) packed as five
            // u64s. The arm prologue reads the first trailing pair to
            // reconstruct the continuation closure; Stage 3b extended
            // the layout with the second trailing pair (return_arm);
            // Stage 5 adds the fired_ptr slot.
            let total_args = args_len + 5;
            let ns = sigil_next_step_call(arm_closure, arm_fn, total_args);
            let ns_args = sigil_next_step_args_ptr(ns);
            // Copy user args. ns_args points into the non-GC arena;
            // args_ptr is a caller-owned u64 buffer. The offsets drive
            // value-copying loads/stores, not retained pointers.
            for i in 0..(args_len as usize) {
                // SAFETY: gc-heap-ptr arithmetic (see comment above).
                ns_args.add(i).write(*args_ptr.add(i));
            }
            // Append the five trailing slots.
            ns_args
                .add(args_len as usize)
                .write(actual_k_closure as u64);
            ns_args.add(args_len as usize + 1).write(actual_k_fn as u64);
            ns_args
                .add(args_len as usize + 2)
                .write(return_arm_closure_ptr as u64);
            ns_args
                .add(args_len as usize + 3)
                .write(return_arm_fn_ptr as u64);
            ns_args
                .add(args_len as usize + 4)
                .write(return_arm_fired_ptr as u64);
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

/// Plan D Task 111 — caller-owned terminal channel for `sigil_run_loop`.
///
/// **Layout** (`#[repr(C)]`, 16 bytes):
/// - `value: u64` at offset 0 — the terminal value (DONE's payload OR
///   DISCHARGED's payload).
/// - `tag: u64` at offset 8 — `NEXT_STEP_TAG_DONE` (= 0) or
///   `NEXT_STEP_TAG_DISCHARGED` (= 2).
///
/// **Why `tag: u64` not `u32`.** Uniform 8-byte fields keep the layout
/// simple: every slot is `Store/Load.i64`-shaped, no half-word
/// arithmetic, no padding question. The high 32 bits of `tag` are
/// unused.
///
/// **Threading discipline (post-111d).** Caller-owned: the top-level
/// `main` shim allocates a `TerminalResult` on the C stack and passes
/// its pointer to `user_main` via the trailing Sync ABI param. Every
/// Sigil user fn (Sync OR Cps) propagates the pointer to its callees
/// (Sync ABI's trailing param, Cps ABI's 4th positional param via
/// `cps_signature`). Every `sigil_run_loop` invocation receives the
/// pointer from its caller (Sync→Cps interop wrapper, custom handle
/// body-call wrapper, perform-side run_loop drive, branched k-call
/// dispatch, Slice B fallback, Phase 4g return-arm dispatch); the
/// trampoline writes `(value, tag)` to `*out` at every terminal site
/// (DONE + DISCHARGED) before returning. Codegen at handle-exit
/// queries the slot via load from `terminal_out_param + {0,8}` to
/// determine return-arm dispatch. Cross-fn discharge propagation
/// works because all writes/reads reference the SAME memory location
/// threaded through the call chain.
#[repr(C)]
pub struct TerminalResult {
    pub value: u64,
    pub tag: u64,
    /// Effect ID of the discharging arm (only meaningful when
    /// `tag == NEXT_STEP_TAG_DISCHARGED`). Written by the arm body's
    /// codegen emit (store to `terminal_out + 16` before
    /// `sigil_next_step_discharged`). Used by nested handle
    /// expressions to distinguish own-effect discharge (restore
    /// snapshot) from foreign-effect discharge (propagate).
    pub effect_id: u64,
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
/// # Plan D Task 111 — `out: *mut TerminalResult`
///
/// **Contract.** The trampoline writes the terminal's `(value, tag)`
/// pair to `*out` before returning at every terminal site (DONE and
/// DISCHARGED bypass). The slot is the **sole terminal channel**
/// post-111d — TLS-mirrored writes that ran during the 111a→111c
/// transition are removed. Codegen always passes a non-null pointer
/// (main shim allocates the root slot; every Sync/Cps/synth fn ABI
/// threads it through). **Null is an accepted ABI value** for
/// runtime unit tests that drive `sigil_run_loop` directly with
/// `ptr::null_mut()` to test dispatch shape without observing the
/// terminal — the `*out` write is skipped under null.
///
/// **Alignment.** `TerminalResult` requires 8-byte alignment (`u64`
/// fields). Callers passing non-null pointers must satisfy this. A
/// `debug_assert!` at function entry catches violations in debug
/// builds.
///
/// # Safety
///
/// `initial_step` must be a valid `*mut NextStep` produced by
/// `sigil_next_step_done` or `sigil_next_step_call`. The fns referenced
/// by any `CALL` step must satisfy the CPS calling convention.
/// `out` must be either null or a valid pointer to a writable
/// 8-byte-aligned `TerminalResult`.
/// Plan E2 Phase 3 Task 12 — public `sigil_run_loop` entrypoint.
/// Production builds wrap the trampoline body in `GC_do_blocking` so
/// Boehm treats the Sigil call chain as "inactive" for conservative
/// stack scan; the `push_other_roots` callback installed by
/// `gc::threads::install_push_other_roots_once` supplies precise
/// roots instead via the stackmap-driven walker.
///
/// `GC_do_blocking`'s C signature is
/// `void *fn(void *cd)`, so the body fn is exposed as a trampoline
/// (`sigil_run_loop_blocking_trampoline`) that threads the
/// `(initial_step, out, result)` triple through a stack-local context.
///
/// In test builds the wrap is bypassed — `cargo test` workloads
/// drive the trampoline in active GC state directly.
#[no_mangle]
pub unsafe extern "C" fn sigil_run_loop(
    initial_step: *mut NextStep,
    out: *mut TerminalResult,
) -> u64 {
    debug_assert!(
        out.is_null() || (out as usize).is_multiple_of(core::mem::align_of::<TerminalResult>()),
        "sigil_run_loop: `out` pointer must be 8-byte aligned (got {:p})",
        out
    );

    #[cfg(not(test))]
    let result = {
        let mut ctx = RunLoopBlockingCtx {
            initial_step,
            out,
            result: 0,
        };
        // Set the TLS shadow used by the alloc-elision fast path so
        // it knows this thread is parked. See `GcBlockingGuard`.
        let _gc_blocking = crate::gc::threads::GcBlockingGuard::enter();
        // SAFETY: `GC_do_blocking` is documented as stack-disciplined
        // (`gc.h:1626`). It transitions the calling thread to "GC-
        // inactive" for the duration of the trampoline body, invokes
        // the trampoline, and restores active state on return. The
        // trampoline reads/writes `ctx` through the type-erased pointer
        // we just took; the lifetime of `ctx` covers the whole call.
        // Nested run_loop calls are safe — `GC_do_blocking` is stack-
        // disciplined and may be re-entered.
        unsafe {
            crate::gc::GC_do_blocking(
                sigil_run_loop_blocking_trampoline,
                &mut ctx as *mut RunLoopBlockingCtx as *mut c_void,
            );
        }
        ctx.result
    };

    // SAFETY: caller upholds the contract on `initial_step` and `out`
    // (see fn-level safety doc on `sigil_run_loop_impl`).
    #[cfg(test)]
    let result = unsafe { sigil_run_loop_impl(initial_step, out) };

    result
}

#[cfg(not(test))]
#[repr(C)]
struct RunLoopBlockingCtx {
    initial_step: *mut NextStep,
    out: *mut TerminalResult,
    result: u64,
}

#[cfg(not(test))]
extern "C" fn sigil_run_loop_blocking_trampoline(cd: *mut c_void) -> *mut c_void {
    // SAFETY: `cd` is the `&mut RunLoopBlockingCtx` we constructed in
    // `sigil_run_loop`; its lifetime extends through the
    // `GC_do_blocking` call containing us. No other thread has access
    // to this stack-local context.
    let ctx = unsafe { &mut *(cd as *mut RunLoopBlockingCtx) };
    // SAFETY: caller of `sigil_run_loop` upheld the contract on
    // `initial_step` and `out`; we forward them unchanged.
    ctx.result = unsafe { sigil_run_loop_impl(ctx.initial_step, ctx.out) };
    std::ptr::null_mut()
}

/// Plan E2 Phase 3 Task 12 — the body of `sigil_run_loop`, factored
/// out so the public entrypoint can wrap it in `GC_do_blocking`. The
/// safety contract on `initial_step` / `out` is identical to
/// `sigil_run_loop`'s — every doc detail on that function applies
/// unchanged here.
#[inline(never)]
unsafe fn sigil_run_loop_impl(initial_step: *mut NextStep, out: *mut TerminalResult) -> u64 {
    let mut current = initial_step;
    // Plotkin fix — install this run_loop's entry depth into the
    // single TLS `RUN_LOOP_ENTRY_DEPTH` Cell so
    // `wrap_continuation_with_outer_post_arm_k` can determine which
    // OUTER_POST_ARM_K entries this run_loop owns (entries above
    // entry_depth) versus inherited from enclosing run_loops
    // (entries at-or-below entry_depth). Save the prior value into
    // our local frame; restore at every return path below so
    // nested run_loops nest correctly via C-stack save/restore.
    let prior_run_loop_entry_depth = run_loop_entry_depth_get();
    let entry_depth_for_wrap = outer_post_arm_k_stack_api::current_depth();
    run_loop_entry_depth_set(entry_depth_for_wrap);
    // Stage-6.8-followup Layer 3c — snapshot OUTER_POST_ARM_K_DEPTH at
    // run_loop entry. On the DISCHARGED bypass terminal (introduced by
    // Layer 3c to preserve algebraic-effects discharge semantics
    // through outer chain routing), drain the stack back to this depth
    // so entries pushed by synth-cont Middle steps during the bypassed
    // chain don't leak across run_loop boundaries. The Bug-2-era
    // routing path naturally pops one entry per terminal; the bypass
    // skips that pop, hence the explicit drain.
    let outer_post_arm_k_entry_depth = outer_post_arm_k_stack_api::current_depth();
    loop {
        counters::incr(CounterId::TrampolineDispatchCount);

        if current.is_null() {
            eprintln!("sigil_run_loop: null NextStep pointer");
            std::process::abort();
        }

        let tag = (*current).tag;
        match tag {
            NEXT_STEP_TAG_DONE | NEXT_STEP_TAG_DISCHARGED => {
                let v = (*current).value;
                if trace_term() {
                    let d = outer_post_arm_k_stack_api::current_depth();
                    eprintln!(
                        "[TERM-BRANCH] tag={} value={} depth={} entry_depth={}",
                        tag, v, d, outer_post_arm_k_entry_depth
                    );
                }
                // Stage-6.8-followup Layer 3c — DISCHARGED bypasses
                // outer_post_arm_k routing. Algebraic semantics of
                // discharge: when ANY arm in a handle discharges, the
                // handle terminates with the discharged value as its
                // overall — subsequent computations in the body
                // (including outer chain steps) are abandoned. The
                // existing routing logic (Bug 2 era) was designed for
                // multi-shot composition where the outer chain's
                // step_i was waiting for a post-perform value AND the
                // inner arm RESUMES (not discharges); for that case,
                // the routing correctly forwards the resumed value.
                // For the discharge case, routing through the chain
                // is wrong: it converts DISCHARGED to DONE at the
                // outermost terminal (since the routing builds a Call
                // dispatched to identity, which returns Done). When
                // `lower_k_pair_call` (called from a captured-k
                // lifted lambda outside the handle) drives a synth-
                // cont chain that discharges via an inner arm, this
                // bypass preserves the DISCHARGED tag so the call
                // site can correctly skip return arm dispatch on the
                // R-typed discharge value.
                if tag == NEXT_STEP_TAG_DISCHARGED {
                    // Plan D Task 111d — caller-owned `TerminalResult`
                    // slot is the sole terminal channel. Codegen
                    // always passes a non-null pointer (main shim
                    // allocates the root slot; every Sync/Cps/synth
                    // fn ABI threads it through). Null is tolerated
                    // here for runtime tests that drive `sigil_run_-
                    // loop` directly without observing the terminal
                    // (e.g., testing dispatch shape rather than
                    // value); they pass `ptr::null_mut()` and ignore
                    // the channel.
                    if !out.is_null() {
                        // Write value + tag but preserve effect_id
                        // (codegen's arm body already stored the
                        // discharging effect's ID before returning
                        // NextStep::Discharged).
                        (*out).value = v;
                        (*out).tag = tag as u64;
                    }
                    if trace_term() {
                        eprintln!(
                            "[TERM-DISCHARGED] write out=0x{:x} tag={} value={}",
                            out as usize, tag, v
                        );
                    }
                    // Drain outer_post_arm_k stack back to entry-time
                    // depth. Entries pushed by synth-cont Middle steps
                    // during this run_loop's chain stay leaked across
                    // run_loop boundaries otherwise. Subsequent
                    // run_loop calls would consume them via the DONE-
                    // path routing, which happens to be benign for
                    // the canonical (entries from `lower_k_pair_call`
                    // are always `(null, identity)`, so routing is
                    // identity-passthrough), but architecturally
                    // questionable for adversarial nesting and a
                    // capacity-overflow risk for deep chains.
                    //
                    // **Discipline check** (PR #39 review §5). In
                    // debug builds we assert that the current depth
                    // is `>= outer_post_arm_k_entry_depth` — i.e.,
                    // entries on the stack at terminal time are a
                    // suffix of (or equal to) what was pushed during
                    // this run_loop. A violation indicates that
                    // somewhere between entry and terminal we popped
                    // entries belonging to an outer run_loop, which
                    // would silently corrupt the parent's chain
                    // discipline. The drain itself enforces the
                    // invariant; the assertion catches the upstream
                    // bug if the invariant is ever violated by a new
                    // codegen path.
                    let current_depth = outer_post_arm_k_stack_api::current_depth();
                    debug_assert!(
                        current_depth >= outer_post_arm_k_entry_depth,
                        "sigil_run_loop terminal: outer_post_arm_k depth \
                         underflow ({current_depth} < entry-time \
                         {outer_post_arm_k_entry_depth}); a codegen path \
                         popped entries belonging to an outer run_loop"
                    );
                    outer_post_arm_k_stack_api::set_depth(outer_post_arm_k_entry_depth);
                    crate::arena::sigil_arena_reset();
                    run_loop_entry_depth_set(prior_run_loop_entry_depth);
                    return v;
                }
                // Plan B' Stage 6.7 multi-shot composition fix: before
                // returning to the wrapper, check the outer post_arm_k
                // stack. If non-empty, pop the top entry and route the
                // terminal value through that post_arm_k chain (the
                // outer arm's chain step that's waiting for the result
                // of an inner arm's enumeration). If empty, this is
                // top-level — return to wrapper.
                //
                // **Discharged routing through outer post_arm_k:** an
                // inner-arm discharge inside an outer multi-shot
                // continuation chain still feeds the outer chain's
                // expected `k(arg)` slot. The discharged value flows
                // through the outer chain identically to a Done value;
                // the discharge-vs-done distinction matters only at
                // the top-level run_loop terminal, where the handle
                // expression's outer codegen logic loads the tag from
                // the caller-owned `TerminalResult.tag` slot to
                // decide return-arm dispatch (Plan D Task 111d).
                // Plan C Task 81 — respect entry_depth on DONE-path
                // pop. Without the entry_depth gate, a nested
                // `sigil_run_loop` (e.g., the inner invoke driving
                // synth-step-2 inside `sigil_continuation_invoke`'s
                // Phase 1 when an outer chain step pushed an entry)
                // would consume entries pushed by the OUTER run_loop's
                // chain steps. The first inner DONE pop succeeds (pops
                // the outer's entry, leaving the slot null and depth
                // below entry_depth); the next inner invoke's DONE pop
                // then dereferences a null fn_ptr and segfaults. Cap
                // try_pop at entry_depth so each run_loop only consumes
                // entries it owns.
                let current_depth = outer_post_arm_k_stack_api::current_depth();
                if current_depth > outer_post_arm_k_entry_depth {
                    if let Some(entry) = outer_post_arm_k_try_pop() {
                        // Reset the arena before allocating the new Call.
                        crate::arena::sigil_arena_reset();
                        // Build Call(popped_closure, popped_fn_ptr,
                        // [terminal_value]) with args_len=1 — same shape
                        // that helper Final's `emit_dispatch_to_post_arm_k`
                        // builds for terminal post_arm_k dispatch.
                        let ns = sigil_next_step_call(entry.closure_ptr, entry.fn_ptr, 1);
                        let ns_args = sigil_next_step_args_ptr(ns);
                        // SAFETY: gc-heap-ptr arithmetic; ns_args is
                        // arena-owned and only written here.
                        ns_args.write(v);
                        current = ns;
                        continue;
                    }
                }
                // Top-level terminal: record the source tag AND the
                // value so the handle expression's outer codegen logic
                // can branch on the tag (skip return arm dispatch on
                // discharge) AND recover the value when body's
                // synchronous IR-level lowering would have overwritten
                // it with body's post-perform code's natural terminal
                // (Stage-6.8-followup Bug 1 fix).
                //
                // **Discipline check** (Round-3 review §4 symmetric
                // counterpart). The DISCHARGED bypass branch above
                // explicitly drains depth back to entry-time; the DONE
                // path's `try_pop`-then-route loop should leave depth
                // == entry-time naturally (each Middle's push paired
                // with one terminal pop via the routing loop). Assert
                // it: any future codegen path that pushes without a
                // matching terminal pop, OR that pops entries belonging
                // to an outer run_loop, would underflow / overflow this
                // check. The drain assertion on the DISCHARGED path
                // catches bypassed-leak; this assertion catches
                // routing-asymmetry — symmetric coverage of the
                // discipline.
                let current_depth = outer_post_arm_k_stack_api::current_depth();
                debug_assert!(
                    current_depth == outer_post_arm_k_entry_depth,
                    "sigil_run_loop DONE terminal: outer_post_arm_k depth \
                     mismatch ({current_depth} != entry-time \
                     {outer_post_arm_k_entry_depth}); a codegen path \
                     pushed without a matching terminal pop, OR popped \
                     entries belonging to an outer run_loop"
                );
                // Plan D Task 111d — see DISCHARGED bypass site above
                // for the channel discipline note. The `!out.is_null()`
                // check is **unreachable from generated code** post-
                // 111d (codegen always threads a non-null pointer
                // from the main shim's stack-allocated slot through
                // every Sync/Cps/synth ABI). It exists for runtime
                // unit tests that drive `sigil_run_loop` directly
                // with `ptr::null_mut()` to test dispatch shape
                // without observing the terminal channel.
                if !out.is_null() {
                    ptr::write(
                        out,
                        TerminalResult {
                            value: v,
                            tag: tag as u64,
                            effect_id: 0,
                        },
                    );
                }
                if trace_term() {
                    eprintln!(
                        "[TERM] write out=0x{:x} tag={} value={}",
                        out as usize, tag, v
                    );
                }
                // Reset the arena before returning so the next
                // top-level entry starts with a clean slate.
                crate::arena::sigil_arena_reset();
                run_loop_entry_depth_set(prior_run_loop_entry_depth);
                return v;
            }
            NEXT_STEP_TAG_CALL => {
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
                // PR #143 review observation 2 — `args_buf` zero-init is
                // load-bearing for the 2026-05-04 return-arm-via-args
                // convention. Codegen sites that pack `arg_count = N + 4`
                // (Cps user-fn calls under Stage 1) populate the
                // `(return_arm_closure, return_arm_fn)` trailing pair
                // explicitly. Sites that pack `arg_count <= N + 2`
                // (e.g., `sigil_perform`'s arm-fn dispatch with
                // `args_len + 2`, and this helper's own
                // `sigil_next_step_call(_, _, 3)` for the return arm's
                // dispatch) DON'T write the new trailing pair — they
                // rely on this zero-init so the callee's body-fn
                // natural-exit reads `(null, null)` from args_ptr and
                // emits Done. Stage 4 may revisit by making the
                // convention explicit at every emit site.
                let mut args_buf = [0u64; MAX_INLINE_ARGS as usize];
                if arg_count > 0 {
                    let src = sigil_next_step_args_ptr(current);
                    for (i, slot) in args_buf.iter_mut().enumerate().take(arg_count as usize) {
                        *slot = src.add(i).read();
                    }
                }
                if trace_call() {
                    eprintln!(
                        "[CALL] fn=0x{:x} closure=0x{:x} args={:?}",
                        fn_ptr as usize,
                        closure_ptr as usize,
                        &args_buf[..arg_count as usize]
                    );
                }
                // Reset the arena now that we've extracted the
                // dispatch info. Any in-arena pointer the caller might
                // have stashed elsewhere is invalidated by this reset
                // — that's the contract codegen relies on.
                crate::arena::sigil_arena_reset();

                // SAFETY: fn_ptr came from a NextStep::Call constructed
                // by `sigil_next_step_call` and thus reflects a CPS-color
                // fn pointer per the documented calling convention.
                // Plan D Task 111c — forward `out` as the 4th positional
                // arg so handle-exit terminal writes from inside the
                // dispatched Cps callee land in the caller-owned slot.
                let f: CpsFn = core::mem::transmute(fn_ptr);
                // Publish this dispatch's state so a GC triggered inside
                // `f` keeps the in-flight continuation rooted. Under
                // `GC_do_blocking` the trampoline's stack-local
                // `closure_ptr` + `args_buf` are below the conservative-
                // scan boundary and the precise walker only covers the
                // leaf Sigil frame — a deep auto-CPS continuation chain
                // is reachable ONLY through this state. The
                // `push_other_roots` callback scans every published frame.
                // The RAII guard brackets the call so the frame pops even
                // on an unwinding/aborting `f`, and nested
                // `sigil_run_loop`s each keep their own frame. `args_buf`
                // is a stack-local array (`[u64; MAX_INLINE_ARGS]`) on
                // this frame, valid for the whole bracketed call.
                // SAFETY: gc-heap-ptr arithmetic (args_buf is a stack-local array on this frame; this pointer is only read by the push_other_roots callback during a GC that fires within the bracketed f(...) call below, while this frame and args_buf are live; the guard pops the frame immediately after).
                let args_buf_addr = args_buf.as_ptr() as usize;
                let _inflight = crate::gc::threads::trampoline_inflight_enter(
                    closure_ptr as usize,
                    args_buf_addr,
                    arg_count,
                );
                // SAFETY: gc-heap-ptr arithmetic (args_buf is a stack-local array, not GC-managed; pointer is consumed within this call before args_buf can be dropped).
                current = f(closure_ptr, args_buf.as_ptr(), arg_count, out);
                drop(_inflight);
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

// Re-export from sigil-abi so callers inside this crate keep the
// short name. The static-descriptor-table refactor promoted these to
// `pub const fn` in `sigil-abi::effect` so codegen can register the
// 14 handler-frame shapes at compile time without duplicating the
// bitmap derivation.
//
// The runtime entry point (`sigil_handler_frame_new_with_resumes_many`)
// is the load-bearing arm_count-bounds checker — it rejects
// `arm_count > MAX_HANDLER_ARMS` BEFORE these helpers are called,
// so the cap-overflow UB that the prior `debug_assert!` guarded is
// already unreachable in this caller. (Tests of the helpers
// directly stay bounded to MAX_HANDLER_ARMS; see the
// `handler_frame_pointer_bitmap_marks_correct_words` test.)
use sigil_abi::effect::{handler_frame_payload_bytes, handler_frame_pointer_bitmap};

/// Pointer to the start of the variable-length arms array on `frame`.
/// Each arm slot is two pointers: `(fn_ptr, closure_ptr)`.
///
/// # Safety
///
/// `frame` must be non-null and point to a properly-allocated
/// HandlerFrame.
#[inline]
unsafe fn arms_base_ptr(frame: *mut HandlerFrame) -> *mut *mut u8 {
    // SAFETY: gc-heap-ptr arithmetic (the result is computed once per
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
        CROSSED_FRAMES_STACK.with(|cell| cell.borrow_mut().clear());
    }

    fn ensure_gc() {
        crate::gc::sigil_gc_init();
    }

    /// Directly exercise `rereg_outer_post_arm_k_root`: invoke the real
    /// helper through a move, an unchanged-buffer no-op, a second move, and
    /// the first-registration (null-old) path, asserting after each call
    /// both halves of its contract:
    ///   - the GC root-set mutation it issued — the *old extent removed* and
    ///     the *new extent added*, in that order, and *nothing* on the no-op
    ///     path — captured via the test-only [`REROOT_GC_OP_LOG`]; and
    ///   - the tracked last-rooted extent (`OUTER_POST_ARM_K_LAST_ROOTED`),
    ///     which the helper advances in lockstep with those mutations.
    ///
    /// Why no real Boehm calls: under `cfg(test)` the helper records its
    /// add/remove decision instead of calling `GC_add_roots`/
    /// `GC_remove_roots` (see `reroot_gc_remove`/`reroot_gc_add`). Issuing
    /// those on synthetic extents in the shared cargo-test process corrupts
    /// Boehm's dynamic root table — it rounds/coalesces ranges — and SIGSEGVs
    /// at the next collection (the failure mode every prior real-Boehm
    /// attempt hit, on both Linux and macOS). Recording the decision asserts
    /// the exact "old removed, new present" contract the spec asks for with
    /// zero GC-table risk, so this test is portable and crash-proof. The
    /// extents are never dereferenced — only their addresses are recorded —
    /// so they need no backing memory and need not be page-mapped.
    #[test]
    fn rereg_outer_post_arm_k_root_transitions_gc_root_on_buffer_move() {
        let p = |addr: usize| addr as *mut c_void;
        // Three well-separated synthetic extents standing in for the stack
        // backing buffer before and after two reallocations.
        let (old_start, old_end) = (p(0x1000), p(0x2000));
        let (new_start, new_end) = (p(0x3000), p(0x4000));
        let (third_start, third_end) = (p(0x5000), p(0x6000));

        // Start from a known tracker state and an empty op log. Both are
        // thread-locals that can carry over from an earlier test on the same
        // thread under `--test-threads=1`, so seed them explicitly rather
        // than rely on the initial `(0, 0)`.
        reroot_gc_clear_log();
        outer_post_arm_k_stack_api::set_last_rooted(old_start, old_end);

        // (1) Buffer moved old -> new: old root removed, then new root added.
        rereg_outer_post_arm_k_root(old_start, old_end, new_start, new_end);
        assert_eq!(
            reroot_gc_take_log(),
            vec![
                RerootGcOp::Remove(old_start as usize, old_end as usize),
                RerootGcOp::Add(new_start as usize, new_end as usize),
            ],
            "move must drop the old root before adding the new one"
        );
        assert_eq!(
            outer_post_arm_k_stack_api::last_rooted(),
            (new_start as usize, new_end as usize),
            "after move, last-rooted extent must be the new buffer"
        );

        // (2) Buffer unchanged: no-op path — no GC mutation, tracker frozen.
        rereg_outer_post_arm_k_root(new_start, new_end, new_start, new_end);
        assert!(
            reroot_gc_take_log().is_empty(),
            "unchanged buffer must issue no GC root mutation"
        );
        assert_eq!(
            outer_post_arm_k_stack_api::last_rooted(),
            (new_start as usize, new_end as usize),
            "no-op must leave the last-rooted extent untouched"
        );

        // (3) Buffer moved new -> third: new root removed, then third added.
        rereg_outer_post_arm_k_root(new_start, new_end, third_start, third_end);
        assert_eq!(
            reroot_gc_take_log(),
            vec![
                RerootGcOp::Remove(new_start as usize, new_end as usize),
                RerootGcOp::Add(third_start as usize, third_end as usize),
            ],
            "second move must drop the new root before adding the third"
        );
        assert_eq!(
            outer_post_arm_k_stack_api::last_rooted(),
            (third_start as usize, third_end as usize),
            "second move must advance the last-rooted extent to the third buffer"
        );

        // (4) First-ever registration: nothing rooted yet (null old) means
        // add-only, never a remove of a phantom extent.
        reroot_gc_clear_log();
        outer_post_arm_k_stack_api::set_last_rooted(ptr::null_mut(), ptr::null_mut());
        rereg_outer_post_arm_k_root(ptr::null_mut(), ptr::null_mut(), old_start, old_end);
        assert_eq!(
            reroot_gc_take_log(),
            vec![RerootGcOp::Add(old_start as usize, old_end as usize)],
            "first registration (null old) must add only, never remove"
        );

        // Leave the thread-local tracker clean for any thread-reusing test.
        outer_post_arm_k_stack_api::set_last_rooted(ptr::null_mut(), ptr::null_mut());
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
        let v = unsafe { sigil_run_loop(ns, std::ptr::null_mut()) };
        let dispatches_after = counters::read(CounterId::TrampolineDispatchCount);
        assert_eq!(v, 99);
        assert_eq!(dispatches_after - dispatches_before, 1);
        reset_state();
    }

    /// Plan D Task 111a — verify `sigil_run_loop` writes to caller-
    /// passed `*out` at the DONE terminal. Pins the new ABI's contract
    /// so PRs 111b/c can rely on it without proving the runtime side
    /// works on top of their codegen-side diff.
    #[test]
    fn run_loop_done_terminal_writes_caller_passed_terminal_result() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let mut term = TerminalResult {
            value: 0,
            tag: 0,
            effect_id: 0,
        };
        let ns = unsafe { sigil_next_step_done(0xDEAD_BEEF_u64) };
        let v = unsafe { sigil_run_loop(ns, &mut term as *mut _) };
        assert_eq!(v, 0xDEAD_BEEF_u64);
        assert_eq!(
            term.value, 0xDEAD_BEEF_u64,
            "*out.value must hold the DONE terminal's value"
        );
        assert_eq!(
            term.tag, NEXT_STEP_TAG_DONE as u64,
            "*out.tag must be NEXT_STEP_TAG_DONE on DONE-terminal"
        );
        reset_state();
    }

    /// Plan D Task 111a — verify `sigil_run_loop` writes to caller-
    /// passed `*out` at the DISCHARGED bypass terminal.
    #[test]
    fn run_loop_discharged_terminal_writes_caller_passed_terminal_result() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let mut term = TerminalResult {
            value: 0,
            tag: 0,
            effect_id: 0,
        };
        let ns = unsafe { sigil_next_step_discharged(0xCAFE_BABE_u64) };
        let v = unsafe { sigil_run_loop(ns, &mut term as *mut _) };
        assert_eq!(v, 0xCAFE_BABE_u64);
        assert_eq!(
            term.value, 0xCAFE_BABE_u64,
            "*out.value must hold the DISCHARGED terminal's value"
        );
        assert_eq!(
            term.tag, NEXT_STEP_TAG_DISCHARGED as u64,
            "*out.tag must be NEXT_STEP_TAG_DISCHARGED on DISCHARGED-bypass terminal"
        );
        reset_state();
    }

    /// Plan D Task 111a — verify the null-`*out` ABI is accepted and
    /// the trampoline still produces correct DONE values via the TLS
    /// path (the path codegen actually uses in 111a). Sanity check
    /// that pre-fix-state still works exactly as before.
    #[test]
    fn run_loop_null_out_does_not_panic_and_returns_value() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let ns = unsafe { sigil_next_step_done(7_u64) };
        let v = unsafe { sigil_run_loop(ns, std::ptr::null_mut()) };
        assert_eq!(v, 7);
        reset_state();
    }

    /// Plan D Task 111a — verify the trampoline does NOT write `*out`
    /// during non-terminal iteration (only at terminal time). Pins the
    /// contract that *out is touched at most once per `sigil_run_loop`
    /// invocation, on terminal.
    #[test]
    fn run_loop_does_not_write_out_during_non_terminal_iteration() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        // Pre-fill *out with a sentinel that the trampoline must not
        // overwrite during non-terminal iteration.
        let mut term = TerminalResult {
            value: 0xFFFF_FFFF_FFFF_FFFF_u64,
            tag: 0xAAAA_AAAA_AAAA_AAAA_u64,
            effect_id: 0,
        };
        // Drive a Call → Done sequence; the Call iteration is
        // non-terminal, so *out must stay at sentinel until the Done
        // terminal overwrites with the actual value.
        let ns = unsafe { sigil_next_step_call(ptr::null_mut(), cps_done_plus_one as *mut u8, 1) };
        let args = unsafe { sigil_next_step_args_ptr(ns) };
        unsafe { args.write(41) };
        let v = unsafe { sigil_run_loop(ns, &mut term as *mut _) };
        assert_eq!(v, 42);
        // After the terminal, *out reflects the DONE value (42, DONE
        // tag). If *out had been written during the non-terminal Call
        // iteration, we'd see some intermediate state — the existing
        // contract guarantees we don't.
        assert_eq!(term.value, 42, "*out.value at terminal");
        assert_eq!(term.tag, NEXT_STEP_TAG_DONE as u64, "*out.tag at terminal");
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
        let v = unsafe { sigil_run_loop(ns, std::ptr::null_mut()) };
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
        let v = unsafe { sigil_run_loop(ns, std::ptr::null_mut()) };
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
        // SAFETY: gc-heap-ptr arithmetic (stack array, non-GC, outlives the call).
        let args_ptr = args.as_ptr();
        let ns = unsafe { sigil_continuation_identity(ptr::null(), args_ptr, 1, ptr::null_mut()) };
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
        let v = unsafe { sigil_run_loop(ns, std::ptr::null_mut()) };
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
        // scope), identity sees args_len=3 with trailing pair set to
        // `(null, &sigil_continuation_identity)` — identity is the
        // terminal continuation and recognises its own self address
        // as a no-op trailing fn.
        //
        // Identity must read `args_ptr[0]`, observe the (null,
        // identity) trailing pair, and produce
        // `NextStep::Done(args_ptr[0])`.
        //
        // Plotkin fix: identity NOW dispatches through the trailing
        // pair when `post_arm_k_fn` is non-null AND not its own
        // self-address (required for tail-perform body shapes where
        // the chain step_0 isn't pushed by the body's own chain).
        // This test pins the self-address-skip contract; the
        // sibling test
        // `continuation_identity_dispatches_through_non_identity_trailing_fn`
        // pins the dispatch contract.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let known: u64 = 0xFEEDFACE_DEADBEEF;
        let identity_self_addr = sigil_continuation_identity as *const () as usize as u64;
        // [arg, post_arm_k_closure (null), post_arm_k_fn (identity)]
        let args: [u64; 3] = [known, 0, identity_self_addr];
        // SAFETY: gc-heap-ptr arithmetic (stack array, non-GC, outlives the call).
        let args_ptr = args.as_ptr();
        let ns = unsafe { sigil_continuation_identity(ptr::null(), args_ptr, 3, ptr::null_mut()) };
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
    fn continuation_identity_dispatches_through_non_identity_trailing_fn() {
        // Plotkin fix — when identity is dispatched as the k_fn for
        // an arm's k(arg) call with a multi-shot chain trailing
        // pair (a non-null, non-identity post_arm_k_fn), identity
        // forwards to the chain step instead of returning Done.
        // This is the load-bearing path for tail-perform body
        // shapes (e.g. `body() => perform Effect.op()`) where the
        // chain step_0 isn't pushed by the body's own chain machinery.
        //
        // Verify: identity returns NextStep::Call(post_arm_k_closure,
        // post_arm_k_fn, [args[0]]) so the trampoline dispatches the
        // chain step with the captured value as its single arg.
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        let known: u64 = 0xFEEDFACE_DEADBEEF;
        // Arbitrary non-null, non-identity sentinel addresses — the
        // dispatch only inspects them as opaque pointer values.
        let post_arm_k_closure: u64 = 0x1000;
        let post_arm_k_fn: u64 = 0x2000;
        let args: [u64; 3] = [known, post_arm_k_closure, post_arm_k_fn];
        // SAFETY: gc-heap-ptr arithmetic (stack array, non-GC, outlives the call).
        let args_ptr = args.as_ptr();
        let ns = unsafe { sigil_continuation_identity(ptr::null(), args_ptr, 3, ptr::null_mut()) };
        unsafe {
            assert_eq!((*ns).tag, NEXT_STEP_TAG_CALL);
            assert_eq!((*ns).closure_ptr as u64, post_arm_k_closure);
            assert_eq!((*ns).fn_ptr as u64, post_arm_k_fn);
            assert_eq!((*ns).arg_count, 1);
            // arg_count=1 means args_ptr[0] holds the captured value.
            let dispatched_args = sigil_next_step_args_ptr(ns);
            assert_eq!(*dispatched_args, known);
        }
        reset_state();
    }

    // Plan B Task 55, Phase 4e captures+ Slice B polish — the
    // tightened arity assert from Slice A polish (`0dce45f`) cannot
    // be unit-tested via `#[should_panic]`: `sigil_continuation_identity`
    // is `extern "C"` and Rust aborts (non-unwinding) on panics
    // across the C ABI boundary, so the test framework's panic
    // catch never fires. The assert's contract — accept exactly
    // `args_len == 1` or `args_len == 3`, panic on any other shape
    // in debug builds — is documented at the assert site (search
    // `args_len must be exactly 1`); the codegen-side test surface
    // for "no other args_len is reachable" is the existing
    // `continuation_identity_returns_done_with_args_ptr_value`
    // (args_len=1) and
    // `continuation_identity_tolerates_args_len_3_trailing_pair_convention`
    // (args_len=3) tests, plus the e2e suite which exercises both
    // paths via PR #26 captures-bearing tests + Slice B's
    // `slice_b_arm_body_let_then_pure_tail_post_arm_k_synth_fn_fires`.

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
        assert_eq!(args_len, 6); // raised_value + (k_closure, k_fn) + (return_arm_closure, return_arm_fn, return_arm_fired_ptr) per Stage 3b/5
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
                /* return_arm_closure_ptr */ ptr::null_mut(),
                /* return_arm_fn_ptr */ ptr::null_mut(),
                /* return_arm_fired_ptr */ ptr::null_mut(),
            );
            let walk_count_after = counters::read(CounterId::HandlerWalkCount);
            let depth_sum_after = counters::read(CounterId::HandlerWalkDepthSum);
            assert_eq!(walk_count_after - walk_count_before, 1);
            assert_eq!(depth_sum_after - depth_sum_before, 1);

            // Dispatch the resulting NextStep through the trampoline.
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let depth_after = counters::read(CounterId::HandlerWalkDepthSum);
            // Outer is on top, target is one below; walk depth = 2.
            assert_eq!(depth_after - depth_before, 2);
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
            // 2026-05-04 return-arm-via-args lift Stage 3b — args_len
            // bumped from `N + 2` → `N + 4` → `N + 5` to include the
            // second trailing pair (return_arm_closure, return_arm_fn)
            // and Stage 5's third trailing slot (return_arm_fired_ptr).
            assert_eq!(args_len, 7); // 2 user args + (k_closure, k_fn) + (return_arm_closure, return_arm_fn, return_arm_fired_ptr)
                                     // SAFETY: gc-heap-ptr arithmetic (args_ptr points at a
                                     // non-GC arena buffer; reads are value loads, no GC retention).
            assert_eq!(*args_ptr, 100);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(1), 200);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(2) as usize, 0xCC);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(3) as usize, 0xDD);
            // Stage 3b — slots 4, 5 are (return_arm_closure, return_arm_fn);
            // the test passes nulls.
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(4) as usize, 0);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(5) as usize, 0);
            sigil_next_step_done(0)
        }

        let frame = unsafe { sigil_handler_frame_new(7, 1) };
        unsafe {
            sigil_handler_frame_set_arm(frame, 0, arm_layout_check as *mut u8, ptr::null_mut());
            sigil_handle_push(frame);
            let user_args = [100u64, 200u64];
            // user_args is a stack local; the runtime copies bytes via the pointer.
            // SAFETY: gc-heap-ptr arithmetic (user_args is a stack local).
            let user_args_ptr = user_args.as_ptr();
            let ns = sigil_perform(
                7,
                0,
                user_args_ptr,
                2,
                0xCC as *mut u8,
                0xDD as *mut u8,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let _ = sigil_run_loop(ns, std::ptr::null_mut());
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
        // Max (14) arms → bits 5, 7, 9, ..., 31.
        let max = handler_frame_pointer_bitmap(MAX_HANDLER_ARMS);
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
            handler_frame_payload_bytes(MAX_HANDLER_ARMS),
            32 + 16 * MAX_HANDLER_ARMS as usize
        );
    }

    #[test]
    fn handler_frame_return_offsets_match_abi_constants() {
        // Plan B Task 55 Phase 4g: codegen reads `return_fn` and
        // `return_closure` directly off `frame_1_ptr_snapshot` at
        // handle exit using the offset constants in `sigil_abi::effect`.
        // This test pins the runtime struct's `#[repr(C)]` field
        // offsets to match those constants — a future struct reorder
        // breaks this test rather than silently miscompiling in
        // codegen.
        assert_eq!(
            core::mem::offset_of!(HandlerFrame, return_fn),
            sigil_abi::effect::HANDLER_FRAME_RETURN_FN_OFF as usize
        );
        assert_eq!(
            core::mem::offset_of!(HandlerFrame, return_closure),
            sigil_abi::effect::HANDLER_FRAME_RETURN_CLOSURE_OFF as usize
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
            let ns = sigil_perform(
                100,
                0,
                user_args_ptr,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let depth_after = counters::read(CounterId::HandlerWalkDepthSum);
            assert_eq!(
                depth_after - depth_before,
                3,
                "expected walk depth 3 (outer + middle + target)"
            );
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
                let _ = crate::gc::sigil_alloc(h.raw(), 8, u32::MAX);
            }
            // Force a full collection.
            crate::gc::GC_gcollect();
            // perform succeeds iff the frame is still reachable.
            let arg = 9u64;
            let arg_ptr = &arg as *const u64;
            let ns = sigil_perform(
                4242,
                0,
                arg_ptr,
                1,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
            let closure = crate::gc::sigil_alloc(h.raw(), 8, u32::MAX);
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
                let _ = crate::gc::sigil_alloc(h.raw(), 8, u32::MAX);
            }
            crate::gc::GC_gcollect();

            // Dispatch through the arm. The trampoline invokes
            // arm_read_closure_sentinel with closure_ptr = the original
            // closure; it reads the sentinel and returns it.
            let ns = sigil_perform(
                7777,
                0,
                ptr::null(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            let result = sigil_run_loop(ns, std::ptr::null_mut());
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
        let target_closure = crate::gc::sigil_alloc(h.raw(), 8, u32::MAX);
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
            let result = sigil_run_loop(initial, std::ptr::null_mut());
            assert_eq!(result, STRESS_CLOSURE_SENTINEL);
        }
        reset_state();
    }

    // ---------------------------------------------------------------
    // Plan B' Stage 6.7 multi-shot composition fix — outer post_arm_k
    // stack discipline tests. Direct unit coverage of push / pop
    // balance, GC root coverage, and trampoline-Done routing through
    // a popped entry. R6 review finding: e2e coverage alone is thin
    // for a TLS-rooted heap-pointer mechanism with non-trivial GC
    // interaction.
    // ---------------------------------------------------------------

    fn reset_outer_post_arm_k_stack() {
        // Drain any lingering entries from prior tests in the same
        // thread. The runtime's OUTER_POST_ARM_K_DEPTH is reset on
        // unregister; tests that share a thread must clear between
        // iterations.
        outer_post_arm_k_stack_api::set_depth(0);
    }

    // CPS-color terminal: returns Done(value-passed-in-args_ptr[0] + 1).
    // Wired as the popped fn pointer in the trampoline-routes-Done
    // test; verifies the trampoline writes Done's value to args_ptr
    // [0] and dispatches the popped fn.
    unsafe extern "C" fn cps_done_with_arg(
        _closure: *mut u8,
        args_ptr: *const u64,
        args_len: u32,
    ) -> *mut NextStep {
        assert_eq!(args_len, 1);
        let v = *args_ptr;
        sigil_next_step_done(v + 1)
    }

    // CPS-color initial: pushes a (heap_closure, cps_done_with_arg)
    // pair onto the outer post_arm_k stack, then returns Done(42). The
    // trampoline's Done branch should pop the pushed entry and route
    // 42 through cps_done_with_arg, which returns Done(43). The
    // closure_ptr is GC-allocated so the TLS-rooted scan range never
    // sees a synthetic non-heap pointer.
    unsafe extern "C" fn cps_push_then_done(
        _closure: *mut u8,
        _args_ptr: *const u64,
        _args_len: u32,
    ) -> *mut NextStep {
        let heap_closure = crate::gc::sigil_alloc(0, 64, u32::MAX);
        sigil_outer_post_arm_k_push(heap_closure, cps_done_with_arg as *mut u8);
        sigil_next_step_done(42)
    }

    // Each test re-execs the test binary in a subprocess (matches the
    // pattern used by the GC-stress tests above) so the scenario runs
    // against fresh Boehm state. Without the subprocess pattern, a
    // prior test in the same process can leave Boehm root state that
    // segfaults the next allocation. Inner mode (env var set) runs the
    // body; outer mode just spawns the child.

    #[test]
    fn outer_post_arm_k_push_pop_round_trips_one_entry() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_push_pop_round_trips_one_entry");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        // Real Boehm-allocated buffer as `closure_ptr`: the TLS-rooted
        // stack range gets scanned conservatively, so we want pointers
        // that resolve to valid heap blocks. The buffer's contents are
        // never read; only pointer-identity round-trip is checked.
        let closure_ptr = crate::gc::sigil_alloc(0, 64, u32::MAX);
        let fn_ptr = cps_done_with_arg as *mut u8; // text-segment pointer.
        unsafe {
            sigil_outer_post_arm_k_push(closure_ptr, fn_ptr);
        }
        let popped = match outer_post_arm_k_try_pop() {
            Some(e) => e,
            None => {
                eprintln!("test bug: push then try_pop returned None");
                std::process::abort();
            }
        };
        assert_eq!(popped.closure_ptr, closure_ptr);
        assert_eq!(popped.fn_ptr, fn_ptr);
        // Stack now empty.
        assert!(
            outer_post_arm_k_try_pop().is_none(),
            "second try_pop on emptied stack returns None"
        );
        reset_outer_post_arm_k_stack();
    }

    #[test]
    fn outer_post_arm_k_pop_on_empty_returns_none() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_pop_on_empty_returns_none");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        assert!(
            outer_post_arm_k_try_pop().is_none(),
            "try_pop on freshly-reset stack returns None"
        );
        reset_outer_post_arm_k_stack();
    }

    #[test]
    fn outer_post_arm_k_stack_lifo_order() {
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_stack_lifo_order");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        let closures: Vec<*mut u8> = (0..3)
            .map(|_| crate::gc::sigil_alloc(0, 64, u32::MAX))
            .collect();
        let fns: [*mut u8; 3] = [
            cps_done_with_arg as *mut u8,
            cps_done_plus_one as *mut u8,
            cps_call_then_plus_one as *mut u8,
        ];
        unsafe {
            for (c, f) in closures.iter().zip(fns.iter()) {
                sigil_outer_post_arm_k_push(*c, *f);
            }
        }
        // Pop in reverse (LIFO).
        for (c, f) in closures.iter().zip(fns.iter()).rev() {
            let popped = match outer_post_arm_k_try_pop() {
                Some(e) => e,
                None => {
                    eprintln!("test bug: try_pop returned None mid-LIFO walk");
                    std::process::abort();
                }
            };
            assert_eq!(popped.closure_ptr, *c);
            assert_eq!(popped.fn_ptr, *f);
        }
        assert!(outer_post_arm_k_try_pop().is_none());
        reset_outer_post_arm_k_stack();
    }

    #[test]
    fn outer_post_arm_k_stack_fills_to_cap_minus_one() {
        // Push OUTER_POST_ARM_K_STACK_SIZE - 1 entries; the initial cap is
        // OUTER_POST_ARM_K_STACK_SIZE. Verifies that pushing up to the cap-1
        // stays safe + LIFO and preserves the invariant that all live
        // continuations remain rooted across any reallocations.
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_stack_fills_to_cap_minus_one");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        let n = OUTER_POST_ARM_K_STACK_SIZE - 1;
        let closures: Vec<*mut u8> = (0..n)
            .map(|_| crate::gc::sigil_alloc(0, 64, u32::MAX))
            .collect();
        let fn_ptr = cps_done_with_arg as *mut u8;
        unsafe {
            for c in closures.iter() {
                sigil_outer_post_arm_k_push(*c, fn_ptr);
            }
        }
        for c in closures.iter().rev() {
            let popped = match outer_post_arm_k_try_pop() {
                Some(e) => e,
                None => {
                    eprintln!("test bug: try_pop returned None at depth in cap-fill walk");
                    std::process::abort();
                }
            };
            assert_eq!(popped.closure_ptr, *c);
            assert_eq!(popped.fn_ptr, fn_ptr);
        }
        assert!(outer_post_arm_k_try_pop().is_none());
        reset_outer_post_arm_k_stack();
    }

    #[test]
    fn outer_post_arm_k_stack_grows_beyond_cap() {
        // Push 300+ entries, well beyond OUTER_POST_ARM_K_STACK_SIZE (256).
        // Verifies that the stack grows unbounded, re-rooting on buffer
        // reallocation/growth so all live continuations remain GC-rooted.
        // This is the critical acceptance criterion for the growth task:
        // deep-CPS workloads that previously aborted at 256 now succeed.
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_stack_grows_beyond_cap");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        let n = 300;
        let closures: Vec<*mut u8> = (0..n)
            .map(|_| crate::gc::sigil_alloc(0, 64, u32::MAX))
            .collect();
        let fn_ptr = cps_done_with_arg as *mut u8;
        unsafe {
            for c in closures.iter() {
                sigil_outer_post_arm_k_push(*c, fn_ptr);
            }
        }
        for c in closures.iter().rev() {
            let popped = match outer_post_arm_k_try_pop() {
                Some(e) => e,
                None => {
                    eprintln!("test bug: try_pop returned None at depth in beyond-cap walk");
                    std::process::abort();
                }
            };
            assert_eq!(popped.closure_ptr, *c);
            assert_eq!(popped.fn_ptr, fn_ptr);
        }
        assert!(outer_post_arm_k_try_pop().is_none());
        reset_outer_post_arm_k_stack();
    }

    #[test]
    fn trampoline_done_routes_through_popped_outer_post_arm_k() {
        // Round-trip test: push a fn pointer onto the outer post_arm_k
        // stack, then return Done from the same dispatch. Trampoline's
        // Done branch pops, re-dispatches Call(popped_closure,
        // popped_fn, [42]); popped_fn is `cps_done_with_arg` which
        // returns Done(43). Trampoline observes Done(43) with empty
        // stack and returns 43.
        if !in_stress_subprocess() {
            run_stress_in_subprocess("trampoline_done_routes_through_popped_outer_post_arm_k");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        unsafe {
            let initial = sigil_next_step_call(ptr::null_mut(), cps_push_then_done as *mut u8, 0);
            let result = sigil_run_loop(initial, std::ptr::null_mut());
            // 42 + 1 = 43; cps_done_with_arg adds 1 to its arg.
            assert_eq!(result, 43);
        }
        // Stack must be empty at end (1 push + 1 pop).
        assert!(
            outer_post_arm_k_try_pop().is_none(),
            "trampoline Done branch must have popped the pushed entry"
        );
        reset_outer_post_arm_k_stack();
    }

    /// Pin the Slice A 3-slot trailing-pair convention for builtin
    /// IO arms. `write_k_dispatch_value` must allocate a 3-slot args
    /// buffer (not 1) and write `[value, null, &identity]` so the
    /// synth-cont generated by codegen for compound-match-with-arm-
    /// perform shapes reads valid post-arm-k slots at offsets 8 and
    /// 16. Pre-fix, the helper allocated only 1 slot and the synth-
    /// cont read garbage at offsets 8/16 — manifested as
    /// `match xs { Cons(h, _) => { perform IO.println(h); 0 } }`
    /// SIGSEGVing after IO.println's output reached stdout.
    #[test]
    fn write_k_dispatch_value_emits_three_slot_trailing_pair() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        unsafe {
            let ns = write_k_dispatch_value(ptr::null_mut(), ptr::null_mut(), 0xDEAD_BEEF);
            assert_eq!((*ns).tag, NEXT_STEP_TAG_CALL);
            assert_eq!(
                (*ns).arg_count,
                3,
                "builtin arm dispatch must allocate 3 slots per the Slice A trailing-pair \
                 convention so the synth-cont's args_ptr+POST_ARM_K_CLOSURE_OFF / FN_OFF \
                 reads land on initialised memory"
            );
            let args = sigil_next_step_args_ptr(ns);
            assert_eq!(args.read(), 0xDEAD_BEEF, "slot 0 = the k(value) argument");
            assert_eq!(
                args.add(1).read(),
                0,
                "slot 1 = post_arm_k_closure (null for builtin handlers — top of \
                 handler stack)"
            );
            assert_eq!(
                args.add(2).read(),
                sigil_continuation_identity as *const () as usize as u64,
                "slot 2 = post_arm_k_fn (sigil_continuation_identity for builtin \
                 handlers — terminal Done(value) on synth-cont's tail dispatch)"
            );
        }
        reset_state();
    }

    /// Companion to the helper-level test — exercise `sigil_io_println_arm`
    /// directly with a real heap-string and verify the dispatched
    /// `NextStep::Call` matches the 3-slot Slice A convention.
    ///
    /// 2026-05-04 return-arm-via-args lift Stage 3b/5 — `sigil_perform`
    /// now packs `args_len = N + 5` (5 trailing slots: k_pair +
    /// return_arm triple including fired_ptr). For IO.println: 1 user
    /// arg + 5 trailing = 6. The arm doesn't consume return_arm /
    /// fired_ptr slots; they're read past.
    #[test]
    fn io_println_arm_emits_three_slot_trailing_pair() {
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        reset_state();
        unsafe {
            // Allocate a one-character heap-string so `sigil_println`
            // has something legal to consume; capturing stdout in a
            // unit test is brittle, but the println side-effect
            // doesn't matter for this assertion — we're pinning the
            // outbound NextStep shape only.
            // SAFETY: gc-heap-ptr arithmetic (transient byte-pointer into a static UTF-8 source slice).
            let s = crate::gc::sigil_string_new(b"x".as_ptr(), 1);
            // Build the in_args buffer the trampoline would pass to
            // the arm: `[heap_string_ptr, k_closure, k_fn,
            // return_arm_closure, return_arm_fn, return_arm_fired_ptr]`.
            // We use distinguishable sentinel values for the k pair so
            // the arm's read of slots 1/2 is observable. Return-arm
            // slots 3/4/5 stay null.
            let in_args: [u64; 6] = [s as u64, 0xC10C_u64, 0xF00F_u64, 0u64, 0u64, 0u64];
            // SAFETY: gc-heap-ptr arithmetic (transient stack-buffer address handed to the arm for the call duration; args_len=6 matches the local array per Stage 5).
            let ns = sigil_io_println_arm(ptr::null(), in_args.as_ptr(), 6, std::ptr::null_mut());
            assert_eq!((*ns).tag, NEXT_STEP_TAG_CALL);
            assert_eq!(
                (*ns).arg_count,
                3,
                "sigil_io_println_arm's NextStep must carry 3 args for the synth-cont \
                 reading post-arm-k at offsets 8/16"
            );
            assert_eq!(
                (*ns).closure_ptr as usize,
                0xC10C,
                "k_closure forwarded from in_args[1]"
            );
            assert_eq!(
                (*ns).fn_ptr as usize,
                0xF00F,
                "k_fn forwarded from in_args[2]"
            );
            let out_args = sigil_next_step_args_ptr(ns);
            assert_eq!(
                out_args.read(),
                0,
                "slot 0 = unit (i64 0) — IO.println's declared Unit return"
            );
            assert_eq!(
                out_args.add(1).read(),
                0,
                "slot 1 = post_arm_k_closure (null)"
            );
            assert_eq!(
                out_args.add(2).read(),
                sigil_continuation_identity as *const () as usize as u64,
                "slot 2 = post_arm_k_fn (&sigil_continuation_identity)"
            );
        }
        reset_state();
    }
}
