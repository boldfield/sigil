# Sigil: A Language LLMs Get Right the First Time

**The thesis:** explicit types, mandatory effect rows, exhaustive matching, and no operator overloading aren't just good engineering — they make a language LLMs can author correctly. Sigil moves the error surface forward, from a runtime exception you see in production to a compile-time error caught before the program ever runs.

The numbers below come from 1,626 independent LLM-authoring trials across three test corpora. Every result is reproducible — raw traces under `spec/` and `comp/log/`.

---

## The headline

Across 1,626 fresh-session LLM-authoring trials of Sigil code from `claude-opus-4-7`, `claude-sonnet-4-6`, and `claude-haiku-4-5`:

- **~85% first-shot correctness** (compile + run + match oracle on the very first sampled program)
- **~99% after one edit-loop iteration** (the model sees its own compile error and tries again)
- **>99% of failures caught at compile time** — not in production

**The 99% compile-side failure rate is the load-bearing claim.** Python is friendlier to write but every bug ships to runtime. Go catches more at type-check but its type system is intentionally thin. Sigil's typecheck + effect row + exhaustivity together catch the LLM's mistakes where they belong: before the program runs.

---

## Three corpora, one story

### Corpus 1: spec validation (P01–P62)

62 prompts walking the language surface from `hello world` to multi-shot effects to `MutByteArray` checksums. **1,240 runs** — 62 prompts × 2 models × 10 fresh API sessions each.

| Metric | Opus | Sonnet |
|---|---|---|
| First-pass | 93.1% | 93.9% |
| Final-pass | **99.8%** | **100.0%** |

**Failure-mode breakdown across all 82 failed attempts: 82 compile errors, 0 runtime errors.** Zero. Every single time an LLM wrote bad Sigil, the compiler caught it.

Only one cell across the entire 1,240-run grid didn't reach 10/10 after one edit-loop: P20 (multi-shot Choose finding all `(a, b)` pairs with `a + b == 7`). That's the hardest construct in the language and it's at 9/10 with Opus, 10/10 with Sonnet.

### Corpus 2: cross-language comparison (C01–C20)

The same prompts authored against Sigil, Python, and Go. 380 Sigil runs across three models, 240 Python runs, 240 Go runs.

On C01–C10 (the parity tier), first-pass numbers — yes, Python and Go beat Sigil here:

| Language | Pass rate | Failure mode |
|---|---|---|
| Python | 100.0% | n/a (no failures) |
| Go | 99.5% | **runtime** (silent stdout mismatch) |
| Sigil | 76.0% | **99% compile-time** |

The pass-rate gap is real. Sigil's stricter surface — required effect rows, `Result[Int, ParseError]` instead of "raises ValueError", exhaustive matching — means the LLM has to author it correctly. **But the rare Go failure shipped a binary that ran and produced wrong output. The Sigil failures were caught before the binary existed.**

After one edit-loop, Sigil closes the gap to **93–96% final-pass** across all three models. The same prompts the LLM gets wrong on first try, it fixes after seeing its own compile error.

On C11–C20 (the runtime-fragility tier — division-by-zero, missing keys, integer overflow), Sigil's discipline becomes the asset: the LLM is forced to think about the failure cases up front.

### Corpus 3: H-tier hard correctness (H04 stable sort)

The H-tier exists to stress subtle correctness traps — places where idiomatic code in other languages silently does the wrong thing. H04 specifically targets stable-sort tie-breaking; in Go, `sort.Slice` is unstable and produces nondeterministic output for ties, while `sort.SliceStable` is correct.

Sigil's `list_sort` from `std.list` is documented as stable. The LLM has to use it correctly. After [PR #142](https://github.com/boldfield/sigil/pull/142) closed an outer-match-arm-binding codegen ICE that surfaced on Sonnet's natural H04 program:

| Model | First-pass | Final-pass |
|---|---|---|
| Opus | 3/3 | 3/3 |
| Sonnet | 2/3 | 3/3 |

6/6. The one Sonnet first-pass miss was an unrelated `![ArithError]` annotation on integer division — fixed in one edit-loop turn. (That rule has since been removed: `/` and `%` now trap on a zero divisor and carry no effect, so the same code compiles first-pass today.)

---

## Why this works

LLMs are pattern matchers trained on a corpus of human code. That corpus has:
- Implicit types everywhere (Python, Ruby, JavaScript)
- Runtime exceptions for "the call site forgot to check" (KeyError, IndexError, NullPointerException)
- Operator overloading that means `+` does whatever the runtime decides
- Optional patterns that compile and fail at runtime

Sigil deliberately removes each of those:
- **Explicit types** at every binding, parameter, and return
- **Effect rows** — a function's effects (e.g. `![IO]`) are part of its type; you can't perform an effect without declaring it in your row
- **Exhaustive matching** — the compiler rejects a `match` that doesn't cover every variant
- **No operator overloading** — `+` is `+` on `Int`, and `string_concat` on strings
- **Result and Option** — fallible operations return values, not exceptions

These aren't "nice to have" — they're the load-bearing structural choices that turn LLM-authored Sigil from "ships to runtime and breaks" to "compiles or doesn't." The empirical signal validates the design.

---

## What the numbers don't say

There are real gaps, documented honestly in [CAPABILITIES.md](./CAPABILITIES.md):

- **P20 multi-shot Choose**: the hardest single construct. 1 final-pass miss across 1,240 runs.
- **C12 parse invalid integer**: persistent 0/9 final-pass. Sigil's `Result[Int, ParseError]` API differs enough from `int(s)` that the LLM keeps reaching for the wrong shape.
- **C20 postfix evaluator**: 1/3 — 2/3 final-pass. Stack-discipline programs are hard for LLMs in any language; Sigil's amplifies the difficulty by requiring explicit error handling on each pop.
- **Sonnet vs. Opus on C-tier**: Sonnet first-pass is materially lower (64.4% vs. 88.1% on C* prompts), driven by stdlib under-utilization. (Division without `![ArithError]` was a contributor at the time of the run but is no longer a compile error — `/` and `%` now trap and need no row — so a re-run today would close part of this gap automatically; the broader stdlib-utilization deficit persists.) Edit-loop closes most of the gap; opus is the safer authoring partner today.

These gaps are inputs to the next iteration of the spec and stdlib. The validation-prompt corpus + cross-language harness + H-tier corpus aren't just demos — they're a feedback loop that turns LLM friction into language design data.

---

## Reproducing the numbers

Everything is in the repo:

```bash
# P* spec validation (62 prompts × 2 models × N runs)
export ANTHROPIC_API_KEY=...
python3 scripts/validate_spec.py --runs 10
# → spec/validation-results-YYYYMMDDtHHMMSS.jsonl
#   spec/validation-log.md

# C*/H* cross-language harness
python3 comp/scripts/compare.py --langs sigil,python,go --runs 10
# → comp/log/comparison-results-YYYYMMDDtHHMMSS.jsonl
#   comp/log/comparison-log.md

# Single prompt, sigil only, both models, 3 runs:
python3 comp/scripts/compare.py --langs sigil --filter '^H04$' --runs 3
```

The harness is deterministic about its part of the pipeline (oracle matching, edit-loop discipline, K/N aggregation). The model is stochastic. Both halves are visible in the JSONL trace for every cell.

---

## Where this is going

Sigil is an academic exercise in compiler design — but the LLM-authorship hypothesis is the load-bearing motivation. Every spec choice ("must have effect row," "no operator overloading," "exhaustive match") gets tested against the corpus. Choices that don't improve LLM authorship eventually get the conversation "do we still need this?"

The harness data feeds back into:
- **Stdlib expansion**: where the LLM reaches for an idiom Sigil doesn't have, add the idiom.
- **Diagnostic improvement**: where the same compile error fires repeatedly across the corpus, improve the error message so the edit-loop fixes it in one turn.
- **Spec teaching**: where the LLM consistently mis-uses a construct, the spec text gets clearer examples or a new dedicated section.

PRs #135 through #142 in the last week of 2026-05 each closed a specific friction point surfaced by harness data. The cycle is short and the signal is empirical.

---

If you want the raw cut: see [CAPABILITIES.md](./CAPABILITIES.md). If you want to challenge a number: every JSONL trace is in the repo. If you want to run your own harness against a different model or language: the scripts are in `comp/scripts/` and `scripts/`.

Sigil isn't trying to be the language humans want most. It's trying to be the language LLMs get right.
