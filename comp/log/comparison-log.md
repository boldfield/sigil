# Cross-language comparison log — run 2026-05-17T09:14:19-0700

Trace: `comp/log/comparison-results-20260517T091419.jsonl`
Runs per (prompt, language, model): **1**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-haiku-4-5` | 21/25 (84.0%) | 23/25 (92.0%) |
| `sigil` | `claude-sonnet-4-6` | 14/25 (56.0%) | 24/25 (96.0%) |

## Per-prompt × language × model — first-pass

| Prompt | `sigil` `claude-haiku-4-5` | `sigil` `claude-sonnet-4-6` |
|---|---|---|
| **C01** — hello world | ✅ | ✅ |
| **C02** — sum 1 to 100 | ✅ | ❌ |
| **C03** — fibonacci(15) | ✅ | ✅ |
| **C04** — factorial(10) | ✅ | ✅ |
| **C05** — fizzbuzz 1 to 15 | ✅ | ✅ |
| **C06** — primality test for 29 | ✅ | ❌ |
| **C07** — gcd(48, 18) | ✅ | ❌ |
| **C08** — count digits in 12345 | ✅ | ❌ |
| **C09** — max of a hardcoded list | ❌ | ❌ |
| **C10** — Collatz steps for 27 | ✅ | ❌ |
| **C11** — map missing-key lookup | ✅ | ❌ |
| **C12** — parse invalid integer | ✅ | ❌ |
| **C13** — find first matching element when none exists | ✅ | ❌ |
| **C14** — index out of bounds | ✅ | ✅ |
| **C15** — integer-vs-float division (average) | ❌ | ✅ |
| **C16** — handle division by zero | ✅ | ❌ |
| **C17** — reverse a string | ✅ | ✅ |
| **C18** — Roman numeral to integer | ✅ | ✅ |
| **C19** — validate balanced brackets | ✅ | ✅ |
| **C20** — postfix expression evaluator | ❌ | ✅ |
| **H01** — Wordle scoring | ❌ | ✅ |
| **H02** — JSON number validator | ✅ | ✅ |
| **H03** — Right-associative power evaluator | ✅ | ❌ |
| **H04** — Stable sort with tie-breaking | ✅ | ✅ |
| **H05** — Floor division (round toward negative infinity) | ✅ | ✅ |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-haiku-4-5` | `sigil` `claude-sonnet-4-6` |
|---|---|---|
| **C01** — hello world | ✅ | ✅ |
| **C02** — sum 1 to 100 | ✅ | ✅ |
| **C03** — fibonacci(15) | ✅ | ✅ |
| **C04** — factorial(10) | ✅ | ✅ |
| **C05** — fizzbuzz 1 to 15 | ✅ | ✅ |
| **C06** — primality test for 29 | ✅ | ✅ |
| **C07** — gcd(48, 18) | ✅ | ❌ |
| **C08** — count digits in 12345 | ✅ | ✅ |
| **C09** — max of a hardcoded list | ✅ | ✅ |
| **C10** — Collatz steps for 27 | ✅ | ✅ |
| **C11** — map missing-key lookup | ✅ | ✅ |
| **C12** — parse invalid integer | ✅ | ✅ |
| **C13** — find first matching element when none exists | ✅ | ✅ |
| **C14** — index out of bounds | ✅ | ✅ |
| **C15** — integer-vs-float division (average) | ❌ | ✅ |
| **C16** — handle division by zero | ✅ | ✅ |
| **C17** — reverse a string | ✅ | ✅ |
| **C18** — Roman numeral to integer | ✅ | ✅ |
| **C19** — validate balanced brackets | ✅ | ✅ |
| **C20** — postfix expression evaluator | ✅ | ✅ |
| **H01** — Wordle scoring | ❌ | ✅ |
| **H02** — JSON number validator | ✅ | ✅ |
| **H03** — Right-associative power evaluator | ✅ | ✅ |
| **H04** — Stable sort with tie-breaking | ✅ | ✅ |
| **H05** — Floor division (round toward negative infinity) | ✅ | ✅ |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile |
|---|---|
| `sigil` | 18 |

## Failures (3 cell(s), 3 run(s))

### `C07` × `sigil` × `claude-sonnet-4-6`

Final attempt category: **compile**

```
error[E0042]: `operator `%` (may abort with ArithError)` requires `ArithError` in the enclosing function's effect row
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C07-sigil-lkzs792n/program.sigil:9:23
```

### `C15` × `sigil` × `claude-haiku-4-5`

Final attempt category: **compile**

```
error[E0010]: expected an expression
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C15-sigil-s26iy4mf/program.sigil:12:45
```

### `H01` × `sigil` × `claude-haiku-4-5`

Final attempt category: **compile**

```
error[E0150]: `MutArray[A]` element type `Char` is not supported in v1 (the runtime element slot is i64; narrow scalars don't fit without per-call type-arg threading at codegen — see v2). For Bool: use `MutArray[Int]` with 0/1 sentinels. For Byte: use `ByteArray` / `MutByteArray` (the flat-byte primitives). For Char: use `MutArray[Int]` of codepoints or `String` / `List[Char]`.
```

