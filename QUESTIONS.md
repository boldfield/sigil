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

**Status:** resolved (2026-04-23) by implementor decision; open to reviewer
override.

**Resolution:** Implementor chose (a) for Task 22 — the explicit typing-rule
list wins, Byte ordering via `byte_to_int` + int compare until a later plan
generalises. Deviation is not logged (plan bullet about ordering is descriptive
of capability, not a contradictory typing rule). Reviewer may override at PR
#2's Task 22 commit; changing to (b) is one additional arm in the
typechecker's binop handler and one additional test.
