# First-pass-success dashboard — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a permanent measurement dashboard over `comp/`-corpus JSONL traces with per-E-code histograms, hand-curated semantic clusters, and per-attempt diffs for after-edit failures. Plumb `--tier baseline|iteration` and a `corpus_version` field through `compare.py` so dashboards can refuse to mix runs across teaching-material versions.

**Architecture:** Two-stage pipeline. `compare.py` (existing) keeps driving `claude -p` calls and writing JSONL; it gains `--tier`, a `tier` field on each row, and a `corpus_version` field. `dashboard.py` (new) is a pure-analysis renderer that loads JSONL traces, applies cluster matchers from `clusters.py` (new), and emits a single rich markdown report. Re-rendering is free — `compare.py` runs are rare and expensive; `dashboard.py` runs are cheap and frequent.

**Tech Stack:** Python 3 (existing harness language), pytest 9.x (already installed), stdlib only. No new runtime dependencies.

**Spec:** `docs/superpowers/specs/2026-05-17-first-pass-success-dashboard-design.md`

---

## File structure

**Create:**

- `comp/scripts/clusters.py` — cluster taxonomy + matcher engine. Pure data + small classifier function. Imported only by `dashboard.py`.
- `comp/scripts/dashboard.py` — dashboard renderer CLI. Loads one or more JSONL traces, applies cluster taxonomy, writes a single markdown file.
- `comp/tests/conftest.py` — pytest path setup so tests can import sibling scripts.
- `comp/tests/test_clusters.py` — unit tests for matcher engine + each seed cluster.
- `comp/tests/test_dashboard.py` — integration tests that build synthetic JSONL fixtures and check rendered markdown.
- `comp/tests/test_compare_tier.py` — tests for the new `--tier` flag and `corpus_version` computation in `compare.py`.

**Modify:**

- `comp/scripts/compare.py` — add `--tier` flag, `corpus_version` helper, two new fields on each JSONL row, tier suffix on the JSONL filename, tier in `_build_run_slug`. Touches: `main()` (argparse + post-processing), `write_jsonl()` (new fields), `_build_run_slug()` (new param), top-level helpers (one new function).
- `comp/README.md` — document `--tier` and the dashboard tool in the "Running the full comparison" section.

**Untouched (by design):**

- `comp/scripts/eval-*.sh` — eval drivers.
- `comp/scripts/compare.sh` — wrapper passes args through.
- `comp/contexts/*.md`, `comp/prompts.md` — content lives elsewhere.
- All existing JSONL traces in `comp/log/` — schema additions are append-only; missing fields surface as `None`.

---

## Conventions used in this plan

- All commands run from the repo root unless stated otherwise.
- Tests use pytest. Test discovery: `python3 -m pytest comp/tests/ -v`.
- TDD cycle: write failing test → run to confirm failure → implement minimal code → run to confirm pass → commit.
- Commits use the existing repo style: `[comp] <imperative summary>` for harness/corpus changes.
- Every commit gets `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

---

## Task 1: `corpus_version` helper + `--tier` flag in compare.py

**Files:**
- Create: `comp/tests/conftest.py`
- Create: `comp/tests/test_compare_tier.py`
- Modify: `comp/scripts/compare.py` — add `compute_corpus_version()`, add `--tier` arg, post-process `--runs` against tier, write `tier` and `corpus_version` into each JSONL row, append tier to JSONL filename, pass tier into `_build_run_slug`.

- [ ] **Step 1: Create the test-path conftest**

```python
# comp/tests/conftest.py
"""Pytest path setup so tests can import sibling scripts directly."""
from __future__ import annotations

import sys
from pathlib import Path

# comp/scripts/ contains free-standing scripts (compare.py, dashboard.py,
# clusters.py). They're invoked as scripts, not imported as a package, so
# we add comp/scripts/ to sys.path and import top-level (e.g. `import
# clusters`). Mirrors how dashboard.py imports clusters at runtime.
COMP_SCRIPTS = Path(__file__).resolve().parent.parent / "scripts"
if str(COMP_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(COMP_SCRIPTS))
```

- [ ] **Step 2: Write failing tests for `compute_corpus_version` and `--tier` plumbing**

```python
# comp/tests/test_compare_tier.py
"""Tests for the --tier flag and corpus_version computation in compare.py."""
from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path

import pytest

import compare  # type: ignore


def test_compute_corpus_version_returns_sha_and_hash(tmp_path, monkeypatch):
    sigil_md = tmp_path / "sigil.md"
    spec_md = tmp_path / "language.md"
    sigil_md.write_text("sigil context body\n")
    spec_md.write_text("spec body\n")
    monkeypatch.setattr(compare, "SIGIL_CONTEXT_PATH", sigil_md)
    monkeypatch.setattr(compare, "SPEC_PATH", spec_md)

    cv = compare.compute_corpus_version()

    assert set(cv.keys()) == {"git_sha", "teaching_hash"}
    assert isinstance(cv["git_sha"], str)
    expected_hash = hashlib.sha256(
        sigil_md.read_bytes() + spec_md.read_bytes()
    ).hexdigest()
    assert cv["teaching_hash"] == expected_hash


def test_compute_corpus_version_handles_missing_git(monkeypatch):
    def fake_run(*args, **kwargs):
        raise FileNotFoundError("git not on PATH")

    monkeypatch.setattr(subprocess, "run", fake_run)
    cv = compare.compute_corpus_version()
    assert cv["git_sha"] == "unknown"


def test_tier_baseline_implies_runs_10():
    # `--tier baseline` with no explicit --runs should yield runs=10.
    resolved = compare.resolve_runs(tier="baseline", explicit_runs=None)
    assert resolved == 10


def test_tier_iteration_implies_runs_5():
    resolved = compare.resolve_runs(tier="iteration", explicit_runs=None)
    assert resolved == 5


def test_explicit_runs_overrides_tier():
    # User passed --runs 3 alongside --tier baseline; explicit wins.
    resolved = compare.resolve_runs(tier="baseline", explicit_runs=3)
    assert resolved == 3


def test_no_tier_defaults_to_runs_1():
    resolved = compare.resolve_runs(tier=None, explicit_runs=None)
    assert resolved == 1


def test_write_jsonl_emits_tier_and_corpus_version(tmp_path):
    out = tmp_path / "trace.jsonl"
    results = [
        compare.CellResult(
            prompt_id="C01",
            language="sigil",
            model="claude-haiku-4-5",
            run_idx=0,
            first_attempt=None,
            edit_attempt=None,
            final_pass=False,
            error=None,
        )
    ]
    corpus_version = {"git_sha": "abc1234", "teaching_hash": "deadbeef"}
    compare.write_jsonl(results, out, tier="baseline", corpus_version=corpus_version)
    rows = [json.loads(line) for line in out.read_text().splitlines()]
    assert len(rows) == 1
    assert rows[0]["tier"] == "baseline"
    assert rows[0]["corpus_version"] == corpus_version
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `python3 -m pytest comp/tests/test_compare_tier.py -v`
Expected: every test fails with `AttributeError` (the functions don't exist yet) or `TypeError` (write_jsonl doesn't take `tier`/`corpus_version`).

- [ ] **Step 4: Add `SIGIL_CONTEXT_PATH` constant**

In `comp/scripts/compare.py`, near the other path constants (just after `EVAL_SCRIPTS_DIR = COMP_DIR / "scripts"` around line 80, exact line: locate `LOG_DIR = COMP_DIR / "log"` and add the new constant directly below it):

```python
SIGIL_CONTEXT_PATH = CONTEXTS_DIR / "sigil.md"
```

- [ ] **Step 5: Add `compute_corpus_version()` helper**

In `comp/scripts/compare.py`, add this function just above the existing `def parse_prompts(...)` (around line 115). The function reads `SIGIL_CONTEXT_PATH` and `SPEC_PATH` from module globals so the test can monkeypatch them.

```python
def compute_corpus_version() -> dict[str, str]:
    """Return {git_sha, teaching_hash} identifying the teaching-material
    state at the time of a run. teaching_hash is SHA-256 over the
    concatenation of comp/contexts/sigil.md and spec/language.md; lets
    the dashboard refuse to mix runs across versions that change either."""
    try:
        proc = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            cwd=REPO_ROOT, capture_output=True, text=True, check=False,
        )
        git_sha = proc.stdout.strip() if proc.returncode == 0 else "unknown"
    except (FileNotFoundError, OSError):
        git_sha = "unknown"
    try:
        body = SIGIL_CONTEXT_PATH.read_bytes() + SPEC_PATH.read_bytes()
        teaching_hash = hashlib.sha256(body).hexdigest()
    except FileNotFoundError:
        teaching_hash = "unknown"
    return {"git_sha": git_sha, "teaching_hash": teaching_hash}
```

Add `import hashlib` to the import block near the top of the file (alphabetical placement: between `import dataclasses` and `import json`).

- [ ] **Step 6: Add `resolve_runs()` helper**

In `comp/scripts/compare.py`, add just below `compute_corpus_version()`:

```python
def resolve_runs(*, tier: Optional[str], explicit_runs: Optional[int]) -> int:
    """Map tier → runs. Explicit --runs always wins. Tier defaults:
    baseline=10, iteration=5. No tier and no --runs → 1 (single-cell ad-hoc)."""
    if explicit_runs is not None:
        return explicit_runs
    if tier == "baseline":
        return 10
    if tier == "iteration":
        return 5
    return 1
```

- [ ] **Step 7: Update `write_jsonl()` to take `tier` + `corpus_version`**

Replace the existing `write_jsonl` (currently at line 692) with:

```python
def write_jsonl(
    results: list[CellResult],
    path: pathlib.Path,
    *,
    tier: Optional[str],
    corpus_version: dict[str, str],
) -> None:
    with path.open("w") as f:
        for r in results:
            f.write(json.dumps({
                "prompt_id": r.prompt_id,
                "language": r.language,
                "model": r.model,
                "run_idx": r.run_idx,
                "tier": tier,
                "corpus_version": corpus_version,
                "first_attempt": _attempt_to_json(r.first_attempt),
                "edit_attempt": _attempt_to_json(r.edit_attempt),
                "final_pass": r.final_pass,
                "error": r.error,
            }) + "\n")
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_compare_tier.py -v`
Expected: all six tests PASS.

- [ ] **Step 9: Wire `--tier` into `main()` and update the JSONL/report call sites**

In `comp/scripts/compare.py`, inside `main()`'s argparse block (around line 970, between the `--no-edit-loop` arg and the `--results-dir` arg), add:

```python
    parser.add_argument(
        "--tier",
        default=None,
        choices=["baseline", "iteration"],
        help="Tiered run preset. baseline=10 runs/cell (authoritative measurement); "
             "iteration=5 runs/cell (cheap spot-check). Explicit --runs N overrides.",
    )
```

Then change the existing `--runs` argument so `default=None` (currently `default=1`):

```python
    parser.add_argument(
        "--runs",
        type=int,
        default=None,
        help="Number of independent runs per (prompt, lang, model). >1 enables "
             "K/N aggregation. Default depends on --tier (baseline=10, iteration=5, "
             "else 1).",
    )
```

Right after argparse returns (just above the `if shutil.which("claude") is None:` block, around line 977), insert:

```python
    args.runs = resolve_runs(tier=args.tier, explicit_runs=args.runs)
```

Then in the `if args.runs < 1:` validation (line 997), the message still works as-is.

Update the `_build_run_slug` call (around line 1098) to include tier:

```python
    run_slug = _build_run_slug(
        filter_expr=args.filter,
        full=args.full,
        all_langs=args.all_langs,
        runs=args.runs,
        no_edit_loop=args.no_edit_loop,
        tier=args.tier,
    )
```

Update the JSONL filename (line 1106) to include tier when set:

```python
    tier_suffix = f"-{args.tier}" if args.tier else ""
    jsonl_path = results_dir / f"comparison-results-{timestamp}{tier_suffix}.jsonl"
```

Compute `corpus_version` once (just before the `with concurrent.futures.ThreadPoolExecutor` block, around line 1057):

```python
    corpus_version = compute_corpus_version()
```

Update the `write_jsonl(...)` call (line 1109):

```python
    write_jsonl(results, jsonl_path, tier=args.tier, corpus_version=corpus_version)
```

- [ ] **Step 10: Update `_build_run_slug()` to accept `tier`**

Replace the signature (line 884) and add tier handling. The new function:

```python
def _build_run_slug(
    *,
    filter_expr: Optional[str],
    full: bool,
    all_langs: bool,
    runs: int,
    no_edit_loop: bool,
    tier: Optional[str],
) -> str:
    """Compose a short slug describing the distinctive flags of this run.

    Used in the per-run report filename (comparison-log-<ts>[-<slug>].md)
    so a directory listing tells you what each historical report was.
    Returns empty string when nothing is distinctive — the timestamp alone
    is enough to uniquely identify a default-bank run."""
    parts: list[str] = []
    if tier:
        parts.append(tier)
    if filter_expr:
        sanitized = re.sub(r"[^A-Za-z0-9_-]+", "", filter_expr)[:32]
        if sanitized:
            parts.append(sanitized)
    if all_langs:
        parts.append("cross")
    if full:
        parts.append("full")
    if runs > 1 and not tier:
        # Skip r<N> when tier already implies the run count; keeps the
        # slug from saying baseline-r10 redundantly.
        parts.append(f"r{runs}")
    if no_edit_loop:
        parts.append("noedit")
    return "-".join(parts)
```

- [ ] **Step 11: Smoke-test `--tier baseline --filter C01` (dry-run shape only, no claude calls)**

The full smoke needs `claude` on PATH and burns rate-limit, so this step just checks argparse + help text:

Run: `python3 comp/scripts/compare.py --tier baseline --help | head -40`
Expected: `--tier {baseline,iteration}` appears in help output.

Run: `python3 comp/scripts/compare.py --tier baseline --filter NONE_MATCHES 2>&1 | head -5`
Expected: `compare.py: no prompts matched filter 'NONE_MATCHES'` (exit 2). Confirms argparse + post-processing succeed before prompt filtering.

- [ ] **Step 12: Re-run full test suite + commit**

Run: `python3 -m pytest comp/tests/ -v`
Expected: all tests pass.

```bash
git add comp/scripts/compare.py comp/tests/conftest.py comp/tests/test_compare_tier.py
git commit -m "$(cat <<'EOF'
[comp] Add --tier flag and corpus_version to compare.py

Plumbs --tier baseline (runs=10) and --tier iteration (runs=5) through
the harness; explicit --runs overrides. Each JSONL row gains a tier
field and a corpus_version object (git short-sha + SHA-256 of
sigil.md+language.md concatenated) so a later dashboard can refuse to
mix runs across teaching-material versions.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Cluster taxonomy module

**Files:**
- Create: `comp/scripts/clusters.py`
- Create: `comp/tests/test_clusters.py`

- [ ] **Step 1: Write failing tests for the cluster engine and each seed cluster**

```python
# comp/tests/test_clusters.py
"""Tests for the cluster taxonomy + classifier in clusters.py."""
from __future__ import annotations

import pytest

import clusters  # type: ignore


def _row(eval_category="compile", eval_detail="", eval_raw_output=""):
    """Build a synthetic failure row matching compare.py's JSONL shape."""
    return {
        "first_attempt": {
            "eval_passed": False,
            "eval_category": eval_category,
            "eval_detail": eval_detail,
            "eval_raw_output": eval_raw_output,
        },
        "edit_attempt": None,
    }


def test_classify_returns_uncategorized_for_unknown_error():
    row = _row(eval_detail="error[E9999]: brand new error not yet in taxonomy")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "uncategorized"


def test_effect_row_missing_arith():
    row = _row(eval_detail="error[E0042]: `operator `%`` requires `ArithError` "
                           "in the enclosing function's effect row")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "effect-row-missing-arith"
    assert cluster.lever == "spec/context"


def test_effect_row_missing_io():
    row = _row(eval_detail="error[E0042]: requires `IO` in the enclosing function's effect row")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "effect-row-missing-io"


def test_effect_row_missing_other_falls_through_after_specific_matches():
    row = _row(eval_detail="error[E0042]: requires `Async` in the enclosing function's effect row")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "effect-row-missing-other"


def test_bare_name_ambiguous():
    row = _row(eval_detail="error[E0147]: ambiguous bare name `map`")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "bare-name-ambiguous"


def test_field_access_missing():
    row = _row(eval_detail="error[E0151]: no field access operator")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "field-access-missing"


def test_mut_array_element_type():
    row = _row(eval_detail="error[E0150]: `MutArray[A]` element type `Char` is not supported")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "mut-array-element-type"


def test_unknown_import_or_name():
    row = _row(eval_detail="error[E0123]: unknown name `list_zap`")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "unknown-import-or-name"


def test_match_non_exhaustive():
    row = _row(eval_detail="error[E0099]: non-exhaustive match on `Result[A, E]`")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "match-non-exhaustive"


def test_syntax_parse_error():
    row = _row(eval_detail="error[E0010]: expected an expression")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "syntax-parse-error"


def test_wrong_output_category():
    row = _row(eval_category="wrong_output",
               eval_detail="expected 42 got 24",
               eval_raw_output="diff: expected 42 got 24")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "wrong-output"


def test_runtime_panic_category():
    row = _row(eval_category="runtime", eval_detail="panic: index out of bounds")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "runtime-panic"


def test_timeout_category():
    row = _row(eval_category="timeout", eval_detail="killed after 30s")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "timeout"


def test_compile_other_for_unmatched_compile_errors():
    # An E-code that isn't pre-seeded but is a compile failure.
    row = _row(eval_category="compile",
               eval_detail="error[E0500]: invented compile error for this test")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "compile-other"


def test_passes_return_none():
    """A passing attempt has no cluster — classifier should return None."""
    row = {
        "first_attempt": {
            "eval_passed": True,
            "eval_category": None,
            "eval_detail": "pass",
            "eval_raw_output": "pass",
        },
        "edit_attempt": None,
    }
    assert clusters.classify_failure(row, attempt="first") is None


def test_edit_attempt_classification():
    """classify_failure should look at the edit_attempt when attempt='edit'."""
    row = {
        "first_attempt": {
            "eval_passed": False,
            "eval_category": "compile",
            "eval_detail": "error[E0010]: expected an expression",
            "eval_raw_output": "",
        },
        "edit_attempt": {
            "eval_passed": False,
            "eval_category": "compile",
            "eval_detail": "error[E0150]: `MutArray` element type `Char` is not supported",
            "eval_raw_output": "",
        },
    }
    first = clusters.classify_failure(row, attempt="first")
    edit = clusters.classify_failure(row, attempt="edit")
    assert first.id == "syntax-parse-error"
    assert edit.id == "mut-array-element-type"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python3 -m pytest comp/tests/test_clusters.py -v`
Expected: every test fails with `ModuleNotFoundError: No module named 'clusters'`.

- [ ] **Step 3: Implement `clusters.py`**

```python
# comp/scripts/clusters.py
"""Cluster taxonomy + classifier for the corpus dashboard.

Each failure (compile / runtime / wrong-output / timeout) maps to one
cluster_id via the first matching entry in CLUSTER_TAXONOMY. The
matchers run against `eval_category` and `eval_detail + eval_raw_output`
concatenated. Adding a new cluster is one PR-friendly diff: append a
Cluster() to the list, add a test in comp/tests/test_clusters.py.

Imported by comp/scripts/dashboard.py only — no runtime dependency
from compare.py."""
from __future__ import annotations

import dataclasses
import re
from typing import Callable, Literal, Optional


Attempt = Literal["first", "edit"]


@dataclasses.dataclass(frozen=True)
class Cluster:
    id: str
    description: str
    lever: str
    # Either a compiled regex (matched against detail+raw_output) or a
    # predicate on (category, detail, raw_output). First-match wins,
    # ordered for specificity (e.g. arith before "any E0042").
    matcher: object


def _re(pattern: str) -> re.Pattern[str]:
    return re.compile(pattern, re.IGNORECASE | re.DOTALL)


def _category_is(want: str) -> Callable[[str, str, str], bool]:
    def _check(category: str, _detail: str, _raw: str) -> bool:
        return category == want
    return _check


CLUSTER_TAXONOMY: list[Cluster] = [
    # Effect-row teaching gaps — most specific first.
    Cluster(
        id="effect-row-missing-arith",
        description="Missing `ArithError` in effect row (% or / without discharge)",
        lever="spec/context",
        matcher=_re(r"E0042.*ArithError.*effect row"),
    ),
    Cluster(
        id="effect-row-missing-io",
        description="Missing `IO` in effect row",
        lever="spec/context",
        matcher=_re(r"E0042.*\bIO\b.*effect row"),
    ),
    Cluster(
        id="effect-row-missing-other",
        description="Missing some other effect in effect row (catch-all for E0042)",
        lever="spec/context",
        matcher=_re(r"E0042.*effect row"),
    ),
    # Language gaps queued for v2.
    Cluster(
        id="bare-name-ambiguous",
        description="Ambiguous bare-name resolution (E0147)",
        lever="language (qualified call syntax — v2)",
        matcher=_re(r"E0147"),
    ),
    Cluster(
        id="field-access-missing",
        description="Field-access operator not yet supported (E0151)",
        lever="language (field-access operator — v2)",
        matcher=_re(r"E0151"),
    ),
    Cluster(
        id="mut-array-element-type",
        description="MutArray element type not supported in v1 (Char/Bool/narrow scalars)",
        lever="language (v2 type-arg threading)",
        matcher=_re(r"E0150.*MutArray"),
    ),
    # Name / stdlib gaps.
    Cluster(
        id="unknown-import-or-name",
        description="Unknown name or unresolved import (likely stdlib gap or wrong name)",
        lever="context (or stdlib add)",
        matcher=_re(r"E0\d{3}.*(unknown name|no item named|unresolved)"),
    ),
    Cluster(
        id="match-non-exhaustive",
        description="Non-exhaustive match",
        lever="spec/context",
        matcher=_re(r"E0\d{3}.*non-exhaustive"),
    ),
    Cluster(
        id="syntax-parse-error",
        description="Parse error (often a symptom of an earlier teaching gap)",
        lever="investigate (usually points elsewhere)",
        matcher=_re(r"E0010|expected an expression|expected\s+\S+\s+token"),
    ),
    # Non-compile categories.
    Cluster(
        id="wrong-output",
        description="Program ran but produced the wrong output (algorithmic mistake)",
        lever="spec/prompt (algorithmic)",
        matcher=_category_is("wrong_output"),
    ),
    Cluster(
        id="runtime-panic",
        description="Program crashed at runtime",
        lever="spec/teaching or language",
        matcher=_category_is("runtime"),
    ),
    Cluster(
        id="timeout",
        description="Program exceeded the eval timeout",
        lever="algorithmic / runtime",
        matcher=_category_is("timeout"),
    ),
    # Catch-alls — must be last.
    Cluster(
        id="compile-other",
        description="Compile failure not matched by any specific cluster — triage queue",
        lever="uncategorized → triage",
        matcher=_category_is("compile"),
    ),
]


UNCATEGORIZED = Cluster(
    id="uncategorized",
    description="No matcher fired — review and add a taxonomy entry",
    lever="uncategorized → triage",
    matcher=lambda *_: False,  # never matched at classification time
)


def classify_failure(row: dict, *, attempt: Attempt) -> Optional[Cluster]:
    """Return the Cluster for the given attempt on the row, or None if
    the attempt passed / is missing. Failures that don't match any
    seeded matcher fall back to UNCATEGORIZED."""
    a = row.get(f"{attempt}_attempt")
    if a is None or a.get("eval_passed"):
        return None
    category = a.get("eval_category") or ""
    detail = a.get("eval_detail") or ""
    raw = a.get("eval_raw_output") or ""
    haystack = f"{detail}\n{raw}"
    for cluster in CLUSTER_TAXONOMY:
        m = cluster.matcher
        if isinstance(m, re.Pattern):
            if m.search(haystack):
                return cluster
        elif callable(m):
            if m(category, detail, raw):
                return cluster
    return UNCATEGORIZED


def all_known_cluster_ids() -> list[str]:
    """For the dashboard — render zero-count rows for known clusters in
    seed order. Includes UNCATEGORIZED at the front of the maintenance
    queue."""
    return [UNCATEGORIZED.id] + [c.id for c in CLUSTER_TAXONOMY]
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_clusters.py -v`
Expected: all 16 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/clusters.py comp/tests/test_clusters.py
git commit -m "$(cat <<'EOF'
[comp] Add cluster taxonomy module for dashboard

Hand-curated taxonomy mapping compile/runtime/wrong-output failures to
(cluster_id, lever) pairs. First-match wins; unmatched failures fall to
UNCATEGORIZED for review. Seed list covers effect-row gaps (arith/io),
known v2 language gaps (E0147 bare-name, E0151 field-access, E0150
MutArray element type), name/stdlib misses, non-exhaustive match,
parse errors, and the non-compile category catch-alls.

Imported only by dashboard.py — zero coupling to compare.py.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Dashboard scaffold + header section

**Files:**
- Create: `comp/scripts/dashboard.py`
- Create: `comp/tests/test_dashboard.py`

- [ ] **Step 1: Write failing tests for trace loading + header**

```python
# comp/tests/test_dashboard.py
"""Tests for the dashboard renderer."""
from __future__ import annotations

import json
from pathlib import Path

import pytest

import dashboard  # type: ignore


def _make_trace(tmp_path: Path, name: str, rows: list[dict]) -> Path:
    """Write rows to a JSONL file under tmp_path, return its path."""
    p = tmp_path / name
    with p.open("w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    return p


def _row(prompt_id="C01", model="claude-haiku-4-5", tier="baseline",
         run_idx=0, first_passed=True, first_detail="pass",
         edit_attempt=None, final_pass=True,
         corpus_version=None):
    corpus_version = corpus_version or {"git_sha": "abc1234", "teaching_hash": "deadbeef"}
    first = {
        "program": "fn main() -> Int ![IO] { 0 }",
        "raw_response": "```sigil\nfn main()...\n```",
        "eval_passed": first_passed,
        "eval_category": None if first_passed else "compile",
        "eval_detail": first_detail,
        "eval_raw_output": first_detail,
    }
    return {
        "prompt_id": prompt_id, "language": "sigil", "model": model,
        "run_idx": run_idx, "tier": tier, "corpus_version": corpus_version,
        "first_attempt": first, "edit_attempt": edit_attempt,
        "final_pass": final_pass, "error": None,
    }


def test_load_traces_reads_rows(tmp_path):
    rows = [_row(prompt_id="C01"), _row(prompt_id="C02")]
    trace = _make_trace(tmp_path, "comparison-results-20260101T000000-baseline.jsonl", rows)
    loaded = dashboard.load_traces([trace])
    assert len(loaded.rows) == 2
    assert loaded.rows[0]["prompt_id"] == "C01"


def test_load_traces_warns_on_mixed_corpus_versions(tmp_path):
    a = _make_trace(tmp_path, "a.jsonl",
                    [_row(corpus_version={"git_sha": "aaa", "teaching_hash": "111"})])
    b = _make_trace(tmp_path, "b.jsonl",
                    [_row(corpus_version={"git_sha": "bbb", "teaching_hash": "222"})])
    loaded = dashboard.load_traces([a, b])
    assert loaded.mixed_corpus_versions is True
    assert len(loaded.corpus_versions) == 2


def test_load_traces_no_warning_when_versions_match(tmp_path):
    cv = {"git_sha": "aaa", "teaching_hash": "111"}
    a = _make_trace(tmp_path, "a.jsonl", [_row(corpus_version=cv)])
    b = _make_trace(tmp_path, "b.jsonl", [_row(corpus_version=cv)])
    loaded = dashboard.load_traces([a, b])
    assert loaded.mixed_corpus_versions is False


def test_load_traces_handles_missing_tier_and_corpus_version(tmp_path):
    """Old JSONL traces (pre-tier) must load without crashing."""
    legacy_row = {
        "prompt_id": "C01", "language": "sigil",
        "model": "claude-haiku-4-5", "run_idx": 0,
        "first_attempt": {"program": "x", "raw_response": "x",
                          "eval_passed": True, "eval_category": None,
                          "eval_detail": "pass", "eval_raw_output": "pass"},
        "edit_attempt": None, "final_pass": True, "error": None,
    }
    trace = _make_trace(tmp_path, "legacy.jsonl", [legacy_row])
    loaded = dashboard.load_traces([trace])
    assert loaded.rows[0]["tier"] is None
    assert loaded.rows[0]["corpus_version"] is None
    assert loaded.has_legacy_rows is True


def test_render_header_includes_metadata(tmp_path):
    cv = {"git_sha": "abc1234", "teaching_hash": "0123456789abcdef" * 4}
    trace = _make_trace(tmp_path, "trace-baseline.jsonl",
                        [_row(corpus_version=cv)])
    loaded = dashboard.load_traces([trace])
    out = dashboard.render(loaded)
    assert "# Corpus dashboard" in out
    assert "baseline" in out
    assert "abc1234" in out
    assert "0123456789abcdef" in out  # teaching hash present
    assert "trace-baseline.jsonl" in out


def test_render_header_warns_on_mixed_corpus_versions(tmp_path):
    a = _make_trace(tmp_path, "a.jsonl",
                    [_row(corpus_version={"git_sha": "aaa", "teaching_hash": "111"})])
    b = _make_trace(tmp_path, "b.jsonl",
                    [_row(corpus_version={"git_sha": "bbb", "teaching_hash": "222"})])
    loaded = dashboard.load_traces([a, b])
    out = dashboard.render(loaded)
    assert "WARNING" in out
    assert "corpus_version" in out


def test_render_header_warns_on_legacy_rows(tmp_path):
    legacy_row = {
        "prompt_id": "C01", "language": "sigil",
        "model": "claude-haiku-4-5", "run_idx": 0,
        "first_attempt": {"program": "x", "raw_response": "x",
                          "eval_passed": True, "eval_category": None,
                          "eval_detail": "pass", "eval_raw_output": "pass"},
        "edit_attempt": None, "final_pass": True, "error": None,
    }
    trace = _make_trace(tmp_path, "legacy.jsonl", [legacy_row])
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "legacy trace" in out.lower() or "missing corpus_version" in out.lower()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: every test fails with `ModuleNotFoundError: No module named 'dashboard'`.

- [ ] **Step 3: Implement `dashboard.py` scaffold + `load_traces` + `render` header-only**

```python
# comp/scripts/dashboard.py
#!/usr/bin/env python3
"""
dashboard.py — render a rich attribution report over comp/ corpus JSONL
traces. Reads one or more JSONL trace files, applies the cluster
taxonomy from clusters.py, and emits a single markdown report.

Pure analysis: never calls Claude, never modifies traces. Re-running is
free — edit clusters.py and re-render in seconds.

Usage:
    # Render against the newest baseline-tier trace in comp/log/:
    python3 comp/scripts/dashboard.py --latest-tier baseline

    # Render against the newest iteration-tier trace:
    python3 comp/scripts/dashboard.py --latest-tier iteration

    # Render against a specific trace file:
    python3 comp/scripts/dashboard.py --trace comp/log/comparison-results-….jsonl

    # Render against multiple traces (will warn if corpus_version differs):
    python3 comp/scripts/dashboard.py --trace a.jsonl --trace b.jsonl
"""
from __future__ import annotations

import argparse
import dataclasses
import json
import pathlib
import sys
import time
from typing import Optional

import clusters

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LOG_DIR = REPO_ROOT / "comp" / "log"


@dataclasses.dataclass
class LoadedTraces:
    rows: list[dict]
    source_paths: list[pathlib.Path]
    tier: Optional[str]
    corpus_versions: list[dict]
    mixed_corpus_versions: bool
    has_legacy_rows: bool


def load_traces(paths: list[pathlib.Path]) -> LoadedTraces:
    rows: list[dict] = []
    corpus_versions: list[dict] = []
    seen_cv_keys: set[tuple] = set()
    tiers_seen: set[Optional[str]] = set()
    has_legacy = False
    for p in paths:
        for line in p.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            # Tolerate legacy rows that pre-date the schema additions.
            row.setdefault("tier", None)
            row.setdefault("corpus_version", None)
            rows.append(row)
            tiers_seen.add(row["tier"])
            cv = row["corpus_version"]
            if cv is None:
                has_legacy = True
                continue
            key = (cv.get("git_sha"), cv.get("teaching_hash"))
            if key not in seen_cv_keys:
                seen_cv_keys.add(key)
                corpus_versions.append(cv)
    # If every row had the same tier, surface it; else None (mixed/unknown).
    non_null_tiers = [t for t in tiers_seen if t is not None]
    tier = non_null_tiers[0] if len(set(non_null_tiers)) == 1 else None
    return LoadedTraces(
        rows=rows,
        source_paths=paths,
        tier=tier,
        corpus_versions=corpus_versions,
        mixed_corpus_versions=len(corpus_versions) > 1,
        has_legacy_rows=has_legacy,
    )


def _resolve_latest_trace(tier: str) -> pathlib.Path:
    """Find the newest comparison-results-*-<tier>.jsonl in LOG_DIR."""
    candidates = sorted(LOG_DIR.glob(f"comparison-results-*-{tier}.jsonl"))
    if not candidates:
        raise FileNotFoundError(
            f"no trace files matching tier {tier!r} in {LOG_DIR}"
        )
    return candidates[-1]


def render(loaded: LoadedTraces) -> str:
    lines: list[str] = []
    lines.extend(_render_header(loaded))
    return "\n".join(lines) + "\n"


def _render_header(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("# Corpus dashboard\n")
    out.append(f"Generated: `{time.strftime('%Y-%m-%dT%H:%M:%S%z')}`")
    out.append("")
    out.append("**Source trace(s):**")
    for p in loaded.source_paths:
        try:
            rel = p.relative_to(REPO_ROOT)
            out.append(f"- `{rel}`")
        except ValueError:
            out.append(f"- `{p}`")
    out.append("")
    out.append(f"**Tier:** `{loaded.tier or 'unspecified / mixed'}`")
    out.append(f"**Rows loaded:** {len(loaded.rows)}")
    if loaded.corpus_versions:
        out.append("**corpus_version(s):**")
        for cv in loaded.corpus_versions:
            sha = cv.get("git_sha", "?")
            teaching = cv.get("teaching_hash", "?")
            out.append(f"- git `{sha}` · teaching SHA-256 `{teaching}`")
    out.append("")
    if loaded.mixed_corpus_versions:
        out.append("> ⚠️ **WARNING:** these traces span multiple `corpus_version` values.")
        out.append("> Aggregate numbers below mix runs across different "
                   "`comp/contexts/sigil.md` or `spec/language.md` states; "
                   "treat comparisons across versions with care.")
        out.append("")
    if loaded.has_legacy_rows:
        out.append("> ℹ️  One or more rows are missing `corpus_version` "
                   "(legacy trace pre-dating the schema). Their `corpus_version` "
                   "is treated as unknown.")
        out.append("")
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description="Render the corpus dashboard.")
    src = parser.add_mutually_exclusive_group(required=True)
    src.add_argument(
        "--latest-tier",
        choices=["baseline", "iteration"],
        help="Render against the newest comp/log/comparison-results-*-<tier>.jsonl",
    )
    src.add_argument(
        "--trace",
        action="append",
        type=pathlib.Path,
        help="Path to a JSONL trace. Pass multiple times to aggregate.",
    )
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        default=None,
        help="Output markdown path. Default: comp/log/dashboard-<ts>[-<tier>].md.",
    )
    args = parser.parse_args()

    if args.latest_tier:
        paths = [_resolve_latest_trace(args.latest_tier)]
    else:
        paths = args.trace
        for p in paths:
            if not p.exists():
                print(f"dashboard.py: trace not found: {p}", file=sys.stderr)
                return 2

    loaded = load_traces(paths)
    out_text = render(loaded)

    if args.output is None:
        ts = time.strftime("%Y%m%dT%H%M%S")
        tier_suffix = f"-{loaded.tier}" if loaded.tier else ""
        args.output = LOG_DIR / f"dashboard-{ts}{tier_suffix}.md"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(out_text)
    print(f"dashboard.py: wrote {args.output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: all seven tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/dashboard.py comp/tests/test_dashboard.py
git commit -m "$(cat <<'EOF'
[comp] Add dashboard.py scaffold with trace loader + header section

dashboard.py loads one or more JSONL traces (--trace path, or
--latest-tier baseline|iteration that picks the newest matching file
in comp/log/), aggregates corpus_version metadata, and renders the
report header. Tolerates legacy rows missing tier/corpus_version
(surface as warnings, never crash). Mixed corpus_versions across
loaded traces produce a prominent warning so cross-version
comparisons don't get mistaken for like-for-like.

Body sections will be added in subsequent tasks.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Topline pass-rate table

**Files:**
- Modify: `comp/scripts/dashboard.py`
- Modify: `comp/tests/test_dashboard.py`

- [ ] **Step 1: Add failing test for topline table**

Append to `comp/tests/test_dashboard.py`:

```python
def test_topline_table_renders_first_and_final_pass_rates(tmp_path):
    rows = [
        # haiku: 2/3 first, 3/3 final
        _row(prompt_id="C01", model="claude-haiku-4-5", run_idx=0,
             first_passed=True, final_pass=True),
        _row(prompt_id="C01", model="claude-haiku-4-5", run_idx=1,
             first_passed=True, final_pass=True),
        _row(prompt_id="C01", model="claude-haiku-4-5", run_idx=2,
             first_passed=False, first_detail="error[E0010]: expected an expression",
             edit_attempt={"program": "x", "raw_response": "x",
                           "eval_passed": True, "eval_category": None,
                           "eval_detail": "pass", "eval_raw_output": "pass"},
             final_pass=True),
        # sonnet: 1/2 first, 1/2 final
        _row(prompt_id="C01", model="claude-sonnet-4-6", run_idx=0,
             first_passed=True, final_pass=True),
        _row(prompt_id="C01", model="claude-sonnet-4-6", run_idx=1,
             first_passed=False, first_detail="error[E0042]: requires `ArithError`",
             edit_attempt=None, final_pass=False),
    ]
    trace = _make_trace(tmp_path, "trace-baseline.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## Topline pass rates" in out
    # Haiku row.
    assert "claude-haiku-4-5" in out
    assert "2/3" in out  # first-pass
    assert "3/3" in out  # final-pass
    # Sonnet row.
    assert "claude-sonnet-4-6" in out
    assert "1/2" in out


def test_topline_skips_models_with_zero_rows(tmp_path):
    rows = [_row(prompt_id="C01", model="claude-haiku-4-5")]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "claude-sonnet-4-6" not in out
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `python3 -m pytest comp/tests/test_dashboard.py::test_topline_table_renders_first_and_final_pass_rates -v`
Expected: AssertionError — "## Topline pass rates" not in output.

- [ ] **Step 3: Add `_render_topline` and wire it into `render()`**

In `comp/scripts/dashboard.py`, add after `_render_header`:

```python
def _render_topline(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("## Topline pass rates\n")
    out.append("| Language | Model | First-pass | Final-pass |")
    out.append("|---|---|---|---|")
    # Group rows by (language, model) preserving first-seen order.
    seen: list[tuple[str, str]] = []
    grouped: dict[tuple[str, str], list[dict]] = {}
    for row in loaded.rows:
        key = (row["language"], row["model"])
        if key not in grouped:
            seen.append(key)
            grouped[key] = []
        grouped[key].append(row)
    for (lang, model) in seen:
        cell_rows = grouped[(lang, model)]
        n = len(cell_rows)
        first_pass = sum(
            1 for r in cell_rows
            if r["first_attempt"] and r["first_attempt"].get("eval_passed")
        )
        final = sum(1 for r in cell_rows if r["final_pass"])
        out.append(f"| `{lang}` | `{model}` | "
                   f"{first_pass}/{n} ({100.0 * first_pass / n:.1f}%) | "
                   f"{final}/{n} ({100.0 * final / n:.1f}%) |")
    out.append("")
    return out
```

Update `render()` to call it:

```python
def render(loaded: LoadedTraces) -> str:
    lines: list[str] = []
    lines.extend(_render_header(loaded))
    lines.extend(_render_topline(loaded))
    return "\n".join(lines) + "\n"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/dashboard.py comp/tests/test_dashboard.py
git commit -m "$(cat <<'EOF'
[comp] dashboard: topline pass-rate table

One row per (language, model) seen in the loaded traces; columns are
first-pass K/N and final-pass K/N. Cells with zero rows for a model
don't appear (no "0/0" rows).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Per-prompt × model tables (first-pass + final-pass)

**Files:**
- Modify: `comp/scripts/dashboard.py`
- Modify: `comp/tests/test_dashboard.py`

- [ ] **Step 1: Add failing test for per-prompt tables**

Append to `comp/tests/test_dashboard.py`:

```python
def test_per_prompt_tables_render_first_and_final(tmp_path):
    rows = [
        # C01 haiku: 2/3 first, 3/3 final
        _row(prompt_id="C01", run_idx=0, first_passed=True),
        _row(prompt_id="C01", run_idx=1, first_passed=True),
        _row(prompt_id="C01", run_idx=2, first_passed=False,
             first_detail="error[E0010]",
             edit_attempt={"program": "x", "raw_response": "x",
                           "eval_passed": True, "eval_category": None,
                           "eval_detail": "pass", "eval_raw_output": "pass"},
             final_pass=True),
        # C02 haiku: 0/2 first, 0/2 final
        _row(prompt_id="C02", run_idx=0, first_passed=False,
             first_detail="error[E0042]: requires `ArithError`",
             final_pass=False),
        _row(prompt_id="C02", run_idx=1, first_passed=False,
             first_detail="error[E0042]: requires `ArithError`",
             final_pass=False),
    ]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## Per-prompt × model — first-pass" in out
    assert "## Per-prompt × model — final-pass" in out
    # First-pass table cells.
    assert "2/3" in out
    assert "0/2" in out
    # Final-pass: C01 row should contain 3/3.
    # Allow that 3/3 may also appear in topline; check the per-prompt row layout.
    assert "**C01**" in out
    assert "**C02**" in out
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `python3 -m pytest comp/tests/test_dashboard.py::test_per_prompt_tables_render_first_and_final -v`
Expected: AssertionError on "## Per-prompt × model — first-pass".

- [ ] **Step 3: Add `_render_per_prompt_tables` and wire into `render()`**

In `comp/scripts/dashboard.py`, add after `_render_topline`:

```python
def _cell_kn(cell_rows: list[dict], passed_pred) -> str:
    n = len(cell_rows)
    if n == 0:
        return "—"
    k = sum(1 for r in cell_rows if passed_pred(r))
    return f"{k}/{n}"


def _render_per_prompt_tables(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    # Discover models + prompts in first-seen order.
    models: list[str] = []
    prompts: list[str] = []
    seen_models: set[str] = set()
    seen_prompts: set[str] = set()
    by_cell: dict[tuple[str, str], list[dict]] = {}
    for row in loaded.rows:
        if row["model"] not in seen_models:
            seen_models.add(row["model"])
            models.append(row["model"])
        if row["prompt_id"] not in seen_prompts:
            seen_prompts.add(row["prompt_id"])
            prompts.append(row["prompt_id"])
        by_cell.setdefault((row["prompt_id"], row["model"]), []).append(row)

    def first_passed(r: dict) -> bool:
        a = r.get("first_attempt")
        return bool(a and a.get("eval_passed"))

    def final_passed(r: dict) -> bool:
        return bool(r.get("final_pass"))

    def emit_table(title: str, predicate) -> None:
        out.append(f"## {title}\n")
        headers = ["Prompt"] + [f"`{m}`" for m in models]
        out.append("| " + " | ".join(headers) + " |")
        out.append("|" + "|".join(["---"] * len(headers)) + "|")
        for pid in prompts:
            row_cells = [f"**{pid}**"]
            for model in models:
                row_cells.append(_cell_kn(by_cell.get((pid, model), []), predicate))
            out.append("| " + " | ".join(row_cells) + " |")
        out.append("")

    emit_table("Per-prompt × model — first-pass", first_passed)
    emit_table("Per-prompt × model — final-pass", final_passed)
    return out
```

Update `render()`:

```python
def render(loaded: LoadedTraces) -> str:
    lines: list[str] = []
    lines.extend(_render_header(loaded))
    lines.extend(_render_topline(loaded))
    lines.extend(_render_per_prompt_tables(loaded))
    return "\n".join(lines) + "\n"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/dashboard.py comp/tests/test_dashboard.py
git commit -m "$(cat <<'EOF'
[comp] dashboard: per-prompt × model first-pass + final-pass tables

K/N fractions per (prompt, model) cell. Models and prompts appear in
first-seen order across the loaded traces so column ordering tracks
the data, not an arbitrary hard-coded list.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Cluster histogram + per-E-code histogram

**Files:**
- Modify: `comp/scripts/dashboard.py`
- Modify: `comp/tests/test_dashboard.py`

- [ ] **Step 1: Add failing tests for both histograms**

Append to `comp/tests/test_dashboard.py`:

```python
def test_cluster_histogram_counts_and_levers(tmp_path):
    rows = [
        # 2 ArithError misses on sonnet.
        _row(prompt_id="C07", model="claude-sonnet-4-6", run_idx=0,
             first_passed=False,
             first_detail="error[E0042]: requires `ArithError` in the enclosing function's effect row",
             final_pass=False),
        _row(prompt_id="C07", model="claude-sonnet-4-6", run_idx=1,
             first_passed=False,
             first_detail="error[E0042]: requires `ArithError` in the enclosing function's effect row",
             final_pass=False),
        # 1 MutArray[Char] miss on haiku.
        _row(prompt_id="H01", model="claude-haiku-4-5", run_idx=0,
             first_passed=False,
             first_detail="error[E0150]: `MutArray[A]` element type `Char` is not supported",
             final_pass=False),
        # 1 pass — should not appear in cluster counts.
        _row(prompt_id="C01", model="claude-haiku-4-5", run_idx=0, first_passed=True),
    ]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## Cluster histogram" in out
    assert "effect-row-missing-arith" in out
    assert "mut-array-element-type" in out
    # Counts in the table: 2 for arith, 1 for mut-array.
    arith_line = next(l for l in out.splitlines() if "effect-row-missing-arith" in l)
    mut_line = next(l for l in out.splitlines() if "mut-array-element-type" in l)
    assert "| 2 |" in arith_line
    assert "| 1 |" in mut_line
    # Suggested lever should appear.
    assert "spec/context" in arith_line


def test_cluster_histogram_surfaces_uncategorized_at_top(tmp_path):
    rows = [
        _row(prompt_id="X01", first_passed=False,
             first_detail="error[E9999]: completely novel diagnostic",
             final_pass=False),
    ]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    # uncategorized must appear even with a single hit, at top of section.
    cluster_section = out.split("## Cluster histogram", 1)[1].split("##", 1)[0]
    lines = [l for l in cluster_section.splitlines() if "|" in l]
    # First data row (skip header + divider) should be uncategorized.
    first_data_row = lines[2]
    assert "uncategorized" in first_data_row


def test_ecode_histogram_extracts_codes(tmp_path):
    rows = [
        _row(prompt_id="C07", first_passed=False,
             first_detail="error[E0042]: requires `ArithError`", final_pass=False),
        _row(prompt_id="C15", first_passed=False,
             first_detail="error[E0010]: expected an expression", final_pass=False),
        _row(prompt_id="C16", first_passed=False,
             first_detail="error[E0042]: requires `ArithError`", final_pass=False),
    ]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## Per-E-code histogram" in out
    # E0042 should count 2, E0010 should count 1.
    e42_line = next(l for l in out.splitlines() if "E0042" in l)
    e10_line = next(l for l in out.splitlines() if "E0010" in l)
    assert "| 2 |" in e42_line
    assert "| 1 |" in e10_line
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v -k "cluster_histogram or ecode_histogram"`
Expected: AssertionError on missing section headers.

- [ ] **Step 3: Implement both histograms**

In `comp/scripts/dashboard.py`, add `import re` to the imports near the top of the file (alphabetical placement: between `import pathlib` and `import sys`). Then add after `_render_per_prompt_tables`:

```python
_ECODE = re.compile(r"E\d{4}")


def _iter_failed_attempts(loaded: LoadedTraces):
    """Yield (row, attempt_name, attempt_dict) for every failed attempt."""
    for row in loaded.rows:
        for name in ("first", "edit"):
            a = row.get(f"{name}_attempt")
            if a is None or a.get("eval_passed"):
                continue
            yield row, name, a


def _render_cluster_histogram(loaded: LoadedTraces) -> list[str]:
    counts: dict[str, int] = {}
    models_per_cluster: dict[str, set[str]] = {}
    cluster_meta: dict[str, clusters.Cluster] = {}
    for row, name, _attempt in _iter_failed_attempts(loaded):
        c = clusters.classify_failure(row, attempt=name)
        if c is None:
            continue
        counts[c.id] = counts.get(c.id, 0) + 1
        models_per_cluster.setdefault(c.id, set()).add(row["model"])
        cluster_meta[c.id] = c
    cluster_meta.setdefault(clusters.UNCATEGORIZED.id, clusters.UNCATEGORIZED)

    out: list[str] = []
    out.append("## Cluster histogram\n")
    if not counts:
        out.append("_No failures in the loaded traces._")
        out.append("")
        return out
    total = sum(counts.values())
    out.append("| Cluster | Count | % of failures | Models affected | Suggested lever |")
    out.append("|---|---|---|---|---|")
    # Maintenance queue: uncategorized + compile-other first regardless of count.
    priority = ["uncategorized", "compile-other"]
    rest = sorted(
        (cid for cid in counts if cid not in priority),
        key=lambda cid: (-counts[cid], cid),
    )
    ordered = [cid for cid in priority if cid in cluster_meta] + rest
    for cid in ordered:
        n = counts.get(cid, 0)
        meta = cluster_meta.get(cid)
        if meta is None:
            continue
        if cid in priority and n == 0:
            # Still surface the maintenance queue row, marked empty.
            out.append(f"| `{cid}` | 0 | — | — | {meta.lever} |")
            continue
        pct = f"{100.0 * n / total:.1f}%"
        models = ", ".join(sorted(models_per_cluster.get(cid, set())))
        out.append(f"| `{cid}` | {n} | {pct} | {models} | {meta.lever} |")
    out.append("")
    return out


def _render_ecode_histogram(loaded: LoadedTraces) -> list[str]:
    counts: dict[str, int] = {}
    for _row, _name, attempt in _iter_failed_attempts(loaded):
        haystack = (attempt.get("eval_detail") or "") + "\n" + (attempt.get("eval_raw_output") or "")
        seen_in_attempt: set[str] = set()
        for m in _ECODE.findall(haystack):
            # Count each E-code at most once per attempt — otherwise a
            # diagnostic that repeats the code in a hint inflates counts.
            if m in seen_in_attempt:
                continue
            seen_in_attempt.add(m)
            counts[m] = counts.get(m, 0) + 1

    out: list[str] = []
    out.append("## Per-E-code histogram\n")
    if not counts:
        out.append("_No E-codes found in failure details._")
        out.append("")
        return out
    out.append("| E-code | Count |")
    out.append("|---|---|")
    for code in sorted(counts, key=lambda c: (-counts[c], c)):
        out.append(f"| `{code}` | {counts[code]} |")
    out.append("")
    return out
```

Update `render()`:

```python
def render(loaded: LoadedTraces) -> str:
    lines: list[str] = []
    lines.extend(_render_header(loaded))
    lines.extend(_render_topline(loaded))
    lines.extend(_render_cluster_histogram(loaded))
    lines.extend(_render_ecode_histogram(loaded))
    lines.extend(_render_per_prompt_tables(loaded))
    return "\n".join(lines) + "\n"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/dashboard.py comp/tests/test_dashboard.py
git commit -m "$(cat <<'EOF'
[comp] dashboard: cluster histogram + per-E-code histogram

Cluster histogram is the headline attribution view — one row per
cluster with count, % of failures, models affected, suggested lever.
Maintenance-queue clusters (uncategorized, compile-other) surface at
top regardless of count so they're visible as triage work.

Per-E-code histogram extracts E\\d{4} codes from eval_detail+raw_output
(deduped per attempt) as a drill-down complement to clusters.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Failure detail grouped by cluster + after-edit diffs

**Files:**
- Modify: `comp/scripts/dashboard.py`
- Modify: `comp/tests/test_dashboard.py`

- [ ] **Step 1: Add failing tests for both sections**

Append to `comp/tests/test_dashboard.py`:

```python
def test_failure_detail_grouped_by_cluster(tmp_path):
    rows = [
        _row(prompt_id="C07", model="claude-sonnet-4-6", run_idx=0,
             first_passed=False,
             first_detail="error[E0042]: requires `ArithError` in the enclosing function's effect row\n"
                          "  --> .../program.sigil:9:23",
             final_pass=False),
        _row(prompt_id="H01", model="claude-haiku-4-5", run_idx=0,
             first_passed=False,
             first_detail="error[E0150]: `MutArray[A]` element type `Char` is not supported",
             final_pass=False),
    ]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## Failure detail by cluster" in out
    # Both cluster sub-headers should appear.
    assert "### `effect-row-missing-arith`" in out
    assert "### `mut-array-element-type`" in out
    # Cell identifiers should appear under the right cluster.
    arith_section = out.split("### `effect-row-missing-arith`", 1)[1].split("### `", 1)[0]
    assert "C07" in arith_section
    assert "claude-sonnet-4-6" in arith_section
    assert "ArithError" in arith_section


def test_after_edit_diff_section_only_shows_failed_edits(tmp_path):
    rows = [
        # First failed, edit also failed — should appear in diff section.
        _row(prompt_id="X01", model="claude-sonnet-4-6", run_idx=0,
             first_passed=False,
             first_detail="error[E0010]: expected an expression",
             edit_attempt={
                 "program": "fn main() -> Int ![IO] { 1 }\n",
                 "raw_response": "```sigil\nfn main()...\n```",
                 "eval_passed": False, "eval_category": "compile",
                 "eval_detail": "error[E0042]: requires `ArithError`",
                 "eval_raw_output": "error[E0042]: requires `ArithError`",
             },
             final_pass=False),
        # First failed, edit passed — should NOT appear in diff section.
        _row(prompt_id="X02", model="claude-sonnet-4-6", run_idx=0,
             first_passed=False,
             first_detail="error[E0010]: expected an expression",
             edit_attempt={
                 "program": "fn main() -> Int ![IO] { 0 }\n",
                 "raw_response": "```sigil\n...\n```",
                 "eval_passed": True, "eval_category": None,
                 "eval_detail": "pass", "eval_raw_output": "pass",
             },
             final_pass=True),
    ]
    # Give the first row a first_attempt.program so the diff has both sides.
    rows[0]["first_attempt"]["program"] = "fn main() -> Int ![IO] { 0 }\n"
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## After-edit failure diffs" in out
    assert "X01" in out.split("## After-edit failure diffs", 1)[1]
    assert "X02" not in out.split("## After-edit failure diffs", 1)[1]
    # Unified diff markers should appear (presence of '---' or '+++' headers).
    diff_section = out.split("## After-edit failure diffs", 1)[1]
    assert "---" in diff_section
    assert "+++" in diff_section


def test_after_edit_section_handles_no_failed_edits(tmp_path):
    rows = [_row(prompt_id="C01", first_passed=True)]
    trace = _make_trace(tmp_path, "trace.jsonl", rows)
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "## After-edit failure diffs" in out
    # The "none" branch should produce a friendly message, not crash.
    after_section = out.split("## After-edit failure diffs", 1)[1]
    assert "no after-edit failures" in after_section.lower()
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v -k "failure_detail or after_edit"`
Expected: failures on missing section headers.

- [ ] **Step 3: Implement both sections**

In `comp/scripts/dashboard.py`, add `import difflib` to the imports near the top of the file. Then add after `_render_ecode_histogram`:

```python
def _render_failure_detail(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("## Failure detail by cluster\n")
    # Bucket (row, attempt_name) by cluster_id.
    buckets: dict[str, list[tuple[dict, str, dict]]] = {}
    cluster_meta: dict[str, clusters.Cluster] = {}
    for row, name, attempt in _iter_failed_attempts(loaded):
        c = clusters.classify_failure(row, attempt=name)
        if c is None:
            continue
        buckets.setdefault(c.id, []).append((row, name, attempt))
        cluster_meta[c.id] = c
    if not buckets:
        out.append("_No failures in the loaded traces._")
        out.append("")
        return out
    for cid in sorted(buckets, key=lambda k: (-len(buckets[k]), k)):
        meta = cluster_meta[cid]
        items = buckets[cid]
        out.append(f"### `{cid}` — {meta.description}\n")
        out.append(f"<details><summary>{len(items)} failure(s)</summary>\n")
        for (row, name, attempt) in items:
            label = f"- **{row['prompt_id']}** × `{row['model']}` (run {row['run_idx']}, attempt={name})"
            out.append(label)
            detail = (attempt.get("eval_detail") or "").strip()
            snippet = "\n".join(detail.splitlines()[:6])
            if snippet:
                out.append("  ```")
                for line in snippet.splitlines():
                    out.append(f"  {line}")
                out.append("  ```")
        out.append("\n</details>\n")
    return out


def _render_after_edit_diffs(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("## After-edit failure diffs\n")
    failures: list[dict] = []
    for row in loaded.rows:
        if row.get("final_pass"):
            continue
        first = row.get("first_attempt")
        edit = row.get("edit_attempt")
        if first is None or edit is None:
            continue
        if edit.get("eval_passed"):
            continue  # final_pass=False contradicts this, but be defensive
        failures.append(row)
    if not failures:
        out.append("_No after-edit failures in the loaded traces._")
        out.append("")
        return out
    out.append(f"_{len(failures)} cell(s) where the edit-loop turn also failed._\n")
    for row in failures:
        first = row["first_attempt"]
        edit = row["edit_attempt"]
        out.append(f"### `{row['prompt_id']}` × `{row['model']}` (run {row['run_idx']})\n")
        out.append("**Edit-attempt failure:**")
        edit_detail = (edit.get("eval_detail") or "").strip()
        if edit_detail:
            out.append("```")
            for line in edit_detail.splitlines()[:8]:
                out.append(line)
            out.append("```")
        first_program = (first.get("program") or "").splitlines(keepends=True)
        edit_program = (edit.get("program") or "").splitlines(keepends=True)
        diff = list(difflib.unified_diff(
            first_program, edit_program,
            fromfile=f"{row['prompt_id']}-first.sigil",
            tofile=f"{row['prompt_id']}-edit.sigil",
            n=3,
        ))
        if diff:
            out.append("**Diff (first → edit):**")
            out.append("```diff")
            for line in diff:
                out.append(line.rstrip("\n"))
            out.append("```")
        else:
            out.append("_(no textual diff between first and edit programs)_")
        out.append("")
    return out
```

Update `render()` — final layout per spec section 5:

```python
def render(loaded: LoadedTraces) -> str:
    lines: list[str] = []
    lines.extend(_render_header(loaded))
    lines.extend(_render_topline(loaded))
    lines.extend(_render_cluster_histogram(loaded))
    lines.extend(_render_ecode_histogram(loaded))
    lines.extend(_render_per_prompt_tables(loaded))
    lines.extend(_render_failure_detail(loaded))
    lines.extend(_render_after_edit_diffs(loaded))
    return "\n".join(lines) + "\n"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `python3 -m pytest comp/tests/test_dashboard.py -v`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add comp/scripts/dashboard.py comp/tests/test_dashboard.py
git commit -m "$(cat <<'EOF'
[comp] dashboard: failure detail by cluster + after-edit diffs

Failure detail groups every failed attempt by its cluster_id, then
lists the (prompt × model × run × attempt) cells with a 6-line
eval_detail snippet under a collapsed <details> block per cluster.
After-edit diffs surface only cells where the edit-loop turn also
failed (the rarest, most expensive-to-debug bucket) and shows the
unified diff between first and edit programs alongside the edit's
eval_detail.

Closes the seven-section dashboard layout from the design spec.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: README update + end-to-end smoke against existing traces

**Files:**
- Modify: `comp/README.md`
- (No code changes — verification + docs only)

- [ ] **Step 1: Smoke-test the dashboard against the most recent published trace**

Run:
```bash
python3 comp/scripts/dashboard.py \
  --trace comp/log/comparison-results-20260517T091419.jsonl \
  --output /tmp/dashboard-smoke.md
```
Expected: exit 0, file written.

Run:
```bash
grep -E "^## " /tmp/dashboard-smoke.md
```
Expected (in order):
```
## Topline pass rates
## Cluster histogram
## Per-E-code histogram
## Per-prompt × model — first-pass
## Per-prompt × model — final-pass
## Failure detail by cluster
## After-edit failure diffs
```

Run:
```bash
grep -E "effect-row-missing-arith|mut-array-element-type|syntax-parse-error" /tmp/dashboard-smoke.md
```
Expected: at least one match per cluster — those are the three published failures in that trace.

- [ ] **Step 2: Smoke-test `--latest-tier` resolution**

The current trace pre-dates the tier suffix, so `--latest-tier` should error cleanly:
```bash
python3 comp/scripts/dashboard.py --latest-tier baseline
```
Expected: exit non-zero with `no trace files matching tier 'baseline' in ...comp/log`.

- [ ] **Step 3: Update `comp/README.md`**

In the "Running the full comparison" section, after the existing example block (after the `./comp/scripts/compare.sh --no-edit-loop` line), add a new subsection. Locate the block of example commands ending with `./comp/scripts/compare.sh --no-edit-loop` and add directly after the trailing fence:

```markdown
### Tiered runs (recommended for the methodology campaign)

For the first-pass-success measurement campaign, prefer tiered runs:

```shell
./comp/scripts/compare.sh --tier baseline       # K=10 per cell, authoritative
./comp/scripts/compare.sh --tier iteration      # K=5 per cell, cheap spot-check
```

`--tier baseline` sets `--runs 10`; `--tier iteration` sets `--runs 5`.
Explicit `--runs N` overrides the tier-implied K. The tier label is
written into the output filename (`comparison-results-<ts>-<tier>.jsonl`)
and into each JSONL row, so the dashboard can pick the newest baseline
trace without parsing filenames.

Each row also gets a `corpus_version` field: git short-sha plus a
SHA-256 of `comp/contexts/sigil.md` and `spec/language.md`
concatenated. The dashboard refuses (with a prominent warning) to mix
runs across different `corpus_version`s.

### Rendering the dashboard

```shell
# Render against the newest baseline-tier trace:
python3 comp/scripts/dashboard.py --latest-tier baseline

# Render against a specific trace file:
python3 comp/scripts/dashboard.py --trace comp/log/comparison-results-….jsonl
```

Writes `comp/log/dashboard-<ts>[-<tier>].md`. Re-rendering is free —
edit `comp/scripts/clusters.py` and re-run the dashboard in seconds.
```

- [ ] **Step 4: Run the full test suite as a final check**

Run: `python3 -m pytest comp/tests/ -v`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add comp/README.md
git commit -m "$(cat <<'EOF'
[comp] README: document --tier flag and dashboard.py

Adds two subsections to the "Running the full comparison" section: a
"Tiered runs" block describing --tier baseline/iteration and the new
JSONL fields, and a "Rendering the dashboard" block showing
--latest-tier and --trace invocations of dashboard.py.

Closes the dashboard build per docs/superpowers/specs/
2026-05-17-first-pass-success-dashboard-design.md.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review notes

Validation criteria from spec:

1. **`compare.py --tier baseline --filter C01` finishes, writes tier-tagged JSONL with `tier`+`corpus_version`** — covered by Task 1 Step 11 (argparse smoke) + Task 1's test that `write_jsonl` emits the new fields. Full end-to-end requires `claude` on PATH and is left for the user to run after the plan is implemented (it costs rate-limit).
2. **`dashboard.py --latest-tier baseline` renders a markdown with all seven sections, empty-state messages instead of crashes** — covered by Task 3 (header + load), Task 4 (topline), Task 5 (per-prompt), Task 6 (histograms), Task 7 (detail + diffs); empty-state branches are tested in `test_after_edit_section_handles_no_failed_edits` and the empty branches in `_render_cluster_histogram` / `_render_ecode_histogram`.
3. **Re-running `dashboard.py` against an old JSONL succeeds with warnings, not crashes** — covered by `test_load_traces_handles_missing_tier_and_corpus_version` + `test_render_header_warns_on_legacy_rows`.
4. **Dashboard against `comparison-results-20260517T091419.jsonl` reproduces pass-rate table and shows non-zero counts in three known clusters** — Task 8 Step 1.
5. **Cluster taxonomy edits → re-render is < 5 seconds on the full historical trace** — implicit in the pure-Python stdlib-only renderer; the full trace is ~25 rows × ~14 files = 350 rows, dominated by file I/O. Task 8 Step 1 will surface any pathological slowness.
