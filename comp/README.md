# Cross-language LLM authorship comparison

Rough comparison harness for measuring LLM ability to produce working programs across **Sigil**, **Python**, **Go**, and **Rust**, given identical problem statements.

This is a **sketch** — the structural pieces (prompts, oracles, per-language eval drivers) work today. Claude calls route through `claude -p` (Claude Code headless mode), so they bill against the user's Claude subscription rather than an API key. A more rigorous methodology is being developed separately; this directory captures the rough shape so the methodology work has something concrete to evolve.

## Thesis

Sigil is designed around LLM failure modes: explicit types, mandatory effect rows, no shadowing, exhaustive matching, no operator overloading. The hypothesis is that a fresh LLM session given **only the spec** can produce working programs at a rate competitive with the same model writing Python or Go from training-data familiarity.

The interesting metric is **after-one-edit pass rate** with attention to **error shape distribution** — Sigil should fail at compile time more often than at runtime; Python/Go should fail at runtime more often.

## Layout

```
comp/
  README.md                  this file
  prompts.md                 20 cross-language prompts; problem statements + oracles
  contexts/
    sigil.md                 system prompt prefix for Sigil sessions
    python.md                system prompt prefix for Python sessions
    go.md                    system prompt prefix for Go sessions
    rust.md                  system prompt prefix for Rust sessions
  scripts/
    eval-sigil.sh            compile + run + diff oracle for one Sigil program
    eval-python.sh           same shape for Python
    eval-go.sh               same shape for Go
    eval-rust.sh             same shape for Rust
    compare.py               full harness: per-cell `claude -p` + edit loop + report
    compare.sh               thin wrapper that invokes compare.py
  log/                       result logs (one file per run)
```

## How to run (manual)

The eval drivers work today. To smoke-test the harness end-to-end without Claude API integration, write a known-good program by hand and pass it through the eval driver:

```bash
# Sigil
echo 'fn main() -> Int ![IO] { perform IO.println("hello, world"); 0 }' > /tmp/c01.sigil
comp/scripts/eval-sigil.sh /tmp/c01.sigil C01

# Python
echo 'print("hello, world")' > /tmp/c01.py
comp/scripts/eval-python.sh /tmp/c01.py C01

# Go
cat > /tmp/c01.go <<EOF
package main
import "fmt"
func main() { fmt.Println("hello, world") }
EOF
comp/scripts/eval-go.sh /tmp/c01.go C01
```

Each driver prints `pass` or `fail: <reason>` and exits 0/1.

## Running the full comparison

`scripts/compare.py` (driven by `scripts/compare.sh`) implements the full Claude loop via `claude -p` (Claude Code headless mode). For each `(prompt × language × model × run)` it sends the prompt to Claude with the language-specific system context, extracts the program from the response's fenced code block, hands it to the matching `eval-<lang>.sh` driver, and records the result. On first-shot failure it sends a follow-up turn (via `--resume <session-id>`, so the system prompt isn't resent) with the eval driver's failure category for an after-one-edit retry.

**Auth:** Calls go through the Claude Code CLI, which inherits your subscription auth (`claude /login`) or a long-lived OAuth token (`claude setup-token`, then export `CLAUDE_CODE_OAUTH_TOKEN`). No `ANTHROPIC_API_KEY` is needed or used.

**Rate-limit reality:** Subscription access is metered by 5-hour and weekly windows, not per-request billing. Defaults are tuned for **grammar-change iteration** — Sigil only, Haiku + Sonnet. Two convenience flags expand scope:

- `--all-langs` adds Python/Go/Rust for cross-language thesis comparisons.
- `--full` adds Opus on top for before/after measurements.

A full `(20 prompts × 4 langs × 3 models × N runs × ≤2 turns)` matrix can hit the rate-limit caps, especially the Opus runs. Use `--filter` and `--no-edit-loop` for cheap iteration and reserve `--all-langs --full` K/N runs for the comparisons that need it.

```shell
# Prereq: Claude Code installed and authenticated.
cargo build --release        # sigil eval driver invokes target/release/sigil

./comp/scripts/compare.sh                                  # full bank, sigil only, Haiku+Sonnet
./comp/scripts/compare.sh --filter C01 --runs 3            # one prompt, K/N aggregation
./comp/scripts/compare.sh --langs sigil,python --models claude-opus-4-7
./comp/scripts/compare.sh --all-langs                      # cross-language baseline
./comp/scripts/compare.sh --all-langs --full --runs 5      # full thesis comparison
./comp/scripts/compare.sh --no-edit-loop                   # measure first-shot only
```

Outputs:
- `comp/log/comparison-results-<timestamp>.jsonl` — per-cell trace (raw response, extracted program, eval pass/category/detail). Local-only (gitignored).
- `comp/log/comparison-log.md` — markdown report: per-(language, model) pass-rate table + per-prompt × per-cell K/N matrix + **failure-category histogram per language** (the central thesis-relevant comparison).

See `scripts/compare.py --help` for the full CLI.

## Selection rationale

The 20 prompts (`C01`–`C20`) split into three difficulty tiers:

**Tier 1 — C01–C10 — basic algorithmic surface.** Trivial to moderate complexity (hello world to Collatz). Uses only basic surface (IO, Int, recursion, match/branching) common to all three target languages, so first-shot success doesn't hinge on stdlib breadth. The N=10 cross-language run showed Python and Go both pass 100% — these prompts establish a baseline calibration but don't surface Python/Go runtime fragility.

**Tier 2 — C11–C16 — runtime fragility stress tests.** Designed to surface the failure modes Sigil's compile-time discipline is meant to minimize: missing-key map lookups (C11), invalid-input parsing (C12), search-with-no-match (C13), index out of bounds (C14), integer-vs-float division (C15), uncaught divide-by-zero (C16). These prompts force each language's natural failure path: Python's exceptions (KeyError, ValueError, IndexError, ZeroDivisionError, StopIteration), Go's silent zero-values and panics, Sigil's typed Option/Result/handler chains. Most are textbook safe-pattern tests — modern LLMs handle them reliably; the value is calibrating the *thesis-relevant histogram* (compile-side vs runtime-side failures).

**Tier 3 — C17–C20 — algorithmic complexity (failure-prone).** Genuinely hard problems where LLMs are likely to make algorithmic mistakes that survive the first attempt: string reversal (C17), Roman numeral conversion with subtractive notation (C18), balanced-bracket validation requiring a real stack (C19), postfix expression evaluation requiring tokenization + stack + operator-operand-order correctness (C20). Common failure modes: wrong operand order for non-commutative operators, naive pair-counting for brackets, forgetting subtractive notation, byte-level vs character-level string operations. These prompts measure how often each language's failures show up as wrong-output (Sigil's worst case, Python/Go's natural failure mode) versus crash/exception.

Common across both groups:
- Have deterministic stdout (no input parsing, no time/random dependence).
- Avoid Sigil-specific idioms in the problem statement — the prompt body never mentions Sigil, Python, or Go. The runner attaches a language-specific system prompt.
- Sigil's `std.char`, `std.env`/`std.fs`/`std.process`, `std.map`/`list_sort_int`, `std.format`, `panic`/`assert` have all shipped (post Plan D); C11 in particular exercises `std.map`. The corpus deliberately doesn't go deeper into stdlib because the comparison is about authoring core idioms, not stdlib name recall.

### Sigil-specific traps to watch for

Several prompts (C05 fizzbuzz, C06 primality, C07 gcd, C08 digit count, C10 Collatz, C16 div-by-zero handling, C20 postfix evaluator) use the `%` (modulo) or `/` (division) operators. In Sigil, both require `ArithError` in the enclosing function's effect row (per spec §4.2 — the operators may abort with `ArithError.div_by_zero` / `mod_by_zero` and that effect must be discharged or propagated).

The N=10 spec validation harness data (P05/P07) and the comp/ N=10 run both showed LLMs reliably miss this on first attempt — they default to `![IO]` when the natural row is `![ArithError, IO]`. PRs #132–#134 progressively strengthened the spec teaching for this rule (§3.3 early callout + §4.2 prominent table row + dodge-warning + canonical example). Re-runs measure whether the teaching took.

This isn't a bug in the prompts or the language — it's a measurable spec-teachability data point.

### Bias caveats

This corpus is **biased** in two ways and the methodology work should fix both:

1. **Algorithm-narrow.** No structured I/O parsing, no string processing beyond concatenation, no data structures beyond integers, lists, and basic maps. C11–C16 added some runtime-fragility surface but the corpus still doesn't exercise stdlib breadth or non-algorithmic patterns. A real comparison needs more.
2. **No external benchmark.** Pulling from HumanEval / MBPP / BIG-Bench would defuse author bias. Stage 9 P01–P20 (and these C01–C20) were both written inside the Sigil project and skew toward Sigil's strengths.

## What success looks like

Per (prompt, language, model, run): record first-shot pass/fail, after-one-edit pass/fail, generated-program LOC, error category (compile / runtime / wrong output / timeout). Aggregate per language. Headline numbers:

- After-one-edit pass rate by language.
- First-shot pass rate by language.
- Mean error-category distribution by language.

If Sigil's after-one-edit pass rate matches Python's first-shot, the LLM-design hypothesis has empirical support.
