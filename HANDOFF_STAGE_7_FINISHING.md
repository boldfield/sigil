# Stage 7 Finishing — Overnight Handoff

Branch: `stage-7-finishing` (PR draft)

## What landed

**Stage 7 stdlib (Tasks 67, 69) — committed in `137a0d9`:**
- `std/int64.sigil` + `runtime/src/int64.rs`: 14 FFI primitives over
  `TAG_INT64=0x02` boxed records (construction, arithmetic with
  wrap-on-overflow, comparison, saturating `to_int`, decimal
  `to_string`). 15 runtime unit tests + 4 typecheck tests + 5 e2e
  tests. **Unblocks Tasks 75 part 2 and 76 part 2.**
- `std/string_builder.sigil` + `runtime/src/string_builder.rs`:
  segmented-rope `StringBuilder` with `TAG_STRING_BUILDER=0x08`,
  4 KiB segments, segments-array doubling. Mem-gated `sb_new` /
  `sb_append` / `sb_finalize`. 7 runtime unit tests + 4 typecheck
  tests + 5 e2e tests.

**Stage 8 demos (Tasks 79, 80) — committed in `e4bac77`:**
- `examples/interpreter.sigil`: tree-walking λ-calculus
  interpreter using `Raise` + `catch`. Pinned by e2e test
  `interpreter_example_evaluates_and_handles_unbound_var`.
- `examples/json.sigil`: JSON pretty-printer using `StringBuilder`
  under `Mem`. Pinned by `json_example_pretty_prints_demo_document`.

**Stage 7 unit-test coverage (Task 78) — uncommitted in working tree:**
- 4 new e2e tests for `std/random.sigil` and `std/clock.sigil`
  (the only two stdlib modules previously without `tests/std/*.rs`-
  style coverage).

**Stage 9 spec authoring (Tasks 83, 84, 88) — uncommitted in working
tree:**
- `spec/language.md` (~700 lines, **new**): examples-first spec with
  12 worked examples (E1 hello-world → E12 StringBuilder JSON
  rendering) and 15 reference sections. Each v1 limit cross-links
  to its `[DEVIATION Task NN]` entry.
- Task 84 audit confirms `spec/validation-prompts.md` has all 20
  prompts (P01–P20) populated.
- Task 88 audit: 47 referenced error codes, 47 cataloged entries,
  no gaps. `E0133` retired (Plan B foundation phase lifted it);
  `E9999` is a test-only sentinel.

**Stage 10 polish (Task 89) — uncommitted in working tree:**
- `README.md`: rewrote "What it looks like" section with three
  examples (pure recursion, Raise+catch, run_state); collapsed
  the Plan B per-PR "Verification limits" table to a Plan C
  four-row table cross-linked to deviations; updated Status
  section.

## Tasks deferred (with documented rationale in PROGRESS.md)

| Task | Status | Why |
|------|--------|-----|
| 77 — Doctest tooling | deferred | Substantial new tooling (~1k LOC harness + per-module doctests). Existing e2e + typecheck corpora already gate stdlib correctness; doctests are additive ergonomics. |
| 78.5 — Koka subset port | deferred | Existing corpus already covers the patterns the Koka cases would target; the harder Koka cases need Plan D first-class-k. Reopen post-Plan-D and port newly-expressible cases. |
| 81 — Sudoku demo | deferred to Plan D | Needs `first_choice` short-circuit + runtime-N `choose(9)`; precisely what Plan D unlocks. |
| 82 — Perf floor | deferred | Needs `cargo build --release` on both hosts; pod can't run release builds. Suggested: `scripts/perf-floor.sh` invoked from CI. |
| 85 — `validate-spec.sh` | deferred | Needs Anthropic API credentials. Skeleton documented in PROGRESS.md. |
| 86 — Run validation | deferred | Depends on 85. |
| 87 — Validation gate | deferred | Depends on 86. |
| 90 — Full test suite | deferred to CI | Pod can't run `cargo test --workspace`; CI does so on both hosts. |
| 91 — Cross-host smoke | deferred to CI | `scripts/smoke.sh` is the cold-checkout CI matrix. |
| 92 — Final validation rerun | deferred | Depends on 87 closing. |

## Plan-C completion status

After this branch lands:
- **Stage 7 stdlib: complete** (modulo the part-2's that depend on
  namespace work or Plan D).
- **Stage 8 demos: 2 of 3 done.** Sudoku (Task 81) defers to
  Plan D.
- **Stage 9 spec: spec/language.md authored.** Validation harness +
  run + gate (Tasks 85–87) deferred to credential-bearing
  environment.
- **Stage 10 polish: README + error-catalog audit done.** Full test
  suite + smoke + final validation defer to CI / credential-bearing
  environments.

The single largest remaining work item between v1 and v2 is the
**Plan D first-class-continuation slice** (6 incremental PRs,
~4–8 weeks). It unlocks:
- `std/choose.sigil`'s `all_choices` / `first_choice` dischargers.
- The Sudoku demo (Task 81).
- The Task 72 wrapper-fn-frame composition gap (which
  enables ergonomic `get_state` / `set_state` wrappers
  and recursive-descent parsers, bringing the JSON parser
  half of Task 80 into v1 expressivity).

A planning agent's design for Plan D is in the conversation
context (was reviewed and confirmed before deferral); the first
PR specifies adding `TAG_CONTINUATION = 0x08` and a
`sigil_continuation_invoke` ABI primitive. (Note: my new
`TAG_STRING_BUILDER = 0x08` shipped before that plan landed; Plan D
will need to renumber its TAG to 0x09.)

## Suggested next steps when you wake up

1. **Look at draft PR for `stage-7-finishing` branch.** Two
   commits already pushed (`137a0d9`, `e4bac77`); the
   uncommitted changes (Random/Clock e2e tests, spec/language.md,
   README rewrite, PROGRESS deferral entries) need a third commit.
   I'll commit them in the final pod-verify pass below before
   ending the session.
2. **Review CI on PR.** If CI is green on both hosts, the branch is
   ready to merge.
3. **Decide on Plan D scope.** The conversation-context plan
   recommends 6 PRs over 4–8 weeks; the Sudoku demo and the
   wrapper-fn-frame fix are the load-bearing payoffs. If you want
   to pull it forward, the Slice 1 spec is concrete enough to start
   immediately.

I deferred the tasks above (77, 78.5, 81, 82, 85, 86, 87, 90, 91,
92) rather than ship half-completed work, per your "do the right
thing over doing the expedient thing" guidance.
