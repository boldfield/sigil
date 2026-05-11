# Cross-language comparison log — run 2026-05-10T17:11:13-0700

Trace: `comp/log/comparison-results-20260510T171113.jsonl`
Runs per (prompt, language, model): **3**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-opus-4-7` | 3/3 (100.0%) | 3/3 (100.0%) |
| `sigil` | `claude-sonnet-4-6` | 2/3 (66.7%) | 3/3 (100.0%) |

## Per-prompt × language × model — first-pass

Cells: ✅ all runs passed; ⚠️ some runs passed (stochastic); ❌ all runs failed.

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` |
|---|---|---|
| **H04** — Stable sort with tie-breaking | ✅ 3/3 | ⚠️ 2/3 |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` |
|---|---|---|
| **H04** — Stable sort with tie-breaking | ✅ 3/3 | ✅ 3/3 |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile |
|---|---|
| `sigil` | 1 |

