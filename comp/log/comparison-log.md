# Cross-language comparison log — re-run after PR #132

Re-run timestamp: 2026-05-10T00:16:43-0700.

Two runs against the comp/ harness (10 prompts × 3 languages = 30 subagents per run, N=1, no edit loop):
- **Run 1** (`agent-outputs.json`): pre PR #132. Sigil context = spec/language.md as of commit 7a9f327.
- **Run 2** (`agent-outputs-run2.json`): post PR #132. Sigil context picks up the new §4.2 ArithError callout (own table row + blockquote naming E0042) and the §5 `_` no-placeholder note.

Same Claude Code subagent invocation pattern in both runs, same prompt corpus, same eval drivers.

## Pass rates by language

| Language | Run 1 (pre-#132) | Run 2 (post-#132) | Δ |
|---|---|---|---|
| `sigil` | 6/10 (60%) | 7/10 (70%) | +1 |
| `python` | 10/10 (100%) | 10/10 (100%) | 0 |
| `go` | 10/10 (100%) | 10/10 (100%) | 0 |

## Per-prompt × language — diff

| Prompt | sigil run1 → run2 | python run1 → run2 | go run1 → run2 |
|---|---|---|---|
| **C01** — hello world | ✅ → ✅ | ✅ → ✅ | ✅ → ✅ |
| **C02** — sum 1 to 100 | ✅ → ✅ | ✅ → ✅ | ✅ → ✅ |
| **C03** — fibonacci(15) | ✅ → ✅ | ✅ → ✅ | ✅ → ✅ |
| **C04** — factorial(10) | ✅ → ✅ | ✅ → ✅ | ✅ → ✅ |
| **C05** — fizzbuzz 1 to 15 | ❌ → **✅** (gained) | ✅ → ✅ | ✅ → ✅ |
| **C06** — primality test for 29 | ✅ → **❌** (regressed) | ✅ → ✅ | ✅ → ✅ |
| **C07** — gcd(48, 18) | ❌ → ❌ | ✅ → ✅ | ✅ → ✅ |
| **C08** — count digits in 12345 | ❌ → ❌ | ✅ → ✅ | ✅ → ✅ |
| **C09** — max of a hardcoded list | ✅ → ✅ | ✅ → ✅ | ✅ → ✅ |
| **C10** — Collatz steps for 27 | ❌ → **✅** (gained) | ✅ → ✅ | ✅ → ✅ |

## Run 2 failure detail

### `C06` × `sigil` — compile

```
error[E0042]: `operator `/` (may abort with ArithError)` requires `ArithError` in the enclosing function's effect row
  --> /var/folders/1h/
```

### `C07` × `sigil` — compile

```
error[E0042]: `operator `/` (may abort with ArithError)` requires `ArithError` in the enclosing function's effect row
  --> /var/folders/1h/
```

### `C08` × `sigil` — compile

```
error[E0042]: `operator `/` (may abort with ArithError)` requires `ArithError` in the enclosing function's effect row
  --> /var/folders/1h/
```

## Observations

**Net effect of PR #132 spec teaching: sigil first-shot 6/10 → 7/10 (+1).** Mixed wins and a regression.

**Wins (FAIL → PASS):**

- **C05 (fizzbuzz)**: run 1 declared `![]` and hit E0042. Run 2 declared `![ArithError]` on the helper and `![IO, ArithError]` on `main` — exactly what the new §4.2 callout teaches.
- **C10 (Collatz)**: same pattern. Run 1 used `![]` everywhere; run 2 declared `![ArithError]` properly.

**Regression (PASS → FAIL):**

- **C06 (primality test)**: run 1 *passed* because that subagent's preamble explicitly quoted the §4.2 table and declared `![ArithError]` everywhere. Run 2 took a different path — tried to AVOID `%` by computing modulo as `n - (n/d) * d` — but `/` is ALSO listed in §4.2's may-abort row, so the workaround still fails E0042 with the row stuck at `![]`. **The spec teaching pushed the agent toward avoidance instead of declaration, and avoidance is harder.**

**Persistent failures (FAIL → FAIL):**

- **C07 (gcd)**: same `n - (n/b) * b` workaround as C06. Same `/` trap.
- **C08 (count digits)**: uses `n / 10` directly in a `![]`-rowed helper. Forgot or didn't realize `/` carries ArithError.

**Methodological insight from this re-run:**

The §4.2 callout works for the straightforward case (declare `![ArithError]`) but inadvertently encourages workarounds for cases where the LLM thinks it can dodge the row. The next round of spec polish should explicitly note that the `n - (n/d)*d` style modulo workaround does NOT eliminate the row requirement, since `/` is in the same may-abort table row as `%`. A simple inline note would catch this.

Comparing to the spec validation harness's N=10 runs (which target a different set of prompts but the same underlying ArithError rule), PR #132 should also flip P05/P07 toward pass-first-shot. Re-run the spec validation harness to measure that.
