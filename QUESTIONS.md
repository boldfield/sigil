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

## 2026-04-23 — [PLAN-A2] Factorial example needs Stage-3 function-call support

**Context:** Plan A2's Stage 2 (Tasks 20–28) ends with three tasks that
depend on features the plan schedules in Stage 3 (Tasks 29–35):

- Task 26 — "Create `examples/factorial.sigil` (recursive factorial,
  prints the result)."
- Task 27 — Performance floor: "`factorial(10)` compiles and runs in
  <100ms on both hosts". `factorial(20)` also mentioned.
- Task 28 — Prompt bank entries that reference recursion (P04:
  "sum-to-n via recursion", P06: "multiplication table using nested
  recursion").

Recursive `factorial` requires **user function definitions with
parameters** and **user-fn call expressions**. Both arrive in Stage 3:

- Task 29 (Stage 3): "Extend parser: multi-argument function
  declarations; function-call expressions with arguments; lambda
  expressions."
- Task 30 (Stage 3): "Extend type checker: function types;
  application-site unification; closure capture analysis."
- Task 32 (Stage 3): "Extend codegen: closure calling convention;
  indirect call via the closure's code pointer; closure allocation
  on the GC heap."

Plan A1's parser grammar already includes `fn_decl` with `param_list?`,
so multi-parameter function declarations parse today — but Task 30's
call-site typing and Task 32's codegen don't exist in Stage 2, and the
existing typecheck rejects `Expr::Call` with `E0043`. Without Stage 3's
machinery, `factorial` is literally un-compilable in Stage 2.

**Question:** How should Stage 2 (Tasks 26–28) handle the factorial /
recursion dependency?

- (a) Defer Tasks 26 factorial / 27 perf floor / 28 recursion-bearing
  prompts to Stage 3. Stage 2 acceptance shrinks to `examples/
  arith.sigil` + `div_by_zero.sigil`, and Task 28's prompt bank adds
  only P05 (mod + if/else) and P07 (divide guard) at this stage.
- (b) Pull Task 29's grammar/typing/codegen for single-arg functions
  earlier — effectively merging the minimum subset of Stage 3 into
  Stage 2 so factorial compiles. Scope creep at plan level; deviation
  must be logged and reviewed.
- (c) Rewrite `factorial.sigil` to use a non-recursive approach (CPS
  trampoline via a closure built ad-hoc, or unrolled multiplication).
  Contradicts the plan's stated "recursive factorial" wording.

**Status:** open (2026-04-23) — surfaces at Task 26. Current Tasks
24+25 do not depend on this resolution; `factorial.sigil` is a Task 26
concern.

**Resolution:** (pending) — implementor defaults to option (a) when Task
26 arrives unless reviewer decides otherwise before then. The planned
scope adjustment: Task 26 ships `examples/arith.sigil` +
`div_by_zero.sigil` only; Task 27's perf floor drops to `arith.sigil`
run-time; Task 28 ships P05 + P07 only (P04 + P06 wait for Stage 3).
Task 33's `examples/fibonacci.sigil` (already Stage 3) absorbs the
recursive-oracle role `factorial.sigil` was meant to play.
