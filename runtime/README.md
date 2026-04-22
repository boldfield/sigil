# Sigil Runtime

Static library linked into every compiled Sigil program. Written in Rust,
built with `cargo build -p sigil-runtime --release`. v1 bundles a
conservative Boehm GC and a minimal effect-free runtime shim; v2 upgrades
to precise GC via Cranelift stack maps.

## Boehm GC dependency

v1 links against the system Boehm GC library (`libgc`) via direct C ABI
(`GC_init`, `GC_malloc`, `GC_size`). No Rust wrapper crate: one less
dependency to pin, and Boehm's ABI is stable enough that direct FFI is the
simplest option.

Install requirements:

- **Linux (Debian/Ubuntu):** `sudo apt-get install -y libgc-dev pkg-config`
- **macOS (Homebrew):** `brew install bdw-gc pkg-config` and add
  `$(brew --prefix)/opt/bdw-gc/lib/pkgconfig` to `PKG_CONFIG_PATH`.

The compiler's linker driver (task 13) invokes `cc` with `-lgc` on both
hosts.

## Standard library embedding

The `std/` tree at the repo root is embedded into the compiler binary via
`include_dir!`. The compiler resolves `import std.<name>` exclusively
against the embedded tree; nothing is read from disk at user-compile time.
This means a single `sigil` binary distribution carries its own stdlib.

## Object header layout (64 bits)

Every heap object allocated via `sigil_alloc` begins with an 8-byte header
before the payload:

```
bit 63                                                            bit 0
 [ reserved (18) | pointer bitmap (32) | payload word count (6) | tag (8) ]
```

| Range  | Width | Field                | Notes                                  |
|--------|-------|----------------------|----------------------------------------|
| 0–7    | 8     | type tag             | 0x00..0xFE = per-type descriptor; 0xFF = external descriptor (v2). |
| 8–13   | 6     | payload word count   | 0..63; larger objects use the external-descriptor tag. |
| 14–45  | 32    | GC pointer bitmap    | Bit `k` set ⇒ payload word `k` is a GC-managed pointer. |
| 46–63  | 18    | reserved             | forwarding pointer / generation / mark bits in v2; zero in v1. |

The layout is committed for v1/v2 forward compatibility. v1's Boehm
conservative GC ignores the bitmap; v2's precise GC walks only the set
bits.

The single source of truth for header construction is `runtime/src/header.rs`
(`Header::new(tag, count, bitmap)`). Every allocation site — in both runtime
and generated code — constructs headers through this helper. Constructing a
`u64` header by hand at an allocation site is a bug.

## No interior pointers

Runtime code and generated code never materialise a pointer to the interior
of a heap object. All pointers either land on an object header or are
non-heap values (raw integer bytes or static data). Array element access
returns a value; string slicing returns a new `StringSlice` record, not a
byte pointer.

CI enforces the rule via `scripts/check-no-interior-pointers.sh` with a
`// SAFETY: not an interior pointer (<reason>)` escape-hatch comment for
legitimate raw-buffer arithmetic. v2 precise GC depends on the invariant;
it is not optional.

## Stackmap section

v1 codegen emits one safepoint record per Cranelift `call` /
`call_indirect`. Records are written to:

- **ELF (Linux):** section `.sigil_stackmaps`
- **Mach-O (macOS):** segment `__SIGIL`, section `__stackmaps`

The binary format is per-host little-endian:

```
struct Record {
    u32 pc_offset;        // relative to the function's first byte
    u16 live_count;       // number of live values at this safepoint
    u16 _pad;             // reserved, zero
    Entry entries[live_count];
}

struct Entry {
    u32 cl_type;          // Cranelift type encoding (see entries.rs in v2)
    i32 stack_offset;     // signed offset from the frame pointer
    u8  gc_pointer;       // 1 = GC-managed pointer, 0 = scalar
    u8  _pad[3];          // zero
}
```

v1 Boehm never reads the section. v2 precise GC reads it directly; no
codegen rewrite is required at that time. The constants above live in
`runtime/src/stackmap.rs`.

## Runtime instrumentation counters

Ten atomic `u64` counters, relaxed-ordering increments. Read via the FFI
symbol `sigil_counter_read(u32 id) -> u64`. Stable IDs:

| ID | Name                                      | Populated by |
|----|-------------------------------------------|--------------|
| 0  | SIGIL_COUNTER_BOEHM_ALLOC_COUNT           | `sigil_alloc` |
| 1  | SIGIL_COUNTER_BOEHM_ALLOC_BYTES           | `sigil_alloc` |
| 2  | SIGIL_COUNTER_ARENA_ALLOC_COUNT           | Plan B       |
| 3  | SIGIL_COUNTER_ARENA_ALLOC_BYTES           | Plan B       |
| 4  | SIGIL_COUNTER_ARENA_ESCAPE_COUNT          | Plan B       |
| 5  | SIGIL_COUNTER_HANDLER_WALK_COUNT          | Plan B       |
| 6  | SIGIL_COUNTER_HANDLER_WALK_DEPTH_SUM      | Plan B       |
| 7  | SIGIL_COUNTER_TRAMPOLINE_DISPATCH_COUNT   | Plan B       |
| 8  | SIGIL_COUNTER_CPS_CALL_COUNT              | Plan B       |
| 9  | SIGIL_COUNTER_NATIVE_CALL_COUNT           | Plan B       |

`sigil --print-runtime-stats <program>` runs the compiled program and
prints the counters to stderr at exit, one `NAME=value` per line.

v1 itself does not make decisions from these counters; they exist so v2
optimisation work (precise GC tuning, arena sizing, handler-dispatch
specialisation) is data-driven.
