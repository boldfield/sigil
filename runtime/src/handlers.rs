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

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr;

use sigil_abi::effect::{NEXT_STEP_TAG_CALL, NEXT_STEP_TAG_DISCHARGED, NEXT_STEP_TAG_DONE};

use crate::counters::{self, CounterId};
use crate::header::{Header, TAG_CLOSURE};

/// CPS-color calling convention (see module-level docs).
type CpsFn = unsafe extern "C" fn(
    closure_ptr: *mut u8,
    args_ptr: *const u64,
    args_len: u32,
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
    /// Per-thread last-terminal tag: records whether the most recent
    /// `sigil_run_loop` invocation terminated via `Done` (body normal
    /// completion, value subject to return arm wrapping) or
    /// `Discharged` (op arm body's discard-`k` tail, value IS the
    /// handle's final value, return arm bypassed).
    ///
    /// Initial value `NEXT_STEP_TAG_DONE` so any caller that queries
    /// before any run_loop has run gets the conservative answer
    /// (don't bypass return arm). Set by `sigil_run_loop` immediately
    /// before returning the terminal value; queried by codegen via
    /// `sigil_last_terminal_tag` after the run_loop call returns.
    ///
    /// **No GC root** — the value is `u32`, not a heap pointer. No
    /// `register_*_for_calling_thread` is needed.
    ///
    /// **Single-threaded v1**: the TLS keeps the API multi-thread-safe
    /// for future expansion; today's `sigil_run_loop` runs on a single
    /// thread, but the per-thread design avoids globally-mutable state.
    static LAST_TERMINAL_TAG: Cell<u32> = const { Cell::new(NEXT_STEP_TAG_DONE) };

    /// Per-thread last-terminal value: records the u64 value the most
    /// recent `sigil_run_loop` invocation returned at terminal time
    /// (whether DONE or DISCHARGED).
    ///
    /// Stage-6.8-followup Bug 1 fix companion to `LAST_TERMINAL_TAG`.
    /// Codegen at `Expr::Handle`'s `lower_expr` queries this when the
    /// body has post-perform code (`{ let _ = perform; tail }` shape):
    /// without it, body_val computed by synchronous body lowering
    /// reflects the body's IR-local terminal value, NOT the discharged
    /// arm's value. The arm's discharged value flows back through the
    /// trampoline's terminal but gets narrowed at the perform's
    /// narrow-back to perform's declared return type (lossy when arm
    /// discharges a value of a different type) and then the body's
    /// post-perform code overwrites it. By saving the run_loop's raw
    /// terminal u64 here, the handle expression can recover the
    /// discharged value when tag == DISCHARGED.
    ///
    /// Initial value `0` (any value is safe — codegen only reads when
    /// LAST_TERMINAL_TAG == DISCHARGED, and the discharge tag is only
    /// set by `sigil_run_loop`'s terminal). Set by `sigil_run_loop`
    /// immediately before returning the terminal value; queried by
    /// codegen via `sigil_last_terminal_value` after run_loop returns.
    ///
    /// **No GC root** — the value is u64. The TLS does NOT carry a
    /// stable heap-pointer rooting contract; codegen must consume the
    /// value before any GC-triggering operation that could reclaim a
    /// heap object whose pointer was parked here. In practice the
    /// consumer site (`Expr::Handle` discharge_block) reads the value
    /// immediately and threads it through the merge_block's param,
    /// where Cranelift's register / spill discipline keeps it live for
    /// Boehm's conservative scan. A future precise-GC pass would
    /// require either rooting this TLS or moving the value into a
    /// stack slot at consumption time.
    static LAST_TERMINAL_VALUE: Cell<u64> = const { Cell::new(0) };

    /// Task 78.5 G4 Approach 6 deep-redo — body-fn natural-exit
    /// return-arm dispatch pair (closure + fn-ptr).
    ///
    /// Set by codegen at the handle expression's body-call entry
    /// (custom Sync→Cps wrapper for direct-Cps-call bodies); read by
    /// `sigil_done_or_dispatch_return_arm` at body-fn natural-exit
    /// emit sites (codegen.rs:8415, 10713, 10983, 11402) to wrap the
    /// terminal value via the return arm.
    ///
    /// **Why TLS** (vs the existing `HandlerFrame.return_closure` /
    /// `return_fn` fields used by Phase 4g): Phase 4g fires post-body-
    /// `run_loop` on the chain-unwound value (shallow handler
    /// semantics). Generator's collecting handler requires deep
    /// semantics — wrap at the deepest `k(arg)` terminus so the
    /// `outer_post_arm_k_stack` chain unwinds with already-wrapped
    /// values. The TLS pair is set/cleared/saved/restored at run_loop
    /// boundaries by codegen so the helper can read the active handle's
    /// return arm without traversing the handler-frame stack.
    ///
    /// **GC reasoning**: stores a heap pointer (closure record) but
    /// no separate Boehm root is registered — the same closure record
    /// is also stored on the handle's `HandlerFrame.return_closure`
    /// (registered via `sigil_handler_frame_set_return`), and that
    /// frame is rooted via `HANDLER_STACK`. The TLS read path
    /// (`sigil_done_or_dispatch_return_arm`) executes while the body's
    /// frame is still on the handler stack, so the closure record is
    /// reachable. Codegen save/restore around nested `run_loop` calls
    /// (lower_call's Cps branch + Sync shim) holds the saved values in
    /// Cranelift SSA Values which Boehm's conservative scan covers via
    /// register / spill discipline.
    static BODY_RETURN_ARM_CLOSURE: Cell<*mut u8> = const { Cell::new(ptr::null_mut()) };
    /// Companion fn-ptr slot to `BODY_RETURN_ARM_CLOSURE`. Non-GC
    /// (fn-ptrs are not heap pointers).
    static BODY_RETURN_ARM_FN: Cell<*mut u8> = const { Cell::new(ptr::null_mut()) };
    /// Task 78.5 G4 Approach 6 deep-redo — suppression flag, set by
    /// `sigil_done_or_dispatch_return_arm` when it dispatches the
    /// return arm. Two readers:
    ///
    /// 1. The helper itself, on subsequent invocations within the same
    ///    body `run_loop` (e.g., during `outer_post_arm_k_stack` chain
    ///    unwinding when the post-arm-k synth fn's body Done emit is
    ///    site 10713 / 10983, which we replace with the helper) — if
    ///    set, the helper falls through to `sigil_next_step_done(v)`
    ///    instead of double-wrapping.
    /// 2. The handle expression's Phase 4g code (codegen.rs:13826) —
    ///    if set, skips the second nested `run_loop` that would
    ///    otherwise apply the return arm to the (already-wrapped) body
    ///    value.
    ///
    /// Reset to `false` by codegen at the handle expression's body-call
    /// entry, alongside setting `BODY_RETURN_ARM_*`. Saved/restored
    /// across nested `run_loop` boundaries by codegen along with the
    /// closure/fn pair.
    static BODY_RETURN_ARM_FIRED: Cell<bool> = const { Cell::new(false) };

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

const OUTER_POST_ARM_K_STACK_SIZE: usize = 32;

#[repr(C)]
#[derive(Copy, Clone)]
struct OuterPostArmKEntry {
    closure_ptr: *mut u8,
    fn_ptr: *mut u8,
}

thread_local! {
    static OUTER_POST_ARM_K_STACK: Cell<[OuterPostArmKEntry; OUTER_POST_ARM_K_STACK_SIZE]> = const {
        Cell::new([OuterPostArmKEntry {
            closure_ptr: ptr::null_mut(),
            fn_ptr: ptr::null_mut(),
        }; OUTER_POST_ARM_K_STACK_SIZE])
    };
    static OUTER_POST_ARM_K_DEPTH: Cell<usize> = const { Cell::new(0) };
    static OUTER_POST_ARM_K_STACK_ROOTED: Cell<bool> = const { Cell::new(false) };
}

/// Register the calling thread's `OUTER_POST_ARM_K_STACK` TLS cell as
/// a Boehm GC root. Idempotent per thread. Mirrors
/// [`register_handler_stack_root_for_calling_thread`]'s discipline.
pub(crate) fn register_outer_post_arm_k_stack_root_for_calling_thread() -> (*mut c_void, *mut c_void)
{
    OUTER_POST_ARM_K_STACK.with(|cell| {
        let start =
            cell as *const Cell<[OuterPostArmKEntry; OUTER_POST_ARM_K_STACK_SIZE]> as *mut c_void;
        let end = unsafe {
            (start as *mut u8).add(core::mem::size_of::<
                [OuterPostArmKEntry; OUTER_POST_ARM_K_STACK_SIZE],
            >()) as *mut c_void
        };
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
        (start, end)
    })
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
    OUTER_POST_ARM_K_DEPTH.with(|depth| depth.set(0));
    unsafe {
        crate::gc::GC_remove_roots(start, end);
    }
}

/// Push an outer `post_arm_k` pair onto the thread-local stack.
/// Codegen emits a call to this fn from B.2 helper Middle's emit
/// before issuing `sigil_perform` for the next chain step. Aborts on
/// stack overflow (stack size 32; helper-perform chains beyond that
/// depth are not supported in v1).
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
    eprintln!(
        "G4_DIAG_PUSH: closure={closure_ptr:?} fn={fn_ptr:?} depth_before={}",
        OUTER_POST_ARM_K_DEPTH.with(|c| c.get())
    );
    OUTER_POST_ARM_K_DEPTH.with(|depth_cell| {
        let depth = depth_cell.get();
        if depth >= OUTER_POST_ARM_K_STACK_SIZE {
            eprintln!(
                "sigil_outer_post_arm_k_push: stack overflow (depth {} >= cap {})",
                depth, OUTER_POST_ARM_K_STACK_SIZE
            );
            std::process::abort();
        }
        OUTER_POST_ARM_K_STACK.with(|stack_cell| {
            let mut stack = stack_cell.get();
            stack[depth] = OuterPostArmKEntry {
                closure_ptr,
                fn_ptr,
            };
            stack_cell.set(stack);
        });
        depth_cell.set(depth + 1);
    });
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
    OUTER_POST_ARM_K_DEPTH.with(|depth_cell| {
        let depth = depth_cell.get();
        if depth == 0 {
            eprintln!("G4_DIAG_TRY_POP: empty (depth=0) → None");
            None
        } else {
            depth_cell.set(depth - 1);
            OUTER_POST_ARM_K_STACK.with(|stack_cell| {
                let mut stack = stack_cell.get();
                let popped = stack[depth - 1];
                eprintln!(
                    "G4_DIAG_TRY_POP: depth_before={depth} popped closure={:?} fn={:?}",
                    popped.closure_ptr, popped.fn_ptr
                );
                // Clear the slot so a stale pointer doesn't survive
                // in the TLS-rooted range across the next GC scan.
                stack[depth - 1] = OuterPostArmKEntry {
                    closure_ptr: ptr::null_mut(),
                    fn_ptr: ptr::null_mut(),
                };
                stack_cell.set(stack);
                Some(popped)
            })
        }
    })
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
    // SAFETY: gc-heap-ptr arithmetic (the cast is to a single
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
    let arm_count = (*frame).arm_count;
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

/// Read the per-thread `LAST_TERMINAL_TAG` set by the most recent
/// `sigil_run_loop` invocation. Returns `NEXT_STEP_TAG_DONE` if the
/// run_loop terminated via body-normal completion (return arm should
/// fire), or `NEXT_STEP_TAG_DISCHARGED` if it terminated via op arm
/// body's discard-`k` tail (return arm should be bypassed —
/// discharged value IS the handle's final value per algebraic-effects
/// semantics).
///
/// Initial value (before any `sigil_run_loop` runs) is
/// `NEXT_STEP_TAG_DONE` — conservative for callers that query before
/// running anything.
///
/// # Safety
///
/// Safe to call. Reads thread-local state without mutation.
#[no_mangle]
pub unsafe extern "C" fn sigil_last_terminal_tag() -> u32 {
    LAST_TERMINAL_TAG.with(|c| c.get())
}

/// Reset the per-thread `LAST_TERMINAL_TAG` to `NEXT_STEP_TAG_DONE`.
/// Emitted by codegen at the entry of each `Expr::Handle` lowering
/// before the body is lowered, so that handle bodies which do not
/// invoke `sigil_run_loop` (e.g., `handle 5 with { ... }`) see a
/// clean `DONE` state when their outer logic queries
/// `sigil_last_terminal_tag` post-body. Without this reset, the tag
/// would carry over from any prior run_loop on the same thread,
/// producing spurious return-arm-skips for handles whose bodies
/// never discharge.
///
/// # Safety
///
/// Safe to call. Writes thread-local state with no other side effects.
#[no_mangle]
pub unsafe extern "C" fn sigil_reset_last_terminal_tag() {
    LAST_TERMINAL_TAG.with(|c| c.set(NEXT_STEP_TAG_DONE));
}

/// Read the per-thread `LAST_TERMINAL_VALUE` set by the most recent
/// `sigil_run_loop` invocation. Returns the raw u64 the trampoline
/// returned at terminal time (whether DONE or DISCHARGED).
///
/// Stage-6.8-followup Bug 1 fix companion to `sigil_last_terminal_tag`.
/// Codegen reads this when `LAST_TERMINAL_TAG == NEXT_STEP_TAG_DISCHARGED`
/// to recover the discharged value when body has post-perform code that
/// would otherwise overwrite the natural body_val with body's IR-locally-
/// computed terminal.
///
/// Initial value (before any `sigil_run_loop` runs) is `0`. Codegen only
/// reads when the tag indicates DISCHARGED, so the initial-zero is never
/// observable in well-formed programs.
///
/// # Safety
///
/// Safe to call. Reads thread-local state without mutation.
#[no_mangle]
pub unsafe extern "C" fn sigil_last_terminal_value() -> u64 {
    LAST_TERMINAL_VALUE.with(|c| c.get())
}

/// Reset the per-thread `LAST_TERMINAL_VALUE` to `0`. Emitted alongside
/// `sigil_reset_last_terminal_tag` at the top of each `Expr::Handle`
/// lowering so handles whose body never runs the trampoline see a
/// clean state instead of inheriting a prior run's value. Reading this
/// when the tag is DONE is undefined behavior at the source level
/// (codegen's discharge_block is gated on tag == DISCHARGED), but
/// resetting prevents any future code path from observing a stale
/// value.
///
/// # Safety
///
/// Safe to call. Writes thread-local state with no other side effects.
#[no_mangle]
pub unsafe extern "C" fn sigil_reset_last_terminal_value() {
    LAST_TERMINAL_VALUE.with(|c| c.set(0));
}

// ---------------------------------------------------------------------
// Task 78.5 G4 Approach 6 deep-redo — body-fn-natural-exit return-arm
// dispatch (deep-handler semantics)
// ---------------------------------------------------------------------

/// Set the per-thread `BODY_RETURN_ARM_CLOSURE` / `BODY_RETURN_ARM_FN`
/// pair. Codegen calls this at the handle expression's body-call entry
/// (custom Sync→Cps wrapper for direct-Cps-call bodies) so the helper
/// at body-fn natural-exit emit sites can dispatch through the active
/// handle's return arm.
///
/// Does NOT reset `BODY_RETURN_ARM_FIRED`. Use
/// `sigil_clear_body_return_arm` for the full-reset semantic.
///
/// # Safety
///
/// Safe to call. Writes thread-local state; no other side effects.
/// `closure` may be null (when the return arm has no captures, mirroring
/// the existing `HandlerFrame.return_closure` discipline). `fn_addr` is
/// expected non-null by the helper read path; passing null effectively
/// disables the helper (helper falls through to plain `Done(v)`).
#[no_mangle]
pub unsafe extern "C" fn sigil_set_body_return_arm(closure: *mut u8, fn_addr: *mut u8) {
    BODY_RETURN_ARM_CLOSURE.with(|c| c.set(closure));
    BODY_RETURN_ARM_FN.with(|c| c.set(fn_addr));
}

/// Set the per-thread `BODY_RETURN_ARM_FIRED` flag from a u8 (0/non-0).
/// Codegen calls this when restoring TLS after a nested `run_loop`
/// (lower_call's Cps branch's save+restore pattern around sub-Cps-fn
/// calls).
///
/// # Safety
///
/// Safe to call. Writes thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sigil_set_body_return_arm_fired(fired: u8) {
    BODY_RETURN_ARM_FIRED.with(|c| c.set(fired != 0));
}

/// Read the per-thread `BODY_RETURN_ARM_CLOSURE`. Codegen reads this
/// to save TLS before clearing for nested `run_loop` calls.
///
/// # Safety
///
/// Safe to call. Reads thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sigil_get_body_return_arm_closure() -> *mut u8 {
    BODY_RETURN_ARM_CLOSURE.with(|c| c.get())
}

/// Read the per-thread `BODY_RETURN_ARM_FN`. Codegen reads this to
/// save TLS before clearing for nested `run_loop` calls.
///
/// # Safety
///
/// Safe to call. Reads thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sigil_get_body_return_arm_fn() -> *mut u8 {
    BODY_RETURN_ARM_FN.with(|c| c.get())
}

/// Read the per-thread `BODY_RETURN_ARM_FIRED` flag as u8 (0 or 1).
/// Two readers:
///
/// 1. Codegen at the handle expression's Phase 4g (codegen.rs:13826) —
///    after body's `run_loop` returns, if this flag is set, the helper
///    already wrapped via the return arm; Phase 4g skips the second
///    nested `run_loop` that would double-wrap.
/// 2. Codegen save/restore around nested `run_loop` calls (lower_call
///    Cps branch + Sync shim).
///
/// # Safety
///
/// Safe to call. Reads thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sigil_get_body_return_arm_fired() -> u8 {
    if BODY_RETURN_ARM_FIRED.with(|c| c.get()) {
        1
    } else {
        0
    }
}

/// Clear the body-return-arm TLS triplet (closure, fn, fired). Codegen
/// calls this at handle expression entry before SET to ensure clean
/// state across handle expressions (the SET helper does not reset
/// FIRED; this combined helper does).
///
/// Also called by lower_call's Cps branch to clear TLS before driving
/// a nested `run_loop` for a sub-Cps-fn call (so the sub-call does NOT
/// inherit the outer body's return-arm pair — Risk 3 discipline). The
/// caller then `sigil_set_body_return_arm` + `sigil_set_body_return_arm_fired`
/// to restore after the nested run_loop returns.
///
/// # Safety
///
/// Safe to call. Writes thread-local state.
#[no_mangle]
pub unsafe extern "C" fn sigil_clear_body_return_arm() {
    BODY_RETURN_ARM_CLOSURE.with(|c| c.set(ptr::null_mut()));
    BODY_RETURN_ARM_FN.with(|c| c.set(ptr::null_mut()));
    BODY_RETURN_ARM_FIRED.with(|c| c.set(false));
}

/// Body-fn natural-exit terminal helper. Codegen replaces 4 Done-emit
/// sites with a call to this helper:
///
/// - codegen.rs:8415 — B.2 hand-rolled compound-match dispatcher's
///   ConstantDone arm (e.g., Generator iterate's Nil arm).
/// - codegen.rs:10713 — Slice B post-arm-k synth fn body Done emit
///   (e.g., Generator's `Cons(x, rest)` arm body during chain unwind).
/// - codegen.rs:10983 — Slice C post-arm-k chain Final step Done emit.
/// - codegen.rs:11402 — `CpsContinuationKind::ConstantDone` synth-cont
///   dispatch.
///
/// **Behavior:**
///
/// - If `BODY_RETURN_ARM_FIRED == true`: emit `sigil_next_step_done(v)`
///   directly. This case fires during `outer_post_arm_k_stack` chain
///   unwinding AFTER the helper has already wrapped at the deepest
///   natural exit — the post-arm-k synth fns at sites 10713 / 10983
///   evaluate their arm-body tail with the (already-wrapped) `r`
///   binding and emit Done; we must not re-wrap their result.
/// - Else if `BODY_RETURN_ARM_FN` is non-null: dispatch the return arm
///   via `NextStep::Call(closure, fn, [v, null, identity])` per the
///   Phase 4g return-arm-synth-fn dispatch contract (3-slot trailing
///   pair). The trailing pair is `(null, sigil_continuation_identity)`
///   matching Phase 4g's hard-coded "no further chaining" terminator.
///   Sets `BODY_RETURN_ARM_FIRED = true` so subsequent invocations + the
///   handle expression's Phase 4g check skip re-wrapping.
/// - Else: emit `sigil_next_step_done(v)`. Body fn is not running under
///   an active handle (or no return arm declared) — standard Done emit.
///
/// # Safety
///
/// Safe to call. Reads/writes thread-local state. The returned NextStep
/// pointer is owned by the per-dispatch arena; the trampoline frees it
/// after dispatch.
#[no_mangle]
pub unsafe extern "C" fn sigil_done_or_dispatch_return_arm(v: u64) -> *mut NextStep {
    let fired = BODY_RETURN_ARM_FIRED.with(|c| c.get());
    let fn_addr = BODY_RETURN_ARM_FN.with(|c| c.get());
    eprintln!(
        "G4_DIAG_HELPER: v={v} fired_before={fired} fn_addr={fn_addr:?} → action={}",
        if !fired && !fn_addr.is_null() {
            "WRAP"
        } else {
            "DONE"
        }
    );
    if !fired && !fn_addr.is_null() {
        let closure = BODY_RETURN_ARM_CLOSURE.with(|c| c.get());
        BODY_RETURN_ARM_FIRED.with(|c| c.set(true));
        let ns = sigil_next_step_call(closure, fn_addr, 3);
        let args = sigil_next_step_args_ptr(ns);
        // Trailing-pair slots match Phase 4g's return-arm-synth-fn
        // dispatch contract: [v, null_post_handle_k_closure,
        // identity_post_handle_k_fn]. The synth fn's body emits
        // Call(post_handle_k_closure_loaded, post_handle_k_fn_loaded,
        // [tail_widened, null, identity]); identity → Done(tail).
        ptr::write(args.add(0), v);
        ptr::write(args.add(1), 0u64);
        ptr::write(
            args.add(2),
            sigil_continuation_identity as *const () as usize as u64,
        );
        ns
    } else {
        sigil_next_step_done(v)
    }
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
/// `LAST_TERMINAL_TAG` for the handle expression's outer codegen logic
/// to query via `sigil_last_terminal_tag`.
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
    // holding the captured arg at slot 0. Trailing slots, if any
    // (Slice A's post-arm-k pair at slots 1..3), are ignored —
    // identity is the terminal continuation.
    let value = *args_ptr;
    sigil_next_step_done(value)
}

// ---------------------------------------------------------------------
// Builtin top-level handler arm fns (Plan B Task 57)
// ---------------------------------------------------------------------

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
) -> *mut NextStep {
    debug_assert!(
        args_len == 3,
        "sigil_io_println_arm: args_len must be exactly 3 (in_args = \
         [heap_string_ptr, k_closure, k_fn]); got {args_len}"
    );
    debug_assert!(
        !in_args.is_null(),
        "sigil_io_println_arm: in_args must be non-null when args_len == 3"
    );
    // SAFETY: caller (sigil_perform via the dispatched NextStep::Call)
    // guarantees in_args points to 3 readable u64. Slot 0 is the
    // heap-string pointer the user passed to `IO.println`; slots 1..3
    // are the trailing-pair continuation.
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
    // Build the outbound NextStep::Call dispatching to the trailing-
    // pair k. arg_count=1: the unit value (`i64 0`) representing
    // IO.println's declared Unit return type.
    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    // SAFETY: sigil_next_step_call returns a non-null *mut NextStep
    // with arg_count slots; sigil_next_step_args_ptr returns a pointer
    // to slot 0.
    let out_args = sigil_next_step_args_ptr(ns);
    *out_args = 0; // unit value
    ns
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
) -> *mut NextStep {
    debug_assert!(args_len == 3);
    debug_assert!(!in_args.is_null());
    let heap_ptr = *in_args as *const u8;
    debug_assert!(!heap_ptr.is_null());
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;
    crate::io::sigil_print(heap_ptr);
    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    let out_args = sigil_next_step_args_ptr(ns);
    *out_args = 0; // unit
    ns
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
) -> *mut NextStep {
    debug_assert!(args_len == 2);
    debug_assert!(!in_args.is_null());
    let k_closure = *in_args as *mut u8;
    let k_fn = *in_args.add(1) as *mut u8;
    let line_ptr = crate::io::sigil_read_line();
    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    let out_args = sigil_next_step_args_ptr(ns);
    *out_args = line_ptr as u64;
    ns
}

/// Plan C Task 70 — runtime-side default handler for `IO.read_file`.
/// One user arg (path: String); returns the file contents as a
/// fresh Sigil String. Aborts on IO error or invalid UTF-8.
///
/// # Safety
///
/// Same as `sigil_io_println_arm`.
#[no_mangle]
pub unsafe extern "C" fn sigil_io_read_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
) -> *mut NextStep {
    debug_assert!(args_len == 3);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    debug_assert!(!path_ptr.is_null());
    let k_closure = *in_args.add(1) as *mut u8;
    let k_fn = *in_args.add(2) as *mut u8;
    let contents_ptr = crate::io::sigil_read_file(path_ptr);
    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    let out_args = sigil_next_step_args_ptr(ns);
    *out_args = contents_ptr as u64;
    ns
}

/// Plan C Task 70 — runtime-side default handler for `IO.write_file`.
/// Two user args (path, data); writes the data to the file path,
/// replacing any existing contents. Returns Unit. Aborts on IO error.
///
/// # Safety
///
/// `in_args` must point at 4 readable u64 (path, data, k_closure, k_fn).
#[no_mangle]
pub unsafe extern "C" fn sigil_io_write_file_arm(
    _closure_ptr: *const u8,
    in_args: *const u64,
    args_len: u32,
) -> *mut NextStep {
    debug_assert!(args_len == 4);
    debug_assert!(!in_args.is_null());
    let path_ptr = *in_args as *const u8;
    let data_ptr = *in_args.add(1) as *const u8;
    debug_assert!(!path_ptr.is_null() && !data_ptr.is_null());
    let k_closure = *in_args.add(2) as *mut u8;
    let k_fn = *in_args.add(3) as *mut u8;
    crate::io::sigil_write_file(path_ptr, data_ptr);
    let ns = sigil_next_step_call(k_closure, k_fn, 1);
    let out_args = sigil_next_step_args_ptr(ns);
    *out_args = 0; // unit
    ns
}

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
) -> *mut NextStep {
    arith_error_default_arm("remainder by zero", args_len)
}

/// Internal helper for the two `ArithError` default arm fns. Writes
/// `"sigil: arithmetic error: <reason>\n"` to stderr and calls
/// `std::process::exit(2)`. Never returns.
///
/// `args_len` is debug-asserted to be 2 (zero user args + trailing
/// pair). Caller (`sigil_perform`) guarantees the invariant.
fn arith_error_default_arm(reason: &str, args_len: u32) -> ! {
    debug_assert!(
        args_len == 2,
        "sigil_arith_error_*_arm: args_len must be exactly 2 (zero user args + \
         trailing-pair `(k_closure, k_fn)`); got {args_len}"
    );
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(stderr, "sigil: arithmetic error: {reason}");
    let _ = stderr.flush();
    drop(stderr);
    std::process::exit(2);
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
                // SAFETY: gc-heap-ptr arithmetic (see comment above).
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
    // Stage-6.8-followup Layer 3c — snapshot OUTER_POST_ARM_K_DEPTH at
    // run_loop entry. On the DISCHARGED bypass terminal (introduced by
    // Layer 3c to preserve algebraic-effects discharge semantics
    // through outer chain routing), drain the stack back to this depth
    // so entries pushed by synth-cont Middle steps during the bypassed
    // chain don't leak across run_loop boundaries. The Bug-2-era
    // routing path naturally pops one entry per terminal; the bypass
    // skips that pop, hence the explicit drain.
    let outer_post_arm_k_entry_depth = OUTER_POST_ARM_K_DEPTH.with(|c| c.get());
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
                    LAST_TERMINAL_TAG.with(|c| c.set(tag));
                    LAST_TERMINAL_VALUE.with(|c| c.set(v));
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
                    let current_depth = OUTER_POST_ARM_K_DEPTH.with(|c| c.get());
                    debug_assert!(
                        current_depth >= outer_post_arm_k_entry_depth,
                        "sigil_run_loop terminal: outer_post_arm_k depth \
                         underflow ({current_depth} < entry-time \
                         {outer_post_arm_k_entry_depth}); a codegen path \
                         popped entries belonging to an outer run_loop"
                    );
                    OUTER_POST_ARM_K_DEPTH.with(|c| c.set(outer_post_arm_k_entry_depth));
                    crate::arena::sigil_arena_reset();
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
                // expression's outer codegen logic queries
                // `LAST_TERMINAL_TAG` to decide return-arm dispatch.
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
                let current_depth = OUTER_POST_ARM_K_DEPTH.with(|c| c.get());
                debug_assert!(
                    current_depth == outer_post_arm_k_entry_depth,
                    "sigil_run_loop DONE terminal: outer_post_arm_k depth \
                     mismatch ({current_depth} != entry-time \
                     {outer_post_arm_k_entry_depth}); a codegen path \
                     pushed without a matching terminal pop, OR popped \
                     entries belonging to an outer run_loop"
                );
                LAST_TERMINAL_TAG.with(|c| c.set(tag));
                LAST_TERMINAL_VALUE.with(|c| c.set(v));
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
                // SAFETY: gc-heap-ptr arithmetic (args_buf is a stack local, no GC retention).
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
        // SAFETY: gc-heap-ptr arithmetic (stack array, non-GC, outlives the call).
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
        // SAFETY: gc-heap-ptr arithmetic (stack array, non-GC, outlives the call).
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
                                     // SAFETY: gc-heap-ptr arithmetic (args_ptr points at a
                                     // non-GC arena buffer; reads are value loads, no GC retention).
            assert_eq!(*args_ptr, 100);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(1), 200);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(2) as usize, 0xCC);
            // SAFETY: gc-heap-ptr arithmetic (same as above).
            assert_eq!(*args_ptr.add(3) as usize, 0xDD);
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
        OUTER_POST_ARM_K_DEPTH.with(|d| d.set(0));
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
        let heap_closure = crate::gc::sigil_alloc(0, 64);
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
        let closure_ptr = crate::gc::sigil_alloc(0, 64);
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

        let closures: Vec<*mut u8> = (0..3).map(|_| crate::gc::sigil_alloc(0, 64)).collect();
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
        // Push OUTER_POST_ARM_K_STACK_SIZE - 1 entries; the cap is
        // OUTER_POST_ARM_K_STACK_SIZE. The Nth push (where N == cap)
        // would abort; verifying the cap-1 case stays safe + LIFO.
        if !in_stress_subprocess() {
            run_stress_in_subprocess("outer_post_arm_k_stack_fills_to_cap_minus_one");
            return;
        }
        let _guard = crate::test_support::gc_test_lock();
        ensure_gc();
        let _enrol = crate::test_support::GcThreadEnrolment::acquire();
        reset_outer_post_arm_k_stack();

        let n = OUTER_POST_ARM_K_STACK_SIZE - 1;
        let closures: Vec<*mut u8> = (0..n).map(|_| crate::gc::sigil_alloc(0, 64)).collect();
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
            let result = sigil_run_loop(initial);
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
}
