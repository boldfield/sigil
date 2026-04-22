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
