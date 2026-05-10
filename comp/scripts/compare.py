#!/usr/bin/env python3
"""
compare.py — Cross-language LLM authorship comparison harness.

For each (prompt × language × model) cell, runs N independent
Claude API sessions with a language-specific system prompt, extracts
the produced program from the response, hands it to the matching
language eval driver (compile + run + diff oracle), and records the
result. On first-shot failure, an edit-loop turn feeds the failure
back to the model; the second attempt is also recorded.

Mirrors `scripts/validate_spec.py`'s pattern (Claude API client,
edit-loop, K/N multi-run aggregation, JSONL trace + markdown report)
but is wired to:

- `comp/prompts.md`        cross-language prompts (C01..C10)
- `comp/contexts/<lang>.md` per-language system prompt prefix
- `comp/scripts/eval-<lang>.sh` per-language compile+run+oracle driver

Aggregates per (prompt, language, model) cell so the markdown report
makes language-level pass-rate differences visible.

Usage:
    export ANTHROPIC_API_KEY=sk-...
    python3 comp/scripts/compare.py
    python3 comp/scripts/compare.py --filter C01 --runs 3
    python3 comp/scripts/compare.py --langs sigil,python --models claude-opus-4-7

Exit codes:
    0  — every cell passed (all runs, all prompts, all langs, all models)
    1  — at least one cell failed
    2  — harness error (missing env, missing files, bad config)
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from typing import Optional

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
COMP_DIR = REPO_ROOT / "comp"
PROMPTS_PATH = COMP_DIR / "prompts.md"
CONTEXTS_DIR = COMP_DIR / "contexts"
EVAL_SCRIPTS_DIR = COMP_DIR / "scripts"
LOG_DIR = COMP_DIR / "log"
SPEC_PATH = REPO_ROOT / "spec" / "language.md"

ANTHROPIC_API_URL = "https://api.anthropic.com/v1/messages"
ANTHROPIC_API_VERSION = "2023-06-01"

DEFAULT_LANGUAGES = ["sigil", "python", "go"]
DEFAULT_MODELS = ["claude-opus-4-7", "claude-sonnet-4-6"]
DEFAULT_MAX_CONCURRENCY = 4
DEFAULT_MAX_TOKENS = 4096
DEFAULT_REQUEST_TIMEOUT_S = 120
DEFAULT_EVAL_TIMEOUT_S = 60

# Token in `comp/contexts/sigil.md` that gets substituted with the
# contents of `spec/language.md` at session-prep time. Other languages
# don't need substitution; their contexts are self-contained.
SIGIL_SPEC_PLACEHOLDER = "{{SPEC_LANGUAGE_MD}}"


# ---------------------------------------------------------------------------
# Prompt-bank parser
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Prompt:
    id: str  # "C01" .. "C10"
    title: str
    prompt_text: str  # language-neutral problem statement
    notes: str = ""


_HEADING_RE = re.compile(r"^## (C\d+) — (.+)$")
_PROMPT_FIELD_RE = re.compile(r"^\*\*Prompt:\*\*\s*(.*)$")
_NOTES_RE = re.compile(r"^\*\*Notes:\*\*\s*(.*)$")


def parse_prompts(path: pathlib.Path) -> list[Prompt]:
    """Parse comp/prompts.md into structured Prompt records.

    Note: oracle parsing is deferred entirely to the per-language eval
    drivers (eval-{sigil,python,go}.sh) — they parse the same prompts.md
    file via awk to extract their oracle. We only need the prompt text
    here (what the LLM sees) plus the id (for routing to the driver)."""
    text = path.read_text()
    lines = text.split("\n")
    prompts: list[Prompt] = []
    i = 0
    while i < len(lines):
        m = _HEADING_RE.match(lines[i])
        if not m:
            i += 1
            continue
        pid = m.group(1)
        title = m.group(2).strip()

        section_start = i + 1
        section_end = len(lines)
        for j in range(section_start, len(lines)):
            if lines[j].startswith("## ") or lines[j].strip() == "---":
                section_end = j
                break

        section = lines[section_start:section_end]
        prompt_text = _extract_prompt(section)
        notes = _extract_notes(section)
        prompts.append(Prompt(
            id=pid,
            title=title,
            prompt_text=prompt_text,
            notes=notes,
        ))
        i = section_end
    return prompts


def _extract_prompt(section: list[str]) -> str:
    out: list[str] = []
    in_prompt = False
    for ln in section:
        if _PROMPT_FIELD_RE.match(ln):
            in_prompt = True
            first = _PROMPT_FIELD_RE.match(ln).group(1)
            if first:
                out.append(first)
            continue
        if in_prompt:
            if ln.startswith("**Oracle") or ln.startswith("**Notes"):
                break
            out.append(ln)
    return "\n".join(out).strip()


def _extract_notes(section: list[str]) -> str:
    out: list[str] = []
    in_notes = False
    for ln in section:
        if _NOTES_RE.match(ln):
            in_notes = True
            first = _NOTES_RE.match(ln).group(1)
            if first:
                out.append(first)
            continue
        if in_notes:
            if ln.startswith("**") or ln.startswith("##"):
                break
            out.append(ln)
    return "\n".join(out).strip()


# ---------------------------------------------------------------------------
# Per-language system prompt loading
# ---------------------------------------------------------------------------


def load_system_prompt(lang: str, spec_text: str) -> str:
    """Load comp/contexts/<lang>.md and substitute {{SPEC_LANGUAGE_MD}}
    if present (the Sigil context inlines the full spec)."""
    ctx_path = CONTEXTS_DIR / f"{lang}.md"
    if not ctx_path.exists():
        raise FileNotFoundError(f"context file missing: {ctx_path}")
    body = ctx_path.read_text()
    if SIGIL_SPEC_PLACEHOLDER in body:
        body = body.replace(SIGIL_SPEC_PLACEHOLDER, spec_text)
    return body


# ---------------------------------------------------------------------------
# Anthropic API client (mirrors scripts/validate_spec.py)
# ---------------------------------------------------------------------------


def call_claude(
    *,
    api_key: str,
    model: str,
    system: str,
    messages: list[dict],
    max_tokens: int = DEFAULT_MAX_TOKENS,
    timeout_s: int = DEFAULT_REQUEST_TIMEOUT_S,
) -> str:
    body = json.dumps({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": messages,
    }).encode("utf-8")

    last_exc: Optional[Exception] = None
    for attempt in range(3):
        if attempt > 0:
            time.sleep(2 ** attempt)
        req = urllib.request.Request(
            ANTHROPIC_API_URL,
            data=body,
            headers={
                "x-api-key": api_key,
                "anthropic-version": ANTHROPIC_API_VERSION,
                "content-type": "application/json",
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=timeout_s) as resp:
                payload = json.loads(resp.read().decode("utf-8"))
                return _extract_text_content(payload)
        except urllib.error.HTTPError as e:
            last_exc = e
            if e.code == 429 or 500 <= e.code < 600:
                continue
            err_body = e.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"Claude API HTTP {e.code}: {err_body}") from e
        except urllib.error.URLError as e:
            last_exc = e
            continue
    raise RuntimeError(f"Claude API failed after retries: {last_exc}")


def _extract_text_content(payload: dict) -> str:
    content = payload.get("content", [])
    out: list[str] = []
    for block in content:
        if block.get("type") == "text":
            out.append(block.get("text", ""))
    return "".join(out)


# ---------------------------------------------------------------------------
# Program extraction (per-language)
# ---------------------------------------------------------------------------


def extract_program(response: str, lang: str) -> Optional[str]:
    """Extract the language's program from a fenced code block in the
    response. Prefer ```<lang> tagged fences; fall back to any ```
    fence. Returns None if nothing parseable."""
    tagged_re = re.compile(rf"```{re.escape(lang)}\n(.*?)```", re.DOTALL)
    matches = tagged_re.findall(response)
    if matches:
        return matches[-1].rstrip() + "\n"
    any_fence_re = re.compile(r"```(?:[a-zA-Z0-9_+\-]*)?\n(.*?)```", re.DOTALL)
    matches = any_fence_re.findall(response)
    if matches:
        return matches[-1].rstrip() + "\n"
    stripped = response.strip()
    if stripped:
        return stripped + "\n"
    return None


# ---------------------------------------------------------------------------
# Eval driver invocation (shell out to comp/scripts/eval-<lang>.sh)
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class EvalResult:
    passed: bool
    category: Optional[str]  # "compile" / "runtime" / "stdout" / "timeout" / etc.
    detail: str
    raw_output: str  # stdout from the eval driver


def run_eval(program: str, prompt_id: str, lang: str) -> EvalResult:
    """Write `program` to a temp file with a language-appropriate
    suffix, then invoke comp/scripts/eval-<lang>.sh. Parses the
    driver's `pass` / `fail: <category> — <details>` output."""
    suffix_by_lang = {"sigil": ".sigil", "python": ".py", "go": ".go"}
    suffix = suffix_by_lang.get(lang)
    if suffix is None:
        return EvalResult(
            passed=False,
            category="harness",
            detail=f"unknown language {lang!r}",
            raw_output="",
        )
    driver = EVAL_SCRIPTS_DIR / f"eval-{lang}.sh"
    if not driver.exists():
        return EvalResult(
            passed=False,
            category="harness",
            detail=f"eval driver missing: {driver}",
            raw_output="",
        )
    tmpdir = pathlib.Path(tempfile.mkdtemp(prefix=f"comp-{prompt_id}-{lang}-"))
    src_path = tmpdir / f"program{suffix}"
    src_path.write_text(program)
    try:
        proc = subprocess.run(
            [str(driver), str(src_path), prompt_id],
            capture_output=True,
            text=True,
            timeout=DEFAULT_EVAL_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired:
        return EvalResult(
            passed=False,
            category="timeout",
            detail=f"eval driver exceeded {DEFAULT_EVAL_TIMEOUT_S}s",
            raw_output="",
        )
    stdout = proc.stdout.strip()
    if proc.returncode == 0 and stdout.startswith("pass"):
        return EvalResult(
            passed=True,
            category=None,
            detail="pass",
            raw_output=stdout,
        )
    # Failure — parse "fail: <category> — <details>" if present.
    category, detail = "unknown", stdout or proc.stderr.strip()
    m = re.match(r"fail:\s*(\S+)\s*—\s*(.*)$", stdout, re.DOTALL)
    if m:
        category = m.group(1)
        detail = m.group(2).strip()
    return EvalResult(
        passed=False,
        category=category,
        detail=detail,
        raw_output=stdout or proc.stderr.strip(),
    )


# ---------------------------------------------------------------------------
# Cell-run + edit loop
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class AttemptResult:
    program: Optional[str]
    raw_response: str
    eval: Optional[EvalResult]


@dataclasses.dataclass
class CellResult:
    prompt_id: str
    language: str
    model: str
    run_idx: int
    first_attempt: Optional[AttemptResult]
    edit_attempt: Optional[AttemptResult]
    final_pass: bool
    error: Optional[str]


def _execute_attempt(prompt_id: str, lang: str, raw_response: str) -> AttemptResult:
    program = extract_program(raw_response, lang)
    if program is None:
        return AttemptResult(
            program=None,
            raw_response=raw_response,
            eval=EvalResult(passed=False, category="no-code-block",
                            detail="model produced no extractable program",
                            raw_output=""),
        )
    er = run_eval(program, prompt_id, lang)
    return AttemptResult(program=program, raw_response=raw_response, eval=er)


def _build_edit_followup(prior: AttemptResult, lang: str) -> str:
    parts = ["Your program failed the evaluation. Details:\n"]
    if prior.eval is None:
        parts.append("(no eval result captured)\n")
    elif prior.eval.passed:
        parts.append("(unexpected: previous attempt passed)\n")
    else:
        cat = prior.eval.category or "unknown"
        parts.append(f"Failure category: **{cat}**\n")
        if prior.eval.detail:
            parts.append("```\n")
            parts.append(prior.eval.detail.strip())
            parts.append("\n```\n")
    parts.append(
        f"\nProduce a corrected {lang} program. Respond with ONLY the corrected "
        f"program inside a single ```{lang} ... ``` fenced block."
    )
    return "".join(parts)


def run_one_cell(
    *,
    prompt: Prompt,
    language: str,
    model: str,
    run_idx: int,
    api_key: str,
    spec_text: str,
    edit_loop: bool,
) -> CellResult:
    try:
        system = load_system_prompt(language, spec_text)
    except FileNotFoundError as e:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=None, edit_attempt=None, final_pass=False,
            error=f"system-prompt load failed: {e}",
        )
    messages = [{"role": "user", "content": prompt.prompt_text}]
    try:
        first_response = call_claude(
            api_key=api_key, model=model, system=system, messages=messages,
        )
    except Exception as e:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=None, edit_attempt=None, final_pass=False,
            error=f"api call failed: {e}",
        )
    first_attempt = _execute_attempt(prompt.id, language, first_response)
    first_passed = first_attempt.eval is not None and first_attempt.eval.passed
    if first_passed or not edit_loop:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=first_attempt, edit_attempt=None,
            final_pass=first_passed, error=None,
        )
    followup = _build_edit_followup(first_attempt, language)
    edit_messages = messages + [
        {"role": "assistant", "content": first_response},
        {"role": "user", "content": followup},
    ]
    try:
        edit_response = call_claude(
            api_key=api_key, model=model, system=system, messages=edit_messages,
        )
    except Exception as e:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=first_attempt, edit_attempt=None, final_pass=False,
            error=f"edit-loop api call failed: {e}",
        )
    edit_attempt = _execute_attempt(prompt.id, language, edit_response)
    edit_passed = edit_attempt.eval is not None and edit_attempt.eval.passed
    return CellResult(
        prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
        first_attempt=first_attempt, edit_attempt=edit_attempt,
        final_pass=edit_passed, error=None,
    )


# ---------------------------------------------------------------------------
# Output: JSONL trace + markdown report
# ---------------------------------------------------------------------------


def _attempt_to_json(a: Optional[AttemptResult]) -> Optional[dict]:
    if a is None:
        return None
    return {
        "program": a.program,
        "raw_response": a.raw_response,
        "eval_passed": a.eval.passed if a.eval else None,
        "eval_category": a.eval.category if a.eval else None,
        "eval_detail": a.eval.detail if a.eval else None,
        "eval_raw_output": a.eval.raw_output if a.eval else None,
    }


def write_jsonl(results: list[CellResult], path: pathlib.Path) -> None:
    with path.open("w") as f:
        for r in results:
            f.write(json.dumps({
                "prompt_id": r.prompt_id,
                "language": r.language,
                "model": r.model,
                "run_idx": r.run_idx,
                "first_attempt": _attempt_to_json(r.first_attempt),
                "edit_attempt": _attempt_to_json(r.edit_attempt),
                "final_pass": r.final_pass,
                "error": r.error,
            }) + "\n")


def render_markdown_report(
    results: list[CellResult],
    prompts: list[Prompt],
    languages: list[str],
    models: list[str],
    runs: int,
    out_path: pathlib.Path,
    jsonl_path: pathlib.Path,
) -> None:
    """Per-(lang, model) aggregate pass rates + per-(prompt, lang, model)
    K/N matrix + grouped failure detail. Cells render as ✅/❌ for
    runs=1 and ✅/⚠️/❌ K/N for runs>1 (mirrors scripts/validate_spec.py)."""
    # Group by (language, model, prompt_id). Each cell holds N results.
    by_cell: dict[tuple[str, str, str], list[CellResult]] = {}
    for r in results:
        by_cell.setdefault((r.language, r.model, r.prompt_id), []).append(r)

    lines: list[str] = []
    lines.append(f"# Cross-language comparison log — run {time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n")
    try:
        rel_jsonl = jsonl_path.relative_to(REPO_ROOT)
        lines.append(f"Trace: `{rel_jsonl}`")
    except ValueError:
        lines.append(f"Trace: `{jsonl_path}`")
    lines.append(f"Runs per (prompt, language, model): **{runs}**")
    lines.append("")

    # Aggregate pass-rate table per (language, model). Avg across all runs and prompts.
    lines.append("## Pass rates by language × model\n")
    lines.append("| Language | Model | First-pass | Final-pass |")
    lines.append("|---|---|---|---|")
    for lang in languages:
        for model in models:
            cell_results = [r for r in results if r.language == lang and r.model == model]
            n = len(cell_results)
            if n == 0:
                continue
            first_pass = sum(
                1 for r in cell_results
                if r.first_attempt and r.first_attempt.eval and r.first_attempt.eval.passed
            )
            final = sum(1 for r in cell_results if r.final_pass)

            def pct(k: int) -> str:
                return f"{k}/{n} ({100.0 * k / n:.1f}%)"

            lines.append(f"| `{lang}` | `{model}` | {pct(first_pass)} | {pct(final)} |")
    lines.append("")

    # Per-prompt × per-cell matrix.
    def cell_first(lang: str, model: str, prompt_id: str) -> str:
        rs = by_cell.get((lang, model, prompt_id), [])
        if not rs:
            return "—"
        passed = sum(
            1 for r in rs
            if r.first_attempt and r.first_attempt.eval and r.first_attempt.eval.passed
        )
        if runs == 1:
            return "✅" if passed == 1 else "❌"
        if passed == runs:
            return f"✅ {passed}/{runs}"
        if passed == 0:
            return f"❌ {passed}/{runs}"
        return f"⚠️ {passed}/{runs}"

    def cell_final(lang: str, model: str, prompt_id: str) -> str:
        rs = by_cell.get((lang, model, prompt_id), [])
        if not rs:
            return "—"
        passed = sum(1 for r in rs if r.final_pass)
        if runs == 1:
            return "✅" if passed == 1 else "❌"
        if passed == runs:
            return f"✅ {passed}/{runs}"
        if passed == 0:
            return f"❌ {passed}/{runs}"
        return f"⚠️ {passed}/{runs}"

    lines.append("## Per-prompt × language × model — first-pass\n")
    if runs > 1:
        lines.append("Cells: ✅ all runs passed; ⚠️ some runs passed (stochastic); ❌ all runs failed.")
        lines.append("")
    headers = ["Prompt"]
    for lang in languages:
        for model in models:
            headers.append(f"`{lang}` `{model}`")
    lines.append("| " + " | ".join(headers) + " |")
    lines.append("|---" * len(headers) + "|")
    for p in prompts:
        cells = [f"**{p.id}** — {p.title}"]
        for lang in languages:
            for model in models:
                cells.append(cell_first(lang, model, p.id))
        lines.append("| " + " | ".join(cells) + " |")
    lines.append("")

    lines.append("## Per-prompt × language × model — final-pass (first OR after edit)\n")
    lines.append("| " + " | ".join(headers) + " |")
    lines.append("|---" * len(headers) + "|")
    for p in prompts:
        cells = [f"**{p.id}** — {p.title}"]
        for lang in languages:
            for model in models:
                cells.append(cell_final(lang, model, p.id))
        lines.append("| " + " | ".join(cells) + " |")
    lines.append("")

    # Failure category histogram per language. Aggregate across all
    # failed runs (first_attempt failures even if edit_attempt passed
    # are still logged — they tell us what shapes trip the LLM).
    cat_counts: dict[tuple[str, str], int] = {}
    for r in results:
        for a in (r.first_attempt, r.edit_attempt):
            if a is None or a.eval is None or a.eval.passed:
                continue
            key = (r.language, a.eval.category or "unknown")
            cat_counts[key] = cat_counts.get(key, 0) + 1
    if cat_counts:
        lines.append("## Failure-category histogram\n")
        lines.append("Counts every failed attempt (first OR edit), by language. Reveals "
                     "whether each language fails compile-side or runtime-side dominantly.\n")
        all_cats = sorted({cat for (_, cat) in cat_counts.keys()})
        header = ["Language"] + all_cats
        lines.append("| " + " | ".join(header) + " |")
        lines.append("|---" * len(header) + "|")
        for lang in languages:
            row = [f"`{lang}`"]
            for cat in all_cats:
                row.append(str(cat_counts.get((lang, cat), 0)))
            lines.append("| " + " | ".join(row) + " |")
        lines.append("")

    # Failure detail. Group by (lang, model, prompt) cell.
    cells_with_failures: list[tuple[str, str, str]] = []
    for (lang, model, pid), rs in by_cell.items():
        if any(not r.final_pass for r in rs):
            cells_with_failures.append((lang, model, pid))
    cells_with_failures.sort(key=lambda x: (x[2], x[0], x[1]))

    if cells_with_failures:
        total_failed_runs = sum(
            sum(1 for r in by_cell[(lang, model, pid)] if not r.final_pass)
            for (lang, model, pid) in cells_with_failures
        )
        lines.append(f"## Failures ({len(cells_with_failures)} cell(s), {total_failed_runs} run(s))\n")
        for (lang, model, pid) in cells_with_failures:
            rs = sorted(by_cell[(lang, model, pid)], key=lambda r: r.run_idx)
            failed_rs = [r for r in rs if not r.final_pass]
            label = f"### `{pid}` × `{lang}` × `{model}`"
            if runs > 1:
                label += f" — {len(failed_rs)}/{runs} runs failed"
            lines.append(label + "\n")
            for r in failed_rs:
                if runs > 1:
                    lines.append(f"**Run {r.run_idx}:**")
                if r.error:
                    lines.append(f"Harness error: {r.error}\n")
                else:
                    final = r.edit_attempt or r.first_attempt
                    if final and final.eval:
                        cat = final.eval.category or "unknown"
                        lines.append(f"Final attempt category: **{cat}**\n")
                        if final.eval.detail:
                            lines.append("```")
                            lines.append(final.eval.detail.strip()[:2000])
                            lines.append("```")
                lines.append("")

    out_path.write_text("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description="Cross-language LLM authorship comparison.")
    parser.add_argument(
        "--langs",
        default=",".join(DEFAULT_LANGUAGES),
        help=f"Comma-separated language list (default: {','.join(DEFAULT_LANGUAGES)})",
    )
    parser.add_argument(
        "--models",
        default=",".join(DEFAULT_MODELS),
        help=f"Comma-separated model IDs (default: {','.join(DEFAULT_MODELS)})",
    )
    parser.add_argument(
        "--filter",
        default=None,
        help="Only run prompts whose id matches this regex (e.g., 'C01' or '^C0[1-3]')",
    )
    parser.add_argument(
        "--max-concurrency",
        type=int,
        default=DEFAULT_MAX_CONCURRENCY,
        help=f"Max parallel cells (default: {DEFAULT_MAX_CONCURRENCY})",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=1,
        help="Number of independent runs per (prompt, lang, model). >1 enables "
             "K/N aggregation (default: 1)",
    )
    parser.add_argument(
        "--no-edit-loop",
        action="store_true",
        help="Disable the after-one-edit retry on first failure",
    )
    parser.add_argument(
        "--results-dir",
        default=str(LOG_DIR),
        help=f"Directory for JSONL trace + comparison-log.md (default: {LOG_DIR})",
    )
    args = parser.parse_args()

    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("compare.py: ANTHROPIC_API_KEY not set in environment", file=sys.stderr)
        return 2
    if not PROMPTS_PATH.exists():
        print(f"compare.py: prompts missing at {PROMPTS_PATH}", file=sys.stderr)
        return 2
    if not CONTEXTS_DIR.is_dir():
        print(f"compare.py: contexts dir missing at {CONTEXTS_DIR}", file=sys.stderr)
        return 2
    if not EVAL_SCRIPTS_DIR.is_dir():
        print(f"compare.py: scripts dir missing at {EVAL_SCRIPTS_DIR}", file=sys.stderr)
        return 2
    if not SPEC_PATH.exists():
        print(f"compare.py: spec missing at {SPEC_PATH} (needed for sigil context)", file=sys.stderr)
        return 2
    if args.runs < 1:
        print(f"compare.py: --runs must be >= 1, got {args.runs}", file=sys.stderr)
        return 2

    languages = [s.strip() for s in args.langs.split(",") if s.strip()]
    models = [s.strip() for s in args.models.split(",") if s.strip()]
    prompts = parse_prompts(PROMPTS_PATH)
    if args.filter:
        pat = re.compile(args.filter)
        prompts = [p for p in prompts if pat.search(p.id)]
    if not prompts:
        print(f"compare.py: no prompts matched filter {args.filter!r}", file=sys.stderr)
        return 2

    # Verify each requested language has its eval driver and context.
    for lang in languages:
        ctx = CONTEXTS_DIR / f"{lang}.md"
        drv = EVAL_SCRIPTS_DIR / f"eval-{lang}.sh"
        if not ctx.exists():
            print(f"compare.py: missing context {ctx}", file=sys.stderr)
            return 2
        if not drv.exists():
            print(f"compare.py: missing eval driver {drv}", file=sys.stderr)
            return 2

    spec_text = SPEC_PATH.read_text()
    edit_loop = not args.no_edit_loop

    total = len(prompts) * len(languages) * len(models) * args.runs
    print(f"compare.py: {len(prompts)} prompt(s) × {len(languages)} lang(s) × "
          f"{len(models)} model(s) × {args.runs} run(s) = {total} API calls; "
          f"concurrency={args.max_concurrency}; edit_loop={edit_loop}", file=sys.stderr)

    work = [(p, lang, m, run_idx)
            for lang in languages
            for m in models
            for p in prompts
            for run_idx in range(args.runs)]
    results: list[CellResult] = []

    started = time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_concurrency) as pool:
        future_to_key = {
            pool.submit(
                run_one_cell,
                prompt=p, language=lang, model=m, run_idx=run_idx,
                api_key=api_key, spec_text=spec_text, edit_loop=edit_loop,
            ): (p.id, lang, m, run_idx)
            for p, lang, m, run_idx in work
        }
        completed = 0
        for fut in concurrent.futures.as_completed(future_to_key):
            pid, lang, model, run_idx = future_to_key[fut]
            try:
                result = fut.result()
            except Exception as e:
                result = CellResult(
                    prompt_id=pid, language=lang, model=model, run_idx=run_idx,
                    first_attempt=None, edit_attempt=None,
                    final_pass=False, error=f"unhandled: {e}",
                )
            results.append(result)
            completed += 1
            mark = "✅" if result.final_pass else "❌"
            extra = ""
            if result.first_attempt and result.first_attempt.eval and result.first_attempt.eval.passed:
                extra = " (first try)"
            elif result.final_pass:
                extra = " (after edit)"
            elif result.error:
                extra = f" (error: {result.error[:60]})"
            run_label = f" run={run_idx}" if args.runs > 1 else ""
            print(f"  [{completed:>3}/{len(work)}] {mark} {pid} × {lang} × {model}{run_label}{extra}",
                  file=sys.stderr)

    elapsed = time.time() - started
    print(f"compare.py: completed {len(results)} runs in {elapsed:.1f}s", file=sys.stderr)

    results_dir = pathlib.Path(args.results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%dT%H%M%S")
    jsonl_path = results_dir / f"comparison-results-{timestamp}.jsonl"
    md_path = results_dir / "comparison-log.md"
    write_jsonl(results, jsonl_path)
    render_markdown_report(results, prompts, languages, models, args.runs, md_path, jsonl_path)
    print(f"compare.py: trace -> {jsonl_path}", file=sys.stderr)
    print(f"compare.py: report -> {md_path}", file=sys.stderr)

    failed = sum(1 for r in results if not r.final_pass)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
