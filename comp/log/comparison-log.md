# Cross-language comparison log — run 2026-05-10T13:50:37-0700

Trace: `comp/log/comparison-results-20260510T135037.jsonl`
Runs per (prompt, language, model): **3**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-opus-4-7` | 11/12 (91.7%) | 12/12 (100.0%) |
| `sigil` | `claude-sonnet-4-6` | 8/12 (66.7%) | 10/12 (83.3%) |
| `sigil` | `claude-haiku-4-5-20251001` | 3/12 (25.0%) | 6/12 (50.0%) |
| `python` | `claude-opus-4-7` | 12/12 (100.0%) | 12/12 (100.0%) |
| `python` | `claude-sonnet-4-6` | 12/12 (100.0%) | 12/12 (100.0%) |
| `python` | `claude-haiku-4-5-20251001` | 11/12 (91.7%) | 12/12 (100.0%) |
| `go` | `claude-opus-4-7` | 12/12 (100.0%) | 12/12 (100.0%) |
| `go` | `claude-sonnet-4-6` | 12/12 (100.0%) | 12/12 (100.0%) |
| `go` | `claude-haiku-4-5-20251001` | 12/12 (100.0%) | 12/12 (100.0%) |
| `rust` | `claude-opus-4-7` | 11/12 (91.7%) | 12/12 (100.0%) |
| `rust` | `claude-sonnet-4-6` | 11/12 (91.7%) | 12/12 (100.0%) |
| `rust` | `claude-haiku-4-5-20251001` | 12/12 (100.0%) | 12/12 (100.0%) |

## Per-prompt × language × model — first-pass

Cells: ✅ all runs passed; ⚠️ some runs passed (stochastic); ❌ all runs failed.

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` | `python` `claude-opus-4-7` | `python` `claude-sonnet-4-6` | `python` `claude-haiku-4-5-20251001` | `go` `claude-opus-4-7` | `go` `claude-sonnet-4-6` | `go` `claude-haiku-4-5-20251001` | `rust` `claude-opus-4-7` | `rust` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5-20251001` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **H02** — JSON number validator | ✅ 3/3 | ✅ 3/3 | ⚠️ 1/3 | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 |
| **H03** — Right-associative power evaluator | ✅ 3/3 | ⚠️ 2/3 | ❌ 0/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **H04** — Stable sort with tie-breaking | ⚠️ 2/3 | ❌ 0/3 | ❌ 0/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **H05** — Floor division (round toward negative infinity) | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` | `python` `claude-opus-4-7` | `python` `claude-sonnet-4-6` | `python` `claude-haiku-4-5-20251001` | `go` `claude-opus-4-7` | `go` `claude-sonnet-4-6` | `go` `claude-haiku-4-5-20251001` | `rust` `claude-opus-4-7` | `rust` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5-20251001` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **H02** — JSON number validator | ✅ 3/3 | ✅ 3/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **H03** — Right-associative power evaluator | ✅ 3/3 | ✅ 3/3 | ⚠️ 1/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **H04** — Stable sort with tie-breaking | ✅ 3/3 | ⚠️ 1/3 | ❌ 0/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |
| **H05** — Floor division (round toward negative infinity) | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile | runtime | stdout |
|---|---|---|---|
| `sigil` | 17 | 2 | 3 |
| `python` | 0 | 0 | 1 |
| `go` | 0 | 0 | 0 |
| `rust` | 2 | 0 | 0 |

## Failures (4 cell(s), 8 run(s))

### `H02` × `sigil` × `claude-haiku-4-5-20251001` — 1/3 runs failed

**Run 2:**
Final attempt category: **compile**

```
error[E0112]: unknown type `List` (expected a primitive, a type declared via `type List = ...`, or an in-scope generic parameter)
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H02-sigil-j3y3hb4i/program.sigil:39:25
  = hint: `List` is declared in `std.list` — add `import std.list` at the top of the file
error[E0112]: unknown type `List` (expected a primitive, a ty
```

### `H03` × `sigil` × `claude-haiku-4-5-20251001` — 2/3 runs failed

**Run 0:**
Final attempt category: **runtime**

```
exit 1 (expected 0)
```

**Run 1:**
Final attempt category: **runtime**

```
exit 1 (expected 0)
```

### `H04` × `sigil` × `claude-haiku-4-5-20251001` — 3/3 runs failed

**Run 0:**
Final attempt category: **compile**

```
error[E0010]: expected `)`
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-1d6m_x_y/program.sigil:33:22
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-1d6m_x_y/program.sigil:37:3
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/fol
```

**Run 1:**
Final attempt category: **compile**

```
error[E0010]: expected `)`
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-6vcc93yf/program.sigil:7:22
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-6vcc93yf/program.sigil:10:3
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/fold
```

**Run 2:**
Final attempt category: **compile**

```
error[E0010]: expected `)`
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-k6ezdwsr/program.sigil:33:16
error[E0010]: expected `import`, `fn`, `type`, or `effect` at top level
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp-H04-sigil-k6ezdwsr/program.sigil:34:1
error[E0010]: expected `)`
  --> /var/folders/1h/63kx45_157q098yxtbtncq540000gn/T/comp
```

### `H04` × `sigil` × `claude-sonnet-4-6` — 2/3 runs failed

**Run 1:**
Final attempt category: **stdout**

```
output differs from oracle
```

**Run 2:**
Final attempt category: **stdout**

```
output differs from oracle
```

