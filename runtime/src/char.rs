//! Boxed `Char` (Unicode codepoint) runtime primitives.
//!
//! Layout on the heap:
//!
//! ```text
//! offset 0  : 8-byte header (tag = TAG_CHAR, count = 1, bitmap = 0)
//! offset 8  : u32 codepoint (low 21 bits used)
//! offset 12 : 4 bytes padding (alignment)
//! ```
//!
//! Bitmap is `0` (atomic alloc): the payload is a scalar, never a
//! pointer. Codepoint domain is `0x000000..=0x10FFFF`, excluding
//! surrogates `0xD800..=0xDFFF`. Follows the `TAG_FLOAT` /
//! `TAG_INT64` boxed-scalar precedent exactly.
//!
//! ## List construction (string_chars / string_from_chars)
//!
//! `sigil_string_chars` and `sigil_string_from_chars` build / walk
//! Sigil `List[Char]` Cons-Nil cells. The monomorphized `List[Char]`
//! type tag is assigned at codegen time and varies per-program, so
//! the header words and discriminants are passed in from the
//! generated call site rather than hard-coded here. The runtime
//! constructs Cons / Nil records by stamping the supplied headers
//! over a freshly-allocated payload (8-byte discriminant word + 0
//! or 2 fields). This keeps the runtime free of any compile-time
//! layout knowledge while still letting it produce well-typed
//! Sigil values.

use crate::counters::{self, CounterId};
#[cfg(test)]
use crate::gc::sigil_string_len;
use crate::gc::{sigil_alloc, sigil_string_new, string_bytes};
use crate::header::{Header, TAG_CHAR};

const REPLACEMENT_CODEPOINT: u32 = 0xFFFD;
const MAX_CODEPOINT: u32 = 0x10_FFFF;
const SURROGATE_LO: u32 = 0xD800;
const SURROGATE_HI: u32 = 0xDFFF;

// Cons / Nil payload sizes are fixed by the `List[A] = | Nil | Cons(A, List[A])`
// declaration in `std/list.sigil`: Nil has 1 payload word (the discriminant),
// Cons has 3 (discriminant + head + tail). The variant declaration order is
// pinned by the same file. The header word and discriminant value are still
// passed from codegen because the type tag itself is monomorphization-
// dependent — but the *shape* (payload byte counts) is shared across every
// `List[A]` instantiation.
const NIL_PAYLOAD_BYTES: usize = 8;
const CONS_PAYLOAD_BYTES: usize = 24;

fn alloc_char(codepoint: u32) -> *mut u8 {
    let h = Header::new(TAG_CHAR, 1, 0);
    let obj = sigil_alloc(h.raw(), 8);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u32 store).
    unsafe {
        let p: *mut u32 = obj.add(8).cast();
        p.write(codepoint);
    }
    counters::incr(CounterId::CharAllocCount);
    counters::add(CounterId::CharAllocBytes, 16);
    obj
}

#[inline]
unsafe fn read_char(p: *const u8) -> u32 {
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned u32 read).
    let payload: *const u32 = p.add(8).cast();
    payload.read()
}

#[inline]
fn is_valid_codepoint_i64(n: i64) -> bool {
    if n < 0 {
        return false;
    }
    let n = n as u64;
    if n > MAX_CODEPOINT as u64 {
        return false;
    }
    let n = n as u32;
    !(SURROGATE_LO..=SURROGATE_HI).contains(&n)
}

// ── Boxing / unboxing ──────────────────────────────────────────────

/// Allocate a fresh `Char` from a codepoint passed as i64. Caller must
/// have validated; only the low 21 bits are stored. Used by codegen
/// for both literal lowering and the post-validation construct path
/// in `int_to_char`.
#[no_mangle]
pub extern "C" fn sigil_char_box(codepoint: i64) -> *mut u8 {
    alloc_char(codepoint as u32 & 0x001F_FFFF)
}

/// # Safety
///
/// `p` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_unbox(p: *const u8) -> u32 {
    read_char(p)
}

// ── Equality / ordering ────────────────────────────────────────────

/// # Safety
///
/// `a` and `b` must each point at valid `TAG_CHAR` headers.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_eq(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_char(a) == read_char(b))
}

/// # Safety
///
/// As `sigil_char_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_lt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_char(a) < read_char(b))
}

/// # Safety
///
/// As `sigil_char_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_le(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_char(a) <= read_char(b))
}

/// # Safety
///
/// As `sigil_char_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_gt(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_char(a) > read_char(b))
}

/// # Safety
///
/// As `sigil_char_eq`.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_ge(a: *const u8, b: *const u8) -> u8 {
    u8::from(read_char(a) >= read_char(b))
}

// ── Conversion ─────────────────────────────────────────────────────

/// Return the boxed codepoint as i64 (always fits — codepoints are
/// ≤ 0x10FFFF).
///
/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_to_int(a: *const u8) -> i64 {
    read_char(a) as i64
}

/// Validate an `Int` for `int_to_char`. Returns `0` if `n` is a valid
/// Unicode codepoint (in `0..=0x10FFFF` and not a surrogate); `1`
/// otherwise. Codegen pairs this with `sigil_int_to_char_box` in the
/// validate-then-construct lowering of `int_to_char`.
#[no_mangle]
pub extern "C" fn sigil_int_to_char_validate(n: i64) -> i64 {
    if is_valid_codepoint_i64(n) {
        0
    } else {
        1
    }
}

/// Allocate a fresh `Char` from a validated `Int`. Caller must have
/// invoked `sigil_int_to_char_validate` first. Same shape as the
/// `sigil_string_to_int_parse` post-validation primitive.
#[no_mangle]
pub extern "C" fn sigil_int_to_char_box(n: i64) -> *mut u8 {
    alloc_char(n as u32 & 0x001F_FFFF)
}

/// UTF-8 encode the codepoint into a fresh String (1–4 bytes).
///
/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header AND its codepoint
/// must be a valid Unicode scalar (which the runtime ensures at all
/// allocation sites by validating up front).
#[no_mangle]
pub unsafe extern "C" fn sigil_char_to_string(a: *const u8) -> *mut u8 {
    let cp = read_char(a);
    // SAFETY: every `Char` allocation site validates that the codepoint
    // is a valid scalar value (literal lowering rejects surrogates +
    // out-of-range at parse time; `sigil_int_to_char_validate` rejects
    // them at runtime; `sigil_string_chars` clamps invalid bytes to
    // U+FFFD; ASCII case maps stay in-range; `from_u32_unchecked` is
    // sound here).
    let ch = char::from_u32_unchecked(cp);
    let mut buf = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buf);
    // SAFETY: gc-heap-ptr arithmetic (Rust-owned stack buffer; sigil_string_new copies into a fresh GC alloc).
    sigil_string_new(s.as_ptr(), s.len())
}

// ── ASCII classifiers ──────────────────────────────────────────────

/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_is_ascii(a: *const u8) -> u8 {
    u8::from(read_char(a) < 0x80)
}

/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_is_ascii_digit(a: *const u8) -> u8 {
    let cp = read_char(a);
    u8::from((b'0' as u32..=b'9' as u32).contains(&cp))
}

/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_is_ascii_alpha(a: *const u8) -> u8 {
    let cp = read_char(a);
    let lower = (b'a' as u32..=b'z' as u32).contains(&cp);
    let upper = (b'A' as u32..=b'Z' as u32).contains(&cp);
    u8::from(lower || upper)
}

/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_is_ascii_alphanumeric(a: *const u8) -> u8 {
    let cp = read_char(a);
    let lower = (b'a' as u32..=b'z' as u32).contains(&cp);
    let upper = (b'A' as u32..=b'Z' as u32).contains(&cp);
    let digit = (b'0' as u32..=b'9' as u32).contains(&cp);
    u8::from(lower || upper || digit)
}

/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_is_ascii_whitespace(a: *const u8) -> u8 {
    let cp = read_char(a);
    let is_ws = matches!(cp, 0x20 | 0x09 | 0x0A | 0x0D | 0x0C);
    u8::from(is_ws)
}

// ── ASCII case ─────────────────────────────────────────────────────

/// Lowercase ASCII letters; non-ASCII codepoints pass through unchanged.
/// Allocates a fresh `Char`.
///
/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_to_lower_ascii(a: *const u8) -> *mut u8 {
    let cp = read_char(a);
    let out = if (b'A' as u32..=b'Z' as u32).contains(&cp) {
        cp + 0x20
    } else {
        cp
    };
    alloc_char(out)
}

/// Uppercase ASCII letters; non-ASCII codepoints pass through unchanged.
/// Allocates a fresh `Char`.
///
/// # Safety
///
/// `a` must point at a valid `TAG_CHAR` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_char_to_upper_ascii(a: *const u8) -> *mut u8 {
    let cp = read_char(a);
    let out = if (b'a' as u32..=b'z' as u32).contains(&cp) {
        cp - 0x20
    } else {
        cp
    };
    alloc_char(out)
}

// ── String codepoint ops ───────────────────────────────────────────

/// Decode UTF-8 from `bytes` into a Vec of codepoints, replacing each
/// invalid byte with U+FFFD and resyncing at the next valid leading
/// byte. Implements the same lossy contract as
/// `String::from_utf8_lossy`, but exposed as a `Vec<u32>` so the
/// caller can build a Sigil `List[Char]` cell-by-cell.
fn decode_codepoints_lossy(bytes: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        if b0 < 0x80 {
            out.push(b0 as u32);
            i += 1;
            continue;
        }
        // Decode continuation bytes following b0. Each must satisfy
        // 10xxxxxx; otherwise the sequence is invalid and we emit
        // U+FFFD for b0 alone, advancing one byte. Overlong encodings
        // and surrogate-range codepoints are also rejected per the
        // standard UTF-8 decoder.
        let cont = |b: u8| (b & 0xC0) == 0x80;
        let payload = |b: u8| (b & 0x3F) as u32;
        let (cp, len) = if (b0 & 0xE0) == 0xC0 && i + 1 < bytes.len() && cont(bytes[i + 1]) {
            let v = ((b0 & 0x1F) as u32) << 6 | payload(bytes[i + 1]);
            (v, 2usize)
        } else if (b0 & 0xF0) == 0xE0
            && i + 2 < bytes.len()
            && cont(bytes[i + 1])
            && cont(bytes[i + 2])
        {
            let v = ((b0 & 0x0F) as u32) << 12 | payload(bytes[i + 1]) << 6 | payload(bytes[i + 2]);
            (v, 3usize)
        } else if (b0 & 0xF8) == 0xF0
            && i + 3 < bytes.len()
            && cont(bytes[i + 1])
            && cont(bytes[i + 2])
            && cont(bytes[i + 3])
        {
            let v = ((b0 & 0x07) as u32) << 18
                | payload(bytes[i + 1]) << 12
                | payload(bytes[i + 2]) << 6
                | payload(bytes[i + 3]);
            (v, 4usize)
        } else {
            out.push(REPLACEMENT_CODEPOINT);
            i += 1;
            continue;
        };
        let overlong = match len {
            2 => cp < 0x80,
            3 => cp < 0x800,
            4 => cp < 0x10000,
            _ => false,
        };
        let valid =
            !overlong && cp <= MAX_CODEPOINT && !(SURROGATE_LO..=SURROGATE_HI).contains(&cp);
        if valid {
            out.push(cp);
            i += len;
        } else {
            out.push(REPLACEMENT_CODEPOINT);
            i += 1;
        }
    }
    out
}

/// Allocate a `List[Char]` Cons cell from `head` and `tail` using the
/// codegen-supplied header word and discriminant. The Cons payload
/// layout is fixed by std/list.sigil: discriminant at offset 8, head
/// at offset 16, tail at offset 24.
unsafe fn alloc_cons(head: *mut u8, tail: *mut u8, cons_header: u64, cons_disc: i64) -> *mut u8 {
    let obj = sigil_alloc(cons_header, CONS_PAYLOAD_BYTES);
    // SAFETY: gc-heap-ptr arithmetic (transient base for three aligned 8-byte stores).
    let disc_p: *mut i64 = obj.add(8).cast();
    disc_p.write(cons_disc);
    let head_p: *mut *mut u8 = obj.add(16).cast();
    head_p.write(head);
    let tail_p: *mut *mut u8 = obj.add(24).cast();
    tail_p.write(tail);
    obj
}

/// Allocate a `List[Char]` Nil cell using the codegen-supplied
/// header word and discriminant. Payload layout: just the
/// discriminant word at offset 8.
unsafe fn alloc_nil(nil_header: u64, nil_disc: i64) -> *mut u8 {
    let obj = sigil_alloc(nil_header, NIL_PAYLOAD_BYTES);
    // SAFETY: gc-heap-ptr arithmetic (transient base for one aligned 8-byte store).
    let disc_p: *mut i64 = obj.add(8).cast();
    disc_p.write(nil_disc);
    obj
}

/// UTF-8 decode `s` lossily and return the codepoints as a fresh
/// `List[Char]`. `cons_header` / `nil_header` are the
/// codegen-computed header words for the program-specific
/// `List[Char]` instantiation; `cons_disc` / `nil_disc` are the
/// variant discriminants.
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_chars(
    s: *const u8,
    cons_header: u64,
    cons_disc: i64,
    nil_header: u64,
    nil_disc: i64,
) -> *mut u8 {
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let cps = decode_codepoints_lossy(slice);
    let mut tail = alloc_nil(nil_header, nil_disc);
    // Build right-to-left so the resulting List preserves source order.
    for &cp in cps.iter().rev() {
        let head = alloc_char(cp);
        tail = alloc_cons(head, tail, cons_header, cons_disc);
    }
    tail
}

/// Validate a codepoint index for `string_char_at`. Returns `0` if
/// `idx` is in `0..codepoint_count(s)`; `1` otherwise. Lossy decode
/// (matching `sigil_string_chars`).
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_char_at_validate(s: *const u8, idx: i64) -> i64 {
    if idx < 0 {
        return 1;
    }
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let cp_count = decode_codepoints_lossy(slice).len() as i64;
    if idx < cp_count {
        0
    } else {
        1
    }
}

/// Return the `Char` at codepoint index `idx`. Caller must have
/// invoked `sigil_string_char_at_validate` first.
///
/// # Safety
///
/// `s` must point at a valid `TAG_STRING` header AND `idx` must
/// satisfy `sigil_string_char_at_validate`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_char_at(s: *const u8, idx: i64) -> *mut u8 {
    let (bytes, len) = string_bytes(s);
    let slice = std::slice::from_raw_parts(bytes, len);
    let cps = decode_codepoints_lossy(slice);
    let cp = cps[idx as usize];
    alloc_char(cp)
}

/// UTF-8 encode each `Char` in a `List[Char]` and concatenate into
/// a fresh String. `cons_disc` / `nil_disc` are the codegen-computed
/// discriminants used to dispatch on each cell.
///
/// # Safety
///
/// `list` must point at the head of a well-formed `List[Char]`
/// terminated by a Nil cell whose discriminant equals `nil_disc`.
#[no_mangle]
pub unsafe extern "C" fn sigil_string_from_chars(
    list: *const u8,
    cons_disc: i64,
    nil_disc: i64,
) -> *mut u8 {
    // First pass: total UTF-8 byte length.
    let mut total: usize = 0;
    let mut cur = list;
    loop {
        // SAFETY: gc-heap-ptr arithmetic (transient discriminant read at offset 8).
        let disc: i64 = (cur.add(8) as *const i64).read();
        if disc == nil_disc {
            break;
        }
        if disc != cons_disc {
            eprintln!(
                "sigil_string_from_chars: unexpected list discriminant {disc}; \
                 expected Cons={cons_disc} or Nil={nil_disc}"
            );
            std::process::abort();
        }
        // SAFETY: gc-heap-ptr arithmetic (transient head read at offset 16).
        let head: *const u8 = (cur.add(16) as *const *const u8).read();
        let cp = read_char(head);
        // SAFETY: lossy decode yields valid scalars; literals are validated;
        // `int_to_char_validate` filters surrogates / out-of-range.
        let ch = char::from_u32_unchecked(cp);
        total += ch.len_utf8();
        // SAFETY: gc-heap-ptr arithmetic (transient tail read at offset 24).
        cur = (cur.add(24) as *const *const u8).read();
    }

    // Second pass: encode into a Vec<u8>, then copy into a fresh String.
    let mut buf: Vec<u8> = Vec::with_capacity(total);
    let mut cur = list;
    loop {
        // SAFETY: same shape as the first pass.
        let disc: i64 = (cur.add(8) as *const i64).read();
        if disc == nil_disc {
            break;
        }
        let head: *const u8 = (cur.add(16) as *const *const u8).read();
        let cp = read_char(head);
        let ch = char::from_u32_unchecked(cp);
        let mut enc = [0u8; 4];
        let s = ch.encode_utf8(&mut enc);
        buf.extend_from_slice(s.as_bytes());
        cur = (cur.add(24) as *const *const u8).read();
    }
    debug_assert_eq!(buf.len(), total, "sigil_string_from_chars: length mismatch");

    // SAFETY: gc-heap-ptr arithmetic (Rust-owned Vec buffer; sigil_string_new copies into a fresh GC alloc).
    sigil_string_new(buf.as_ptr(), buf.len())
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::test_support::gc_test_lock;

    fn boxc(cp: u32) -> *mut u8 {
        sigil_char_box(cp as i64)
    }

    unsafe fn cp(p: *const u8) -> u32 {
        read_char(p)
    }

    #[test]
    fn box_unbox_ascii() {
        let _g = gc_test_lock();
        let p = boxc(b'A' as u32);
        unsafe {
            assert_eq!(sigil_char_unbox(p), b'A' as u32);
        }
    }

    #[test]
    fn box_unbox_2byte_bmp() {
        let _g = gc_test_lock();
        let p = boxc(0x00E9); // 'é'
        unsafe {
            assert_eq!(cp(p), 0x00E9);
        }
    }

    #[test]
    fn box_unbox_3byte_bmp() {
        let _g = gc_test_lock();
        let p = boxc(0x4E2D); // '中'
        unsafe {
            assert_eq!(cp(p), 0x4E2D);
        }
    }

    #[test]
    fn box_unbox_4byte_supplementary() {
        let _g = gc_test_lock();
        let p = boxc(0x1F600); // '😀'
        unsafe {
            assert_eq!(cp(p), 0x1F600);
        }
    }

    #[test]
    fn comparisons() {
        let _g = gc_test_lock();
        let a = boxc(b'a' as u32);
        let b = boxc(b'b' as u32);
        unsafe {
            assert_eq!(sigil_char_eq(a, a), 1);
            assert_eq!(sigil_char_eq(a, b), 0);
            assert_eq!(sigil_char_lt(a, b), 1);
            assert_eq!(sigil_char_le(a, a), 1);
            assert_eq!(sigil_char_gt(b, a), 1);
            assert_eq!(sigil_char_ge(a, a), 1);
            assert_eq!(sigil_char_ge(a, b), 0);
        }
    }

    #[test]
    fn classifiers_digit() {
        let _g = gc_test_lock();
        let five = boxc(b'5' as u32);
        let a = boxc(b'a' as u32);
        unsafe {
            assert_eq!(sigil_char_is_ascii_digit(five), 1);
            assert_eq!(sigil_char_is_ascii_digit(a), 0);
        }
    }

    #[test]
    fn classifiers_alpha() {
        let _g = gc_test_lock();
        let a = boxc(b'a' as u32);
        let z = boxc(b'Z' as u32);
        let five = boxc(b'5' as u32);
        let e_acute = boxc(0x00E9);
        unsafe {
            assert_eq!(sigil_char_is_ascii_alpha(a), 1);
            assert_eq!(sigil_char_is_ascii_alpha(z), 1);
            assert_eq!(sigil_char_is_ascii_alpha(five), 0);
            assert_eq!(sigil_char_is_ascii_alpha(e_acute), 0);
        }
    }

    #[test]
    fn classifiers_alphanumeric() {
        let _g = gc_test_lock();
        let a = boxc(b'a' as u32);
        let five = boxc(b'5' as u32);
        let space = boxc(0x20);
        unsafe {
            assert_eq!(sigil_char_is_ascii_alphanumeric(a), 1);
            assert_eq!(sigil_char_is_ascii_alphanumeric(five), 1);
            assert_eq!(sigil_char_is_ascii_alphanumeric(space), 0);
        }
    }

    #[test]
    fn classifiers_whitespace() {
        let _g = gc_test_lock();
        let space = boxc(0x20);
        let tab = boxc(0x09);
        let newline = boxc(0x0A);
        let cr = boxc(0x0D);
        let formfeed = boxc(0x0C);
        let a = boxc(b'a' as u32);
        unsafe {
            assert_eq!(sigil_char_is_ascii_whitespace(space), 1);
            assert_eq!(sigil_char_is_ascii_whitespace(tab), 1);
            assert_eq!(sigil_char_is_ascii_whitespace(newline), 1);
            assert_eq!(sigil_char_is_ascii_whitespace(cr), 1);
            assert_eq!(sigil_char_is_ascii_whitespace(formfeed), 1);
            assert_eq!(sigil_char_is_ascii_whitespace(a), 0);
        }
    }

    #[test]
    fn classifiers_is_ascii() {
        let _g = gc_test_lock();
        let a = boxc(b'a' as u32);
        let e_acute = boxc(0x00E9);
        unsafe {
            assert_eq!(sigil_char_is_ascii(a), 1);
            assert_eq!(sigil_char_is_ascii(e_acute), 0);
        }
    }

    #[test]
    fn case_lower_ascii() {
        let _g = gc_test_lock();
        let a_upper = boxc(b'A' as u32);
        let a_lower = boxc(b'a' as u32);
        let e_acute = boxc(0x00E9);
        unsafe {
            assert_eq!(cp(sigil_char_to_lower_ascii(a_upper)), b'a' as u32);
            assert_eq!(cp(sigil_char_to_lower_ascii(a_lower)), b'a' as u32);
            assert_eq!(cp(sigil_char_to_lower_ascii(e_acute)), 0x00E9);
        }
    }

    #[test]
    fn case_upper_ascii() {
        let _g = gc_test_lock();
        let a_upper = boxc(b'A' as u32);
        let a_lower = boxc(b'a' as u32);
        let e_acute = boxc(0x00E9);
        unsafe {
            assert_eq!(cp(sigil_char_to_upper_ascii(a_upper)), b'A' as u32);
            assert_eq!(cp(sigil_char_to_upper_ascii(a_lower)), b'A' as u32);
            assert_eq!(cp(sigil_char_to_upper_ascii(e_acute)), 0x00E9);
        }
    }

    #[test]
    fn int_to_char_validate_in_range() {
        assert_eq!(sigil_int_to_char_validate(0), 0);
        assert_eq!(sigil_int_to_char_validate(0x7F), 0);
        assert_eq!(sigil_int_to_char_validate(0xD7FF), 0);
        assert_eq!(sigil_int_to_char_validate(0xE000), 0);
        assert_eq!(sigil_int_to_char_validate(0x10_FFFF), 0);
    }

    #[test]
    fn int_to_char_validate_rejects_negative() {
        assert_eq!(sigil_int_to_char_validate(-1), 1);
    }

    #[test]
    fn int_to_char_validate_rejects_out_of_range() {
        assert_eq!(sigil_int_to_char_validate(0x110000), 1);
    }

    #[test]
    fn int_to_char_validate_rejects_surrogates() {
        assert_eq!(sigil_int_to_char_validate(0xD800), 1);
        assert_eq!(sigil_int_to_char_validate(0xDFFF), 1);
    }

    #[test]
    fn char_to_int_round_trip() {
        let _g = gc_test_lock();
        let a = boxc(b'A' as u32);
        let smiley = boxc(0x1F600);
        unsafe {
            assert_eq!(sigil_char_to_int(a), b'A' as i64);
            assert_eq!(sigil_char_to_int(smiley), 0x1F600);
        }
    }

    fn string_bytes_of(p: *const u8) -> Vec<u8> {
        unsafe {
            let len = sigil_string_len(p);
            let payload: *const u8 = p.add(16);
            std::slice::from_raw_parts(payload, len).to_vec()
        }
    }

    #[test]
    fn char_to_string_ascii() {
        let _g = gc_test_lock();
        let a = boxc(b'A' as u32);
        unsafe {
            let s = sigil_char_to_string(a);
            assert_eq!(string_bytes_of(s), b"A");
        }
    }

    #[test]
    fn char_to_string_2byte() {
        let _g = gc_test_lock();
        let e_acute = boxc(0x00E9);
        unsafe {
            let s = sigil_char_to_string(e_acute);
            assert_eq!(string_bytes_of(s), "é".as_bytes());
        }
    }

    #[test]
    fn char_to_string_3byte() {
        let _g = gc_test_lock();
        let zhong = boxc(0x4E2D);
        unsafe {
            let s = sigil_char_to_string(zhong);
            assert_eq!(string_bytes_of(s), "中".as_bytes());
        }
    }

    #[test]
    fn char_to_string_4byte() {
        let _g = gc_test_lock();
        let smiley = boxc(0x1F600);
        unsafe {
            let s = sigil_char_to_string(smiley);
            assert_eq!(string_bytes_of(s), "😀".as_bytes());
        }
    }

    fn make_string(bytes: &[u8]) -> *mut u8 {
        // SAFETY: gc-heap-ptr arithmetic (static byte slice; sigil_string_new copies into a fresh GC alloc).
        unsafe { sigil_string_new(bytes.as_ptr(), bytes.len()) }
    }

    // For runtime-side List tests, build headers using the same formula codegen
    // would. We pick an arbitrary sentinel type tag in the user range; the value
    // is opaque to the runtime — it just gets stamped into the header.
    const TEST_LIST_TAG: u8 = 0x10;
    const TEST_NIL_DISC: i64 = 0;
    const TEST_CONS_DISC: i64 = 1;

    fn nil_header_word() -> u64 {
        sigil_header_constants::header_word(TEST_LIST_TAG, 1, 0)
    }
    fn cons_header_word() -> u64 {
        sigil_header_constants::header_word(TEST_LIST_TAG, 3, 0b110)
    }

    unsafe fn list_to_codepoints(mut list: *const u8) -> Vec<u32> {
        let mut out = Vec::new();
        loop {
            let disc: i64 = (list.add(8) as *const i64).read();
            if disc == TEST_NIL_DISC {
                break;
            }
            assert_eq!(disc, TEST_CONS_DISC, "unexpected list discriminant {disc}");
            let head: *const u8 = (list.add(16) as *const *const u8).read();
            out.push(read_char(head));
            list = (list.add(24) as *const *const u8).read();
        }
        out
    }

    #[test]
    fn string_chars_ascii() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string(b"abc");
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(list_to_codepoints(list), vec![0x61, 0x62, 0x63]);
        }
    }

    #[test]
    fn string_chars_multibyte() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string("héllo".as_bytes());
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(
                list_to_codepoints(list),
                vec![b'h' as u32, 0x00E9, b'l' as u32, b'l' as u32, b'o' as u32]
            );
        }
    }

    #[test]
    fn string_chars_supplementary() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string("😀!".as_bytes());
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(list_to_codepoints(list), vec![0x1F600, 0x21]);
        }
    }

    #[test]
    fn string_chars_invalid_byte_replaces() {
        let _g = gc_test_lock();
        unsafe {
            // Bare 0xFF is never valid as a UTF-8 leading byte.
            let s = make_string(&[b'a', 0xFF, b'b']);
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(
                list_to_codepoints(list),
                vec![b'a' as u32, REPLACEMENT_CODEPOINT, b'b' as u32]
            );
        }
    }

    #[test]
    fn string_chars_truncated_multibyte_replaces() {
        let _g = gc_test_lock();
        unsafe {
            // 0xC3 is a 2-byte leading byte but no continuation follows.
            let s = make_string(&[b'a', 0xC3, b'b']);
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(
                list_to_codepoints(list),
                vec![b'a' as u32, REPLACEMENT_CODEPOINT, b'b' as u32]
            );
        }
    }

    #[test]
    fn string_chars_empty() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string(b"");
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            assert_eq!(list_to_codepoints(list), Vec::<u32>::new());
        }
    }

    #[test]
    fn string_char_at_codepoint_index() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string("héllo".as_bytes());
            assert_eq!(sigil_string_char_at_validate(s, 0), 0);
            assert_eq!(sigil_string_char_at_validate(s, 4), 0);
            assert_eq!(sigil_string_char_at_validate(s, 5), 1);
            assert_eq!(sigil_string_char_at_validate(s, -1), 1);
            let c1 = sigil_string_char_at(s, 1);
            assert_eq!(read_char(c1), 0x00E9);
        }
    }

    #[test]
    fn string_char_at_empty_rejects() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string(b"");
            assert_eq!(sigil_string_char_at_validate(s, 0), 1);
        }
    }

    #[test]
    fn string_from_chars_round_trip() {
        let _g = gc_test_lock();
        unsafe {
            let s = make_string("héllo 😀".as_bytes());
            let list = sigil_string_chars(
                s,
                cons_header_word(),
                TEST_CONS_DISC,
                nil_header_word(),
                TEST_NIL_DISC,
            );
            let s2 = sigil_string_from_chars(list, TEST_CONS_DISC, TEST_NIL_DISC);
            let original_bytes = {
                let len = sigil_string_len(s);
                std::slice::from_raw_parts(s.add(16) as *const u8, len).to_vec()
            };
            let round_bytes = {
                let len = sigil_string_len(s2);
                std::slice::from_raw_parts(s2.add(16) as *const u8, len).to_vec()
            };
            assert_eq!(round_bytes, original_bytes);
        }
    }

    #[test]
    fn string_from_chars_empty() {
        let _g = gc_test_lock();
        unsafe {
            let nil = alloc_nil(nil_header_word(), TEST_NIL_DISC);
            let s = sigil_string_from_chars(nil, TEST_CONS_DISC, TEST_NIL_DISC);
            assert_eq!(sigil_string_len(s), 0);
        }
    }

    #[test]
    fn counter_increments_on_alloc() {
        let _g = gc_test_lock();
        let before_count = counters::read(CounterId::CharAllocCount);
        let before_bytes = counters::read(CounterId::CharAllocBytes);
        let _ = boxc(b'A' as u32);
        let after_count = counters::read(CounterId::CharAllocCount);
        let after_bytes = counters::read(CounterId::CharAllocBytes);
        assert!(after_count > before_count);
        assert!(after_bytes >= before_bytes + 16);
    }
}
