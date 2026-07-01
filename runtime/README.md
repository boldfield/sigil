# Sigil Runtime

Static library linked into every compiled Sigil program. Written in Rust,
built with `cargo build -p sigil-runtime --release`. v1 bundles a
conservative Boehm GC and a minimal effect-free runtime shim; v2 upgrades
to precise GC via Cranelift stack maps.

## Boehm GC dependency

v1 links against the Boehm GC library (`libgc`) via direct C ABI
(`GC_init`, `GC_malloc`, `GC_size`). No Rust wrapper crate: one less
dependency to pin, and Boehm's ABI is stable enough that direct FFI is the
simplest option.

**Build-time requirements** (required only when building the Sigil compiler and runtime from source):

- **Linux (Debian/Ubuntu):** `sudo apt-get install -y libgc-dev pkg-config`
- **macOS (Homebrew):** `brew install bdw-gc pkg-config` and add
  `$(brew --prefix)/opt/bdw-gc/lib/pkgconfig` to `PKG_CONFIG_PATH`.

These are development libraries needed to compile the runtime crate. Users of a prebuilt Sigil
release and the Sigil programs they compile do not require any host `libgc` installation.

**Linking strategy:** The compiler's linker driver prefers static linking when `libgc.a` is 
available (e.g., in a prebuilt release or when built locally). Only when the static archive 
is not found does it fall back to dynamic linking via `-lgc`, which requires the system 
libgc library to be installed.

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

### Plan A1 limitation (v0 — placeholder format)

**What Plan A1 actually writes is placeholder metadata.** `pc_offset`
is the Cranelift `Inst` handle of the call site, **not** a post-regalloc
code offset — real PC offsets require Cranelift's safepoint API, which
Plan B turns on. Live-value lists are likewise absent: every record has
`live_count = 0`. This is sufficient to (a) demonstrate that the section
is populated at every call site the compiler can see, (b) let a v2
reader detect the placeholders and resynthesise safepoint metadata from
relocations rather than trusting the offsets, and (c) give us a stable
extension point via `StackMapBuilder` in `compiler/src/codegen.rs`.

### Binary format (v0, Plan A1)

Per-host little-endian:

```
header  = magic:4 "SGST" | version:4 | record_count:4            // 12 bytes
record  = pc_offset:4   | live_count:2 | flags:2                 //  8 bytes
```

Constants (live in `runtime/src/stackmap.rs` and mirrored in
`compiler/src/codegen.rs`):

| constant                        | value    |
|---------------------------------|----------|
| `STACKMAP_MAGIC`                | `"SGST"` |
| `STACKMAP_VERSION_PLACEHOLDER`  | `0`      |
| `STACKMAP_HEADER_SIZE`          | `12`     |
| `STACKMAP_RECORD_SIZE`          | `8`      |
| `STACKMAP_FLAG_PLACEHOLDER`     | `0x0001` |

**v0 invariants** (asserted in `runtime/src/stackmap.rs::parse_section`
and `compiler/src/codegen::tests`):

- `live_count == 0` for every record.
- `flags & STACKMAP_FLAG_PLACEHOLDER == STACKMAP_FLAG_PLACEHOLDER` for
  every record.
- `pc_offset` is opaque (Cranelift `Inst` handle); a reader MUST NOT
  treat it as a real code offset.

### Plan B (v1) upgrade path

Plan B will emit version 1 records populated from Cranelift's
safepoint API — real post-regalloc `pc_offset` values plus a
per-record live-value list (Cranelift type tag + stack offset + GC
pointer bit). The header magic and layout of the header itself stay
fixed so both versions share one detection codepath. The `flags` field
in v1 records carries a cleared `STACKMAP_FLAG_PLACEHOLDER` bit plus
newly-minted bits for per-record metadata (exact set TBD in Plan B's
design).

v1 Boehm GC never reads this section regardless of version. The
live-value list is consumed by Plan B's precise-GC mark phase only.

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

## Cold-checkout build ordering

Plan A2 task 1.5.5 — the runtime ships a **staticlib** (`libsigil_runtime.a`)
which the compiler's linker driver links into every compiled Sigil program.
Cargo does not automatically produce the staticlib artifact when
`sigil-runtime` is pulled in as a plain dev-dep of `sigil-compiler`, because
the dev-dep only consumes the rlib. On a cold `cargo test --workspace`
this could leave the e2e test binary trying to link without the staticlib
present.

**Fix:** the e2e test (`compiler/tests/e2e.rs`) checks for
`target/<profile>/libsigil_runtime.a` at its entry point and, if missing,
invokes `cargo build -p sigil-runtime` before the test proceeds. This runs
at test-*run* time, after the outer cargo has completed its build phase
and released its per-build-unit locks — so the nested cargo invocation
acquires its own locks cleanly without deadlock. An earlier version of
this fix put the rebuild in `compiler/build.rs`; that deadlocked under
`cargo test --workspace` because the outer cargo held build-unit locks
during build-script execution. See `PLAN_A2_DEVIATIONS.md` [Task 1.5.5]
for the full history.

**CI acceptance check:** the `.github/workflows/ci.yml` file defines a
`cold-checkout-test` job separate from the main `build-test` matrix. It
runs `rm -rf target && cargo test --workspace` twice in succession (the
plan's acceptance criterion) on both supported hosts. The main
`build-test` job uses `actions/cache@v4` on `target/` so it cannot itself
prove the cold-checkout invariant; keeping the cold verification in its
own job lets the warm-path cache still work.

## macOS prerequisites

- `brew install bdw-gc pkg-config` — the runtime crate's `build.rs`
  consults `pkg-config --libs bdw-gc` to find libgc's search path.
  Without this, `cargo build -p sigil-runtime --tests` and
  `sigil <input> -o <output>` both fail at link time with
  `ld: library 'gc' not found`.
- `export PKG_CONFIG_PATH="$(brew --prefix)/opt/bdw-gc/lib/pkgconfig:$PKG_CONFIG_PATH"`
  before the first `cargo` invocation in a shell. CI handles this
  automatically in `.github/workflows/ci.yml`.
