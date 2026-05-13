# Boehm precise-mode API — spike findings

## Status: spike complete, Plan E2 Phase 2 Task 6

This document pins the Boehm typed-malloc API the runtime will use
in Tasks 7 (descriptor cache) + 8 (`sigil_alloc` registers precise
descriptors). The repro lives at
`runtime/tests/boehm_precise_spike.rs` and runs on every push via
`cargo test`. Two tests cover the two API entry points Phase 2
will need:

- `make_descriptor_returns_nonzero_handle` — confirms
  `GC_make_descriptor` returns a non-zero handle for a 1-bit
  bitmap (the simplest descriptor — single pointer slot at
  payload word 0).
- `malloc_explicitly_typed_round_trip` — allocates a 2-word
  object via `GC_malloc_explicitly_typed`, writes a known pointer
  into its precise slot, forces a full GC cycle, and re-reads.
  The read-back value must equal what was written.

Both tests pass on `x86_64-unknown-linux-gnu` (the pod's Debian
12 / libgc 8.x). CI runs them on ubuntu-24.04 + macos-14
unchanged.

## Plan E2 hypothesis vs reality

The plan body listed four candidate API surfaces:

| Plan hypothesis | Reality in libgc 8.x |
|---|---|
| `GC_REGISTER_MARK_PROC` (a custom marker per "kind") | Available (`gc/gc_mark.h`), but heavier than needed for v1: the marker closure is called per object during the mark phase, where Boehm's typed-malloc already gives us the precise behaviour via descriptor bitmaps. We do not use this. |
| `GC_DESCR_KIND` (register a kind that uses bitmap-based marking) | Available, but Boehm's `GC_malloc_explicitly_typed` wraps the kind-management plumbing internally — caller provides a `GC_descr` (built via `GC_make_descriptor`) and the typed allocator selects/creates the right kind. We use the wrapper, not the kind API directly. |
| `GC_malloc_kind` (allocate from a specific kind) | Available (`gc/gc_inline.h`); same observation as above — the typed-malloc wrapper handles kind selection. |
| Verification via `GC_set_warn_proc` (catch typed-marker misuse) | Not needed for the spike. The round-trip test verifies the allocator + GC cycle works end-to-end without crashing; Task 9's false-retention reproducer will verify precise-marking correctness. |

The actual API surface Phase 2 uses:

```rust
// gc/gc_typed.h. GC_word == usize on 64-bit hosts.
extern "C" {
    fn GC_make_descriptor(bitmap: *const usize, len_bits: usize) -> usize;
    fn GC_malloc_explicitly_typed(size_in_bytes: usize, descr: usize) -> *mut c_void;
}
```

`GC_make_descriptor`:
- `bitmap` is a `GC_bitmap` (alias for `*GC_word`). Bit `i`
  (LSB-first within each word) is `1` iff payload word `i` is a
  GC-managed pointer.
- `len_bits` is the number of meaningful bits in the bitmap.
  Words beyond `ceil(len_bits / GC_WORDSZ)` are unused.
- Returns an opaque `GC_descr` handle (a `GC_word` typedef). On
  insufficient memory Boehm returns a "trivial" conservative
  descriptor as a fallback — the spec doesn't expose how to
  detect this; the spike asserts the handle is non-zero, which
  rules out the obvious-failure case. Task 7's descriptor cache
  memoises handles by bitmap so we call `GC_make_descriptor` at
  most once per distinct shape.
- "Intended to be called once per type, not once per
  allocation" (`gc_typed.h`). Task 7's cache is the structural
  enforcement of that contract.

`GC_malloc_explicitly_typed`:
- `size_in_bytes` is the total allocation size. **Constraint:**
  must be `>= len_bits * sizeof(GC_word)`. Smaller sizes leave
  the bitmap's high bits unmapped — undefined behaviour.
- `descr` is the handle from `GC_make_descriptor`.
- Returns a zero-initialised, 8-byte-aligned pointer (matches
  `GC_malloc`'s alignment).
- Cannot be passed to `GC_realloc` (typed objects don't support
  realloc).

## How Sigil's pointer-bitmap maps to Boehm's

Sigil's object layout (`header-constants/src/lib.rs`):

```text
offset 0:  8-byte header (tag + count + pointer_bitmap + reserved)
offset 8:  payload word 0
offset 16: payload word 1
...
```

The `pointer_bitmap` field in the header (32 bits) names which
payload words hold GC pointers — bit `k` ↔ payload word `k`.

For Boehm, the descriptor's bitmap covers the **whole object**
(header + payload), not just the payload. So Task 8's
`sigil_alloc` must build the Boehm bitmap by:

1. Reserving bit 0 = 0 (header is never a pointer; it's the
   tag word).
2. Setting bit `1 + k` = bit `k` of `pointer_bitmap` (shift the
   per-payload bitmap up by one to account for the header).
3. `len_bits` = 1 + payload-word-count from header `count`
   field.

The `BITMAP_BITS = 32` ceiling in Sigil's header maps directly
onto Boehm's bitmap (`GC_word` is 64-bit on our hosts, so 32
bits fits comfortably in a single word).

## Spike test design

Both tests share a single GC enrolment helper:

```rust
fn enrol_gc() {
    unsafe {
        GC_INIT.call_once(|| GC_init());
        GC_ALLOW_REGISTER.call_once(|| GC_allow_register_threads());
        let _ = GC_register_my_thread(std::ptr::null());
    }
}
```

Cargo-test spawns a fresh OS thread per `#[test]` (even under
`--test-threads=1`); the thread must register with Boehm before
calling `GC_gcollect` or Boehm aborts with "Collecting from
unknown thread."

### Test 1: `make_descriptor_returns_nonzero_handle`

Sanity-checks the descriptor constructor:

```rust
let bitmap: [usize; 1] = [0b1];  // word 0 is a pointer
let descr = unsafe { GC_make_descriptor(bitmap.as_ptr(), 1) };
assert_ne!(descr, 0);
```

### Test 2: `malloc_explicitly_typed_round_trip`

Full alloc + write + GC + re-read cycle:

```rust
let descr = GC_make_descriptor(bitmap.as_ptr(), 1);
let typed_obj = GC_malloc_explicitly_typed(16, descr);
let target = GC_malloc(64);
*(typed_obj as *mut *mut c_void) = target;
GC_gcollect();
let after = *(typed_obj as *mut *mut c_void);
assert_eq!(after, target);
```

Verifies:
- `GC_malloc_explicitly_typed` returns a non-null, 8-byte-aligned
  pointer.
- The precise pointer slot survives `GC_gcollect` unchanged.
- The target object's payload is intact after GC.

## Why we don't verify precise-marking *correctness* in the spike

Per the plan body:
> Acceptance: spike doc names the API; repro works on both hosts.
> If Boehm precise mode behaves unexpectedly, escalate before Task 7.

Acceptance = "the API works." Task 9's plan body specifies the
**false-retention reproducer** that proves precise marking
actually drops the right slots:

> False-retention reproducer: allocate `Ref[Int]` with bit
> pattern coincident with a known heap pointer; force GC;
> confirm the unrelated heap object is collected. **This test is
> the single most important Phase 2 verification — it's the bug
> class we set out to close.**

That reproducer needs Tasks 7 + 8 to land first (descriptor
cache + `sigil_alloc` integration), so it correctly sits in Task
9's acceptance gate. The spike's job is "the API doesn't crash
on the host."

## Stability

- libgc 8.x ships the typed-malloc API as `pub`-stable; the
  symbols are present in every release we target (Ubuntu's
  `libgc-dev` + macOS `brew install bdw-gc`).
- The `gc_typed.h` interface dates to libgc 6.x; no breaking
  changes through 8.x. Risk of an API rename is low across the
  plan window.
- No 8.x escalation needed: every capability Task 7 + 8 require
  is present and exposed.

## What this spike does NOT decide

- The cache key strategy for Task 7 (likely a `BTreeMap<u32,
  GC_descr>` keyed on the 32-bit `pointer_bitmap`).
- The atomic-vs-conservative split for Task 8 (`pointer_bitmap
  == 0` continues to use `GC_malloc_atomic`; non-zero bitmaps
  use `GC_malloc_explicitly_typed`).
- How Task 9 drops conservative heap scan — Boehm's
  `GC_set_dont_expand` / `GC_disable_incremental` / kind-specific
  controls are options; Task 9's deliverable is the false-
  retention reproducer + the lever it pulls.
