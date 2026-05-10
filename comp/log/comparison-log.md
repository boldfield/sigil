# Cross-language comparison log — run 2026-05-10T12:48:10-0700

Trace: `comp/log/comparison-results-20260510T124810.jsonl`
Runs per (prompt, language, model): **3**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-opus-4-7` | 3/3 (100.0%) | 3/3 (100.0%) |
| `sigil` | `claude-sonnet-4-6` | 3/3 (100.0%) | 3/3 (100.0%) |
| `sigil` | `claude-haiku-4-5-20251001` | 2/3 (66.7%) | 2/3 (66.7%) |
| `python` | `claude-opus-4-7` | 3/3 (100.0%) | 3/3 (100.0%) |
| `python` | `claude-sonnet-4-6` | 3/3 (100.0%) | 3/3 (100.0%) |
| `python` | `claude-haiku-4-5-20251001` | 3/3 (100.0%) | 3/3 (100.0%) |
| `go` | `claude-opus-4-7` | 3/3 (100.0%) | 3/3 (100.0%) |
| `go` | `claude-sonnet-4-6` | 3/3 (100.0%) | 3/3 (100.0%) |
| `go` | `claude-haiku-4-5-20251001` | 3/3 (100.0%) | 3/3 (100.0%) |
| `rust` | `claude-opus-4-7` | 3/3 (100.0%) | 3/3 (100.0%) |
| `rust` | `claude-sonnet-4-6` | 3/3 (100.0%) | 3/3 (100.0%) |
| `rust` | `claude-haiku-4-5-20251001` | 3/3 (100.0%) | 3/3 (100.0%) |

## Per-prompt × language × model — first-pass

Cells: ✅ all runs passed; ⚠️ some runs passed (stochastic); ❌ all runs failed.

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` | `python` `claude-opus-4-7` | `python` `claude-sonnet-4-6` | `python` `claude-haiku-4-5-20251001` | `go` `claude-opus-4-7` | `go` `claude-sonnet-4-6` | `go` `claude-haiku-4-5-20251001` | `rust` `claude-opus-4-7` | `rust` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5-20251001` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **H03** — Right-associative power evaluator | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` | `python` `claude-opus-4-7` | `python` `claude-sonnet-4-6` | `python` `claude-haiku-4-5-20251001` | `go` `claude-opus-4-7` | `go` `claude-sonnet-4-6` | `go` `claude-haiku-4-5-20251001` | `rust` `claude-opus-4-7` | `rust` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5-20251001` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **H03** — Right-associative power evaluator | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile |
|---|---|
| `sigil` | 2 |
| `python` | 0 |
| `go` | 0 |
| `rust` | 0 |

## Failures (1 cell(s), 1 run(s))

### `H03` × `sigil` × `claude-haiku-4-5-20251001` — 1/3 runs failed

**Run 1:**
Final attempt category: **compile**

```
error[E0043]: wrong argument count at call site: expected 3, got 2
  --> string.sigil:165:18
error[E0043]: wrong argument count at call site: expected 3, got 2
  --> string.sigil:208:18
```

