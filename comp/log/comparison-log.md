# Cross-language comparison log — run 2026-05-17T08:17:25-0700

Trace: `comp/log/comparison-results-20260517T081725.jsonl`
Runs per (prompt, language, model): **1**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-haiku-4-5` | 18/25 (72.0%) | 23/25 (92.0%) |
| `sigil` | `claude-sonnet-4-6` | 14/25 (56.0%) | 22/25 (88.0%) |
| `python` | `claude-haiku-4-5` | 25/25 (100.0%) | 25/25 (100.0%) |
| `python` | `claude-sonnet-4-6` | 24/25 (96.0%) | 25/25 (100.0%) |
| `go` | `claude-haiku-4-5` | 25/25 (100.0%) | 25/25 (100.0%) |
| `go` | `claude-sonnet-4-6` | 25/25 (100.0%) | 25/25 (100.0%) |
| `rust` | `claude-haiku-4-5` | 23/25 (92.0%) | 25/25 (100.0%) |
| `rust` | `claude-sonnet-4-6` | 22/25 (88.0%) | 24/25 (96.0%) |

## Per-prompt × language × model — first-pass

| Prompt | `sigil` `claude-haiku-4-5` | `sigil` `claude-sonnet-4-6` | `python` `claude-haiku-4-5` | `python` `claude-sonnet-4-6` | `go` `claude-haiku-4-5` | `go` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5` | `rust` `claude-sonnet-4-6` |
|---|---|---|---|---|---|---|---|---|
| **C01** — hello world | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C02** — sum 1 to 100 | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C03** — fibonacci(15) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C04** — factorial(10) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **C05** — fizzbuzz 1 to 15 | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C06** — primality test for 29 | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C07** — gcd(48, 18) | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C08** — count digits in 12345 | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ |
| **C09** — max of a hardcoded list | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C10** — Collatz steps for 27 | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C11** — map missing-key lookup | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C12** — parse invalid integer | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ✅ |
| **C13** — find first matching element when none exists | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C14** — index out of bounds | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C15** — integer-vs-float division (average) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C16** — handle division by zero | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C17** — reverse a string | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C18** — Roman numeral to integer | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C19** — validate balanced brackets | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **C20** — postfix expression evaluator | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H01** — Wordle scoring | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H02** — JSON number validator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H03** — Right-associative power evaluator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H04** — Stable sort with tie-breaking | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H05** — Floor division (round toward negative infinity) | ✅ | ✅ | ✅ | ❌ | ✅ | ✅ | ✅ | ❌ |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-haiku-4-5` | `sigil` `claude-sonnet-4-6` | `python` `claude-haiku-4-5` | `python` `claude-sonnet-4-6` | `go` `claude-haiku-4-5` | `go` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5` | `rust` `claude-sonnet-4-6` |
|---|---|---|---|---|---|---|---|---|
| **C01** — hello world | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C02** — sum 1 to 100 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C03** — fibonacci(15) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C04** — factorial(10) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C05** — fizzbuzz 1 to 15 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C06** — primality test for 29 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C07** — gcd(48, 18) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C08** — count digits in 12345 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C09** — max of a hardcoded list | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C10** — Collatz steps for 27 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C11** — map missing-key lookup | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C12** — parse invalid integer | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C13** — find first matching element when none exists | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C14** — index out of bounds | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C15** — integer-vs-float division (average) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C16** — handle division by zero | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C17** — reverse a string | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C18** — Roman numeral to integer | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **C19** — validate balanced brackets | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| **C20** — postfix expression evaluator | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H01** — Wordle scoring | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H02** — JSON number validator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H03** — Right-associative power evaluator | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H04** — Stable sort with tie-breaking | ✅ | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| **H05** — Floor division (round toward negative infinity) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile | stdout |
|---|---|---|
| `sigil` | 21 | 2 |
| `python` | 0 | 1 |
| `go` | 0 | 0 |
| `rust` | 2 | 4 |

## Failures (6 cell(s), 6 run(s))

### `C11` × `sigil` × `claude-sonnet-4-6`

Final attempt category: **compile**

```
error[E0046]: unknown identifier `int_to_string`
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C11-sigil-s21zviaj/program.sigil:14:35
```

### `C16` × `sigil` × `claude-sonnet-4-6`

Final attempt category: **stdout**

```
output differs from oracle
```

### `C19` × `rust` × `claude-sonnet-4-6`

Final attempt category: **stdout**

```
output differs from oracle
```

### `C20` × `sigil` × `claude-haiku-4-5`

Final attempt category: **compile**

```
error[E0010]: expected `{` opening block
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-f96ui5ry/program.sigil:30:14
error[E0010]: expected `}` closing match arms
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-f96ui5ry/program.sigil:30:80
error[E0010]: expected `import`, `use`, `fn`, `type`, or `effect` at top level
  --> /var/folders
```

### `H01` × `sigil` × `claude-haiku-4-5`

Final attempt category: **compile**

```
thread 'main' (68414473) panicked at compiler/src/codegen.rs:26688:25:
internal error: entered unreachable code: codegen: ctor pattern `Some` not in ctor_index
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

### `H04` × `sigil` × `claude-sonnet-4-6`

Final attempt category: **compile**

```
error[E0042]: `operator `/` (may abort with ArithError)` requires `ArithError` in the enclosing function's effect row
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-rb0nawaw/program.sigil:14:23
```

