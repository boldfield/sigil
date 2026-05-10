# Cross-language comparison log — run 2026-05-10T11:45:18-0700

Trace: `comp/log/comparison-results-20260510T114518.jsonl`
Runs per (prompt, language, model): **3**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-opus-4-7` | 5/6 (83.3%) | 6/6 (100.0%) |
| `sigil` | `claude-sonnet-4-6` | 4/6 (66.7%) | 6/6 (100.0%) |
| `sigil` | `claude-haiku-4-5-20251001` | 2/6 (33.3%) | 3/6 (50.0%) |

## Per-prompt × language × model — first-pass

Cells: ✅ all runs passed; ⚠️ some runs passed (stochastic); ❌ all runs failed.

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` |
|---|---|---|---|
| **C12** — parse invalid integer | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 |
| **C20** — postfix expression evaluator | ⚠️ 2/3 | ⚠️ 1/3 | ❌ 0/3 |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` |
|---|---|---|---|
| **C12** — parse invalid integer | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **C20** — postfix expression evaluator | ✅ 3/3 | ✅ 3/3 | ❌ 0/3 |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile |
|---|---|
| `sigil` | 10 |

## Failures (1 cell(s), 3 run(s))

### `C20` × `sigil` × `claude-haiku-4-5-20251001` — 3/3 runs failed

**Run 0:**
Final attempt category: **compile**

```
error[E0010]: expected pattern (literal, `_`, identifier, constructor, or tuple)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-9z9ln2ic/program.sigil:12:9
error[E0010]: expected pattern (literal, `_`, identifier, constructor, or tuple)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-9z9ln2ic/program.sigil:16:9
error[E0010]: expected `
```

**Run 1:**
Final attempt category: **compile**

```
error[E0010]: expected pattern (literal, `_`, identifier, constructor, or tuple)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-2_x9slqs/program.sigil:13:9
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-2_x9slqs/program.sigil:16:9
error[E0010]: expected `import`,
```

**Run 2:**
Final attempt category: **compile**

```
error[E0010]: expected pattern (literal, `_`, identifier, constructor, or tuple)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-j9fwtue0/program.sigil:20:9
error[E0010]: expected pattern (literal, `_`, identifier, constructor, or tuple)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-C20-sigil-j9fwtue0/program.sigil:24:9
error[E0010]: expected `
```

