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
import difflib
import json
import pathlib
import re
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
    lines.extend(_render_cluster_histogram(loaded))
    lines.extend(_render_ecode_histogram(loaded))
    lines.extend(_render_per_prompt_tables(loaded))
    lines.extend(_render_failure_detail(loaded))
    lines.extend(_render_after_edit_diffs(loaded))
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


_ECODE = re.compile(r"E\d{4}")


def _iter_failed_attempts(loaded: LoadedTraces):
    """Yield (row, attempt_name, attempt_dict) for every failed attempt."""
    for row in loaded.rows:
        for name in ("first", "edit"):
            attempt = row.get(f"{name}_attempt")
            if attempt is None or attempt.get("eval_passed"):
                continue
            yield row, name, attempt


def _render_cluster_histogram(loaded: LoadedTraces) -> list[str]:
    counts: dict[str, int] = {}
    models_per_cluster: dict[str, set[str]] = {}
    cluster_meta: dict[str, clusters.Cluster] = {}
    for row, attempt_name, _attempt in _iter_failed_attempts(loaded):
        cluster = clusters.classify_failure(row, attempt=attempt_name)
        if cluster is None:
            continue
        counts[cluster.id] = counts.get(cluster.id, 0) + 1
        models_per_cluster.setdefault(cluster.id, set()).add(row["model"])
        cluster_meta[cluster.id] = cluster
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
        cluster_count = counts.get(cid, 0)
        meta = cluster_meta.get(cid)
        if meta is None:
            continue
        if cid in priority and cluster_count == 0:
            # Still surface the maintenance queue row, marked empty.
            out.append(f"| `{cid}` | 0 | — | — | {meta.lever} |")
            continue
        pct = f"{100.0 * cluster_count / total:.1f}%"
        affected_models = ", ".join(sorted(models_per_cluster.get(cid, set())))
        out.append(f"| `{cid}` | {cluster_count} | {pct} | {affected_models} | {meta.lever} |")
    out.append("")
    return out


def _render_ecode_histogram(loaded: LoadedTraces) -> list[str]:
    counts: dict[str, int] = {}
    for _row, _attempt_name, attempt in _iter_failed_attempts(loaded):
        haystack = (attempt.get("eval_detail") or "") + "\n" + (attempt.get("eval_raw_output") or "")
        seen_in_attempt: set[str] = set()
        for matched_code in _ECODE.findall(haystack):
            # Count each E-code at most once per attempt — otherwise a
            # diagnostic that repeats the code in a hint inflates counts.
            if matched_code in seen_in_attempt:
                continue
            seen_in_attempt.add(matched_code)
            counts[matched_code] = counts.get(matched_code, 0) + 1

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


def _render_failure_detail(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("## Failure detail by cluster\n")
    # Bucket (row, attempt_name) by cluster_id.
    buckets: dict[str, list[tuple[dict, str, dict]]] = {}
    cluster_meta: dict[str, clusters.Cluster] = {}
    for row, attempt_name, attempt in _iter_failed_attempts(loaded):
        cluster = clusters.classify_failure(row, attempt=attempt_name)
        if cluster is None:
            continue
        buckets.setdefault(cluster.id, []).append((row, attempt_name, attempt))
        cluster_meta[cluster.id] = cluster
    if not buckets:
        out.append("_No failures in the loaded traces._")
        out.append("")
        return out
    for cluster_id in sorted(buckets, key=lambda cid: (-len(buckets[cid]), cid)):
        meta = cluster_meta[cluster_id]
        cluster_items = buckets[cluster_id]
        out.append(f"### `{cluster_id}` — {meta.description}\n")
        out.append(f"<details><summary>{len(cluster_items)} failure(s)</summary>\n")
        for (row, attempt_name, attempt) in cluster_items:
            label = (f"- **{row['prompt_id']}** × `{row['model']}` "
                     f"(run {row['run_idx']}, attempt={attempt_name})")
            out.append(label)
            detail = (attempt.get("eval_detail") or "").strip()
            snippet = "\n".join(detail.splitlines()[:6])
            if snippet:
                out.append("  ```")
                for detail_line in snippet.splitlines():
                    out.append(f"  {detail_line}")
                out.append("  ```")
        out.append("\n</details>\n")
    return out


def _render_after_edit_diffs(loaded: LoadedTraces) -> list[str]:
    out: list[str] = []
    out.append("## After-edit failure diffs\n")
    after_edit_failures: list[dict] = []
    for row in loaded.rows:
        if row.get("final_pass"):
            continue
        first_attempt = row.get("first_attempt")
        edit_attempt = row.get("edit_attempt")
        if first_attempt is None or edit_attempt is None:
            continue
        if edit_attempt.get("eval_passed"):
            continue  # final_pass=False contradicts this, but be defensive
        after_edit_failures.append(row)
    if not after_edit_failures:
        out.append("_No after-edit failures in the loaded traces._")
        out.append("")
        return out
    out.append(f"_{len(after_edit_failures)} cell(s) where the edit-loop turn also failed._\n")
    for row in after_edit_failures:
        first_attempt = row["first_attempt"]
        edit_attempt = row["edit_attempt"]
        out.append(f"### `{row['prompt_id']}` × `{row['model']}` (run {row['run_idx']})\n")
        out.append("**Edit-attempt failure:**")
        edit_detail = (edit_attempt.get("eval_detail") or "").strip()
        if edit_detail:
            out.append("```")
            for detail_line in edit_detail.splitlines()[:8]:
                out.append(detail_line)
            out.append("```")
        first_program_lines = (first_attempt.get("program") or "").splitlines(keepends=True)
        edit_program_lines = (edit_attempt.get("program") or "").splitlines(keepends=True)
        diff_lines = list(difflib.unified_diff(
            first_program_lines,
            edit_program_lines,
            fromfile=f"{row['prompt_id']}-first.sigil",
            tofile=f"{row['prompt_id']}-edit.sigil",
            n=3,
        ))
        if diff_lines:
            out.append("**Diff (first → edit):**")
            out.append("```diff")
            for diff_line in diff_lines:
                out.append(diff_line.rstrip("\n"))
            out.append("```")
        else:
            out.append("_(no textual diff between first and edit programs)_")
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
