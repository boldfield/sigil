//! `StringBuilder` runtime primitives — Plan C Task 67.
//!
//! Segmented rope for incremental string construction under the
//! `Mem` effect. Compare to Java's `StringBuilder`, .NET's
//! `StringBuilder`, and OCaml's `Buffer`.
//!
//! ## Layout
//!
//! Each `StringBuilder` is a heap record with 4 payload words:
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_STRING_BUILDER, count = 4, bitmap = 0b1000)
//! offset 8  : u64 total_len     (running total bytes appended)
//! offset 16 : u64 seg_count     (number of allocated segments)
//! offset 24 : u64 seg_used_tail (bytes used in the tail segment)
//! offset 32 : *mut u8 segments  (pointer to a TAG_ARRAY of segment pointers)
//! ```
//!
//! `bitmap = 0b1000` marks payload word 3 (`segments`) as a GC
//! pointer; the three preceding scalar fields are skipped during
//! mark-phase precision tracing. (Boehm scans conservatively in
//! v1 so the bitmap is mostly informational; it'll matter under
//! v2's precise walker.)
//!
//! ## Segments
//!
//! Each segment is a fixed-size `TAG_MUT_BYTE_ARRAY` of
//! [`SEG_SIZE`] = 4096 bytes (one page). Bytes are written
//! linearly into the tail segment; on overflow, a fresh segment
//! is allocated and pushed onto the segments array.
//!
//! ## Segments array
//!
//! A `TAG_ARRAY` of segment pointers (one per slot, 64-bit
//! slots). Initial capacity [`INITIAL_SEG_CAP`] = 4 slots; doubles
//! on overflow (Vec-style growth). The array's length word
//! reflects allocated slots, not used slots — `seg_count`
//! tracks the latter. Boehm conservatively scans the array's
//! pointer payload, so segment liveness is automatic.
//!
//! ## GC reachability
//!
//! Boehm's conservative scan keeps the SB record reachable from
//! any pointer the user holds; from there, the `segments`
//! pointer is traced to the array, which traces each segment.
//! No write barriers needed in v1 (mutation of SB record fields
//! is plain word-stores; the conservative collector tolerates
//! the transient inconsistency window).
//!
//! ## Why no in-place segment mutation
//!
//! Each `sb_append` writes new bytes into the tail segment's
//! existing buffer (no realloc per append). On overflow, only
//! the segments array is realloc'd — segments themselves are
//! never moved or freed during the SB's lifetime.
//!
//! ## Interior-pointer arithmetic
//!
//! All payload reads / writes use `obj.add(N)` interior pointers,
//! transiently used for one aligned load/store before discard.
//! Same convention as `array.rs` / `byte_array.rs`.

use crate::counters::{self, CounterId};
use crate::gc::sigil_alloc;
use crate::header::{Header, TAG_ARRAY, TAG_MUT_BYTE_ARRAY, TAG_STRING_BUILDER};

/// Bytes per segment. One page; matches typical write-buffer
/// sweet-spot and stays comfortably below the 6-bit count-cap
/// workaround threshold for `TAG_MUT_BYTE_ARRAY`.
pub const SEG_SIZE: usize = 4096;

/// Initial capacity (slot count) of the segments array. Doubles
/// on overflow.
pub const INITIAL_SEG_CAP: u64 = 4;

const SB_PAYLOAD_BYTES: usize = 32; // 4 payload words × 8 bytes each

/// Allocate a fresh segment (TAG_MUT_BYTE_ARRAY of `SEG_SIZE`
/// bytes, zero-filled). Mirrors `mem::sigil_mut_byte_array_new(SEG_SIZE, 0)`
/// without going through the public FFI layer.
fn alloc_segment() -> *mut u8 {
    let payload_bytes = 8usize.saturating_add(SEG_SIZE); // length word + payload
    let h = Header::new(TAG_MUT_BYTE_ARRAY, 0, 0);
    let obj = sigil_alloc(h.raw(), payload_bytes);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(SEG_SIZE as u64);
    }
    counters::add(
        CounterId::StringBuilderAllocBytes,
        (8 + payload_bytes) as u64,
    );
    obj
}

/// Allocate a fresh segments array (TAG_ARRAY) with `cap` slots,
/// each initialised to null.
fn alloc_segments_array(cap: u64) -> *mut u8 {
    let payload_bytes = 8usize.saturating_add((cap as usize).saturating_mul(8));
    let h = Header::new(TAG_ARRAY, 0, 1);
    let obj = sigil_alloc(h.raw(), payload_bytes);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u64 store).
    unsafe {
        let len_ptr: *mut u64 = obj.add(8).cast();
        len_ptr.write(cap);
    }
    counters::add(
        CounterId::StringBuilderAllocBytes,
        (8 + payload_bytes) as u64,
    );
    obj
}

/// Read the segment slot count of a segments-array.
///
/// # Safety
///
/// `arr` must be a pointer to a valid `TAG_ARRAY` returned by
/// `alloc_segments_array`.
#[inline]
unsafe fn segments_array_capacity(arr: *const u8) -> u64 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one u64 read).
    let len_ptr: *const u64 = arr.add(8).cast();
    len_ptr.read()
}

/// Read segment pointer at slot `i` of a segments-array.
///
/// # Safety
///
/// As `segments_array_capacity`. `i` must be `< capacity(arr)`.
#[inline]
unsafe fn segments_array_get(arr: *const u8, i: u64) -> *mut u8 {
    // SAFETY: gc-heap-ptr arithmetic (transient slot read).
    let slot: *const *mut u8 = arr.add(16).add((i * 8) as usize).cast();
    slot.read()
}

/// Write segment pointer at slot `i` of a segments-array.
///
/// # Safety
///
/// As `segments_array_capacity`. `i` must be `< capacity(arr)`.
#[inline]
unsafe fn segments_array_set(arr: *mut u8, i: u64, val: *mut u8) {
    // SAFETY: gc-heap-ptr arithmetic (transient slot write).
    let slot: *mut *mut u8 = arr.add(16).add((i * 8) as usize).cast();
    slot.write(val);
}

/// Helpers for reading / writing SB record fields.
mod fields {
    pub const TOTAL_LEN_OFF: usize = 8;
    pub const SEG_COUNT_OFF: usize = 16;
    pub const SEG_USED_TAIL_OFF: usize = 24;
    pub const SEGMENTS_OFF: usize = 32;

    /// # Safety
    ///
    /// `sb` must be a pointer to a valid `TAG_STRING_BUILDER`.
    #[inline]
    pub unsafe fn read_u64(sb: *const u8, off: usize) -> u64 {
        let p: *const u64 = sb.add(off).cast();
        p.read()
    }

    /// # Safety
    ///
    /// As `read_u64`.
    #[inline]
    pub unsafe fn write_u64(sb: *mut u8, off: usize, v: u64) {
        let p: *mut u64 = sb.add(off).cast();
        p.write(v);
    }

    /// # Safety
    ///
    /// As `read_u64`.
    #[inline]
    pub unsafe fn read_segments(sb: *const u8) -> *mut u8 {
        let p: *const *mut u8 = sb.add(SEGMENTS_OFF).cast();
        p.read()
    }

    /// # Safety
    ///
    /// As `read_u64`.
    #[inline]
    pub unsafe fn write_segments(sb: *mut u8, v: *mut u8) {
        let p: *mut *mut u8 = sb.add(SEGMENTS_OFF).cast();
        p.write(v);
    }
}

/// Allocate a fresh, empty StringBuilder.
#[no_mangle]
pub extern "C" fn sigil_string_builder_new() -> *mut u8 {
    let h = Header::new(TAG_STRING_BUILDER, 4, 0b1000);
    let sb = sigil_alloc(h.raw(), SB_PAYLOAD_BYTES);
    // SAFETY: gc-heap-ptr arithmetic (transient stores into freshly-allocated SB).
    unsafe {
        fields::write_u64(sb, fields::TOTAL_LEN_OFF, 0);
        fields::write_u64(sb, fields::SEG_COUNT_OFF, 0);
        fields::write_u64(sb, fields::SEG_USED_TAIL_OFF, 0);
        let segs = alloc_segments_array(INITIAL_SEG_CAP);
        fields::write_segments(sb, segs);
    }
    counters::incr(CounterId::StringBuilderAllocCount);
    counters::add(
        CounterId::StringBuilderAllocBytes,
        (8 + SB_PAYLOAD_BYTES) as u64,
    );
    sb
}

/// Append a String's UTF-8 payload to the StringBuilder.
///
/// # Safety
///
/// `sb` must be a pointer to a valid `TAG_STRING_BUILDER` and `s`
/// to a valid `TAG_STRING`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_builder_append(sb: *mut u8, s: *const u8) {
    let s_len = crate::gc::sigil_string_len(s);
    if s_len == 0 {
        return;
    }
    // SAFETY: gc-heap-ptr arithmetic (TAG_STRING payload at offset 16).
    let s_payload: *const u8 = s.add(16);
    append_bytes(sb, s_payload, s_len);
}

/// Append `len` bytes from `src` into the StringBuilder, allocating
/// new segments and growing the segments array as needed.
///
/// # Safety
///
/// `sb` must be valid; `src` must point to at least `len` readable
/// bytes.
unsafe fn append_bytes(sb: *mut u8, src: *const u8, len: usize) {
    let mut written = 0usize;
    while written < len {
        let mut seg_count = fields::read_u64(sb, fields::SEG_COUNT_OFF);
        let mut seg_used_tail = fields::read_u64(sb, fields::SEG_USED_TAIL_OFF);

        // Allocate the first segment lazily on the first append, or
        // a new segment if the tail is full.
        let need_new_segment = seg_count == 0 || seg_used_tail as usize >= SEG_SIZE;
        if need_new_segment {
            let segs = fields::read_segments(sb);
            let cap = segments_array_capacity(segs);
            if seg_count >= cap {
                // Grow segments array (Vec-style doubling).
                let new_cap = if cap == 0 { INITIAL_SEG_CAP } else { cap * 2 };
                let new_segs = alloc_segments_array(new_cap);
                let mut i = 0u64;
                while i < seg_count {
                    let p = segments_array_get(segs, i);
                    segments_array_set(new_segs, i, p);
                    i += 1;
                }
                fields::write_segments(sb, new_segs);
            }
            let segs_now = fields::read_segments(sb);
            let new_seg = alloc_segment();
            segments_array_set(segs_now, seg_count, new_seg);
            seg_count += 1;
            seg_used_tail = 0;
            fields::write_u64(sb, fields::SEG_COUNT_OFF, seg_count);
            fields::write_u64(sb, fields::SEG_USED_TAIL_OFF, 0);
        }

        // Copy as much as fits into the tail segment.
        let tail_idx = seg_count - 1;
        let segs = fields::read_segments(sb);
        let tail_seg = segments_array_get(segs, tail_idx);
        let space_in_tail = SEG_SIZE - seg_used_tail as usize;
        let to_copy = (len - written).min(space_in_tail);
        // SAFETY: gc-heap-ptr arithmetic (segment payload at offset 16; bounded copy).
        let dst: *mut u8 = tail_seg.add(16).add(seg_used_tail as usize);
        std::ptr::copy_nonoverlapping(src.add(written), dst, to_copy);
        written += to_copy;
        let new_used = seg_used_tail + to_copy as u64;
        fields::write_u64(sb, fields::SEG_USED_TAIL_OFF, new_used);
        let total = fields::read_u64(sb, fields::TOTAL_LEN_OFF);
        fields::write_u64(sb, fields::TOTAL_LEN_OFF, total + to_copy as u64);
    }
}

/// Walk every segment, copying its bytes into a fresh `TAG_STRING`
/// of `total_len` bytes. The SB itself is unchanged; users may
/// continue appending after `sb_finalize`, but doing so produces a
/// new String each time.
///
/// # Safety
///
/// `sb` must be a pointer to a valid `TAG_STRING_BUILDER`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_builder_finalize(sb: *const u8) -> *mut u8 {
    let total = fields::read_u64(sb, fields::TOTAL_LEN_OFF);
    if total == 0 {
        return crate::gc::sigil_string_new(std::ptr::null(), 0);
    }
    // Allocate a TAG_STRING and copy segment-by-segment.
    let result = crate::gc::sigil_string_new(std::ptr::null(), total as usize);
    // SAFETY: gc-heap-ptr arithmetic (TAG_STRING payload at offset 16).
    let dst_base: *mut u8 = result.add(16);
    let segs = fields::read_segments(sb);
    let seg_count = fields::read_u64(sb, fields::SEG_COUNT_OFF);
    let seg_used_tail = fields::read_u64(sb, fields::SEG_USED_TAIL_OFF);
    let mut written = 0usize;
    let mut i = 0u64;
    while i < seg_count {
        let seg = segments_array_get(segs, i);
        let bytes_in_seg = if i + 1 == seg_count {
            seg_used_tail as usize
        } else {
            SEG_SIZE
        };
        if bytes_in_seg > 0 {
            // SAFETY: gc-heap-ptr arithmetic (bounded src/dst regions).
            let src: *const u8 = seg.add(16);
            let dst: *mut u8 = dst_base.add(written);
            std::ptr::copy_nonoverlapping(src, dst, bytes_in_seg);
            written += bytes_in_seg;
        }
        i += 1;
    }
    result
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    /// Allocate a Sigil String from a Rust slice for test fixtures.
    unsafe fn make_string(s: &[u8]) -> *mut u8 {
        // SAFETY: gc-heap-ptr arithmetic (test-only Rust slice; sigil_string_new copies into a fresh GC allocation).
        crate::gc::sigil_string_new(s.as_ptr(), s.len())
    }

    /// Read finalized String contents into a Vec for assertion.
    unsafe fn finalized_bytes(sb: *const u8) -> Vec<u8> {
        let s = sigil_string_builder_finalize(sb);
        let len = crate::gc::sigil_string_len(s);
        let payload: *const u8 = s.add(16);
        std::slice::from_raw_parts(payload, len).to_vec()
    }

    #[test]
    fn new_then_finalize_returns_empty_string() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            assert_eq!(finalized_bytes(sb), b"");
        }
    }

    #[test]
    fn single_append_round_trips() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            let s = make_string(b"hello");
            sigil_string_builder_append(sb, s);
            assert_eq!(finalized_bytes(sb), b"hello");
        }
    }

    #[test]
    fn multiple_appends_concatenate_in_order() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            let a = make_string(b"foo");
            let b = make_string(b"bar");
            let c = make_string(b"baz");
            sigil_string_builder_append(sb, a);
            sigil_string_builder_append(sb, b);
            sigil_string_builder_append(sb, c);
            assert_eq!(finalized_bytes(sb), b"foobarbaz");
        }
    }

    #[test]
    fn empty_string_append_is_noop() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            let empty = make_string(b"");
            let s = make_string(b"abc");
            sigil_string_builder_append(sb, empty);
            sigil_string_builder_append(sb, s);
            sigil_string_builder_append(sb, empty);
            assert_eq!(finalized_bytes(sb), b"abc");
        }
    }

    #[test]
    fn append_crosses_segment_boundary() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            // Fill exactly to segment boundary, then write one more
            // byte that must trigger a new segment.
            let big = vec![b'x'; SEG_SIZE];
            let s_big = make_string(&big);
            sigil_string_builder_append(sb, s_big);
            let s_tail = make_string(b"y");
            sigil_string_builder_append(sb, s_tail);
            let result = finalized_bytes(sb);
            assert_eq!(result.len(), SEG_SIZE + 1);
            assert!(result[..SEG_SIZE].iter().all(|&c| c == b'x'));
            assert_eq!(result[SEG_SIZE], b'y');
            // Two segments after the cross-boundary write.
            assert_eq!(
                fields::read_u64(sb, fields::SEG_COUNT_OFF),
                2,
                "expected 2 segments after crossing boundary"
            );
        }
    }

    #[test]
    fn append_grows_segments_array() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            // Force more than INITIAL_SEG_CAP segments.
            let chunk = vec![b'A'; SEG_SIZE];
            let s = make_string(&chunk);
            for _ in 0..(INITIAL_SEG_CAP + 2) {
                sigil_string_builder_append(sb, s);
            }
            let total = fields::read_u64(sb, fields::TOTAL_LEN_OFF);
            assert_eq!(total, (INITIAL_SEG_CAP + 2) * SEG_SIZE as u64);
            // segments array must have grown beyond INITIAL_SEG_CAP.
            let segs = fields::read_segments(sb);
            assert!(segments_array_capacity(segs) >= INITIAL_SEG_CAP + 2);
            // Spot-check finalize correctness on a large buffer.
            let result = finalized_bytes(sb);
            assert_eq!(result.len(), total as usize);
            assert!(result.iter().all(|&c| c == b'A'));
        }
    }

    #[test]
    fn append_after_finalize_extends_existing_buffer() {
        let _g = gc_test_lock();
        let sb = sigil_string_builder_new();
        unsafe {
            let a = make_string(b"hello, ");
            let b = make_string(b"world");
            sigil_string_builder_append(sb, a);
            // Mid-build snapshot.
            let snap = sigil_string_builder_finalize(sb);
            let snap_len = crate::gc::sigil_string_len(snap);
            assert_eq!(snap_len, 7);
            // Continue appending.
            sigil_string_builder_append(sb, b);
            assert_eq!(finalized_bytes(sb), b"hello, world");
        }
    }
}
