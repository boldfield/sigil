# stdlib fallible-ops audit

**Status:** **Shipped** in PR #137. Phase 1 (inventory + design
decisions) and Phase 2 (implementation) both landed; this doc now
records the as-shipped state for future reference. Decisions D1–D5
were user-confirmed before Phase 2 began.

**Driver:** PR #136 (canonical `string_to_int`) closed one C12-shaped
teachability hazard. This document catalogues the remaining stdlib
operations that could fail at runtime without an idiomatic
Option/Result surface, records the design decisions, and tracks
what shipped where.

**Plan:** [`/repos/designs/done/2026-05-10-sigil-stdlib-fallible-ops-audit.md`](#)

---

## Decisions

### D1 — Naming convention: `_opt` suffix

Safe wrappers that return `Option[T]` get an `_opt` suffix on the
existing primitive name: `array_get` → `array_get_opt`,
`string_byte_at` → `string_byte_at_opt`, etc.

**Why:** Short. Clearly signals "Option-returning." Pairs naturally
with the unsafe builtin name (alphabetical neighbour, single common
prefix). Doesn't conflict with Sigil's existing prefix-namespaced
naming (`string_*`, `array_*`, `byte_array_*`) which precludes the
Rust `.get()` shorthand.

Wrappers that return `Result[T, E]` (only `string_to_int` so far) do
**not** get a suffix — the canonical name is unadorned, and the
underlying validate/_parse pair stays as the low-level surface. This
matches PR #136's `string_to_int` shipping pattern.

### D2 — Float parse: `Option[Float]`, not `Result[Float, ParseError]`

`string_to_float_validate` (`runtime/src/float.rs`) has a single
failure mode: returns `0` on clean parse, `1` for everything else
(empty / invalid UTF-8 / unparseable as f64). Rust's `f64::parse`
doesn't distinguish overflow because IEEE 754 represents overflow as
±Inf, not an error.

With a single failure mode there's no information to encode in an
error sum, so the canonical surface is:

```sigil
fn string_to_float(s: String) -> Option[Float] ![]
```

**This dissolves the open design question raised in PR #136 review**
(reuse `ParseError` / introduce `FloatParseError` / shared sum with
namespacing). None of the three apply: there's nothing to
discriminate. `ParseError` stays in `std.string` as integer-parse-
specific. Future `string_to_X` canonicals follow the same rule:
single failure mode → `Option[X]`; multiple distinct failure modes →
fresh `XParseError` sum. This rule is the "consistent answer" the
plan's open question asked for.

### D3 — `string_from_bytes`: `Option[String]`, not `Result[String, Utf8Error]`

`string_from_bytes_validate` returns `-1` for valid UTF-8, otherwise
the byte offset of the first invalid byte. The byte offset is
potentially meaningful — but most callers just want "did it parse?"

Rather than bloat the namespace with `type Utf8Error = |
InvalidUtf8(Int)`, return `Option[String]` for symmetry with
`string_to_float`. Callers who genuinely need the byte offset call
`string_from_bytes_validate` directly (the low-level builtin remains
available, same pattern as `string_to_int_validate`).

```sigil
fn string_from_bytes(ba: ByteArray) -> Option[String] ![]
```

**Note:** `std/byte_array.sigil`'s historical deviation note
(lines 75-87) claims that pulling `std.list + std.option +
std.result` into one module triggers `map`-collisions in the flat
namespace. **That note is outdated.** Verified by direct test:
importing all three from a user file and using `length` /
`Some` / `Ok` compiles cleanly — Sigil's typechecker resolves
`map` calls inside each std file to that file's own scheme via the
`current_fn_file` qualifier (`compiler/src/typecheck.rs`). Wrapper
modules can import what they need; the deviation note should be
updated alongside Phase 2.

### D4 — `_set` safe variants included; `_alloc` safe variants excluded

`array_set` / `mut_array_set` / `mut_byte_array_set` abort on OOB.
Their safe variants are useful and follow the same pattern as
`_get`:

```sigil
fn array_set_opt[A](arr: Array[A], i: Int, v: A) -> Option[Array[A]] ![]
fn mut_array_set_opt[A](arr: MutArray[A], i: Int, v: A) -> Option[Unit] ![Mem]
fn mut_byte_array_set_opt(ba: MutByteArray, i: Int, v: Byte) -> Option[Unit] ![Mem]
```

`Option[Unit]` for the mutable forms is awkward but principled: `None`
signals "out of bounds, no mutation applied." `Bool` would conflate
"didn't apply" with the truth-value semantics elsewhere; `Option[Unit]`
keeps the safe-accessor shape uniform across all `_opt` wrappers.

`array_alloc` / `byte_array_alloc` / `mut_array_new` /
`mut_byte_array_new` aborting on negative-or-huge length is closer to
"programmer error" (like passing `-1` to `Vec::with_capacity`) than
"recoverable runtime condition." Rust panics in this case without an
`alloc_opt` variant. **Exclude from this audit.** A user who wants
defensive allocation can bounds-check `len >= 0` before the call.

### D5 — `byte_from_int` ships as part of the audit

`byte_truncate(n) -> Byte` truncates to low 8 bits without
bounds-checking; the std/byte_array header explicitly defers
`byte_from_int(n) -> Option[Byte]` as a wrapper. Phase 2 ships it.

```sigil
fn byte_from_int(n: Int) -> Option[Byte] ![]
```

Implementation: `if byte_in_range(n) { Some(byte_truncate(n)) } else
{ None }`.

---

## Inventory

### Category A — validate/_parse C-style pairs

| # | Primitive(s) | Module | Canonical surface | Notes |
|---|---|---|---|---|
| A1 | `string_to_int_validate` / `string_to_int_parse` | `std.string` | `string_to_int(s) -> Result[Int, ParseError] ![]` | **Already shipped** in PR #136. Three failure modes (Empty / NonDecimal / Overflow) justify the sum. |
| A2 | `string_from_bytes_validate` / `string_from_bytes_alloc` | `std.byte_array` | `string_from_bytes(ba) -> Option[String] ![]` | Single failure mode (invalid UTF-8) — Option suffices. See D3. |
| A3 | `string_to_float_validate` / `string_to_float_parse` | `std.float` | `string_to_float(s) -> Option[Float] ![]` | Single failure mode — Option. See D2. |

### Category B — panic-on-OOB accessors

| # | Primitive | Module | Aborts on | Safe wrapper |
|---|---|---|---|---|
| B1 | `array_get[A](arr, i)` | `std.array` | OOB | `array_get_opt[A](arr, i) -> Option[A] ![]` |
| B2 | `array_set[A](arr, i, v)` | `std.array` | OOB | `array_set_opt[A](arr, i, v) -> Option[Array[A]] ![]` |
| B3 | `byte_array_get(ba, i)` | `std.byte_array` | OOB | `byte_array_get_opt(ba, i) -> Option[Byte] ![]` |
| B4 | `byte_array_slice(ba, s, e)` | `std.byte_array` | `s > e` or `e > length` | `byte_array_slice_opt(ba, s, e) -> Option[ByteArray] ![]` |
| B5 | `string_byte_at(s, i)` | `std.string` | OOB | `string_byte_at_opt(s, i) -> Option[Byte] ![]` |
| B6 | `string_substring(s, st, e)` | `std.string` | `st > e` or `e > length` | `string_substring_opt(s, st, e) -> Option[String] ![]` |
| B7 | `mut_array_get[A](arr, i)` | `std.mut_array` | OOB | `mut_array_get_opt[A](arr, i) -> Option[A] ![Mem]` |
| B8 | `mut_array_set[A](arr, i, v)` | `std.mut_array` | OOB | `mut_array_set_opt[A](arr, i, v) -> Option[Unit] ![Mem]` |
| B9 | `mut_byte_array_get(ba, i)` | `std.mut_byte_array` | OOB | `mut_byte_array_get_opt(ba, i) -> Option[Byte] ![Mem]` |
| B10 | `mut_byte_array_set(ba, i, v)` | `std.mut_byte_array` | OOB | `mut_byte_array_set_opt(ba, i, v) -> Option[Unit] ![Mem]` |

`mut_*` variants inherit the `![Mem]` row from their underlying
primitives (per D4).

### Category C — validate-then-truncate

| # | Primitive | Module | Canonical surface |
|---|---|---|---|
| C1 | `byte_truncate(n)` (+ `byte_in_range`) | `std.byte_array` | `byte_from_int(n) -> Option[Byte] ![]` (D5) |

### Out of scope (for this audit)

- **`array_alloc` / `byte_array_alloc` / `mut_array_new` /
  `mut_byte_array_new`** — abort on negative/huge `len`. Programmer-
  error class; matches Rust precedent (D4).
- **`int64_div` / `int64_mod`** — abort on `0` / `i64::MIN/-1`.
  Belongs to the `ArithError`-effect-row story, not the
  Option/Result wrapper story. Different idiom shift, different
  trade-offs (effect rows vs sum types). Out of scope here.
- **`/` / `%` on `Int`** — already use `ArithError` effect row (the
  intentional v1 idiom; recently strengthened in PRs #132–#134).
- **`panic` / `assert`** — intentional aborts. Excluded by design.
- **Effect-handler-driven ops** (`Fs`, `Process`, `Random`,
  `Clock`, `Env`, `IO`) — already use `Result[T, E]` for fallible
  ops via the effect handlers (`std.fs`, `std.process`).

---

## Compiler-layer prerequisites

Of the touched modules, several are currently in
`BUILTIN_INJECTED` (`compiler/src/imports.rs:61-74`) — meaning their
source is doc-only and skipped at the resolver:

```
io.sigil
array.sigil           ← affects B1, B2
mut_array.sigil       ← affects B7, B8
byte_array.sigil      ← affects A2, B3, B4, C1
mut_byte_array.sigil  ← affects B9, B10
mem.sigil
int64.sigil
string_builder.sigil
float.sigil           ← affects A3
char.sigil
panic.sigil
```

To ship pure-Sigil wrappers in those files, each needs to be moved
**off** `BUILTIN_INJECTED`, mirroring what was already done for
`string.sigil` (per the comment at imports.rs:67: "string.sigil
ships real source"). The move is mechanical but each file's removal
should be paired with verification that the file's contents
typecheck cleanly when loaded — historically these files were
documentation-only and may have stale claims that compile-error in a
real load.

**Affected files:** `array.sigil`, `byte_array.sigil`, `float.sigil`,
`mut_array.sigil`, `mut_byte_array.sigil`.

---

## Phase 2 — as shipped (PR #137)

All seven Phase 2 tasks landed in PR #137 across six commits.

| Task | Commit | Module(s) | Wrappers shipped |
|---|---|---|---|
| 1 | `92986f6` | `compiler/docs/` | This audit doc (Phase 1 inventory + decisions) |
| 2 | `18e7e3d` | `std/byte_array.sigil` | `string_from_bytes`, `byte_from_int`, `byte_array_get_opt`, `byte_array_slice_opt` |
| 3 | `4626fdd` | `std/float.sigil` | `string_to_float` |
| 4 | `835a3d2` | `std/array.sigil` | `array_get_opt`, `array_set_opt` |
| 5 | `232ebde` | `std/string.sigil` | `string_byte_at_opt`, `string_substring_opt` |
| 6 | `c9f8475` | `std/mut_array.sigil`, `std/mut_byte_array.sigil` | `mut_array_get_opt`, `mut_array_set_opt`, `mut_byte_array_get_opt`, `mut_byte_array_set_opt` |
| 7 | (composite verification — no commit) | full e2e/lib suite | 248 runtime + 802 typecheck + 528 e2e pass |
| 8 | `<this commit>` | `compiler/docs/` | This as-shipped update |

Plus one review fixup commit (`ac84fde`): tightened
`BUILTIN_INJECTED` comments + relaxed test-helper effect rows
(`![Mem]` → `![]` where the helper performs no actual `Mem` effect).

Each implementation commit followed the same shape:

1. Move the relevant file off `BUILTIN_INJECTED` (Tasks 2, 3, 4, 6).
2. Verify the file's existing contents typecheck cleanly under real
   loading.
3. Add the canonical wrapper(s).
4. Update the spec §13 row to name the new surface.
5. Add e2e tests covering Some/Ok happy path AND None/Err edge
   cases (boundary, negative-index, above-range; slice variants
   additionally test `start > end`).

### Inventory deltas vs original plan

The Phase 1 inventory walk surfaced 7 candidates the queued plan
didn't enumerate:

- `array_set_opt`, `byte_array_slice_opt` (parallel to their `_get`
  siblings — D4)
- `mut_array_set_opt`, `mut_byte_array_set_opt` (mutable
  counterparts)
- `byte_from_int` (already noted as a deferred wrapper in
  std/byte_array.sigil; D5)
- `array_alloc` / `byte_array_alloc` / `mut_array_new` /
  `mut_byte_array_new` were considered and **excluded** per D4 —
  programmer-error class, not recoverable

### Stale inventory observations resolved

1. **`map` collision concern was dead.** The historical deviation
   note in `std/byte_array.sigil:75-87` claimed cross-module name
   collisions on `map`. Verified false during inventory — the
   typechecker uses `current_fn_file` qualification so each std
   file's `map` resolves to its own scheme. The deviation note was
   removed in Task 2's commit (`18e7e3d`).

2. **`BUILTIN_INJECTED` move was uneventful.** All five moves
   (`array.sigil`, `byte_array.sigil`, `float.sigil`,
   `mut_array.sigil`, `mut_byte_array.sigil`) loaded cleanly under
   real-import semantics with zero parse / typecheck failures —
   the doc-only files had no stale comment-as-code claims that
   would have errored.

3. **Validation prompts** P41, P62 were migrated to `string_to_int`
   in PR #136. No prompt in the bank exercises the other newly-
   wrapped surfaces; future prompts touching those will use the
   canonical wrappers as a matter of course.

---

## Out-of-scope items (not migrated)

These were considered during inventory and intentionally excluded:

- `array_alloc` / `byte_array_alloc` / `mut_array_new` /
  `mut_byte_array_new` — abort on negative/huge `len`. Programmer-
  error class (matches Rust's `Vec::with_capacity` precedent which
  doesn't have a safe variant). D4.
- `int64_div` / `int64_mod` — abort on `0` / `i64::MIN/-1`. Belongs
  to the `ArithError`-effect-row story (PR #132–#134's spec
  teaching arc), not the Option/Result wrapper story.
- `Int` `/` and `%` — already use `ArithError` effect row (the
  intentional v1 idiom).
- `panic` / `assert` — intentional aborts.
- Effect-handler-driven ops (`Fs`, `Process`, `Random`, `Clock`,
  `Env`, `IO`) — already use `Result[T, E]` for fallible ops via
  the effect handlers (`std.fs`, `std.process`).
