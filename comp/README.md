# Cross-language LLM authorship comparison

Rough comparison harness for measuring LLM ability to produce working programs across **Sigil**, **Python**, and **Go**, given identical problem statements.

This is a **sketch** — the structural pieces (prompts, oracles, per-language eval drivers) work today. The Claude-API integration step is a stub. A more rigorous methodology is being developed separately; this directory captures the rough shape so the methodology work has something concrete to evolve.

## Thesis

Sigil is designed around LLM failure modes: explicit types, mandatory effect rows, no shadowing, exhaustive matching, no operator overloading. The hypothesis is that a fresh LLM session given **only the spec** can produce working programs at a rate competitive with the same model writing Python or Go from training-data familiarity.

The interesting metric is **after-one-edit pass rate** with attention to **error shape distribution** — Sigil should fail at compile time more often than at runtime; Python/Go should fail at runtime more often.

## Layout

```
comp/
  README.md                  this file
  prompts.md                 10 cross-language prompts; problem statements + oracles
  contexts/
    sigil.md                 system prompt prefix for Sigil sessions
    python.md                system prompt prefix for Python sessions
    go.md                    system prompt prefix for Go sessions
  scripts/
    eval-sigil.sh            compile + run + diff oracle for one Sigil program
    eval-python.sh           same shape for Python
    eval-go.sh               same shape for Go
    compare.sh               orchestrator: iterates (prompt × language), calls eval drivers
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

## Future surface (API integration)

`scripts/compare.sh` documents the Claude-API integration shape but currently exits with a stub message. To complete it:

1. Implement `claude_call(system_prompt, user_prompt) -> response_text` using `curl` against `api.anthropic.com/v1/messages` (matching the auth pattern that `validate-spec.sh` will use when Stage 9 Task 85 lands — see `scripts/validate-spec.sh` for the future shape).
2. Extract the program from the response (look for the first ```sigil/```python/```go fenced block).
3. Write to a temp file; pass to the appropriate eval driver.
4. On first-shot failure, send a second turn with the program + compile/run output and re-evaluate.
5. Append result to `log/<timestamp>.md`.

## Selection rationale

The 10 prompts (`C01`–`C10`) are chosen to:

- Compile cleanly in current Sigil **without** dependencies on queued plans (no `Char`, no `Env`/`Fs`/`Process`, no `Map`/`sort`, no `format`/`panic`).
- Have deterministic stdout (no input parsing, no time/random dependence).
- Span complexity from trivial (hello world) to moderate (fizzbuzz, Collatz steps).
- Avoid Sigil-specific idioms in the problem statement — the prompt body never mentions Sigil, Python, or Go. The runner attaches a language-specific system prompt.

This corpus is **biased** in two ways and the methodology work should fix both:

1. **Algorithm-only.** No I/O parsing, no string processing beyond concatenation, no data structures beyond integers and lists. A real comparison needs prompts that exercise stdlib breadth.
2. **No external benchmark.** Pulling from HumanEval / MBPP / BIG-Bench would defuse author bias. Stage 9 P01–P20 (and these C01–C10) were both written inside the Sigil project and skew toward Sigil's strengths.

## What success looks like

Per (prompt, language, model, run): record first-shot pass/fail, after-one-edit pass/fail, generated-program LOC, error category (compile / runtime / wrong output / timeout). Aggregate per language. Headline numbers:

- After-one-edit pass rate by language.
- First-shot pass rate by language.
- Mean error-category distribution by language.

If Sigil's after-one-edit pass rate matches Python's first-shot, the LLM-design hypothesis has empirical support.
