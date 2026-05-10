# Cross-language comparison log — run 2026-05-10T12:22:32-0700

Trace: `comp/log/comparison-results-20260510T122232.jsonl`
Runs per (prompt, language, model): **3**

## Pass rates by language × model

| Language | Model | First-pass | Final-pass |
|---|---|---|---|
| `sigil` | `claude-opus-4-7` | 2/3 (66.7%) | 3/3 (100.0%) |
| `sigil` | `claude-sonnet-4-6` | 1/3 (33.3%) | 1/3 (33.3%) |
| `sigil` | `claude-haiku-4-5-20251001` | 0/3 (0.0%) | 2/3 (66.7%) |
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
| **H01** — Wordle scoring | ⚠️ 2/3 | ⚠️ 1/3 | ❌ 0/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |

## Per-prompt × language × model — final-pass (first OR after edit)

| Prompt | `sigil` `claude-opus-4-7` | `sigil` `claude-sonnet-4-6` | `sigil` `claude-haiku-4-5-20251001` | `python` `claude-opus-4-7` | `python` `claude-sonnet-4-6` | `python` `claude-haiku-4-5-20251001` | `go` `claude-opus-4-7` | `go` `claude-sonnet-4-6` | `go` `claude-haiku-4-5-20251001` | `rust` `claude-opus-4-7` | `rust` `claude-sonnet-4-6` | `rust` `claude-haiku-4-5-20251001` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| **H01** — Wordle scoring | ✅ 3/3 | ⚠️ 1/3 | ⚠️ 2/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 | ✅ 3/3 |

## Failure-category histogram

Counts every failed attempt (first OR edit), by language. Reveals whether each language fails compile-side or runtime-side dominantly.

| Language | compile |
|---|---|
| `sigil` | 9 |
| `python` | 0 |
| `go` | 0 |
| `rust` | 0 |

## Failures (2 cell(s), 3 run(s))

### `H01` × `sigil` × `claude-haiku-4-5-20251001` — 1/3 runs failed

**Run 1:**
Final attempt category: **compile**

```
sigil: codegen failed: define wordle_score: Compilation(
    Verifier(
        VerifierErrors(
            [
                VerifierError {
                    location: inst3,
                    context: Some(
                        "v7 = call fn8(v5, v6)  ; v5 = 5, v6 = 0",
                    ),
                    message: "arg 1 (v6) has type i8, expected i64",
```

### `H01` × `sigil` × `claude-sonnet-4-6` — 2/3 runs failed

**Run 1:**
Final attempt category: **compile**

```
sigil: codegen failed: define make_bool_array: Compilation(
    Verifier(
        VerifierErrors(
            [
                VerifierError {
                    location: inst1,
                    context: Some(
                        "v4 = call fn8(v1, v3)  ; v3 = 0",
                    ),
                    message: "arg 1 (v3) has type i8, expected i64",
```

**Run 2:**
Final attempt category: **compile**

```
sigil: codegen failed: define make_bool_array: Compilation(
    Verifier(
        VerifierErrors(
            [
                VerifierError {
                    location: inst1,
                    context: Some(
                        "v4 = call fn8(v1, v3)  ; v3 = 0",
                    ),
                    message: "arg 1 (v3) has type i8, expected i64",
```

