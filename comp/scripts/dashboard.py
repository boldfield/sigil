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
    mixed_tiers: bool
    has_legacy_rows: bool


def load_traces(paths: list[pathlib.Path]) -> LoadedTraces:
    rows: list[dict] = []
    corpus_versions: list[dict] = []
    seen_cv_keys: set[tuple] = set()
    tiers_seen: set[Optional[str]] = set()
    has_legacy = False
    for p in paths:
        for line_num, raw_line in enumerate(p.read_text().splitlines(), start=1):
            stripped = raw_line.strip()
            if not stripped:
                continue
            try:
                row = json.loads(stripped)
            except json.JSONDecodeError as e:
                raise ValueError(
                    f"malformed JSON in {p}:{line_num}: {e}"
                ) from e
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
    unique_tiers = set(non_null_tiers)
    tier = non_null_tiers[0] if len(unique_tiers) == 1 else None
    mixed_tiers = len(unique_tiers) > 1
    return LoadedTraces(
        rows=rows,
        source_paths=paths,
        tier=tier,
        corpus_versions=corpus_versions,
        mixed_corpus_versions=len(corpus_versions) > 1,
        mixed_tiers=mixed_tiers,
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
    lines.extend(_render_topline(loaded))
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
    if loaded.mixed_tiers:
        out.append("> ⚠️ **WARNING:** these traces span multiple tiers "
                   "(e.g. baseline + iteration). Aggregate K/N counts mix "
                   "runs with different K, which makes per-cell pass rates "
                   "misleading.")
        out.append("")
    if loaded.has_legacy_rows:
        out.append("> ℹ️  One or more rows are missing `corpus_version` "
                   "(legacy trace pre-dating the schema). Their `corpus_version` "
                   "is treated as unknown.")
        out.append("")
    return out


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
        try:
            paths = [_resolve_latest_trace(args.latest_tier)]
        except FileNotFoundError as e:
            print(f"dashboard.py: {e}", file=sys.stderr)
            return 2
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
