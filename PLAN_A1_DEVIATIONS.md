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

## [Task 16] e2e test lives in `compiler/tests/e2e.rs`, not a separate `sigil-tests` crate

**Commit:** (pending — next commit)

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

## Format

Format:

```
## [Task <N>] short description

**Commit:** (pending) or <hash>

**Plan text:** (verbatim or precisely paraphrased)

**What was done instead:** ...

**Why:** ...

**Forward implications:** ...
```
