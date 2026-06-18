# JSON float support (`JFloat`) — design

Status: draft (feature_spec for the agentask sigil board)
Date: 2026-06-18

## Problem / motivation

`std/json.sigil`'s `JValue` models JSON numbers as `JInt(Int)` only —
there is no float variant (`std/json.sigil:83`, comment at `:17`). JSON
numbers with a decimal point or exponent (`3.14`, `-0.5`, `1e5`) cannot
be represented: the number parser (`__json_parse_number`,
`std/json.sigil:341`) only ever yields `JInt`. Real-world JSON is full
of floats, so this blocks any honest JSON tool — in particular the
planned `jq` clone, whose value model must round-trip the inputs `jq`
accepts.

## Goal

`JValue` gains a `JFloat(Float)` variant; the parser produces it for
numbers that carry a decimal point or exponent; the renderer emits it;
integers continue to parse as `JInt`. JSON numbers round-trip.

## Behavior

- **Parse.** A number token is scanned as today. If it contains `.`,
  `e`, or `E`, the full numeric substring is parsed to a `Float` via
  `std.float.string_to_float` (`std/float.sigil:94`) and wrapped as
  `JFloat`. Otherwise it remains `JInt` exactly as now. A substring that
  fails `string_to_float` raises the existing parse error.
- **Render.** `JFloat(f)` renders via `std.float.float_to_string`
  (`std/float.sigil:51`); `JInt` rendering is unchanged
  (`std/json.sigil:115`).

## Non-goals

- Arbitrary precision / bignum. `JFloat` is an IEEE-754 `f64`, matching
  sigil's `Float`.
- Changing `JInt` or integer parsing behavior.
- Number-formatting niceties beyond what `float_to_string` already does
  (it emits `.0` for whole floats; inf/NaN unchanged).

## Acceptance criteria

1. `JValue` has a `JFloat(Float)` variant; all `JValue` matches stay
   exhaustive and the module compiles.
2. Parsing `3.14`, `-0.5`, and `1e5` yields `JFloat`; parsing `42` and
   `-7` still yields `JInt`.
3. A parsed `JFloat` renders back to a numerically-equivalent string;
   round-trip (`parse` then `json_render`) holds for a mixed
   int/float document.
4. All existing `std/json.sigil` integer tests and the full existing
   suite still pass (no regression).

## Constraints and gotchas

- **Exhaustiveness.** Sigil requires exhaustive matches, so the
  `JFloat` variant and its render arm must land together — adding the
  variant without a render arm breaks `__json_print_value`
  (`std/json.sigil:107`). The variant + render + the
  `import std.float` / `use std.float.{...}` lines are one task; the
  parser change is a second task on top.
- Touch points: `type JValue` (`std/json.sigil:83`), render arm after
  `JInt` (`std/json.sigil:115`), the number parser
  (`__json_parse_number` `:341`, `__json_parse_digits` `:316`, the
  `JInt` wrappers at `:333`/`:338`).
- All product changes are in the single file `std/json.sigil`, so the
  two implementation tasks are dependency-ordered (no concurrent edits).

## Test expectations

- e2e (`compiler/tests/e2e.rs`): float parse, int-still-int, exponent,
  negative-float, and a mixed-document round-trip.
- No regression across the existing suite.

## Tasks (for the board)

One logical unit per task; each leaves `main` green (CI-gated). The
variant and its render arm are a single atomic unit — Sigil's
exhaustive-match rule means adding the variant without the render arm
fails to compile, so they cannot be split. Sigil does not error on
unused functions, so the parser helper can land before it is wired.

1. **`json-jfloat-variant-render`** — add `JFloat(Float)` to `JValue`,
   its arm in `__json_print_value` via `float_to_string`, and the
   `import std.float` / `use std.float.{float_to_string}` lines. Compiles
   exhaustively; the parser still emits only `JInt`.
2. **`json-jfloat-parse-helper`** — add a `__json_parse_float` helper
   that scans a full number token (integer part + optional `.` fraction
   + optional `e`/`E` exponent) and returns `JFloat` via
   `string_to_float`, raising the existing parse error on failure. Lands
   unwired (green as an unused helper). Depends on (1).
3. **`json-jfloat-parse-wire`** — `__json_parse_number` detects a
   `.`/`e`/`E` in the number token and dispatches to the helper from
   (2); otherwise the existing `JInt` path is unchanged. Depends on (2).
4. **`json-jfloat-e2e`** — parse / render / round-trip / exponent /
   negative-float tests. Depends on (3).

Ships in the next sigil release (v1.2.0, minor — stdlib feature), which
the `jq` clone then builds against.
