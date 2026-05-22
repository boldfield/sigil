# First-pass-success dashboard — design

**Status:** approved, ready for implementation planning
**Date:** 2026-05-17
**Scope:** measurement methodology + tooling for tracking and attributing first-pass and final-pass success of LLM-authored Sigil programs on the `comp/` corpus.

## Goal

Build a permanent measurement dashboard for first-pass and final-pass
success on the corpus, with enough fault attribution (per-E-code
histograms, hand-curated semantic clusters, per-attempt diffs for
after-edit failures) to drive intervention picks across four levers:

1. Sigil context prompt (`comp/contexts/sigil.md`)
2. Spec teaching (`spec/language.md`)
3. Stdlib additions
4. Language / runtime changes

Sample sizes are tiered:

- **Baseline:** K=10 runs per cell. Authoritative measurement after each
  intervention. Up to 1000 LLM calls per run (25 prompts × 2 models ×
  10 runs × ≤2 turns).
- **Iteration:** K=5 runs per cell. Cheap spot-checks between
  interventions. Up to 500 calls per run.

Models: `claude-haiku-4-5` + `claude-sonnet-4-6`. Sigil only.

## Non-goals

- Cross-language comparisons (the existing `--all-langs` path stays
  untouched).
- Opus in the dashboard (rate-limit cost outweighs the marginal data
  given Opus is expected to be near-saturated on first-pass already).
- Web UI, CI integration, scheduled runs, automatic intervention
  suggestions.
- Cumulative trend tracking across runs (separate spec later if
  needed).
- Changes to eval drivers, prompts, or `claude -p` plumbing.

## Why this scope

Without a stable measurement layer, intervention picks across the
four levers compete on vibes. The dashboard makes every intervention
attributable: each one can be scored against a prior K=10 baseline
before being shipped. This is the methodology investment that makes
subsequent levers (which are all on the table — context, spec, stdlib,
language) worth the effort.

## Architecture

Three pieces, clean separation:

### `comp/scripts/compare.py` — minimal changes

Adds two new flags:

- `--tier baseline` — sets `--runs 10` unless `--runs N` was passed
  explicitly.
- `--tier iteration` — sets `--runs 5` unless `--runs N` was passed
  explicitly.

The tier label gets written into both the output filename
(`comparison-results-<ts>-<tier>.jsonl`) and a new `tier` field on each
JSONL row. Everything else — `claude -p` invocation, edit-loop logic,
eval driver wiring, filtering — stays as-is.

### `comp/scripts/clusters.py` — new module

Defines the hand-curated cluster taxonomy as a list of
`(cluster_id, matcher, suggested_lever)` tuples where `matcher` is
either a compiled regex against the failure detail/stderr or a callable
receiving the JSONL row. First-match wins; unmatched failures get
bucketed as `uncategorized`. Imported only by `dashboard.py`. Zero
runtime dependency from `compare.py`.

### `comp/scripts/dashboard.py` — new script

Reads one or more JSONL trace files (defaults to the latest, accepts
`--trace path` or `--latest-tier baseline|iteration`), applies the
cluster taxonomy, and emits a single markdown file at
`comp/log/dashboard-<source-ts>-<tier>.md`. Pure analysis — never calls
Claude, never writes to JSONL. Re-running it is free.

### Lifecycle

- Measurement: `comp/scripts/compare.sh --tier baseline` (rare,
  expensive).
- Re-render: `python comp/scripts/dashboard.py --latest-tier baseline`
  (cheap, fast).

The existing lightweight `comparison-log.md` aggregator inside
`compare.py` stays — it's the "did the run finish, what's the topline"
artifact. The new `dashboard-*.md` is the rich attribution artifact.
They serve different needs.

## Data contract

The existing per-row JSONL shape already carries everything the
dashboard needs for E-code extraction (parse `eval_detail`), cluster
tagging (apply matchers from `clusters.py`), and per-attempt diff (diff
`first_attempt.program` vs `edit_attempt.program`). All three are
derived at dashboard time. No retroactive schema breakage.

Two small additions to each JSONL row, written by `compare.py`:

- **`tier`** — `"baseline"`, `"iteration"`, or `null` for ad-hoc runs.
  Lets the dashboard pick the most recent baseline trace without
  parsing filenames.
- **`corpus_version`** — git short-sha at the time of the run, plus a
  SHA-256 of `comp/contexts/sigil.md` and `spec/language.md`
  concatenated. Lets the dashboard refuse (or warn) when asked to mix
  runs across different teaching-material versions.

Filename stays `comparison-results-<ts>-<tier>.jsonl` for human
readability; the embedded fields are what the dashboard reads.

No changes to `first_attempt` / `edit_attempt` substructure. The
`eval_raw_output` already carries the full error text the matchers
need.

## Cluster taxonomy — seed list

Each cluster is `(cluster_id, matcher, suggested_lever)`. Matchers run
against `eval_category + eval_detail + eval_raw_output` concatenated;
first match wins. The `suggested_lever` annotation is shown in the
dashboard so the cluster histogram directly suggests where the fix
goes.

| cluster_id | matcher (regex on detail) | suggested_lever |
|---|---|---|
| `effect-row-missing-arith` | `E0042.*requires.*ArithError.*effect row` | spec/context |
| `effect-row-missing-io` | `E0042.*requires.*IO.*effect row` | spec/context |
| `effect-row-missing-other` | `E0042.*requires.*effect row` | spec/context |
| `bare-name-ambiguous` | `E0147` | language (qualified call syntax, queued v2) |
| `field-access-missing` | `E0151` | language (field-access operator, queued v2) |
| `mut-array-element-type` | `E0150.*MutArray` | language (v2 type-arg threading) |
| `unknown-import-or-name` | `E0\d{3}.*(unknown name\|no item named\|unresolved)` | context (or stdlib add) |
| `match-non-exhaustive` | `E0\d{3}.*non-exhaustive` | spec/context |
| `syntax-parse-error` | `E0010\|expected an expression\|expected.*token` | investigate (usually points elsewhere) |
| `compile-other` | any compile failure not above | uncategorized → triage |
| `wrong-output` | `eval_category == "wrong_output"` | spec/prompt (algorithmic) |
| `runtime-panic` | `eval_category == "runtime"` | spec/teaching or language |
| `timeout` | `eval_category == "timeout"` | algorithmic / runtime |

The seed exists so the first dashboard run produces meaningful clusters
from existing JSONLs. The list is expected to grow as `uncategorized`
and `compile-other` review surfaces new patterns — that's a deliberate
maintenance loop, not a one-time setup. The matchers live in
`comp/scripts/clusters.py` as a single Python list; adding a cluster is
one PR-friendly diff.

## Dashboard markdown layout

Sections in order, scaled by signal density:

1. **Header** — run timestamp, source trace path(s), tier, model list,
   K, `corpus_version` sha. Plus a single warning line if mixing traces
   across `corpus_version`s.

2. **Topline pass rates** — current table format (model × first-pass ×
   final-pass with K/N). Unchanged from existing.

3. **Cluster histogram** — *the headline attribution view.* One row per
   cluster, columns: count, % of all failures, models affected,
   suggested lever. Sorted by count desc. `uncategorized` and
   `compile-other` highlighted at top regardless of count (they're the
   maintenance queue).

4. **Per-E-code histogram** — flat table: E-code, count, brief
   description (pulled from `compiler/src/errors/catalog.rs` at render
   time if available, or hardcoded short labels initially). Sorted by
   count desc. Drill-down complement to clusters.

5. **Per-prompt × model first-pass and final-pass tables** — current
   layout. With K>1, the cells become K/N fractions (e.g. `7/10`)
   instead of ✅/❌.

6. **Failure detail, grouped by cluster** — for each cluster, list the
   failing (prompt, model, run_idx) cells; for each, show 6-line
   eval_detail snippet. Collapsed by default in markdown via
   `<details>` blocks per cluster.

7. **After-edit failure diffs** — only for cells where
   `final_pass == false` and both `first_attempt` and `edit_attempt`
   exist. Unified diff between the two programs plus the second
   failure's eval_detail. This is the rarest, most expensive-to-debug
   bucket; surface it prominently.

## Run mechanics

Filenames (all under `comp/log/`, all gitignored — same as today):

- `comparison-results-<ts>-<tier>.jsonl` — raw trace, written by
  `compare.py`.
- `comparison-log-<ts>-<tier>.md` — lightweight topline report, written
  by `compare.py` (existing behavior, renamed to include tier).
- `dashboard-<source-ts>-<tier>.md` — rich attribution report, written
  by `dashboard.py`. Re-runnable; overwrites if invoked with same
  source.

Invocation:

```sh
./comp/scripts/compare.sh --tier baseline           # K=10, full corpus, sigil only, Haiku+Sonnet
./comp/scripts/compare.sh --tier iteration          # K=5, same scope
python comp/scripts/dashboard.py --latest-tier baseline   # render against newest baseline trace
python comp/scripts/dashboard.py --trace comp/log/comparison-results-….jsonl
```

`--tier` is the only new flag on `compare.py`. Existing flags
(`--filter`, `--no-edit-loop`, `--runs`, `--models`, `--all-langs`,
`--full`) all still work; passing `--runs N` explicitly overrides the
tier-implied K.

## Validation criteria

How we know the dashboard build itself is done:

1. `compare.py --tier baseline --filter C01` finishes, writes a
   tier-tagged JSONL with `tier`+`corpus_version` populated, no
   regressions on the existing comparison-log output.
2. `dashboard.py --latest-tier baseline` against that single-cell trace
   renders a markdown file with all seven sections populated (sections
   that have no data say "no failures in this run" rather than crashing
   or rendering empty).
3. Re-running `dashboard.py` against an old JSONL (one written before
   this work) succeeds — missing `tier`/`corpus_version` produce a
   header warning, not a crash.
4. Running `dashboard.py` against the existing
   `comparison-results-20260517T091419.jsonl` reproduces the current
   pass-rate table and shows non-zero counts in
   `effect-row-missing-arith`, `mut-array-element-type`, and
   `syntax-parse-error` clusters (those are the three published
   failures).
5. Cluster taxonomy edits → re-render is < 5 seconds on the full
   historical trace.

## Out of scope (parking lot)

Not commitments — fine to add later as separate specs:

- Scheduled / CI runs
- Cross-run trend pages (cumulative dashboard over all historical
  JSONLs)
- Opus in the dashboard
- Web UI
- Automatic intervention suggestions
- Taxonomy auto-generation

## Open questions

None at design time. Open questions during implementation should be
raised in the plan, not deferred.
