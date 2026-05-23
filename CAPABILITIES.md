# Sigil Capabilities — Empirical Report

Empirical data from three independent test corpora exercising Sigil end-to-end through the LLM-authorship harness. All numbers below are from runs against `claude-opus-4-7`, `claude-sonnet-4-6`, and (where noted) `claude-haiku-4-5-20251001`. Each cell is `(passes) / (runs)`.

**First-pass** = compile + run + oracle match on the first sampled program. **Final-pass** = first-pass OR success after one edit-loop iteration where the previous attempt's compile/run failure is fed back to the model.

---

## 1. Spec validation prompts (P01–P62)

Source: `spec/validation-prompts.md`. Run: `spec/validation-results-20260509T234710.jsonl` (2026-05-09T23:47, 10 independent samples per cell). Total: **1,240 runs** (62 prompts × 2 models × 10 runs).

### 1.1 Aggregate

| Model | Runs | First-pass | Final-pass |
|---|---|---|---|
| `claude-opus-4-7` | 620 | **577 (93.1%)** | **619 (99.8%)** |
| `claude-sonnet-4-6` | 620 | **582 (93.9%)** | **620 (100.0%)** |

### 1.2 Failure modes

All 82 failed attempts (across both models, both attempts) were **compile-time failures**. Zero runtime failures.

```
P* failure breakdown: compile=82  runtime=0  timeout=0
```

### 1.3 Per-prompt: cells with <100% first-pass

| Cell | First-pass | Final-pass |
|---|---|---|
| P05 / opus | 2/10 | 10/10 |
| P05 / sonnet | 0/10 | 10/10 |
| P07 / opus | 4/10 | 10/10 |
| P07 / sonnet | 0/10 | 10/10 |
| P19 / opus | 0/10 | 10/10 |
| P19 / sonnet | 0/10 | 10/10 |
| P20 / opus | 9/10 | 9/10 |
| P28 / opus | 8/10 | 10/10 |
| P28 / sonnet | 4/10 | 10/10 |
| P29 / opus | 0/10 | 10/10 |
| P34 / sonnet | 8/10 | 10/10 |
| P51 / opus | 4/10 | 10/10 |

The remaining 110 cells (out of 124) are 10/10 first-pass.

Prompt themes for the cells above:
- P05 — parity check via mod and if/else
- P07 — safe divide with explicit divisor check
- P19 — State[Int]-based counter threaded through a list walk
- P20 — multi-shot Choose finds all (a, b) pairs with a + b == 7
- P28 — multi-arm handler with `std.state`
- P29 — nested handlers on distinct effects
- P34 — `ByteArray` checksum
- P51 — `std.random.run_seeded_random` deterministic

Single residual failure across all P* runs: P20 / opus, one of ten runs failed final-pass — Sigil's multi-shot Choose pattern remains the hardest construct.

---

## 2. Cross-language comparison (C01–C20)

Source: `comp/prompts.md`. Sigil prompts run against Python and Go for parity comparison on C01–C10.

### 2.1 C01–C10 cross-language (10 runs per cell)

Run: `comp/log/comparison-results-20260510T004245.jsonl` (2026-05-10T00:42).

**First-pass:**

| Prompt | Go opus | Go sonnet | Python opus | Python sonnet | Sigil opus | Sigil sonnet |
|---|---|---|---|---|---|---|
| C01 hello world | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C02 sum 1 to 100 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C03 fibonacci(15) | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C04 factorial(10) | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C05 fizzbuzz 1–15 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | 6/10 |
| C06 primality 29 | 10/10 | 10/10 | 10/10 | 10/10 | 1/10 | 1/10 |
| C07 gcd(48, 18) | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | 0/10 |
| C08 count digits | 10/10 | 10/10 | 10/10 | 10/10 | 9/10 | 0/10 |
| C09 max of list | 9/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C10 Collatz(27) | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | 2/10 |

**Final-pass (after one edit-loop):**

| Prompt | Go opus | Go sonnet | Python opus | Python sonnet | Sigil opus | Sigil sonnet |
|---|---|---|---|---|---|---|
| C01 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C02 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C03 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C04 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C05 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | 9/10 |
| C06 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C07 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C08 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C09 | 9/10 | 10/10 | 10/10 | 10/10 | **10/10** | **10/10** |
| C10 | 10/10 | 10/10 | 10/10 | 10/10 | **10/10** | 8/10 |

### 2.2 C01–C20 Sigil — cells under 100% first-pass (v1.1.0 baseline, 10 runs per cell)

Run: `comp/log/comparison-results-20260523T004621-baseline-full.jsonl` (v1.1.0 baseline, 2026-05-23). Dashboard: `comp/log/dashboard-20260523T004700-baseline.md`. All cells not listed below are 10/10 first-pass.

| Cell | First-pass | Final-pass |
|---|---|---|
| C05 / haiku | 4/10 | 9/10 |
| C06 / haiku | 8/10 | 8/10 |
| C09 / haiku | 9/10 | 9/10 |
| C09 / sonnet | 5/10 | 10/10 |
| C10 / haiku | 6/10 | 7/10 |
| C11 / haiku | 3/10 | 8/10 |
| C11 / sonnet | 9/10 | 10/10 |
| C12 / haiku | 9/10 | 10/10 |
| C14 / haiku | 9/10 | 9/10 |
| C15 / sonnet | 8/10 | 10/10 |
| C17 / haiku | 9/10 | 10/10 |
| C18 / opus | 8/10 | 10/10 |
| C19 / sonnet | 9/10 | 10/10 |
| C20 / haiku | 8/10 | 8/10 |
| C20 / sonnet | 9/10 | 9/10 |

C12 (parse invalid integer) — previously 0/3 on every cell under the 3-runs-per-cell sample — is now 10/10 on sonnet and opus and 9/10 on haiku first-pass. The intrinsic prelude (#191) and arith-trap (#192) changes together moved C-tier from "concentrated misses on a few prompts" to "near-saturation."

### 2.3 Sigil aggregate across all C* + H* runs (v1.1.0 baseline)

10 runs/cell × 25 prompts (C01–C20 + H01–H05) = 250 runs per model. Same source as §2.2.

| Model | Runs | First-pass | Final-pass |
|---|---|---|---|
| `claude-opus-4-7` | 250 | **245 (98.0%)** | **249 (99.6%)** |
| `claude-sonnet-4-6` | 250 | **225 (90.0%)** | **239 (95.6%)** |
| `claude-haiku-4-5` | 250 | **205 (82.0%)** | **232 (92.8%)** |

Headline deltas vs the pre-v1.1.0 surface (2026-05-10 run, smaller sample): opus first-pass +9.9pp, sonnet +25.6pp, haiku +40.3pp. The sonnet/haiku jumps are driven by two structural changes: the intrinsic prelude (#191) eliminated the bare-`int_to_string` cluster, and arith-trap (#192) eliminated the `effect-row-missing-arith` cluster (38.6% of pre-1.1 failures, now zero).

### 2.4 Failure modes (Sigil, C* runs)

```
Sigil failure breakdown: compile=134  stdout=1  total=135
```

**99.3% of Sigil failures are caught at compile time.** Of 135 failed attempts across all C* Sigil runs, only 1 reached the runtime as an oracle-mismatch; the other 134 never produced a binary the harness could execute.

---

## 3. H-tier prompts (H01–H05) — hard correctness

Source: `comp/prompts.md`. H-tier prompts target subtle correctness traps (stable sort tie-breaking, JSON number grammar, two-pass scoring, etc.).

### 3.1 H-tier first-pass / final-pass (v1.1.0 baseline, 10 runs per cell)

Source: same baseline run as §2.2/§2.3.

| Prompt | haiku-4-5 | sonnet-4-6 | opus-4-7 |
|---|---|---|---|
| H01 | 0/10 → 7/10 | 2/10 → 2/10 | 7/10 → 9/10 |
| H02 | 5/10 → 8/10 | 10/10 → 10/10 | 10/10 → 10/10 |
| H03 | 9/10 → 10/10 | 6/10 → 10/10 | 10/10 → 10/10 |
| H04 | 6/10 → 9/10 | 7/10 → 8/10 | 10/10 → 10/10 |
| H05 | 10/10 → 10/10 | 10/10 → 10/10 | 10/10 → 10/10 |

### 3.2 Notes on individual prompts

- **H05 (floor division)** was previously deferred as incompatible with Sigil's 63-bit `Int` (i64 overflow at the prompt's edge cases). The arith-trap change (#192) removed the elaborate-time perform rewrite for `/` and `%`; under the new direct-trap lowering the prompt is tractable for all three models — **10/10 first-pass and 10/10 final-pass across haiku, sonnet, and opus**.
- **H04 (stable sort with tie-breaking)** went from "PR #142 was needed to fix a codegen ICE that surfaced on the natural sonnet-authored program" to 10/10 on opus, 7/10→8/10 on sonnet, 6/10→9/10 on haiku.
- **H01** is currently the hardest H-tier — sonnet sits at 2/10 first/final-pass and is the next obvious investigation target.

---

## 4. Summary numbers

| Corpus | Sigil first-pass | Sigil final-pass | Failure shape |
|---|---|---|---|
| P01–P62 spec (1,240 runs) | 93.5% | 99.9% | 100% compile-time |
| C01–C20 + H01–H05 v1.1.0 baseline (750 Sigil runs across haiku + sonnet + opus, 10/cell) | 90.0% | 96.0% | ~99% compile-time |

**Across the 1,990 Sigil runs in this report (1,240 spec + 750 v1.1.0 corpus): ~92.2% first-pass, ~98.4% final-pass.** Failure shape is concentrated at compile time by design — Sigil's explicit types, mandatory effect rows, exhaustive matching, and lack of operator overloading move the error surface forward from runtime to typecheck.

The v1.1.0 surface (post arith-trap, intrinsic prelude, auto-CPS, qualified imports) moves opus close to corpus saturation (98.0% first-pass) and lifts haiku above 82% first-pass — a working LLM-authorship target for the smaller and faster model tier.

---

## 5. Cross-language failure-shape comparison

Aggregate across C01–C10 first-pass runs (60 runs per language per model, 240 runs per language total):

| Language | Pass rate | Failure shape on failures |
|---|---|---|
| Python | 100.0% | n/a (no failures) |
| Go | 99.5% | runtime (1 stdout mismatch in C09 opus) |
| Sigil | 76.0% | 99.3% compile-time |

The pass-rate gap (Sigil ~24 points below Python/Go on C01–C10) is concentrated at four prompts (C06 primality, C07 gcd, C08 count-digits, C10 Collatz, all sonnet) and resolves to ~96% with one edit-loop iteration. The failure-shape inversion is the load-bearing point: Python and Go ship the rare failure to runtime; Sigil ships every failure to compile time where it's caught before the program runs.

---

## 6. Methodology notes

- **Harness:** `comp/scripts/compare.py` (cross-language C*/H*) and `scripts/validate_spec.py` (P*) drive the Claude API with per-language system prompts (`comp/contexts/*.md`), extract the program from the response, compile + run + diff against oracle, and on first-shot failure feed the error back for one edit-loop iteration.
- **Independence:** each "run" is a fresh API session; runs in the same cell sample the underlying distribution.
- **Model snapshot:** all runs against models available as of 2026-05-10. Future model versions may shift the numbers; the corpus and harness are stable.
- **Oracle:** every prompt has an exact stdout + exit-code oracle. No fuzzy matching.
- **Source-of-truth files:** raw JSONL traces under `spec/` and `comp/log/`. Aggregations in `spec/validation-log.md` and `comp/log/comparison-log.md`.

---

## 7. Open holes the data exposes

- **Multi-shot Choose (P20)**: 1/124 final-pass miss across both models. The only structurally-different P* prompt that doesn't get all-green. Hardest single Sigil construct.
- **Sigil sonnet's C-tier gap**: C06–C08, C10 first-pass at 0–2/10 vs. opus 9–10/10. Sonnet under-utilizes Sigil's stdlib (`std.list`, `std.string`) and hits more E0042 effect-row errors. Edit-loop closes the gap.
- **C12, C20**: persistent final-pass 0/9 (C12) / 1–2/9 (C20). C12 (parse invalid integer) hits Sigil's stricter `Result[Int, ParseError]` API; C20 (postfix evaluator) requires multi-step stack semantics that LLMs consistently mis-translate. Candidates for spec-teaching / stdlib improvements.
- **H01–H03, H05 not run**: pending follow-up runs in future evaluation passes.
