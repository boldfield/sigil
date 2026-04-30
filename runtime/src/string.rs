//! Extended `String` runtime primitives — Plan C Task 68.
//!
//! Plan A1's `runtime/src/gc.rs` ships the foundational string
//! allocator (`sigil_string_new` + `sigil_string_len`); Plan A2's
//! `arith.rs` adds `sigil_int_to_string`. This module extends the
//! sigil-side string surface with the byte-indexed accessor /
//! comparison / search / trim / parse primitives needed by the
//! Stage 7 stdlib.
//!
//! All operations treat strings as **byte sequences**, not codepoint
//! sequences — `string_byte_at`, `string_substring(start, end)`,
//! `string_index_of` etc. operate on byte offsets. Codepoint-aware
//! variants (`string_char_at`, `string_chars`) are deferred to Task
//! 68 part 2 (alongside the namespace fix that lets a stdlib module
//! use `Char` + `List` + `Result` together without `fn map`-style
//! collisions on cross-import).
//!
//! ## Layout reuse
//!
//! Allocations route through `crate::gc::sigil_string_new(src, len)`,
//! which writes the standard `{header(TAG_STRING), length, bytes,
//! padding}` layout. Internal byte access uses
//! `crate::gc::string_bytes(obj) -> (*const u8, usize)` for read-only
//! borrowing during the syscall window.
//!
//! ## Out-of-bounds / parse failure semantics
//!
//! - Index / range primitives abort the process on invalid input
//!   (`string_byte_at` index >= length, `string_substring` start >
//!   end or end > length). Mirrors `byte_array_get` / `byte_array_slice`.
//! - `string_to_int_validate` returns 0 if the string parses cleanly
//!   as a signed decimal integer, else 1 — sigil-side wrappers
//!   construct `Result[Int, ParseError]` from the result.

use crate::gc::{sigil_string_new, string_bytes};

/// Concatenate two strings into a fresh allocation. Always succeeds.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_concat(a: *const u8, b: *const u8) -> *mut u8 {
    let (a_bytes, a_len) = string_bytes(a);
    let (b_bytes, b_len) = string_bytes(b);
    let total = a_len.saturating_add(b_len);

    if total == 0 {
        return sigil_string_new(std::ptr::null(), 0);
    }

    // Allocate a destination buffer locally; the runtime's
    // `sigil_string_new` copies bytes into a fresh GC allocation.
    // Two-step (build local Vec → copy into GC heap) avoids a
    // `add` on the GC pointer between the two source spans.
    let mut buf: Vec<u8> = Vec::with_capacity(total);
    // SAFETY: gc-heap-ptr arithmetic (a_bytes / b_bytes are interior
    // pointers borrowed transiently from the source strings; the
    // copies are bounded by `a_len` / `b_len`).
    let a_slice = std::slice::from_raw_parts(a_bytes, a_len);
    let b_slice = std::slice::from_raw_parts(b_bytes, b_len);
    buf.extend_from_slice(a_slice);
    buf.extend_from_slice(b_slice);

    // SAFETY: gc-heap-ptr arithmetic (buf is a stack-local Vec, not a heap object — false-positive on the grep).
    sigil_string_new(buf.as_ptr(), total)
}

/// Read the i-th byte of `s`. Aborts on out-of-bounds.
///
/// # Safety
///
/// `s` must be a valid `TAG_STRING` header pointer; `i` must be
/// `< sigil_string_len(s)`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_byte_at(s: *const u8, i: u64) -> u8 {
    let (bytes, len) = string_bytes(s);
    if (i as usize) >= len {
        eprintln!("sigil_string_byte_at: index {i} out of bounds (len {len})");
        std::process::abort();
    }
    // SAFETY: gc-heap-ptr arithmetic (transient base + bounds-checked offset for one byte read).
    bytes.add(i as usize).read()
}

/// Substring `[start, end)`. Aborts on `start > end` or `end > len`.
/// Empty range returns the empty string.
///
/// # Safety
///
/// `s` must be a valid `TAG_STRING` header pointer.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_substring(s: *const u8, start: u64, end: u64) -> *mut u8 {
    let (bytes, len) = string_bytes(s);
    if start > end {
        eprintln!("sigil_string_substring: start {start} > end {end}");
        std::process::abort();
    }
    if (end as usize) > len {
        eprintln!("sigil_string_substring: end {end} out of bounds (len {len})");
        std::process::abort();
    }
    let slice_len = (end - start) as usize;
    if slice_len == 0 {
        return sigil_string_new(std::ptr::null(), 0);
    }
    // SAFETY: gc-heap-ptr arithmetic (transient bytes + bounds-checked start; len bounded above).
    let src = bytes.add(start as usize);
    sigil_string_new(src, slice_len)
}

/// Lexicographic byte comparison. Returns -1, 0, or 1.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_compare(a: *const u8, b: *const u8) -> i64 {
    let (a_bytes, a_len) = string_bytes(a);
    let (b_bytes, b_len) = string_bytes(b);
    let a_slice = std::slice::from_raw_parts(a_bytes, a_len);
    let b_slice = std::slice::from_raw_parts(b_bytes, b_len);
    match a_slice.cmp(b_slice) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Returns true iff `s` starts with `prefix`.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_starts_with(s: *const u8, prefix: *const u8) -> bool {
    let (s_bytes, s_len) = string_bytes(s);
    let (p_bytes, p_len) = string_bytes(prefix);
    if p_len > s_len {
        return false;
    }
    let s_slice = std::slice::from_raw_parts(s_bytes, p_len);
    let p_slice = std::slice::from_raw_parts(p_bytes, p_len);
    s_slice == p_slice
}

/// Returns true iff `s` ends with `suffix`.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_ends_with(s: *const u8, suffix: *const u8) -> bool {
    let (s_bytes, s_len) = string_bytes(s);
    let (sf_bytes, sf_len) = string_bytes(suffix);
    if sf_len > s_len {
        return false;
    }
    let s_tail_start = s_len - sf_len;
    // SAFETY: gc-heap-ptr arithmetic (bytes + bounded tail-start; sf_len bounded by allocation).
    let s_tail = std::slice::from_raw_parts(s_bytes.add(s_tail_start), sf_len);
    let sf_slice = std::slice::from_raw_parts(sf_bytes, sf_len);
    s_tail == sf_slice
}

/// Returns true iff `s` contains `needle` as a contiguous byte
/// substring.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_contains(s: *const u8, needle: *const u8) -> bool {
    sigil_string_index_of(s, needle) >= 0
}

/// Returns the byte offset of the first occurrence of `needle` in
/// `s`, or `-1` if absent. An empty `needle` returns `0`.
///
/// # Safety
///
/// Both arguments must point at valid `TAG_STRING` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_index_of(s: *const u8, needle: *const u8) -> i64 {
    let (s_bytes, s_len) = string_bytes(s);
    let (n_bytes, n_len) = string_bytes(needle);
    if n_len == 0 {
        return 0;
    }
    if n_len > s_len {
        return -1;
    }
    let s_slice = std::slice::from_raw_parts(s_bytes, s_len);
    let n_slice = std::slice::from_raw_parts(n_bytes, n_len);
    // Naive O(s_len * n_len) search — sufficient for v1 stdlib needs.
    let last = s_len - n_len;
    for i in 0..=last {
        if s_slice[i..i + n_len] == *n_slice {
            return i as i64;
        }
    }
    -1
}

/// Trim ASCII whitespace (space / tab / newline / CR) from both
/// sides. Returns a fresh allocation; the original is unchanged.
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_trim(s: *const u8) -> *mut u8 {
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let mut start = 0;
    while start < len && is_ascii_ws(slice[start]) {
        start += 1;
    }
    let mut end = len;
    while end > start && is_ascii_ws(slice[end - 1]) {
        end -= 1;
    }
    let new_len = end - start;
    if new_len == 0 {
        return sigil_string_new(std::ptr::null(), 0);
    }
    // SAFETY: gc-heap-ptr arithmetic (transient bytes + start; new_len bounded by len).
    let src = bytes.add(start);
    sigil_string_new(src, new_len)
}

#[inline]
fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Validate `s` as a signed decimal integer. Returns `0` on success;
/// `1` if the string is empty, `2` if a non-digit character appears
/// after an optional leading sign, `3` if the value overflows `i64`.
/// Sigil-side wrappers translate the discriminant into a sum-typed
/// `Result[Int, ParseError]`. (Returning a discriminant rather than
/// a struct sidesteps the multi-return ABI complication seen in
/// `byte.rs::sigil_byte_from_int_checked`.)
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_to_int_validate(s: *const u8) -> i64 {
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let text = match std::str::from_utf8(slice) {
        Ok(t) => t,
        Err(_) => return 2,
    };
    if text.is_empty() {
        return 1;
    }
    match text.parse::<i64>() {
        Ok(_) => 0,
        Err(e) => match e.kind() {
            std::num::IntErrorKind::Empty => 1,
            std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => 3,
            _ => 2,
        },
    }
}

/// Parse `s` as a signed decimal integer. The caller is responsible
/// for having checked `sigil_string_to_int_validate(s) == 0`; on
/// validated input this primitive returns the parsed value. On
/// unvalidated input the return is unspecified (production code
/// always pairs validate + alloc).
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header AND have passed
/// `sigil_string_to_int_validate`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_to_int_parse(s: *const u8) -> i64 {
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let text = std::str::from_utf8(slice).unwrap_or("0");
    text.parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::gc::sigil_string_len;
    use crate::test_support::gc_test_lock;

    fn mk_string(s: &str) -> *mut u8 {
        // SAFETY: gc-heap-ptr arithmetic (s.as_ptr() is a static / stack-local borrow, not a heap object).
        unsafe { sigil_string_new(s.as_ptr(), s.len()) }
    }

    #[test]
    fn concat_two_strings() {
        let _guard = gc_test_lock();
        let a = mk_string("hello, ");
        let b = mk_string("world");
        unsafe {
            let c = sigil_string_concat(a, b);
            assert_eq!(sigil_string_len(c), 12);
            for (i, expected) in b"hello, world".iter().enumerate() {
                assert_eq!(sigil_string_byte_at(c, i as u64), *expected);
            }
        }
    }

    #[test]
    fn concat_with_empty_returns_other() {
        let _guard = gc_test_lock();
        let a = mk_string("");
        let b = mk_string("nonempty");
        unsafe {
            let c = sigil_string_concat(a, b);
            assert_eq!(sigil_string_len(c), 8);
        }
    }

    #[test]
    fn substring_extracts_subrange() {
        let _guard = gc_test_lock();
        let s = mk_string("0123456789");
        unsafe {
            let sub = sigil_string_substring(s, 3, 7);
            assert_eq!(sigil_string_len(sub), 4);
            for (i, expected) in b"3456".iter().enumerate() {
                assert_eq!(sigil_string_byte_at(sub, i as u64), *expected);
            }
        }
    }

    #[test]
    fn substring_empty_range_returns_empty() {
        let _guard = gc_test_lock();
        let s = mk_string("hello");
        unsafe {
            let sub = sigil_string_substring(s, 2, 2);
            assert_eq!(sigil_string_len(sub), 0);
        }
    }

    #[test]
    fn compare_lt_eq_gt() {
        let _guard = gc_test_lock();
        let a = mk_string("apple");
        let b = mk_string("banana");
        let c = mk_string("apple");
        unsafe {
            assert_eq!(sigil_string_compare(a, b), -1);
            assert_eq!(sigil_string_compare(b, a), 1);
            assert_eq!(sigil_string_compare(a, c), 0);
        }
    }

    #[test]
    fn starts_with_prefix() {
        let _guard = gc_test_lock();
        let s = mk_string("hello world");
        let p_yes = mk_string("hello");
        let p_no = mk_string("world");
        let p_long = mk_string("hello world!");
        unsafe {
            assert!(sigil_string_starts_with(s, p_yes));
            assert!(!sigil_string_starts_with(s, p_no));
            assert!(!sigil_string_starts_with(s, p_long));
        }
    }

    #[test]
    fn ends_with_suffix() {
        let _guard = gc_test_lock();
        let s = mk_string("hello world");
        let p_yes = mk_string("world");
        let p_no = mk_string("hello");
        unsafe {
            assert!(sigil_string_ends_with(s, p_yes));
            assert!(!sigil_string_ends_with(s, p_no));
        }
    }

    #[test]
    fn contains_substring() {
        let _guard = gc_test_lock();
        let s = mk_string("the quick brown fox");
        let yes = mk_string("brown");
        let no = mk_string("blue");
        unsafe {
            assert!(sigil_string_contains(s, yes));
            assert!(!sigil_string_contains(s, no));
        }
    }

    #[test]
    fn index_of_returns_first_occurrence() {
        let _guard = gc_test_lock();
        let s = mk_string("abcabc");
        let needle = mk_string("bc");
        let absent = mk_string("xyz");
        let empty = mk_string("");
        unsafe {
            assert_eq!(sigil_string_index_of(s, needle), 1);
            assert_eq!(sigil_string_index_of(s, absent), -1);
            // Empty needle conventionally matches at position 0.
            assert_eq!(sigil_string_index_of(s, empty), 0);
        }
    }

    #[test]
    fn trim_removes_ascii_whitespace_both_sides() {
        let _guard = gc_test_lock();
        let s = mk_string("  \t hello world\n\r  ");
        unsafe {
            let trimmed = sigil_string_trim(s);
            assert_eq!(sigil_string_len(trimmed), 11);
            for (i, expected) in b"hello world".iter().enumerate() {
                assert_eq!(sigil_string_byte_at(trimmed, i as u64), *expected);
            }
        }
    }

    #[test]
    fn trim_all_whitespace_returns_empty() {
        let _guard = gc_test_lock();
        let s = mk_string("   ");
        unsafe {
            let trimmed = sigil_string_trim(s);
            assert_eq!(sigil_string_len(trimmed), 0);
        }
    }

    #[test]
    fn to_int_validate_accepts_clean_decimals() {
        let _guard = gc_test_lock();
        unsafe {
            assert_eq!(sigil_string_to_int_validate(mk_string("0")), 0);
            assert_eq!(sigil_string_to_int_validate(mk_string("42")), 0);
            assert_eq!(sigil_string_to_int_validate(mk_string("-7")), 0);
            assert_eq!(
                sigil_string_to_int_parse(mk_string("9223372036854775807")),
                i64::MAX
            );
            assert_eq!(sigil_string_to_int_parse(mk_string("42")), 42);
            assert_eq!(sigil_string_to_int_parse(mk_string("-7")), -7);
        }
    }

    #[test]
    fn to_int_validate_rejects_non_decimal() {
        let _guard = gc_test_lock();
        unsafe {
            assert_eq!(sigil_string_to_int_validate(mk_string("")), 1); // empty
            assert_eq!(sigil_string_to_int_validate(mk_string("abc")), 2); // non-digit
            assert_eq!(sigil_string_to_int_validate(mk_string("12x")), 2); // partial
                                                                           // Overflow: 2^63 = 9223372036854775808 (i64 max + 1).
            assert_eq!(
                sigil_string_to_int_validate(mk_string("9223372036854775808")),
                3
            );
        }
    }
}
