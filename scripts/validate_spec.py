#!/usr/bin/env python3
"""
validate_spec.py — Spec validation harness for Sigil v1.

Reads spec/validation-prompts.md, runs each prompt against a fresh
Claude API session given only spec/language.md as context, compiles
+ runs the produced program, and compares stdout + exit to the
oracle. Aggregates pass rates per model and writes a structured
report to spec/validation-log.md (markdown) and a per-prompt
JSONL trace to spec/validation-results-<timestamp>.jsonl.

Usage:
    export ANTHROPIC_API_KEY=sk-...
    python3 scripts/validate_spec.py
    python3 scripts/validate_spec.py --filter P05
    python3 scripts/validate_spec.py --models claude-opus-4-7,claude-sonnet-4-6
    python3 scripts/validate_spec.py --max-concurrency 4 --no-edit-loop

Exit codes:
    0  — all prompts passed (first attempt OR after one edit) for every model
    1  — at least one prompt failed for some model
    2  — harness error (missing env, bad spec file, etc.)

Per-prompt grading:
    - first_compile : did the model's first program compile?
    - first_run    : did first program run + match the oracle?
    - after_edit   : if first failed, did the one-edit retry pass?
    - final_pass   : first_run OR after_edit
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

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
SPEC_PATH = REPO_ROOT / "spec" / "language.md"
PROMPTS_PATH = REPO_ROOT / "spec" / "validation-prompts.md"
SIGIL_BIN = REPO_ROOT / "target" / "release" / "sigil"

ANTHROPIC_API_URL = "https://api.anthropic.com/v1/messages"
ANTHROPIC_API_VERSION = "2023-06-01"

DEFAULT_MODELS = ["claude-opus-4-7", "claude-sonnet-4-6"]
DEFAULT_MAX_CONCURRENCY = 4
DEFAULT_MAX_TOKENS = 4096
DEFAULT_REQUEST_TIMEOUT_S = 120
DEFAULT_COMPILE_TIMEOUT_S = 60
DEFAULT_RUN_TIMEOUT_S = 30


# ---------------------------------------------------------------------------
# Prompt-bank parser
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Prompt:
    id: str  # "P01" .. "P62"
    title: str
    prompt_text: str
    oracle_stdout: str  # exact bytes; "" if "*(empty)*"
    oracle_exit: int
    oracle_stderr: Optional[str] = None  # None = unchecked; "" = must be empty
    notes: str = ""


_HEADING_RE = re.compile(r"^## (P\d+) — (.+)$")
_PROMPT_FIELD_RE = re.compile(r"^\*\*Prompt:\*\*\s*(.*)$")
_ORACLE_STDOUT_HEADER_RE = re.compile(r"^\*\*Oracle \(stdout\):\*\*\s*(.*)$")
_ORACLE_EXIT_RE = re.compile(r"^\*\*Oracle \(exit\):\*\*\s*`(-?\d+)`")
_ORACLE_STDERR_HEADER_RE = re.compile(r"^\*\*Oracle \(stderr\):\*\*\s*(.*)$")
_ORACLE_NOTES_RE = re.compile(r"^\*\*Oracle \(notes\):\*\*")
_EMPTY_MARKER_RE = re.compile(r"\*\(empty\)\*")


def parse_prompts(path: pathlib.Path) -> list[Prompt]:
    """Parse spec/validation-prompts.md into structured Prompt records."""
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

        # Find the bounds: from this heading to the next "## " heading or EOF.
        section_start = i + 1
        section_end = len(lines)
        for j in range(section_start, len(lines)):
            if lines[j].startswith("## "):
                section_end = j
                break

        section = lines[section_start:section_end]
        prompt_text = _extract_prompt(section)
        oracle_stdout = _extract_oracle_stdout(section)
        oracle_exit = _extract_oracle_exit(section)
        oracle_stderr = _extract_oracle_stderr(section)
        notes = _extract_notes(section)

        prompts.append(Prompt(
            id=pid,
            title=title,
            prompt_text=prompt_text,
            oracle_stdout=oracle_stdout,
            oracle_exit=oracle_exit,
            oracle_stderr=oracle_stderr,
            notes=notes,
        ))
        i = section_end
    return prompts


def _extract_prompt(section: list[str]) -> str:
    """Extract the prompt text — everything from '**Prompt:**' until the
    next '**Oracle' field. Joins multi-paragraph prompts."""
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
            if ln.startswith("**Oracle"):
                break
            out.append(ln)
    return "\n".join(out).strip()


def _extract_oracle_stdout(section: list[str]) -> str:
    """Extract the expected stdout. Returns '' for '*(empty)*' marker;
    otherwise returns the contents of the next ``` fence verbatim."""
    for idx, ln in enumerate(section):
        m = _ORACLE_STDOUT_HEADER_RE.match(ln)
        if not m:
            continue
        rest = m.group(1)
        if _EMPTY_MARKER_RE.search(rest):
            return ""
        # Look ahead for the opening ``` fence.
        for j in range(idx + 1, len(section)):
            if section[j].strip() == "```":
                # Found opening; collect until closing fence.
                body: list[str] = []
                for k in range(j + 1, len(section)):
                    if section[k].strip() == "```":
                        return "\n".join(body) + "\n"
                    body.append(section[k])
                raise ValueError(f"unterminated stdout fence in prompt section: {section[:5]}")
            if section[j].strip().startswith("**"):
                # Hit another field before the fence — empty stdout treated as ""
                return ""
        return ""
    return ""


def _extract_oracle_exit(section: list[str]) -> int:
    for ln in section:
        m = _ORACLE_EXIT_RE.match(ln)
        if m:
            return int(m.group(1))
    raise ValueError(f"no oracle exit found in section: {section[:5]}")


def _extract_oracle_stderr(section: list[str]) -> Optional[str]:
    for idx, ln in enumerate(section):
        m = _ORACLE_STDERR_HEADER_RE.match(ln)
        if not m:
            continue
        rest = m.group(1)
        if _EMPTY_MARKER_RE.search(rest):
            return ""
        # Same fence-extraction as stdout.
        for j in range(idx + 1, len(section)):
            if section[j].strip() == "```":
                body: list[str] = []
                for k in range(j + 1, len(section)):
                    if section[k].strip() == "```":
                        return "\n".join(body) + "\n"
                    body.append(section[k])
                break
            if section[j].strip().startswith("**"):
                break
        return ""
    return None


def _extract_notes(section: list[str]) -> str:
    out: list[str] = []
    in_notes = False
    for ln in section:
        if _ORACLE_NOTES_RE.match(ln):
            in_notes = True
            stripped = ln[len("**Oracle (notes):**"):].strip()
            if stripped:
                out.append(stripped)
            continue
        if in_notes:
            if ln.startswith("**") or ln.startswith("## "):
                break
            out.append(ln)
    return "\n".join(out).strip()


# ---------------------------------------------------------------------------
# Anthropic API client
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
    """Send a Messages API request and return the assistant's text content.
    Raises on transport / API errors. Retries 5xx + rate-limit (429) with
    exponential backoff up to 3 attempts."""
    body = json.dumps({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": messages,
    }).encode("utf-8")

    last_exc: Optional[Exception] = None
    for attempt in range(3):
        if attempt > 0:
            time.sleep(2 ** attempt)  # 2s, 4s
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
    """Extract concatenated text from a Messages API response payload."""
    content = payload.get("content", [])
    out: list[str] = []
    for block in content:
        if block.get("type") == "text":
            out.append(block.get("text", ""))
    return "".join(out)


# ---------------------------------------------------------------------------
# Program extraction
# ---------------------------------------------------------------------------


_SIGIL_FENCE_RE = re.compile(r"```(?:sigil)?\n(.*?)```", re.DOTALL)


def extract_sigil_program(response: str) -> Optional[str]:
    """Extract the Sigil program from the model's response. Prefers fenced
    blocks tagged 'sigil'; falls back to any ``` fence; returns None if
    nothing parseable."""
    # Prefer ```sigil fences first.
    sigil_blocks = re.findall(r"```sigil\n(.*?)```", response, re.DOTALL)
    if sigil_blocks:
        return sigil_blocks[-1].rstrip() + "\n"
    # Fall back to any code fence.
    matches = _SIGIL_FENCE_RE.findall(response)
    if matches:
        return matches[-1].rstrip() + "\n"
    # No fence — assume the whole response is the program.
    stripped = response.strip()
    if stripped:
        return stripped + "\n"
    return None


# ---------------------------------------------------------------------------
# Compile + run
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class CompileResult:
    success: bool
    stderr: str


@dataclasses.dataclass
class RunResult:
    stdout: str
    stderr: str
    exit_code: int
    timed_out: bool


def compile_program(source: str, prompt_id: str) -> tuple[CompileResult, Optional[pathlib.Path]]:
    """Write source to a temp file, compile via target/release/sigil. Returns
    (CompileResult, binary_path_or_None)."""
    if not SIGIL_BIN.exists():
        raise FileNotFoundError(f"sigil binary missing at {SIGIL_BIN}; run `cargo build --release`")
    tmpdir = pathlib.Path(tempfile.mkdtemp(prefix=f"sigil-validate-{prompt_id}-"))
    src_path = tmpdir / f"{prompt_id}.sigil"
    src_path.write_text(source)
    bin_path = tmpdir / prompt_id
    try:
        proc = subprocess.run(
            [str(SIGIL_BIN), str(src_path), "-o", str(bin_path)],
            capture_output=True,
            text=True,
            timeout=DEFAULT_COMPILE_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as e:
        return CompileResult(success=False, stderr=f"compile timed out after {DEFAULT_COMPILE_TIMEOUT_S}s"), None
    if proc.returncode != 0:
        return CompileResult(success=False, stderr=proc.stderr), None
    return CompileResult(success=True, stderr=proc.stderr), bin_path


def run_binary(bin_path: pathlib.Path) -> RunResult:
    try:
        proc = subprocess.run(
            [str(bin_path)],
            capture_output=True,
            text=True,
            timeout=DEFAULT_RUN_TIMEOUT_S,
        )
    except subprocess.TimeoutExpired as e:
        return RunResult(
            stdout=e.stdout.decode() if e.stdout else "",
            stderr=e.stderr.decode() if e.stderr else "",
            exit_code=-1,
            timed_out=True,
        )
    return RunResult(
        stdout=proc.stdout,
        stderr=proc.stderr,
        exit_code=proc.returncode if proc.returncode is not None else -1,
        timed_out=False,
    )


# ---------------------------------------------------------------------------
# Single-prompt run + edit loop
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class AttemptResult:
    program: Optional[str]
    raw_response: str
    compile: Optional[CompileResult]
    run: Optional[RunResult]
    oracle_match: bool
    oracle_diff: Optional[str]


@dataclasses.dataclass
class PromptResult:
    prompt_id: str
    model: str
    first_attempt: Optional[AttemptResult]
    edit_attempt: Optional[AttemptResult]
    final_pass: bool
    error: Optional[str]


SYSTEM_PROMPT_TEMPLATE = """You are an expert programmer authoring code in the Sigil programming language. The full Sigil v1 specification follows. Read it carefully — it is your only reference for the language's syntax, type system, effects, and standard library.

When the user asks you to write a program, respond with ONLY the Sigil program inside a single fenced code block tagged ```sigil ... ```. Do not include explanatory prose, do not output multiple program candidates, do not annotate the code with comments unless the comments are essential to the program's correctness. The program will be compiled and run as-is.

=== BEGIN spec/language.md ===
{spec}
=== END spec/language.md ===
"""


def _grade_attempt(prompt: Prompt, attempt: AttemptResult) -> tuple[bool, Optional[str]]:
    """Compare a successfully-run program's output against the oracle.
    Returns (oracle_match, diff_message_or_None)."""
    if attempt.compile is None or not attempt.compile.success:
        return False, "compile failed"
    if attempt.run is None:
        return False, "did not run"
    diffs: list[str] = []
    if attempt.run.stdout != prompt.oracle_stdout:
        diffs.append(
            f"stdout mismatch:\n"
            f"  expected ({len(prompt.oracle_stdout)} bytes): {prompt.oracle_stdout!r}\n"
            f"  actual   ({len(attempt.run.stdout)} bytes): {attempt.run.stdout!r}"
        )
    if attempt.run.exit_code != prompt.oracle_exit:
        diffs.append(
            f"exit mismatch: expected {prompt.oracle_exit}, got {attempt.run.exit_code}"
        )
    if prompt.oracle_stderr is not None and attempt.run.stderr != prompt.oracle_stderr:
        diffs.append(
            f"stderr mismatch:\n"
            f"  expected: {prompt.oracle_stderr!r}\n"
            f"  actual: {attempt.run.stderr!r}"
        )
    if attempt.run.timed_out:
        diffs.append(f"binary run timed out after {DEFAULT_RUN_TIMEOUT_S}s")
    if diffs:
        return False, "\n".join(diffs)
    return True, None


def _execute_attempt(prompt: Prompt, raw_response: str) -> AttemptResult:
    program = extract_sigil_program(raw_response)
    if program is None:
        return AttemptResult(
            program=None,
            raw_response=raw_response,
            compile=None,
            run=None,
            oracle_match=False,
            oracle_diff="model produced no extractable program",
        )
    compile_result, bin_path = compile_program(program, prompt.id)
    if not compile_result.success:
        return AttemptResult(
            program=program,
            raw_response=raw_response,
            compile=compile_result,
            run=None,
            oracle_match=False,
            oracle_diff="compile failed",
        )
    run_result = run_binary(bin_path)
    attempt = AttemptResult(
        program=program,
        raw_response=raw_response,
        compile=compile_result,
        run=run_result,
        oracle_match=False,
        oracle_diff=None,
    )
    matched, diff = _grade_attempt(prompt, attempt)
    attempt.oracle_match = matched
    attempt.oracle_diff = diff
    return attempt


def _build_edit_followup(prompt: Prompt, prior: AttemptResult) -> str:
    """Build the user message for the one-edit retry, given the prior
    attempt's failure mode."""
    parts = ["Your program failed to satisfy the oracle. Details:\n"]
    if prior.compile is not None and not prior.compile.success:
        parts.append("Compile error (stderr):\n```\n")
        parts.append(prior.compile.stderr.strip() or "(no stderr)")
        parts.append("\n```\n")
    elif prior.run is not None:
        parts.append(f"Compiled successfully but the program's output differs from the oracle:\n")
        parts.append(prior.oracle_diff or "(unknown diff)")
        parts.append("\n")
    elif prior.program is None:
        parts.append("The previous response did not contain a parseable Sigil program.\n")
        parts.append("Respond with the program inside a single ```sigil ... ``` fenced block.\n")
    parts.append(
        "\nProduce a corrected program. Respond with ONLY the corrected program "
        "inside a single ```sigil ... ``` fenced block."
    )
    return "".join(parts)


def run_one_prompt(
    *,
    prompt: Prompt,
    model: str,
    api_key: str,
    spec_text: str,
    edit_loop: bool,
) -> PromptResult:
    system = SYSTEM_PROMPT_TEMPLATE.format(spec=spec_text)
    messages = [{"role": "user", "content": prompt.prompt_text}]
    try:
        first_response = call_claude(
            api_key=api_key,
            model=model,
            system=system,
            messages=messages,
        )
    except Exception as e:
        return PromptResult(
            prompt_id=prompt.id,
            model=model,
            first_attempt=None,
            edit_attempt=None,
            final_pass=False,
            error=f"api call failed: {e}",
        )
    first_attempt = _execute_attempt(prompt, first_response)
    if first_attempt.oracle_match or not edit_loop:
        return PromptResult(
            prompt_id=prompt.id,
            model=model,
            first_attempt=first_attempt,
            edit_attempt=None,
            final_pass=first_attempt.oracle_match,
            error=None,
        )
    # First attempt failed — one-edit retry.
    followup = _build_edit_followup(prompt, first_attempt)
    edit_messages = messages + [
        {"role": "assistant", "content": first_response},
        {"role": "user", "content": followup},
    ]
    try:
        edit_response = call_claude(
            api_key=api_key,
            model=model,
            system=system,
            messages=edit_messages,
        )
    except Exception as e:
        return PromptResult(
            prompt_id=prompt.id,
            model=model,
            first_attempt=first_attempt,
            edit_attempt=None,
            final_pass=False,
            error=f"edit-loop api call failed: {e}",
        )
    edit_attempt = _execute_attempt(prompt, edit_response)
    return PromptResult(
        prompt_id=prompt.id,
        model=model,
        first_attempt=first_attempt,
        edit_attempt=edit_attempt,
        final_pass=edit_attempt.oracle_match,
        error=None,
    )


# ---------------------------------------------------------------------------
# Output: JSONL + markdown
# ---------------------------------------------------------------------------


def _attempt_to_json(a: Optional[AttemptResult]) -> Optional[dict]:
    if a is None:
        return None
    return {
        "program": a.program,
        "raw_response": a.raw_response,
        "compile_success": a.compile.success if a.compile else None,
        "compile_stderr": a.compile.stderr if a.compile else None,
        "run_stdout": a.run.stdout if a.run else None,
        "run_stderr": a.run.stderr if a.run else None,
        "run_exit": a.run.exit_code if a.run else None,
        "run_timed_out": a.run.timed_out if a.run else None,
        "oracle_match": a.oracle_match,
        "oracle_diff": a.oracle_diff,
    }


def write_jsonl(results: list[PromptResult], path: pathlib.Path) -> None:
    with path.open("w") as f:
        for r in results:
            f.write(json.dumps({
                "prompt_id": r.prompt_id,
                "model": r.model,
                "first_attempt": _attempt_to_json(r.first_attempt),
                "edit_attempt": _attempt_to_json(r.edit_attempt),
                "final_pass": r.final_pass,
                "error": r.error,
            }) + "\n")


def render_markdown_report(
    results: list[PromptResult],
    prompts: list[Prompt],
    models: list[str],
    out_path: pathlib.Path,
    jsonl_path: pathlib.Path,
) -> None:
    """Render a per-model + per-prompt markdown table to spec/validation-log.md."""
    by_model_prompt: dict[tuple[str, str], PromptResult] = {
        (r.model, r.prompt_id): r for r in results
    }

    lines: list[str] = []
    lines.append(f"# Spec validation log — run {time.strftime('%Y-%m-%dT%H:%M:%S%z')}\n")
    lines.append(f"Trace: `{jsonl_path.relative_to(REPO_ROOT)}`\n")
    lines.append("")

    # Aggregate pass-rate table.
    lines.append("## Pass rates\n")
    lines.append("| Model | First-compile | First-run | After-edit | Final-pass |")
    lines.append("|---|---|---|---|---|")
    for model in models:
        n = sum(1 for r in results if r.model == model)
        if n == 0:
            continue
        first_compile = sum(
            1 for r in results
            if r.model == model and r.first_attempt and r.first_attempt.compile and r.first_attempt.compile.success
        )
        first_run = sum(
            1 for r in results
            if r.model == model and r.first_attempt and r.first_attempt.oracle_match
        )
        after_edit = sum(
            1 for r in results
            if r.model == model and r.edit_attempt and r.edit_attempt.oracle_match
        )
        final = sum(1 for r in results if r.model == model and r.final_pass)

        def pct(k: int) -> str:
            return f"{k}/{n} ({100.0 * k / n:.1f}%)"

        lines.append(
            f"| `{model}` | {pct(first_compile)} | {pct(first_run)} | {pct(after_edit)} | {pct(final)} |"
        )
    lines.append("")

    # Per-prompt detail.
    lines.append("## Per-prompt results\n")
    lines.append("| Prompt | " + " | ".join(f"`{m}` first" for m in models)
                 + " | " + " | ".join(f"`{m}` final" for m in models) + " |")
    lines.append("|---" + "|---" * (2 * len(models)) + "|")
    for p in prompts:
        cells = [f"**{p.id}** — {p.title}"]
        for m in models:
            r = by_model_prompt.get((m, p.id))
            if r is None:
                cells.append("—")
            elif r.first_attempt and r.first_attempt.oracle_match:
                cells.append("✅")
            else:
                cells.append("❌")
        for m in models:
            r = by_model_prompt.get((m, p.id))
            if r is None:
                cells.append("—")
            elif r.final_pass:
                cells.append("✅")
            else:
                cells.append("❌")
        lines.append("| " + " | ".join(cells) + " |")
    lines.append("")

    # Failures with diffs.
    failures = [r for r in results if not r.final_pass]
    if failures:
        lines.append(f"## Failures ({len(failures)})\n")
        for r in failures:
            lines.append(f"### `{r.prompt_id}` × `{r.model}`\n")
            if r.error:
                lines.append(f"Harness error: {r.error}\n")
            else:
                final = r.edit_attempt or r.first_attempt
                if final and final.oracle_diff:
                    lines.append(f"```\n{final.oracle_diff}\n```\n")
                if final and final.compile and not final.compile.success:
                    stderr = final.compile.stderr.strip()
                    if stderr:
                        lines.append("Compile stderr (truncated):\n")
                        lines.append("```\n")
                        lines.append("\n".join(stderr.splitlines()[:10]))
                        lines.append("\n```\n")
            lines.append("")

    out_path.write_text("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description="Sigil spec validation harness.")
    parser.add_argument(
        "--models",
        default=",".join(DEFAULT_MODELS),
        help=f"Comma-separated model IDs (default: {','.join(DEFAULT_MODELS)})",
    )
    parser.add_argument(
        "--filter",
        default=None,
        help="Only run prompts whose id matches this regex (e.g., 'P05' or '^P[0-2]')",
    )
    parser.add_argument(
        "--max-concurrency",
        type=int,
        default=DEFAULT_MAX_CONCURRENCY,
        help=f"Max parallel prompts (default: {DEFAULT_MAX_CONCURRENCY})",
    )
    parser.add_argument(
        "--no-edit-loop",
        action="store_true",
        help="Disable the after-one-edit retry on first failure",
    )
    parser.add_argument(
        "--results-dir",
        default=str(REPO_ROOT / "spec"),
        help="Directory for JSONL trace + validation-log.md (default: spec/)",
    )
    args = parser.parse_args()

    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        print("validate_spec.py: ANTHROPIC_API_KEY not set in environment", file=sys.stderr)
        return 2
    if not SPEC_PATH.exists():
        print(f"validate_spec.py: spec missing at {SPEC_PATH}", file=sys.stderr)
        return 2
    if not PROMPTS_PATH.exists():
        print(f"validate_spec.py: prompts missing at {PROMPTS_PATH}", file=sys.stderr)
        return 2
    if not SIGIL_BIN.exists():
        print(f"validate_spec.py: sigil binary missing at {SIGIL_BIN}; run `cargo build --release`", file=sys.stderr)
        return 2

    models = [m.strip() for m in args.models.split(",") if m.strip()]
    prompts = parse_prompts(PROMPTS_PATH)
    if args.filter:
        pat = re.compile(args.filter)
        prompts = [p for p in prompts if pat.search(p.id)]
    if not prompts:
        print(f"validate_spec.py: no prompts matched filter {args.filter!r}", file=sys.stderr)
        return 2

    spec_text = SPEC_PATH.read_text()
    edit_loop = not args.no_edit_loop

    print(f"validate_spec.py: {len(prompts)} prompt(s) × {len(models)} model(s) "
          f"= {len(prompts) * len(models)} runs; concurrency={args.max_concurrency}; "
          f"edit_loop={edit_loop}", file=sys.stderr)

    work = [(p, m) for m in models for p in prompts]
    results: list[PromptResult] = []

    started = time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_concurrency) as pool:
        future_to_key = {
            pool.submit(
                run_one_prompt,
                prompt=p,
                model=m,
                api_key=api_key,
                spec_text=spec_text,
                edit_loop=edit_loop,
            ): (p.id, m) for p, m in work
        }
        completed = 0
        for fut in concurrent.futures.as_completed(future_to_key):
            pid, model = future_to_key[fut]
            try:
                result = fut.result()
            except Exception as e:
                result = PromptResult(
                    prompt_id=pid, model=model,
                    first_attempt=None, edit_attempt=None,
                    final_pass=False, error=f"unhandled: {e}",
                )
            results.append(result)
            completed += 1
            mark = "✅" if result.final_pass else "❌"
            extra = ""
            if result.first_attempt and result.first_attempt.oracle_match:
                extra = " (first try)"
            elif result.final_pass:
                extra = " (after edit)"
            elif result.error:
                extra = f" (error: {result.error[:60]})"
            print(f"  [{completed:>3}/{len(work)}] {mark} {pid} × {model}{extra}", file=sys.stderr)

    elapsed = time.time() - started
    print(f"validate_spec.py: completed {len(results)} runs in {elapsed:.1f}s", file=sys.stderr)

    results_dir = pathlib.Path(args.results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)
    timestamp = time.strftime("%Y%m%dT%H%M%S")
    jsonl_path = results_dir / f"validation-results-{timestamp}.jsonl"
    md_path = results_dir / "validation-log.md"
    write_jsonl(results, jsonl_path)
    render_markdown_report(results, prompts, models, md_path, jsonl_path)
    print(f"validate_spec.py: trace -> {jsonl_path}", file=sys.stderr)
    print(f"validate_spec.py: report -> {md_path}", file=sys.stderr)

    failed = sum(1 for r in results if not r.final_pass)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
