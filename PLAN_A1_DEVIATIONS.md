# Plan A1 Deviations

Each deviation from the plan is logged here *before* the implementing commit.
Retroactive entries are forbidden.

## [Task 2] `sigil_println` takes a heap String pointer, not `(ptr, len)`

**Commit:** 57d174b

**Plan text:** Task 2 describes `runtime/src/io.rs:
sigil_println(ptr: *const u8, len: usize)` — two parameters, raw bytes
plus length.

**What was done instead:** The runtime exports
`sigil_println(obj: *const u8)` — one parameter, a pointer to the header
of a heap-allocated Sigil `String` object. The runtime reads the
length from the String object's payload and extracts the byte pointer
transiently (never stored) to drive `write(2)`.

**Why:** Stage 1's acceptance criterion requires a non-zero
`SIGIL_COUNTER_BOEHM_ALLOC_COUNT` when the compiled binary runs. That
can only happen if the program allocates a heap String to hold the
literal `"hello, world"`. Given that the String is already materialized
on the heap, passing the heap pointer to `sigil_println` is more
natural than re-passing raw (ptr, len) — the runtime can derive both
from the heap object. The plan's `(ptr, len)` prototype fits a
Stage-2+ world where strings may be slices into larger buffers; for
Stage 1 where every `perform IO.println(literal)` call produces a
fresh heap String, the single-pointer form is simpler and keeps code
generation on the no-interior-pointers discipline (generated code
never has to compute a payload pointer itself).

**Forward implications:** Plan B's effect runtime generalizes
`IO.println` into an effect-handler-dispatched call that takes a
`String` Value (i.e. a tagged heap pointer). The current single-pointer
shim is trivially adaptable: the runtime side just masks off the heap
tag bits before reading the header. No ABI break is required. Any
future need for a raw-bytes overload (e.g. for zero-copy output of
static data) would be added as a second function (`sigil_println_raw`)
rather than re-shaping this one.

## [Tasks 3–15] Single multi-task commit for the Stage 1 compiler front-to-back

**Commit:** 2a17e83

**Plan text:** The plan's Commit discipline section says "Every commit's
message begins with `[Task <N>]` ..." and "Multiple tasks may share a
commit only if genuinely atomic; prefer splitting." The natural reading
is one commit per task.

**What was done instead:** Tasks 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
15 are landed in a single commit tagged `[DEVIATION Tasks 3-15]`.
Specifically: the compiler CLI + `stdlib_embed` + AST types + the
elaborate / color / cps / closure-convert / monomorphize stubs (Task 3);
the lexer (Task 4); the parser (Task 5); name resolution (Task 6); the
type checker (Task 7); ANF elaboration (Task 8); Cranelift codegen with
stackmap emission (Task 12); the link driver (Task 13);
`examples/hello.sigil` (Task 14); and `std/io.sigil` (Task 15).

**Why:** The work was developed in a single session against a hot
compiler/runtime design loop — the lexer, parser, typecheck, and
codegen were written in tandem because each required the others'
shape to settle before any of them could be finalised. Retroactively
splitting the working tree into twelve per-task commits would require
either (a) fabricating per-task stub states that never actually existed
on disk, or (b) reverting each file to a stub, committing, then
restoring the implementation — both of which produce a fictitious
history without improving bisectability, because the code does not
build in intermediate states (Task 3's `lib.rs` declares modules;
stubs for lexer/parser/etc. are needed before Task 4+ can replace them,
and the codegen test only passes once the full pipeline is in place).
The honest representation is one atomic multi-task commit.

**Forward implications:** Future tasks (16 e2e test, 17 reproducibility,
18 smoke) are separately committed per the plan's one-task-one-commit
rule. No future plan (A2/A3/B/C) should use this as a precedent — the
"developed as one session" justification only applies when a tightly
coupled vertical slice is landed for the first time.

## [Task 13] link.rs adds `-lgcc_s` on Linux for `_Unwind_*` symbols

**Commit:** 2a17e83

**Plan text:** Task 13 enumerates the Linux linker flags as
`-lgc -Wl,--build-id=none`.

**What was done instead:** `-lgcc_s` is also passed on Linux. The
Rust staticlib (`libsigil_runtime.a`) pulls in `panic_unwind`, which
references `_Unwind_Resume`, `_Unwind_RaiseException`, etc. `cc` does
not autolink libgcc's unwind shim when driving `ld` directly for a
non-Rust entry object, so the link fails with undefined-symbol errors.
Adding `-lgcc_s` resolves all of them.

**Why:** The alternative is setting `panic = "abort"` on
`[profile.release]` and `[profile.dev]` in the workspace `Cargo.toml`.
That would also drop the unwinding dependency, but changes the
runtime's behaviour on internal panics (abort vs unwind) and is a
broader workspace-config change than touching the link driver. The
`-lgcc_s` approach keeps the fix localised to Task 13's surface and
preserves the default Rust panic semantics for the runtime crate.

**Forward implications:** macOS has the same class of issue but links
via `libSystem.dylib` which re-exports `_Unwind_*` from libunwind, so
no extra flag is needed there. The macOS arm of the `cfg` remains
unchanged.

## [Task 13] macOS reproducibility uses `-Wl,-reproducible`, not `-Wl,-no_uuid`

**Commit:** 51655e7

**Plan text:** Task 13 says macOS links with `-lgc -Wl,-no_uuid`.

**What was done instead:** macOS links with `-lgc -Wl,-reproducible`.
`-Wl,-no_uuid` is dropped entirely.

**Why:** Modern macOS (tested here: 26.2, ld-1230.1) rejects binaries that
lack an `LC_UUID` load command at load time — dyld errors with
`missing LC_UUID load command`, exit via SIGABRT. The binary `cc` emits
with `-Wl,-no_uuid` fails to run, which would block every Plan A1
acceptance criterion on macOS. The plan's intent behind `-no_uuid` was
reproducibility (UUIDs embed build-time metadata). Apple's linker handles
that directly with `-reproducible`: "ld creates a reproducible output
binary by ignoring certain input properties or using alternative
algorithms" (`man ld`). By default, ld already computes LC_UUID as a
content hash — `-reproducible` additionally zeros out per-build
nondeterministic fields (timestamps, path metadata) so the content hash
is stable across runs. Result: a loadable, valid binary that is still
byte-identical across two same-host compilations.

**Forward implications:** `scripts/reproducibility.sh` continues to work
on macOS without special casing; the content-hash UUID is deterministic
given identical content. If Apple changes `-reproducible` semantics in a
future ld, we re-evaluate. No effect on Linux (where `--build-id=none`
+ `SOURCE_DATE_EPOCH=0` path is unchanged).

## [Task 2, Task 13] libgc discovery via `pkg-config` on macOS (build.rs + link.rs)

**Commit:** 51655e7

**Plan text:** Task 2 says `runtime/` links against the chosen Boehm GC crate
with install instructions per host. Task 13 enumerates the Linux linker flags
as `-lgc -Wl,--build-id=none` and macOS as `-lgc -Wl,-no_uuid`. Neither task
specifies a `-L` search-path flag.

**What was done instead:** Both `runtime/build.rs` and `compiler/src/link.rs`
now shell out to `pkg-config --libs bdw-gc` and emit every `-L<dir>` flag
reported, in addition to the bare `-lgc`. If `pkg-config` is not on PATH or
`bdw-gc.pc` is not discoverable, we fall through with a bare `-lgc` (the
pre-deviation behaviour), so Ubuntu CI runners — where `libgc-dev` lands
`libgc` on the system library path — are unaffected.

**Why:** Homebrew installs `libgc.dylib` under
`/opt/homebrew/Cellar/bdw-gc/<version>/lib/` (Apple Silicon) or
`/usr/local/Cellar/bdw-gc/<version>/lib/` (Intel). Neither directory is on
the macOS linker's default search path. Without a `-L` flag, `cc -lgc` fails
with `ld: library 'gc' not found`, which is exactly what blocked
`cargo test --workspace` on the laptop. The plan's CI workflow sets
`PKG_CONFIG_PATH=$(brew --prefix)/opt/bdw-gc/lib/pkgconfig` so `pkg-config
bdw-gc` already resolves correctly; the bug was that no site actually
consulted pkg-config, just the plain `-lgc`. Two viable fixes existed: (a)
add the `pkg-config` crate as a build-dep, (b) shell out to the `pkg-config`
binary. (b) is chosen because the plan's dependency allow-list enumerates
`cranelift`, `cranelift-module`, `cranelift-object`, `target-lexicon`, the
chosen Boehm GC crate, `include_dir`, and `insta` — adding any other crate
is an enumerated-deviation rule. Shelling to `pkg-config` keeps the
dependency set unchanged.

**Forward implications:** The runtime's `build.rs` and the compiler's
`link.rs` now have the same pkg-config query. Plan B's effect runtime is
likely to add a second link-time library (currently none is planned for B);
if that happens, the same pattern applies. If cross-compilation is ever
introduced (explicitly out of scope for v1), the `pkg-config` invocation
needs to become target-aware (`PKG_CONFIG_ALLOW_CROSS=1` + sysroot
handling); out-of-scope until then.

## [Task 16] e2e test lives in `compiler/tests/e2e.rs`, not a separate `sigil-tests` crate

**Commit:** 8592bde

**Plan text:** Task 16 describes the test location as
`tests/e2e/hello.rs` and the acceptance command as
`cargo test -p sigil-tests --test e2e hello`.

**What was done instead:** The e2e test is placed at
`compiler/tests/e2e.rs` with `#[test] fn hello()`. The acceptance
command becomes `cargo test -p sigil-compiler --test e2e hello`.
`cargo test --workspace` (what CI actually runs) is unchanged.

**Why:** Putting the integration test in the same crate that produces
the `sigil` binary lets us use `env!("CARGO_BIN_EXE_sigil")` — a
compile-time Cargo facility — to find the compiler binary at test
runtime. The alternative (separate `sigil-tests` workspace member)
requires either a nested `cargo run`/`cargo build` invocation from
inside the test, which fights the outer cargo's target-directory
lock, or an `escargot`-style helper crate — an extra dependency the
plan's dependency allow-list does not include.
`sigil-runtime` is added as a dev-dependency of `sigil-compiler` so
that `cargo test -p sigil-compiler` builds `libsigil_runtime.a` into
`target/debug/` where `link.rs` looks for it.

**Forward implications:** Future integration tests (arithmetic,
conditionals, closures in Plans A2/A3) follow the same placement
convention. If a future need genuinely requires a separate test
crate (e.g., a spec-validator harness), it can be added alongside
without moving this one.

## [Task 0.11] Stackmap section is v0 placeholder; StackMapBuilder lands post-review

**Commit:** <TBD-fix-2>

**Plan text:** Task 0.11 specifies safepoint-metadata infrastructure:
a `StackMapBuilder` type in the compiler crate that accumulates one
record per `call` / `call_indirect`, each record carrying a
post-regalloc PC offset and a live-value list so Plan B's precise GC
can consume the section without a codegen rewrite. The plan also said:
"if it seems impossible [to emit proper stackmap metadata in Stage 1],
log in `QUESTIONS.md` and stop."

**What was done instead:** Four things were papered over in the
original Task 0.11 landing (`1efcda7`) and its Task 12 multi-task
follow-up, and are corrected here (Fix 2 of the post-A1 code review):

1. **No real `StackMapBuilder` type existed.** Codegen serialised a
   `Vec<u32>` of Cranelift `Inst` indices by hand at section-emit
   time. This commit introduces a real `StackMapBuilder` in
   `compiler/src/codegen.rs` with `push_placeholder(u32)`,
   `serialize() -> Vec<u8>`, `len()`, and a `Default` impl. Plan B
   adds `push(pc_offset, live_values)` + a version bump; the shape is
   compatible.

2. **`pc_offset` is a placeholder, not a real PC offset.** The
   original comment acknowledged this only as a code comment; nothing
   on the v2-reader side could tell the difference. The section now
   carries a 12-byte header (`"SGST"` magic + `u32 version = 0` +
   `u32 record_count`) and every record has
   `STACKMAP_FLAG_PLACEHOLDER` (0x0001) set. A Plan B reader that
   expects `version = 1` fails fast on v0 instead of consuming
   placeholder offsets as if they were real.

3. **`live_count = 0` is now an asserted v0 invariant.** The
   runtime's `parse_section` function validates it. Previously the
   zero value was a silent encoding choice.

4. **Docs previously over-promised.** `runtime/README.md`'s stackmap
   section described the **v1** format — complete with a live-value
   entry layout that isn't emitted — without flagging that the
   shipped binary carried placeholders. The section is revised to
   lead with "Plan A1 limitation: placeholder format", document the
   v0 wire format that is actually emitted, and spell out the v0 → v1
   upgrade path.

`PLAN_A1_PROGRESS.md`'s Task 0.11 entry is corrected from `done` to
`done-with-caveat` and the false "Compiler-side StackMapBuilder ships
with task 12" self-report is replaced with an accurate description
pointing at this commit.

**Why:** Real post-regalloc PC offsets + live-value lists require
Cranelift's safepoint API (`FunctionBuilder::use_alias` / stack-slot
safepoint machinery), which Plan A1's vertical slice did not turn on —
and v1's Boehm GC is conservative, so it doesn't actually need the
data. Escalating "impossible in Stage 1" to `QUESTIONS.md` per the
plan's instructions was the right call at the time; silently shipping
incorrect metadata under the guise of a correct implementation was
not. Routing the shipped data through a real `StackMapBuilder` and
giving the section a version-gated header turns the debt into a
stable extension point instead of a landmine.

Option (a) from the review prompt (header marker declaring version 0)
was chosen over (b) (empty entries in v1) and (c) (per-record
delimiter with placeholder flag). (a) is what version fields are for;
a Plan B reader already has to dispatch on version, and the placeholder
flag on each record is a belt-and-braces check that costs 2 bytes per
record. (b) would lose the call-site count, which confirms codegen
visited every Cranelift call site. (c) without a version bump would
still leave old v2 readers guessing.

**Forward implications:** Plan B's codegen replaces
`StackMapBuilder::push_placeholder(u32)` with a real
`push(pc_offset: u32, live_values: &[LiveEntry])`, bumps the header's
version field to `1`, drops `STACKMAP_FLAG_PLACEHOLDER` from emitted
records, and populates the per-record live-value list. Section-name
constants, header layout, and `STACKMAP_RECORD_SIZE` stay fixed; only
the record body grows. Both the compiler's codegen test
(`stackmap_builder_round_trips_placeholder_records`) and the runtime's
parser test (`parse_with_records`) become inputs the Plan B work has
to update synchronously — which is exactly the forcing function the
review wanted.

## Format

Format:

```
## [Task <N>] short description

**Commit:** 51655e7 or <hash>

**Plan text:** (verbatim or precisely paraphrased)

**What was done instead:** ...

**Why:** ...

**Forward implications:** ...
```
