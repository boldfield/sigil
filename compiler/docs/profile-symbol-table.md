# `prog.symtab` — runtime profile symbol sidecar

## Purpose

A profiler that captures program counters at sampling time (Task 4 CPU
sampler, Task 6 alloc sampler) needs to map those PCs back to function
names. The sigil compiler emits a sidecar `prog.symtab` next to the
linked executable when `--emit-symbol-table` is set, so a downstream
tool (or our own pprof / folded-stacks writer) can do that lookup
without parsing the linked binary at runtime.

## API surface chosen

**Post-link parse via `object::read::File`** over the just-written
executable, recording every text-section symbol with its image-relative
address and size.

This is the third option from the plan's Task 1 candidate list (the
"fallback" framing). After implementing the first two as a check, this
turned out to be the only sound choice for the sigil toolchain. The
reasoning is below.

### What the candidates actually expose

The Cranelift object backend (`cranelift-object 0.131.0`,
`cranelift_object::backend`) finishes a module via
`ObjectModule::finish() -> ObjectProduct`. The product holds:

```rust
pub struct ObjectProduct {
    pub object: object::write::Object<'static>,
    pub functions: SecondaryMap<FuncId, Option<(SymbolId, bool)>>,
    pub data_objects: SecondaryMap<DataId, Option<(SymbolId, bool)>>,
}
```

Walking `product.object`'s symbol table via `Object::symbol(id)` returns
`object::write::Symbol { name, value, size, section, ... }` where
`value` is the section offset inside the object file's `.text`. That's
enough for option (a) or (b) from the plan — list every cranelift-emitted
function with its offset within the object's `.text`.

### Why those two options don't work standalone

A sigil binary is a multi-object link:

| Source                    | Provides                         |
|---------------------------|----------------------------------|
| `prog.o`                  | cranelift-emitted user code      |
| `libsigil_runtime.a`      | `sigil_alloc`, `sigil_perform`, `sigil_io_println_arm`, all of the runtime arm fns, ... |
| `libgc`                   | Boehm `GC_malloc`, `GC_init`, ... |
| `libc`, `libpthread`, ... | libc surface                     |

A sampled stack trace will land anywhere across all of those. Recording
**only** the cranelift-emitted symbols leaves every runtime / libc /
Boehm frame as `??` in the rendered profile. That defeats v2's premise
that the profile must be evidence-driven enough to inform optimisation
work — we explicitly want to see when a hot path is spending its time
inside `sigil_alloc`, `sigil_run_loop`, or `sigil_perform`.

A second problem: the offsets in `ObjectProduct.object` are
section-relative to the **input** object. After link, the user `.text`
chunk lives at some offset inside the linked binary's combined `.text`
section, determined by the linker's input ordering. We'd have to track
that delta at runtime, which essentially means parsing the linked
binary anyway.

### The post-link approach

After `cc` produces the executable at `out_path`, re-open it as an
`object::read::File`. The file is already on disk from
`pipeline::compile`'s linker step; the symtab emitter is a small
post-step that walks the file once.

For every symbol where `kind == SymbolKind::Text` and `size > 0`:

1. `symbol.address()` — image-relative virtual address. For a PIE
   executable (which sigil always emits — `is_pic=true` at ISA build
   time, `codegen.rs:7415`) the value is relative to image base 0.
2. `symbol.size()` — code size in bytes.
3. `symbol.name()` — the linker-visible symbol name (mangled).

Demangle by pattern:

| Pattern                          | Output                                                       |
|----------------------------------|--------------------------------------------------------------|
| `sigil_user_main`                | `main`                                                       |
| `sigil_user_<rest>`              | `<rest>` with `__` rewritten to `$` (undoes `mangle_user_fn`) |
| `sigil_handler_arm_<idx>`        | passed through verbatim                                      |
| `sigil_handler_return_arm_<idx>` | passed through verbatim                                      |
| `post_arm_k_<idx>_<N>`           | passed through verbatim                                      |
| anything else                    | passed through verbatim                                      |

Anything else covers the runtime crate symbols (`sigil_alloc`,
`sigil_io_println_arm`, ...), libc, libgc, etc. They're already
human-readable; passing them through is correct.

The plan's mangle helper lives at `codegen.rs:7231` (`mangle_user_fn`).
The demangler is the inverse of that function plus a no-op for unknown
prefixes.

### Format

One line per symbol, tab-separated, sorted by ascending
`text_offset_hex`:

```
<text_offset_hex>\t<size_hex>\t<demangled_name>
```

Example (illustrative):

```
0000000000001020	0000000000000034	sigil_gc_init
0000000000001054	0000000000000080	sigil_alloc
00000000000010d4	0000000000000040	main
...
```

`<text_offset_hex>` and `<size_hex>` are 16-character lower-case zero-
padded hex (so binary-search tools can `lexcmp`).

### Runtime lookup contract

The runtime profile module (Phase 3 / Phase 4) takes captured PCs and
turns them into demangled names. It needs the image base address at
runtime to translate `pc → image_relative_va = pc - image_base`, then
binary-searches the symtab for the function whose `[offset, offset +
size)` interval covers `image_relative_va`.

Image base discovery is OS-specific and lives in the runtime profile
module, not in this sidecar:

- **Linux:** `dl_iterate_phdr` callback, find the segment containing
  `main` (or any anchor); image base = `info.dlpi_addr`.
- **macOS:** `_dyld_get_image_vmaddr_slide(0)` for the main image.

The symtab format intentionally records image-relative addresses so the
sidecar stays portable across runs even with ASLR.

### Reproducibility

`prog.symtab` writes deterministically: symbols are sorted by
`text_offset_hex` ascending, with stable secondary sort on name for the
(rare) case of equal offsets. Same input source → same output bytes.
Matches the link step's existing reproducibility commitments (`TZ=UTC`,
`SOURCE_DATE_EPOCH=0`, `--build-id=none` on Linux, `-reproducible` on
macOS — see `link.rs`).

### Dependencies

No new crate dependencies. The `object` crate is already a transitive
dependency through `cranelift-object`; `cranelift_object::object`
reexports it (`cranelift-object/src/lib.rs:14`). The reader API
(`object::read::File`) is in the same crate version (0.39.1) the
backend uses internally.
