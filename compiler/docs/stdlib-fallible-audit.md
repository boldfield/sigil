# stdlib fallible-ops audit

**Status:** Phase 1 (inventory + design decisions). Awaiting user
confirmation before Phase 2 implementation.

**Driver:** PR #136 (canonical `string_to_int`) closed one C12-shaped
teachability hazard. This document catalogues the remaining stdlib
operations that can fail at runtime without an idiomatic
Option/Result surface, and records the design decisions for safe
wrappers.

**Plan:** [`/repos/designs/in-progress/2026-05-10-sigil-stdlib-fallible-ops-audit.md`](#)

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

## Phase 2 task plan (revised)

The original plan's tasks 2-6 map to this inventory but need
adjustment to cover the additional `_set` / `_slice` / `byte_from_int`
items and the BUILTIN_INJECTED moves.

| Task | Scope | Items |
|---|---|---|
| 2 | Move `byte_array.sigil` off BUILTIN_INJECTED + ship A2, B3, B4, C1 | `string_from_bytes`, `byte_array_get_opt`, `byte_array_slice_opt`, `byte_from_int` |
| 3 | Move `float.sigil` off BUILTIN_INJECTED + ship A3 | `string_to_float` |
| 4 | Move `array.sigil` off BUILTIN_INJECTED + ship B1, B2 | `array_get_opt`, `array_set_opt` |
| 5 | Ship B5, B6 (string.sigil already off BUILTIN_INJECTED) | `string_byte_at_opt`, `string_substring_opt` |
| 6 | Move `mut_array.sigil` + `mut_byte_array.sigil` off BUILTIN_INJECTED + ship B7, B8, B9, B10 | mutable accessor safe variants |
| 7 | Composite verification + PR | Per original plan task 7 |
| 8 | Update audit doc with as-shipped state | Per original plan task 8 |

Each Phase 2 commit must:

1. Move the relevant file off `BUILTIN_INJECTED` (where applicable).
2. Verify the file's existing contents typecheck cleanly
   (`cargo test --release -p sigil-compiler --lib` should pass with
   no new errors before adding any new wrappers).
3. Add the canonical wrapper(s).
4. Update the spec §13 row to name the new surface.
5. Add e2e tests covering Ok/Some-path AND None/error-path.

---

## Risks surfaced during inventory

1. **`BUILTIN_INJECTED` move complexity.** Each file currently in
   the skip-list may have stale comment-only "examples" that fail to
   parse / typecheck under real loading. Phase 2 commits should
   front-load the move + verify-clean step before adding wrappers,
   so failures are localized to the correct PR commit.

2. **`map` collision concern is dead.** The deviation note in
   `std/byte_array.sigil:75-87` explicitly claimed cross-module name
   collisions on `map`. **Verified false** — the typechecker uses
   `current_fn_file` qualification so each std file's `map` resolves
   to its own scheme. Fix the deviation note in Phase 2's
   `byte_array.sigil` commit.

3. **`Option[Unit]` for mutable `_set_opt` is awkward.** Considered
   `Bool` instead but rejected — keeps the wrapper-shape uniform and
   matches what an LLM trained on Rust's `try_*` mutating APIs would
   reach for.

4. **Validation prompts.** Existing prompts P41, P62 (already
   migrated to `string_to_int`) plus any prompt referencing the
   newly-wrapped surfaces should be migrated. Phase 2 commits each
   handle their own prompt migrations.

---

## Open questions

None blocking Phase 2. All four design decisions (D1-D5) have a
recommended resolution above. The user's confirmation is requested
on:

- D1 (naming `_opt`)
- D2 (float = Option, not Result)
- D3 (string_from_bytes = Option, not Result)
- D4 (`_set` variants in scope; `_alloc` out of scope)
- D5 (`byte_from_int` in scope)

If any of D1-D5 is overridden, the affected Phase 2 task changes
shape but the inventory stays.
