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
    return re.compile(pattern, re.IGNORECASE)


def _category_is(want: str) -> Callable[[str, str, str], bool]:
    def _check(category: str, _detail: str, _raw: str) -> bool:
        return category == want
    return _check


CLUSTER_TAXONOMY: list[Cluster] = [
    # Infrastructure failures — highest specificity, fire before effect-row
    # matchers in case a harness category ever reaches the taxonomy.
    Cluster(
        id="infra-input",
        description="Eval driver could not find the program file (harness pre-flight)",
        lever="infra (harness bug or filesystem race)",
        matcher=_category_is("input"),
    ),
    Cluster(
        id="infra-harness",
        description="Eval driver hit a toolchain/oracle issue, not a corpus signal",
        lever="infra (missing oracle, missing binary, etc.)",
        matcher=_category_is("harness"),
    ),
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
        matcher=_category_is("stdout"),
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
    Cluster(
        id="no-code-block",
        description="Model returned no extractable code block",
        lever="context (prompt formatting / fence instruction)",
        matcher=_category_is("no-code-block"),
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
    if a is None or a.get("eval_passed") is not False:
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
