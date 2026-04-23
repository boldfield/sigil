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

## Memory-constrained builds

Peak memory during a workspace build is driven by the compiler crate's
Cranelift dependency tree. The table below is the first set of observed
peak-RSS numbers (from `/usr/bin/time -l`) on one host; earlier guidance
that 8–12 GB was sufficient was falsified on Talos-Linux headless pods.

### Observed peaks

Measured on `aarch64-apple-darwin` (Apple M-series laptop, 2026-04-22).
Commands run via `/usr/bin/time -l` with `CARGO_BUILD_JOBS` unset
(default parallelism = physical core count).

| Command                              | peak RSS  |
|--------------------------------------|-----------|
| `cargo build -p sigil-runtime`       | ~140 MB   |
| `cargo build -p sigil-compiler`      | ~930 MB   |
| `cargo test  --workspace`            | ~2.9 GB   |

For `x86_64-unknown-linux-gnu` on a memory-constrained Talos pod
(~8–12 GiB allotment, `CARGO_BUILD_JOBS=1`, lld, `CARGO_INCREMENTAL=0`,
the profile settings below), `cargo test -p sigil-compiler --no-fail-fast`
OOM-killed the pod — once taking the underlying k8s node with it. Plan A1
verification on constrained Linux is therefore out of scope; CI runners
(GitHub-hosted) have enough headroom and the laptop is the reference
environment for completion criteria.

### Workspace profile settings (committed in `Cargo.toml`)

```toml
[profile.dev]
debug = 1              # line tables only; full DWARF blows up link memory
incremental = false    # incremental caches bloat memory and occasionally corrupt
codegen-units = 256    # many small units → smaller per-unit peak memory

[profile.release]
debug = 0
codegen-units = 16     # balances optimisation quality with link-time memory
lto = false            # LTO is a v2+ consideration
```

These apply to every builder (local dev machine, CI, headless pod) without
environment variables.

### `.cargo/config.toml` — Linux lld requirement

`.cargo/config.toml` at the workspace root pins the Linux target to the
lld linker via `clang`:

```toml
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
```

lld uses roughly half the memory of ld.bfd at link time. Install on
hosts:

- **Linux (Debian/Ubuntu):** `sudo apt-get install -y lld clang`
- **macOS:** no action — Apple's ld is used; this block is Linux-only.

If lld is genuinely unavailable on a host, comment out the Linux target
block (not delete — the next contributor will want it back).

### Recommended environment variables

For constrained hosts (headless pods, small CI runners):

- `CARGO_BUILD_JOBS=2` — caps rustc parallelism. This is the single
  biggest lever for peak memory. CI runners with more cores can raise
  or omit this.
- `CARGO_INCREMENTAL=0` — redundant with the profile setting above but
  explicit in CI.

### Build ordering on constrained hosts

Do not run `cargo build --workspace`, `cargo check --workspace`, or
`cargo clippy --all-targets` as a single invocation on memory-constrained
hosts. These co-compile multiple large crates in parallel and can spike
peak memory past 10 GB (Cranelift's dependency tree is the main driver).

Use the per-crate ordering (what CI does):

```shell
cargo build -p sigil-runtime
cargo build -p sigil-compiler
cargo test  --workspace --no-fail-fast
```

For clippy, prefer `cargo clippy --no-deps` over `cargo clippy` — the
`--no-deps` flag skips re-analysing dependency crates, cutting memory
significantly without losing coverage of our own code.

If an OOM is reported during a build, try per-crate ordering and
`CARGO_BUILD_JOBS=1` first — they materially cut peak memory. But they
do not make an arbitrarily small ceiling work: the `cargo test
-p sigil-compiler` step needs enough headroom to compile the test
binary (which links Cranelift and codegen), and on the Talos pod this
alone exceeded ~12 GiB at j=1.

## macOS prerequisites

- `brew install bdw-gc pkg-config` — the runtime crate's `build.rs`
  consults `pkg-config --libs bdw-gc` to find libgc's search path.
  Without this, `cargo build -p sigil-runtime --tests` and
  `sigil <input> -o <output>` both fail at link time with
  `ld: library 'gc' not found`.
- `export PKG_CONFIG_PATH="$(brew --prefix)/opt/bdw-gc/lib/pkgconfig:$PKG_CONFIG_PATH"`
  before the first `cargo` invocation in a shell. CI handles this
  automatically in `.github/workflows/ci.yml`.
