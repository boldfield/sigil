//! Frame-pointer stack walker — plan 2026-05-08-sigil-v2-runtime-
//! profile-data Phase 2, Task 3.
//!
//! Walks the saved-frame-pointer chain to capture return addresses for
//! a profile sample. The compiler enables `preserve_frame_pointers = true`
//! at ISA build time (`compiler/src/codegen.rs` near line 7415), which
//! means every Cranelift-emitted Sigil function pushes/restores `rbp`
//! (x86_64) or `x29` (aarch64) and the standard frame record layout
//! holds:
//!
//! ```text
//! [fp + 0]  saved previous fp
//! [fp + 8]  return address (caller's PC)
//! ```
//!
//! Both x86_64 (`%rbp` chain) and aarch64 (AAPCS `%fp = x29` chain)
//! match this layout, so a single architecture-agnostic walker handles
//! both. The arch difference is restricted to reading the current
//! frame pointer at entry.
//!
//! ## Signal safety
//!
//! [`capture_stack`] is **signal-safe**:
//!
//! - no allocation (it writes into a caller-owned `&mut [usize; MAX_DEPTH]`);
//! - no libc call;
//! - no synchronisation primitive (relaxed atomics only — and even those
//!   are not used by the walker itself, only by the Phase 3 / Phase 4
//!   sampler that calls it);
//! - bounded loop (`MAX_DEPTH`).
//!
//! ## Bounds and safety guards
//!
//! The walker refuses to follow:
//!
//! - `fp == 0`
//! - `fp` misaligned (stack frames are pointer-aligned by ABI)
//! - `prev_fp <= fp` — sanity check for stack-grows-down (a corrupted
//!   chain that walks "down" is rejected immediately; this is the test
//!   pattern in the plan for the corrupted-stack property)
//! - `return_addr == 0` — sentinel that the kernel uses to terminate
//!   user-space backtraces (e.g. the program entry frame)
//!
//! There is no SEGV recovery. Loading from a wild `fp` would still
//! fault; the contract is that the caller invokes from a real signal-
//! handler context (or from the userland helper [`walk_from_here`])
//! where the live thread's stack is valid up the chain.
//!
//! ## aarch64 pointer authentication
//!
//! On Apple Silicon (macOS aarch64) PAC bits may be set in return
//! addresses received from system code. The walker strips them with a
//! 48-bit canonical-VA mask before recording. Linux aarch64 does not
//! activate PAC by default; the mask is a no-op there.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of frames captured per sample. Bigger than typical
/// production stack depths (e.g., Go and Java sample at 32–64 frames);
/// 128 lets a deep CPS chain unwind cleanly without truncation.
pub const MAX_DEPTH: usize = 128;

/// Stripping mask for aarch64 pointer-authentication codes. Apple
/// Silicon signs return addresses with PAC; the canonical user-space
/// VA fits in 48 bits, so masking the upper 16 bits yields the bare
/// address suitable for symtab lookup.
#[cfg(target_arch = "aarch64")]
const AARCH64_PAC_STRIP_MASK: usize = 0x0000_FFFF_FFFF_FFFF;

/// Walk the current thread's frame-pointer chain. Returns the number
/// of frames captured into `buf` (always `<= MAX_DEPTH`).
///
/// Frames are recorded leaf-first: `buf[0]` is the return address of
/// the immediate caller of `capture_stack`, `buf[1]` is the return
/// address of that caller's caller, and so on. The caller is expected
/// to filter out any walker-internal frames it doesn't want to see.
///
/// # Safety
///
/// - Must be invoked on a live thread with a valid frame-pointer chain
///   (every function on the call path must have been compiled with
///   frame pointers preserved). Sigil's Cranelift codegen and the
///   runtime crate (compiled with the default Rust `frame-pointer`
///   profile setting at `debug = 1`, plus a debug-asserts run path)
///   both satisfy this.
/// - The buffer must point to valid writable memory for `MAX_DEPTH`
///   `usize`s.
#[inline(always)]
pub unsafe fn capture_stack(buf: &mut [usize; MAX_DEPTH]) -> usize {
    let fp = current_fp();
    capture_stack_from(fp, buf)
}

/// Walk the frame-pointer chain starting at a caller-supplied `fp`.
/// Lower-level entry point used by the SIGPROF handler (which reads
/// `fp` from the `ucontext_t` for the interrupted thread) and by the
/// unit tests (which build synthetic frame records on the heap).
///
/// # Safety
///
/// `start_fp` must either be 0 (returns 0 immediately) or point to a
/// real frame record laid out per the platform ABI (saved-prev-fp at
/// offset 0, return address at offset 8). Faulting reads are not
/// recovered; if `start_fp` is wild, the read inside the loop will
/// SEGV.
pub unsafe fn capture_stack_from(start_fp: usize, buf: &mut [usize; MAX_DEPTH]) -> usize {
    let mut fp = start_fp;
    let mut depth: usize = 0;
    let align = core::mem::align_of::<usize>();

    while depth < MAX_DEPTH {
        if fp == 0 {
            break;
        }
        if !fp.is_multiple_of(align) {
            break;
        }
        // SAFETY: the caller's contract is that `fp` is either 0
        // (already handled) or a valid frame-record pointer. We've
        // checked alignment; reading two pointer-sized words is in-
        // bounds for a well-formed frame record.
        let frame: *const usize = fp as *const usize;
        let prev_fp = core::ptr::read_volatile(frame);
        let raw_ret = core::ptr::read_volatile(frame.add(1));
        let ret = strip_arch_tag(raw_ret);

        if ret == 0 {
            // Kernel-side termination sentinel for the user-space
            // backtrace.
            break;
        }
        buf[depth] = ret;
        depth += 1;

        if prev_fp == 0 {
            break;
        }
        // Stack-grows-down sanity: a valid prev_fp must be at a
        // higher address than the current fp. Bail on any inversion
        // (corrupted chain, end of stack region, anomaly).
        if prev_fp <= fp {
            break;
        }
        fp = prev_fp;
    }
    depth
}

/// Read the current thread's frame-pointer register without
/// requiring an out-of-line call (the inline asm is its own
/// register read).
#[inline(always)]
#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn current_fp() -> usize {
    let fp: usize;
    #[cfg(target_arch = "x86_64")]
    {
        core::arch::asm!(
            "mov {}, rbp",
            out(reg) fp,
            options(nostack, preserves_flags, nomem),
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        core::arch::asm!(
            "mov {}, x29",
            out(reg) fp,
            options(nostack, preserves_flags, nomem),
        );
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        compile_error!(
            "sigil profile unwinder only supports x86_64 and aarch64 — \
             plan 2026-05-08 hard-rules linux-x86_64 + macos-aarch64"
        );
    }
    fp
}

#[inline(always)]
fn strip_arch_tag(addr: usize) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        addr & AARCH64_PAC_STRIP_MASK
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        addr
    }
}

/// Userland helper for testing: invoke [`capture_stack`] from a
/// normal call site (not a signal handler), returning the captured
/// frames and depth. Used by unit tests to validate that the walker
/// terminates correctly on real call chains.
///
/// The helper is `#[inline(never)]` so its own frame is observable
/// in the captured buffer; tests that check "depth >= N" expect this.
#[inline(never)]
pub fn walk_from_here() -> ([usize; MAX_DEPTH], usize) {
    let mut buf = [0usize; MAX_DEPTH];
    // SAFETY: called from a live thread with a valid fp chain.
    let depth = unsafe { capture_stack(&mut buf) };
    (buf, depth)
}

/// Telemetry counter for samples where the walker exited via a guard
/// other than reaching the end of the chain (corrupted-fp inversion,
/// misalignment, MAX_DEPTH truncation). Relaxed-atomic so the
/// counter bump itself stays signal-safe; readers (Phase 3 / Phase 4
/// drainers) get a coherent snapshot via `Ordering::Relaxed` loads.
pub static WALKER_TRUNCATED_OR_REJECTED: AtomicUsize = AtomicUsize::new(0);

/// Convenience: snapshot the truncate/reject counter. Phase 3 / Phase 4
/// can include this in their final-output summaries so a profile that
/// looks suspiciously short has visible evidence of why.
pub fn truncated_or_rejected_count() -> usize {
    WALKER_TRUNCATED_OR_REJECTED.load(Ordering::Relaxed)
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    /// Counter incremented on every invocation of the recursive
    /// helper. Used by [`walks_real_call_chain_with_increasing_depth`]
    /// to verify the recursion actually executed `N+1` times before
    /// trusting the captured-depth delta.
    static RECURSE_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    /// A deeply recursive helper that captures via [`walk_from_here`]
    /// at the leaf and threads the captured tuple back up the chain.
    ///
    /// The post-call mutation of the returned buffer makes the
    /// recursive call non-tail. We've observed LLVM at `opt-level = 0`
    /// on stable 1.95.0 still tail-call-optimising direct self-
    /// recursion even with `#[inline(never)]` + `black_box` markers in
    /// between, so the test additionally cross-checks via
    /// [`RECURSE_CALL_COUNT`] that the recursion actually ran for the
    /// requested depth before asserting on captured-frame counts.
    #[inline(never)]
    fn recurse_and_walk(depth: usize) -> ([usize; MAX_DEPTH], usize) {
        RECURSE_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
        let mut marker: [u64; 4] = [depth as u64; 4];
        std::hint::black_box(&mut marker);
        if depth == 0 {
            let r = walk_from_here();
            std::hint::black_box(&marker);
            return r;
        }
        let (mut buf, d) = recurse_and_walk(depth - 1);
        // Post-call mutations on the returned buffer make this a
        // non-tail call regardless of TCO heuristics. The XOR cancels
        // so the buffer is byte-identical to what the leaf produced.
        let cell = d.min(MAX_DEPTH - 1);
        buf[cell] ^= std::hint::black_box(marker[0] as usize);
        buf[cell] ^= std::hint::black_box(marker[0] as usize);
        (buf, d)
    }

    #[test]
    fn walks_real_call_chain_with_increasing_depth() {
        RECURSE_CALL_COUNT.store(0, Ordering::Relaxed);
        let (_buf_shallow, depth_shallow) = recurse_and_walk(0);
        let shallow_calls = RECURSE_CALL_COUNT.swap(0, Ordering::Relaxed);

        let (_buf_deep, depth_deep) = recurse_and_walk(40);
        let deep_calls = RECURSE_CALL_COUNT.load(Ordering::Relaxed);

        // Sanity: the recursion must have actually fired N+1 times
        // for the depth-delta assertion below to be meaningful. If
        // LLVM has elided the recursion (TCO-to-loop) the deep call
        // count won't be ~41 and the depth check would be vacuous —
        // surface that explicitly instead.
        assert_eq!(
            shallow_calls, 1,
            "depth=0 must invoke the helper exactly once"
        );
        assert_eq!(deep_calls, 41, "depth=40 must invoke the helper 41 times");

        assert!(
            depth_deep > depth_shallow,
            "deeper recursion must produce deeper walk: shallow={depth_shallow}, deep={depth_deep}"
        );
        // The deep call adds 40 real frames over the shallow call.
        // Each captured frame is one return address; allow some
        // slack for libstd / test-harness frame layout variation.
        assert!(
            depth_deep >= depth_shallow + 20,
            "expected deep walk to add at least ~20 frames over shallow; got \
             shallow={depth_shallow} deep={depth_deep}"
        );
        assert!(
            depth_deep <= MAX_DEPTH,
            "walker depth must be bounded by MAX_DEPTH; got {depth_deep}"
        );
    }

    #[test]
    fn captured_frames_are_nonzero_return_addresses() {
        let (buf, depth) = walk_from_here();
        assert!(depth > 0, "must capture at least one frame");
        for (i, frame) in buf.iter().enumerate().take(depth) {
            assert!(
                *frame != 0,
                "captured frame {i} must be a non-zero return address"
            );
        }
    }

    /// Build a synthetic frame chain in a heap-allocated `Vec<usize>`
    /// and assert the walker traverses it correctly. The synthetic
    /// chain layout matches the platform ABI:
    ///
    /// ```text
    /// frames[0]:  [prev_fp = &frames[2]] [ret = 0x1000]
    /// frames[2]:  [prev_fp = &frames[4]] [ret = 0x1100]
    /// frames[4]:  [prev_fp = 0]          [ret = 0x1200]
    /// ```
    ///
    /// The walker enters at `&frames[0]` and walks up the chain.
    #[test]
    fn walks_synthetic_well_formed_chain() {
        let mut frames: Vec<usize> = vec![0; 6];
        // Synthetic frame record on the system heap, not Boehm-managed.
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record).
        let base = frames.as_mut_ptr() as usize;

        // frames[0..2]: leaf frame
        frames[0] = base + 2 * core::mem::size_of::<usize>(); // prev_fp -> frames[2]
        frames[1] = 0x1000; // ret addr
                            // frames[2..4]: middle frame
        frames[2] = base + 4 * core::mem::size_of::<usize>(); // prev_fp -> frames[4]
        frames[3] = 0x1100;
        // frames[4..6]: top frame; prev_fp = 0 terminates
        frames[4] = 0;
        frames[5] = 0x1200;

        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: frames is a live Vec<usize>; the layout we built is
        // a well-formed frame-record chain. Pointer reads stay inside
        // the vec.
        let depth = unsafe { capture_stack_from(base, &mut buf) };
        assert_eq!(depth, 3, "must capture exactly three frames");
        assert_eq!(buf[0], 0x1000);
        assert_eq!(buf[1], 0x1100);
        assert_eq!(buf[2], 0x1200);
    }

    /// Inverted fp (saved_prev_fp <= current_fp) terminates the walk
    /// at the inversion point. Pins the "stack-grows-down" safety
    /// guard so a corrupted chain can never run the walker off the
    /// end of valid memory.
    #[test]
    fn corrupted_chain_stops_at_inversion() {
        let mut frames: Vec<usize> = vec![0; 4];
        // Synthetic frame record on the system heap, not Boehm-managed.
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record).
        let base = frames.as_mut_ptr() as usize;

        // Leaf frame: well-formed, points to a frame 1 slot AWAY but
        // at a LOWER address (inverted). The walker reads the leaf
        // frame's return addr, then sees the inversion and stops.
        frames[0] = base.saturating_sub(16); // prev_fp INVERTED (smaller addr)
        frames[1] = 0xDEAD; // leaf ret addr — should be captured
        frames[2] = 0;
        frames[3] = 0xBEEF; // never read — inversion stops walk first

        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: frames is a live Vec<usize>; we never follow the
        // inverted fp (the walker rejects it), so no out-of-bounds
        // read occurs.
        let depth = unsafe { capture_stack_from(base, &mut buf) };
        assert_eq!(
            depth, 1,
            "walker must capture only the leaf frame before bailing on inversion"
        );
        assert_eq!(buf[0], 0xDEAD);
    }

    /// Zero starting fp returns depth 0 immediately. Pins the
    /// terminating-sentinel guard.
    #[test]
    fn zero_start_fp_returns_zero_depth() {
        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: zero `start_fp` is explicitly handled by the
        // contract; the walker returns immediately without reading.
        let depth = unsafe { capture_stack_from(0, &mut buf) };
        assert_eq!(depth, 0);
    }

    /// Misaligned starting fp returns depth 0 immediately. Pins the
    /// alignment guard.
    #[test]
    fn misaligned_start_fp_returns_zero_depth() {
        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: misaligned `start_fp` is explicitly handled by the
        // contract; the walker rejects it without reading.
        let depth = unsafe { capture_stack_from(1, &mut buf) };
        assert_eq!(depth, 0);
    }

    /// Return-address sentinel of 0 terminates the walk before
    /// recording the all-zero frame. Pins the kernel-sentinel guard.
    #[test]
    fn null_return_addr_terminates_walk() {
        let mut frames: Vec<usize> = vec![0; 2];
        // Synthetic frame record on the system heap, not Boehm-managed.
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record).
        let base = frames.as_mut_ptr() as usize;
        frames[0] = 0; // prev_fp = 0 (so we'd stop after this frame anyway)
        frames[1] = 0; // ret = 0 — this should stop us BEFORE recording

        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: synthetic frame in a live Vec<usize>; walker reads
        // two slots from `base` then stops on the null ret.
        let depth = unsafe { capture_stack_from(base, &mut buf) };
        assert_eq!(depth, 0);
    }

    /// Capture truncates at `MAX_DEPTH` without writing past the
    /// buffer. Builds a chain of MAX_DEPTH + 8 well-formed frames in
    /// ascending memory order and asserts the walker stops at MAX_DEPTH.
    #[test]
    fn capture_truncates_at_max_depth() {
        let n = MAX_DEPTH + 8;
        let slot_size = core::mem::size_of::<usize>();
        let mut frames: Vec<usize> = vec![0; n * 2];
        // Synthetic frame record on the system heap, not Boehm-managed.
        // SAFETY: gc-heap-ptr arithmetic (test-only synthetic frame record).
        let base = frames.as_mut_ptr() as usize;

        // Each frame at frames[2*i]:
        //   prev_fp = &frames[2*(i+1)] (next frame, higher address) for i < n-1
        //   prev_fp = 0 for the last frame
        //   ret = 0x1000 + i
        for i in 0..n {
            let frame_off = 2 * i;
            frames[frame_off + 1] = 0x1000 + i;
            frames[frame_off] = if i + 1 < n {
                base + 2 * (i + 1) * slot_size
            } else {
                0
            };
        }

        let mut buf = [0usize; MAX_DEPTH];
        // SAFETY: chain is fully constructed in a live Vec; reads are
        // in-bounds for every iteration.
        let depth = unsafe { capture_stack_from(base, &mut buf) };
        assert_eq!(depth, MAX_DEPTH);
        assert_eq!(buf[0], 0x1000);
        assert_eq!(buf[MAX_DEPTH - 1], 0x1000 + MAX_DEPTH - 1);
    }

    /// Spot-check the aarch64 PAC strip — return addresses with high
    /// bits set should mask to canonical 48-bit VAs. On non-aarch64
    /// the function is a no-op.
    #[test]
    fn strip_arch_tag_masks_pac_bits_on_aarch64() {
        // 0xFEDC_0000_DEAD_BEEF — upper 16 bits are PAC, lower 48
        // are the canonical address.
        let raw: usize = 0xFEDC_0000_DEAD_BEEFusize;
        let stripped = strip_arch_tag(raw);
        #[cfg(target_arch = "aarch64")]
        assert_eq!(stripped, 0x0000_0000_DEAD_BEEFusize);
        #[cfg(not(target_arch = "aarch64"))]
        assert_eq!(stripped, raw);
    }
}
