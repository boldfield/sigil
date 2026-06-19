# Growable continuation stack (remove the 256-frame cap) — design

Status: draft (feature_spec for the agentask sigil board)
Date: 2026-06-18

## Problem

The runtime caps the outer-post-arm-k continuation stack at a fixed 256
entries (`runtime/src/handlers.rs:359`,
`const OUTER_POST_ARM_K_STACK_SIZE: usize = 256`, backing a statically-
sized thread-local array `[OuterPostArmKEntry; 256]`). When a CPS
computation pushes more than 256 entries, `sigil_outer_post_arm_k_push`
(`handlers.rs:691`) aborts with `stack overflow (depth N >= cap N)`.

This is not a runtime-tunable value — there is no env var or flag; it is
a compile-time constant behind a fixed array. It was **already bumped
once (32 → 256)** because the prior cap overflowed on real programs, and
the in-code comment (`handlers.rs:343-358`) names the intended fix:

> "pushes accumulate linearly with recursion depth … v2 may revisit with
> a growable VecDeque or a chunked overflow region."

## Why it bites — it's `std.json`, not one program

`std.json`'s parser is recursive-descent with `![State[Int],
Raise[String]]`: each recursive frame performs `State.get`/`State.set`
for the cursor, and each chain step pushes one outer-post-arm-k entry.
Pushes therefore grow **linearly with JSON nesting/length** — roughly 3
entries per array element — so parsing a flat JSON array overflows at
~84 elements.

**Consequently, any Sigil program that parses a JSON array larger than
~84 elements via `std.json` aborts.** This was surfaced by `sjq` (the
first real program on the programs board): correct on small inputs,
`SIGABRT` on anything realistic; `jq` handles 1,000,000 elements in
~86 ms.

## Goal

Remove the fixed cap: the outer-post-arm-k continuation stack grows as
needed, bounded only by available memory, so deep CPS recursion (e.g.
parsing large JSON) no longer aborts.

## Behavior

- `sigil_outer_post_arm_k_push` never aborts on depth alone; the backing
  storage grows (amortized) to hold the entries.
- All existing push/pop/wrap semantics and depth accounting are
  preserved — `wrap_continuation_with_outer_post_arm_k`, the per-run-loop
  `RUN_LOOP_ENTRY_DEPTH` save/restore, and pop ordering are unchanged in
  observable behavior.

## Constraints and gotchas (the crux is GC rooting)

- **Precise-GC rooting is the hard part.** Each entry holds
  `closure_ptr` / `fn_ptr`, which are **GC pointers**. The fixed array is
  registered as a GC root (see `OUTER_POST_ARM_K_STACK_ROOTED`,
  `handlers.rs:376`). A growable backing buffer (`VecDeque`/`Vec`/chunked
  region) **relocates on growth**, so the root registration must track
  the current buffer — the precise GC (shipped in E2) must continue to
  see every live continuation pointer across a grow. Getting this wrong
  is a use-after-free under collection, not a clean failure. This is the
  central correctness requirement.
- **Hot path.** Push/pop is on the per-perform fast path; the shallow
  common case (well under 256) must not regress — amortized growth, no
  per-push allocation in steady state, no extra indirection in the
  common case beyond what a growable structure inherently needs.
- Preserve the depth-tracking invariants exactly: `OUTER_POST_ARM_K_DEPTH`
  and the `RUN_LOOP_ENTRY_DEPTH` snapshot logic (`handlers.rs:368-394`)
  must remain correct; entries below a run_loop's entry depth belong to
  enclosing run_loops and must never be moved.
- **Rebuild the release runtime lib** after editing `runtime/src/*.rs`
  (`cargo build --release -p sigil-runtime`) or local e2e tests link the
  stale archive (per repo CLAUDE.md).

## Acceptance criteria

1. A Sigil program that parses a JSON array of **≥ 10,000 elements** via
   `std.json.json_parse` compiles, runs, and returns the correct result
   with no abort. (This is the regression test `sjq`'s oracle lacked.)
2. The full existing suite passes — especially the CPS / continuation
   charter tests (`scripts/plan-b-invariants.sh`) and `e2e`.
3. `SIGIL_GC_CROSS_CHECK=1` passes on a deep-recursion workload: the
   precise walker sees the continuation pointers across buffer growth
   (no false-retention divergence, no missed roots).
4. No measurable throughput regression on a shallow-recursion benchmark
   (the common case stays on the fast path).

## Relation to v2

Continuation-depth behavior is adjacent to the **per-context CPS** work
(the open v2 frontier). This task is the contained, high-value subset:
it removes a hard *correctness* ceiling that makes `std.json` unusable on
real input, independent of the larger per-context-CPS codegen.

## Decomposition (board tasks)

The risk concentrates in one place — GC rooting across reallocation — so
the decomposition *isolates* it: a behavior-preserving chain on
`handlers.rs` introduces a seam, swaps the backing to a pre-reserved
`Vec` (still capped), and adds a separately-unit-tested re-root helper,
**before** any growth is enabled. Each step compiles green and changes
no observable behavior until the final swap, which keeps every task
Haiku-sized (no model escalation).

**`handlers.rs` chain (serial; behavior-preserving until the last):**

1. **Extract the stack behind an API** (`push`/`pop`/`depth`/
   `root_extent()`) over the existing fixed `[Entry; 256]` array — pure
   refactor.
2. **Route GC rooting through `root_extent()`** — no-op (same bounds).
3. **Swap backing → pre-reserved `Vec<Entry>` (cap 256)** — behavior
   identical; the cap/abort stays, so it never reallocates yet.
4. **Add the re-root-on-realloc helper**, unit-tested directly with two
   extents — the GC crux, isolated and proven before it's on the hot
   path.
5. **Enable growth + delete the abort** — `push` grows the `Vec` and
   re-roots via (4) on relocation. The actual uncapping.

**Validation (depend on #5):**

6. **e2e regression** — parse a ≥10,000-element JSON array via
   `std.json`, assert correct, no abort (the test sjq's oracle lacked).
7. **GC cross-check** — `SIGIL_GC_CROSS_CHECK=1` on the deep-recursion
   workload, wired into the gated invariants.
8. **Perf guard** — shallow-recursion benchmark, no regression on the
   common sub-256 path.

Model `haiku`, opus+sonnet review on each. The GC-rooting requirement
(acceptance criterion 3) lives almost entirely in task 4 (helper + unit
test) and task 5 (wire-in); reviewers should scrutinize those two
hardest.
