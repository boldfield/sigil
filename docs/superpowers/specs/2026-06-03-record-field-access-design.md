# `record.field` field-access operator — design

**Status:** approved (brainstorming complete), ready for implementation plan.

**Goal:** Let an author read a named record field with `binding.field`
(and chains `binding.f1.f2`), instead of the current
match-destructure-only surface. This is an **LLM-ergonomics** feature:
every model's prior expects `entry.name`; Sigil v1 forces a `match` or a
hand-written accessor fn for each field read, which is exactly the
boilerplate observed in the H04 comparison-corpus programs (`entry_name`
/ `entry_score` accessor fns wrapping a `match`). It currently produces
diagnostic **E0151** ("no field-access operator; use match destructure").

## User-facing contract (this is what changes for the author)

```sigil
type Person = { name: String, age: Int }
fn greet(p: Person) -> String ![] {
  p.name                       // <- field access; was E0151
}
```

- `binding.field` reads the field; the expression's type is the field's
  type.
- Chains read through nested records: `node.left.value`.
- Works on **single-variant record types** (`type T = { f: U, … }` — the
  form `std.map`/`std.set` use) including generic ones (`m.size` on
  `Map[K, V]`, with field types substituted).
- Read-only. There is no field *update* (`{ r | f: v }`) — out of scope.
- Existing `match` destructuring is unchanged; field access is purely
  additive. Programs that previously hit E0151 now compile.

## Scope (decided)

- **The chain's *head* is a bare identifier** bound in scope (let /
  param / match-binding). The full chain `head.f1.f2…` is supported:
  `entry.name` and `node.left.value` are both in scope (head `entry` /
  `node` is a bare identifier; the rest walks record fields). What is
  **out of scope** is a non-identifier head — field access on a call
  result (`make_entry(...).name`) or a parenthesised expression — which
  needs a real postfix-`.` parser change and a new AST node; it can be
  added later without reworking what we ship now. Rationale: the observed
  friction is 100% bare-head field reads; this is the smallest change
  that closes it.
- Tuples are excluded naturally — `t.0` does not form an identifier
  chain in the lexer (`0` is an `IntLit`, not an `Ident`).

## Why this is mostly a typecheck change (current machinery)

- **Parser:** unchanged. `person.name` and `node.left.value` already
  parse — the parser accumulates a dotted identifier chain into a single
  `Expr::Ident("person.name")` (the same mechanism that carries
  qualified names like `std.list.map`, `Option.Some`, `IO.println`). It
  only walks the chain while the lookahead is `Dot` followed by an
  `Ident`, so a trailing `.0` (tuple) or `.` stops the chain.
- **Typecheck:** `compiler/src/typecheck.rs:6447-6479` is where a dotted
  `Expr::Ident` that matches no imported-module prefix currently fires
  E0151. That is the exact site the feature replaces.
- **Codegen:** record field offsets already exist (the `TypeLayout`
  machinery that record *patterns* use — `CtorPatternFields::Record`).

## Resolution (the core change)

In the bare-name/dotted-name resolver, for a dotted
`Expr::Ident("a.b.c")`:

1. **Qualified-name resolution keeps priority** (unchanged). If any
   prefix matches a known imported module / the existing qualified-path
   forms (`std.list.map`, `IO.println`, `Option.Some`), resolve as today.
   This deterministically resolves the `.` overload and guarantees no
   regression for existing qualified references.
2. **Else attempt field-access resolution.** Split into head `a` and
   field chain `b.c…`. `a` must resolve to an in-scope binding whose type
   is a **single-variant record type**. Walk the chain: at each step the
   current type must be a record with that field; the step's result type
   is the field's declared type, with the record's generic parameters
   substituted. On success, the whole expression's type is the final
   field type, and the node is **rewritten** (see below) so codegen can
   emit it.
3. **Else, a precise diagnostic** (replacing the blanket E0151):
   - `a` is not bound → existing **E0046** unknown identifier.
   - `a` is bound but its type is not a single-variant record (a
     primitive, a tuple, or a multi-variant sum type) → "cannot read
     field `b`: `a` has type `X`, which is not a record — use `match` to
     destructure a sum type." (A multi-variant sum has no statically
     known field set, so it must stay `match`-only.)
   - `a` is a record but has no field `b` → "no field `b` on record `T`
     (fields: `f1`, `f2`, …)."

These three messages reuse the freed **E0151** code (its old
"no field-access operator" wording is now obsolete). The catalog entry
is rewritten accordingly.

## Implementation strategy: desugar to `match` (no new AST variant)

The semantics of `a.b` are exactly `match a { T { b: v, <other fields>:
_ } => v }`. Rather than add an `Expr::FieldAccess` variant — which
would force every exhaustive `match expr { … }` across ~8 passes
(typecheck, elaborate, color, monomorphize, closure_convert, discharge,
codegen) to grow an arm — the resolver **desugars** a resolved field
access into that equivalent `match` expression during the typecheck
rewrite pass (the same pass that already rewrites resolved/qualified
idents). Downstream passes then see an ordinary `Match` and reuse all
existing record-pattern type-checking and codegen unchanged.

Details:
- The desugared pattern must list **all** of the record's fields (the
  target field bound to a fresh `__field_<n>` name, every other field a
  wildcard `_`) because Sigil record patterns have **no `..` rest
  syntax** (confirmed: `T { f, .. }` is a parse error — note the current
  E0151 message wrongly suggests `..`; that suggestion is corrected as
  part of this work). The full field list comes from the record's type
  declaration, available at resolve time.
- A chain `a.b.c` desugars outside-in: resolve/rewrite `a.b` to a
  `match` yielding a record, then `.c` wraps it in another `match`.
  Because the base is a bare identifier (single env load), evaluating it
  inside a one-arm match introduces no double-evaluation concern.
- The synthesized `match` carries the original expression's span so
  diagnostics and traces point at the source `a.b`, not synthetic
  internals. Fresh binder names use a reserved prefix that cannot
  collide with user identifiers (mirror the existing `$`/`__`-prefixed
  synthetic-name convention).
- Single-variant records mean the `match` has exactly one arm and is
  exhaustive by construction — no new exhaustiveness obligations.

## What this is NOT (YAGNI)

- Field access on non-identifier bases (`f(x).field`, `(e).field`) — a
  documented follow-up needing a postfix-`.` parser change.
- Field **update** / functional record update (`{ r | f: v }`).
- Method-call syntax (`x.f(y)`).
- `..` rest patterns in `match` (separate parser feature; not required —
  the desugar lists fields explicitly).

## Errors / spec / docs

- **E0151** catalog entry (`compiler/src/errors/catalog.rs`) rewritten
  from "no field-access operator" to the field-resolution errors above
  (not-a-record / no-such-field). Its existing tests flip from
  "expect E0151" to "expect success" (field access now works) or to the
  new not-a-record / no-such-field cases.
- **`spec/language.md` §6** (records) currently states "v1 has no
  `.name` field-access syntax; use match destructuring instead" — update
  to document the operator (bare-identifier base, single-variant
  records, read-only, chained), and note `match` destructuring remains
  for sum types and is still valid for records.
- Add a §13.x / idioms note so the LLM-facing surface advertises
  `record.field`.

## Testing

- **Typecheck unit tests:** `p.name` resolves with the field's type;
  chained `a.b.c`; generic record (`Map`-like) field; the three error
  cases (unbound head, non-record head incl. a sum-type binding,
  unknown field); and a regression that qualified names still win
  (`std.list.map`, `IO.println` unaffected by the new path).
- **e2e:** compile + run programs that read fields via `.` — a flat
  record, a nested record (`a.b.c`), and a generic record — asserting
  correct runtime values; confirm a previously-E0151 program now
  compiles and runs.
- **Ergonomic-win demonstration:** an H04-style program written with
  `entry.name` / `entry.score` directly (no accessor fns / no `match`),
  compiling and running — the concrete payoff.

## File map / deliverables

| File | Change |
|---|---|
| `compiler/src/typecheck.rs` | Replace the E0151-firing branch (~6447) with field-access resolution: classify, resolve the field chain against the record type (with generic subst), produce the resolved type; record the resolution for the rewrite. Reuse existing record-field lookup (`resolve_field_ty`, `VariantFields::Record`). |
| `compiler/src/typecheck.rs` (rewrite pass) | Desugar a resolved field access into the equivalent one-arm `match` (all fields listed, target bound, others `_`), preserving span. |
| `compiler/src/errors/catalog.rs` | Rewrite the `E0151` entry to the not-a-record / no-such-field diagnostics. |
| `compiler/tests/*` (unit) + `compiler/tests/e2e.rs` | New tests above; update existing E0151 tests. |
| `spec/language.md` | §6 records: document the operator; idioms/§13 note. |

## Risks

- **`.` overload with qualified names.** Mitigated by resolving
  qualified-module prefixes *first* (step 1) — only names with no known
  module prefix reach field-access resolution, which is exactly today's
  E0151 gate. A local binding shadowing a module name is not possible
  for the qualified forms in use (`IO`, `Option`, `std.*` are
  capitalized / dotted module paths; record bindings are lowercase
  locals).
- **Desugar fidelity.** The synthesized `match` must produce identical
  semantics and good spans. Covered by e2e (runtime values) + keeping
  the original span on the synthetic node.
- **No `..` rest** means the desugar depends on the record's full field
  list at rewrite time; this is available from the type declaration. If
  a field type is unresolved (opaque generic), the wildcard `_` for it
  is still valid (we only bind the target field).
