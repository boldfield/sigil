# Stackmap v1 — wire format, writer, runtime reader, cross-check

Plan E2 Phase 1 Tasks 4 + 5 ship a v1 stackmap section the compiler
writes to every object file and the runtime reads at startup. v0
(Plan A1 placeholder) is retired and rejected as a stale build
artifact. This doc captures the design end-to-end so a future
Phase 2 / Phase 3 contributor can extend it without re-deriving the
invariants.

## Wire format

Section name:

- ELF (Linux):   `sigil_stackmaps` (no leading `.`).
- Mach-O:        `__SIGIL,__stackmaps`.

Bytes are little-endian on the wire regardless of host endianness —
the writer commits to `to_le_bytes()` and the reader uses
`from_le_bytes()` unconditionally. Both currently-supported targets
(aarch64-darwin, x86_64-linux) are LE in practice. A future BE-host
port stays LE on the wire.

```text
section header (12 bytes):
  magic:4 ("SGST") | version:4 (=1) | fn_count:4

per-function block (variable):
  fn_header (12 bytes):
    name_len:4 | record_count:4 | text_offset:4 (reserved=0 in v1)
  name: name_len bytes (UTF-8 linker symbol, no NUL terminator)
  records[record_count]:
    record_header (12 bytes):
      pc_offset:4 (function-local) | frame_size:4 |
      entry_count:2 | flags:2
    entries[entry_count]:
      entry (5 bytes): kind:1 | sp_offset:4
```

Constants live in `sigil-abi::stackmap`:

| Constant | Value | Description |
|---|---|---|
| `STACKMAP_MAGIC` | `b"SGST"` | Section magic. |
| `STACKMAP_VERSION_V1` | `1` | Authoritative version since Task 4. |
| `STACKMAP_VERSION_PLACEHOLDER` | `0` | Retired; rejected at parse. |
| `STACKMAP_HEADER_SIZE` | `12` | Section-header byte count. |
| `STACKMAP_FN_HEADER_SIZE` | `12` | Per-fn-block-header byte count. |
| `STACKMAP_RECORD_HEADER_SIZE_V1` | `12` | Per-record-header byte count. |
| `STACKMAP_ENTRY_SIZE_V1` | `5` | Per-entry byte count. |
| `STACKMAP_ENTRY_KIND_HEAP_POINTER` | `0x01` | Only kind v1 emits. |

### Why the ELF name has no leading dot

The GNU linker auto-generates `__start_<section>` and
`__stop_<section>` symbols for sections whose name matches the regex
`[A-Za-z_][A-Za-z0-9_]*` — i.e., a valid C identifier. `.sigil_stackmaps`
(the Plan A1 / Task 4 name) fails that regex because of the leading
dot; `sigil_stackmaps` (the Task 5 name) matches and gets the
auto-symbols. The runtime reader resolves them via `dlsym` rather
than `extern "C"` static decls, so unit-test binaries that don't
link the section don't fail to link.

### text_offset

Reserved as zero in v1. The runtime resolves each function block's
base address via `dlsym(symbol_name)` at startup; `text_offset` is a
forward-compat slot for a future writer to record the function's
`.text`-relative offset (e.g., to avoid `dlsym` cost on a hot path).

## Compiler writer (`compiler/src/codegen.rs`)

`StackMapV1Builder` accumulates per-function records. After every
`module.define_function(...)`, the helper
`define_fn_and_capture_stackmap` reads the function's
`ctx.compiled_code().unwrap().buffer.user_stack_maps()` (Cranelift's
post-regalloc PC + frame-size + spill-slot data) and pushes a
function block into the builder. The section bytes are serialised at
`emit_object`'s end and added to the `ObjectProduct` as
`SectionKind::ReadOnlyData`.

Funneling discipline:

- **62 alloc sites** (Task 2a + 2c residual) route through
  `lower_alloc_call`, which calls `declare_value_needs_stack_map` on
  the alloc's result.
- **42 heap-pointer loads** (Task 2b) route through
  `lower_heap_pointer_load`, same flag.
- **14 closure_ptr fn-entry block params** (Task 2b cat 3) flagged at
  fn entry.
- **7 type-aware merge-block params** (Task 2b cat 3) flagged via
  `expr_is_known_heap`.

`function_code_offset` and `StackMapBuilder::push_placeholder` (the
v0 placeholder shim) are gone. Cranelift's safepoint pass attaches
entries automatically at every non-tail `call` / `call_indirect`.

## Runtime reader (`runtime/src/stackmap.rs`)

At startup the runtime calls `init_index()` which:

1. Locates the section bytes — Linux: `dlsym("__start_sigil_stackmaps")`
   and `dlsym("__stop_sigil_stackmaps")`. Mach-O: `getsectiondata(
   _dyld_get_image_header(0), "__SIGIL", "__stackmaps", &size)`.
2. Parses the section via `parse_section(bytes)` into a
   `ParsedSection`.
3. Resolves each function block's symbol via `dlsym(symbol_name)`
   and builds an `(absolute_pc -> &ParsedRecord)` index.

Steady-state public API:

```rust
pub fn init_index() -> Option<&'static StackmapIndex>;
pub fn walk_for_gc() -> Vec<RootLocation>;

impl StackmapIndex {
    pub fn lookup(&self, pc: usize) -> Option<&ParsedRecord>;
}
```

`walk_for_gc()` walks the calling thread's frame-pointer chain
(x86_64: rbp; aarch64: x29), looks up each frame's return-PC in the
index (with a small back-off for call-instruction size), and yields
absolute addresses `frame_sp + entry.sp_offset` for every entry in
every matched record.

Sigil's Cranelift-emitted frames use the standard `push fp; mov fp,
sp; sub sp, frame_size` prologue, so `frame_sp = fp - frame_size`
(where `frame_size` comes from the matched record). The
`SAFE_CALL_PC_BACKOFF` constant (5 bytes on x86_64; 4 bytes on
aarch64) covers the common case where Cranelift records `pc_offset
= call_pc` and the return-PC at unwind is `call_pc + call_size`.

## Cross-check harness (`runtime/src/stackmap_xcheck.rs`)

Activated by `SIGIL_GC_CROSS_CHECK=1`. The env var is read once on
first alloc and cached in a relaxed atomic; the steady-state cost on
the fast path is a single load + branch. When enabled, every
`sigil_alloc` call invokes `do_cross_check`:

1. `walk_for_gc()` produces precise root addresses for the current
   thread (set B).
2. The current thread's stack range `[sp, stack_base)` is read via
   `pthread_getattr_np` / `pthread_attr_getstack` on Linux, or
   `pthread_get_stackaddr_np` on macOS. A conservative scanner
   trivially sees every word-aligned address in this range as a
   pointer candidate (set A).
3. For each precise root address:
   - Assert it lies within `[sp, stack_base)`. (B ⊆ A.)
   - Assert the value at that address is heap-pointer-shaped per
     the conservative-scan rules: 8-byte-aligned and ≥ 0x1000
     (i.e., not in the typically-unmapped first page).
4. Divergence aborts the process with a diagnostic on stderr.

Phase 1 ship gate: zero divergence on the existing e2e suite plus
`tree.sigil` (65,535-node alloc-heavy stress test). The cross-check
tests live in `compiler/tests/e2e.rs::cross_check_*`.

## What's deferred to Phase 2 / 3

- **Type-aware cross-check**: per-entry type information beyond
  "heap pointer" is reserved by the `kind` byte but not yet used —
  Phase 2's precise marker may introduce boxed-scalar kinds (Float /
  Int64 / Char) to drive a bitmap-vs-typecheck cross-check.
- **Production use of precise roots**: Boehm conservative scanning
  is still authoritative through Phase 1 — `walk_for_gc()` is
  invoked only by the cross-check harness, not by Boehm's mark
  phase. Phase 3's "drop conservative stack scan on Sigil threads"
  task (Task 12) is where the precise walker becomes load-bearing.
- **Fast PC lookup**: today `lookup(pc)` is O(N) over all safepoints
  (linear scan keyed by absolute PC). Phase 2's hot-path marker will
  need an interval map keyed on `(fn_base, fn_base + fn_size)`. The
  v1 wire format reserves `text_offset` for the writer to surface
  function-size data without re-introducing dlsym at lookup time.
