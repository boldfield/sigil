# Sigil — Open Questions

Questions that block plan progress go here. Resolved questions remain as a
decision log. This file is preserved across all plans (A1, A2, A3, B, C) so
the rationale trail is single-threaded.

Per-plan convention: prefix the `one-line topic` in the heading with a plan
tag so the source is grep-able. Tags: `[PLAN-A1]`, `[PLAN-A2]`, `[PLAN-A3]`,
`[PLAN-B]`, `[PLAN-C]`. Untagged entries are legacy (pre-A2) and implicitly
belong to `[PLAN-A1]`.

Format:

```
## <date> — [PLAN-Ax] <one-line topic>

**Context:** ...

**Question:** ...

**Status:** open | resolved (<date>)

**Resolution:** (if resolved)
```

## 2026-04-23 — [PLAN-A2] Byte ordering via `< > <= >=`

**Context:** Plan A2 Task 22 contains two statements that appear to conflict:

1. The explicit typing-rule enumeration: "`< > <= >=`: Int→Int→Bool".
2. The Byte-feature paragraph: "Ordering via `< > <= >=`" (i.e. Byte is
   orderable through those operators).

A v1 program cannot construct a `Byte` value until Task 25 lands the runtime
primitives, so no Task 22 test can actually exercise Byte ordering either way.
This question is therefore latent until Task 24/25 codegen wants to emit
comparisons on Byte operands.

**Question:** Which rule is authoritative for `< > <= >=`?

- (a) Strict form from the typing-rule list: Int→Int→Bool *only*. Byte ordering
  is expressed via `byte_to_int` + int compare until Plan A3/B extends the
  comparison operators.
- (b) Polymorphic form implied by the Byte-feature paragraph: `< > <= >=`
  accept either (Int, Int) or (Byte, Byte) and return Bool.

**Status:** resolved (2026-04-23) by reviewer decision on PR #2.

**Resolution:** Choice (a) — strict form: `< > <= >=` is `Int → Int → Bool`
only. Confirmed by reviewer in the PR #2 top-level review comment dated
2026-04-23T17:35:29Z. Three reasons:

1. **Normative typing rules outrank descriptive paragraphs when they
   conflict.** Task 22's typing-rule enumeration is the specification;
   the Byte-feature paragraph is describing the forward-looking
   capability surface, not contradicting the formal rules.
2. **Relaxing to polymorphism for one operator in A2 costs more than doing
   it once in A3** when sum types land and ad-hoc polymorphism (over
   `Orderable` or an equivalent constraint) becomes necessary anyway.
   Adding a one-off `Int | Byte` special case now means re-auditing and
   refactoring it when the general mechanism arrives.
3. **`byte_to_int(b1) < byte_to_int(b2)` is a one-line user workaround.**
   For the handful of Byte-ordering use cases in a Plan-A2-era program,
   the lift-to-Int pattern is ergonomic enough.

Byte equality (`==` / `!=`) continues to work via the existing
`T → T → Bool for primitives` rule — this covers the vast majority of
byte-comparison use cases (delimiter matching in network and binary
parsing) without needing operator polymorphism.

Implementation in `compiler/src/typecheck.rs` (as landed in Task 22,
commit `1de46b4`) matches the chosen form; no code change is required by
this resolution.
