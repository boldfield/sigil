#!/usr/bin/env python3
"""
compare.py — Cross-language LLM authorship comparison harness.

For each (prompt × language × model) cell, runs N independent
`claude -p` sessions with a language-specific system prompt, extracts
the produced program from the response, hands it to the matching
language eval driver (compile + run + diff oracle), and records the
result. On first-shot failure, an edit-loop turn feeds the failure
back to the model; the second attempt is also recorded.

Routes through Claude Code headless mode (`claude -p`) so calls are
billed against the user's Claude subscription, not an API key. Auth
precedence (per Claude Code): CLAUDE_CODE_OAUTH_TOKEN env var, or
the OAuth token stored by a prior `claude /login`. For batch runs
on a host without interactive login, generate a long-lived token
with `claude setup-token` and export CLAUDE_CODE_OAUTH_TOKEN. The
harness scrubs `ANTHROPIC_*` and `CLAUDE_CODE_USE_*` env prefixes
from the child env so API-credit and 3P-provider routing can't
override the subscription path.

Multi-turn edit loop pins a caller-generated `--session-id <uuid>`
on turn 1 and resumes against the same uuid on turn 2, so the system
prompt (with the embedded Sigil spec) isn't resent. The system
prompt is passed via `--system-prompt-file <tempfile>` rather than
`--system-prompt <argv>` to dodge the ARG_MAX ceiling — the Sigil
spec is on a monotonic growth path.

Each `claude -p` invocation is wrapped in a 3-attempt exponential-
backoff loop on detectably transient failures (HTTP 429 / 5xx,
process-level timeout). Hard failures — auth, 4xx-other, rate-limit
exhaustion on the 5-hour/weekly subscription windows — bubble up
and the cell is recorded as a failure rather than busy-waiting.

- `comp/prompts.md`        cross-language prompts (C01..C20)
- `comp/contexts/<lang>.md` per-language system prompt prefix
- `comp/scripts/eval-<lang>.sh` per-language compile+run+oracle driver

Aggregates per (prompt, language, model) cell so the markdown report
makes language-level pass-rate differences visible.

Usage:
    # Prereq: `claude` on PATH and authenticated (claude /login OR
    # CLAUDE_CODE_OAUTH_TOKEN env var).
    python3 comp/scripts/compare.py
    python3 comp/scripts/compare.py                          # sigil only, Haiku+Sonnet
    python3 comp/scripts/compare.py --filter C01 --runs 3
    python3 comp/scripts/compare.py --langs sigil,python --models claude-opus-4-7
    python3 comp/scripts/compare.py --all-langs              # cross-language baseline
    python3 comp/scripts/compare.py --all-langs --full --runs 5  # full thesis comparison

Exit codes:
    0  — every cell passed (all runs, all prompts, all langs, all models)
    1  — at least one cell failed
    2  — harness error (missing binary, missing files, bad config)
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from typing import Optional

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
COMP_DIR = REPO_ROOT / "comp"
PROMPTS_PATH = COMP_DIR / "prompts.md"
CONTEXTS_DIR = COMP_DIR / "contexts"
EVAL_SCRIPTS_DIR = COMP_DIR / "scripts"
LOG_DIR = COMP_DIR / "log"
SPEC_PATH = REPO_ROOT / "spec" / "language.md"
SIGIL_CONTEXT_PATH = CONTEXTS_DIR / "sigil.md"

DEFAULT_LANGUAGES = ["sigil"]
FULL_LANGUAGES = ["sigil", "python", "go", "rust"]
DEFAULT_MODELS = ["claude-haiku-4-5", "claude-sonnet-4-6"]
FULL_MODELS = ["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-7"]
DEFAULT_MAX_CONCURRENCY = 4
DEFAULT_REQUEST_TIMEOUT_S = 180
DEFAULT_EVAL_TIMEOUT_S = 60

# Ollama-backed models are addressed via the `ollama:<model_tag>`
# prefix in `--models` (e.g. `ollama:qwen2.5-coder:7b`). The endpoint
# is configured via `--ollama-host` or the `OLLAMA_HOST` env var. A
# `:11434` port-less HTTPS URL is the typical nginx-fronted shape;
# the trailing slash is stripped at use.
OLLAMA_MODEL_PREFIX = "ollama:"
# 7B Q4 on a Jetson Orin Nano (~10-30 tok/s) commonly takes 30s-2min
# per response on the Sigil corpus's prompt/response sizes. The
# Claude-side timeout (180s) is too tight; 600s gives 5-10× headroom.
DEFAULT_OLLAMA_REQUEST_TIMEOUT_S = 600

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


_HEADING_RE = re.compile(r"^## ([A-Z]\d+) — (.+)$")
_PROMPT_FIELD_RE = re.compile(r"^\*\*Prompt:\*\*\s*(.*)$")
_NOTES_RE = re.compile(r"^\*\*Notes:\*\*\s*(.*)$")


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


def parse_prompts(path: pathlib.Path) -> list[Prompt]:
    """Parse comp/prompts.md into structured Prompt records.

    Note: oracle parsing is deferred entirely to the per-language eval
    drivers (eval-{sigil,python,go,rust}.sh) — they parse the same prompts.md
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
# Claude Code headless-mode client (subscription-auth via `claude -p`)
# ---------------------------------------------------------------------------


CLI_RETRY_ATTEMPTS = 3
CLI_RETRY_BACKOFFS_S = (2.0, 4.0)  # gaps before attempts 2 and 3


class _ClaudeCliTransientError(RuntimeError):
    """Detectable transient failure (429 / 5xx / network blip). Eligible
    for in-process retry. Surface as `RuntimeError` after the wrapper
    exhausts attempts."""


def _build_child_env() -> dict[str, str]:
    """Build a child env for `claude -p` that forces Claude Code's
    subscription/OAuth auth path.

    Claude Code's auth precedence prefers `ANTHROPIC_API_KEY` /
    `ANTHROPIC_AUTH_TOKEN` over the keychain OAuth token, which routes
    calls through API credit billing instead of the user's subscription.
    `CLAUDE_CODE_USE_BEDROCK` / `CLAUDE_CODE_USE_VERTEX` (and friends)
    short-circuit further and route to a 3P provider. Scrubbing both
    prefixes leaves Claude Code's keychain-OAuth or
    `CLAUDE_CODE_OAUTH_TOKEN` path as the only choice."""
    child_env = os.environ.copy()
    for key in list(child_env):
        if key == "CLAUDE_CODE_OAUTH_TOKEN":
            continue
        if key.startswith("ANTHROPIC_") or key.startswith("CLAUDE_CODE_USE_"):
            child_env.pop(key, None)
    return child_env


def _is_retryable_cli_error(api_error_status: object) -> bool:
    """Retryable: 429 (rate limit) and 5xx (server errors).

    Not retryable: other 4xx (auth, bad request — won't fix themselves),
    or no api_error_status at all (the error envelope didn't surface a
    network/API status; safer to treat as opaque and fail fast)."""
    if not isinstance(api_error_status, int):
        return False
    return api_error_status == 429 or 500 <= api_error_status < 600


def _call_claude_cli_once(
    *,
    model: str,
    user_message: str,
    system_prompt_path: Optional[str],
    session_id: Optional[str],
    resume_session_id: Optional[str],
    timeout_s: int,
) -> tuple[str, str]:
    """One subprocess invocation. Raises `_ClaudeCliTransientError` on
    detectably transient failures (caller retries those) and `RuntimeError`
    on hard failures."""
    cmd = ["claude", "-p", "--model", model, "--output-format", "json"]
    if system_prompt_path is not None:
        # `--system-prompt-file <path>` (hidden from --help but real;
        # documented in --bare's description). Switching off argv-passed
        # `--system-prompt` removes the ARG_MAX ceiling — `spec/language.md`
        # is on a monotonic growth path and the previous argv form would
        # silently break with E2BIG around 4–5× current size on macOS.
        cmd += ["--system-prompt-file", system_prompt_path]
        # `--session-id <uuid>` lets the caller pin the session id
        # explicitly. The edit-loop turn then resumes against the same
        # uuid we chose, independent of however Claude Code's session-
        # persistence layer would have named it. Concurrency-safe by
        # construction since each cell generates a fresh uuid4.
        if session_id is not None:
            cmd += ["--session-id", session_id]
    else:
        cmd += ["--resume", resume_session_id]

    child_env = _build_child_env()

    try:
        proc = subprocess.run(
            cmd,
            input=user_message,
            capture_output=True,
            text=True,
            timeout=timeout_s,
            env=child_env,
        )
    except FileNotFoundError as e:
        raise RuntimeError(
            "`claude` binary not on PATH; install Claude Code and run "
            "`claude /login` or export CLAUDE_CODE_OAUTH_TOKEN"
        ) from e
    except subprocess.TimeoutExpired as e:
        # Treat process-level timeout as transient — the model server
        # may have been backed up but the next attempt could succeed.
        raise _ClaudeCliTransientError(
            f"claude -p timed out after {timeout_s}s"
        ) from e

    # claude -p stuffs API errors into the JSON `result` field with
    # `is_error: true`, sometimes exiting nonzero and sometimes 0. Try
    # JSON parse before bailing on the exit code so the actual error
    # (rate limit, credit balance, auth failure) reaches the caller.
    data: Optional[dict] = None
    if proc.stdout.strip():
        try:
            data = json.loads(proc.stdout)
        except json.JSONDecodeError:
            data = None

    if data is None:
        stderr_tail = (proc.stderr or "").strip()[-500:]
        # The CLI reports session-id collisions on stderr with exit 1
        # and no JSON. With caller-rotating session_id per attempt
        # (see `call_claude_cli`), retry resolves these cleanly.
        if "Session ID" in stderr_tail and "already in use" in stderr_tail:
            raise _ClaudeCliTransientError(
                f"claude -p session-id collision (will retry with fresh uuid): "
                f"{stderr_tail!r}"
            )
        raise RuntimeError(
            f"claude -p exited {proc.returncode} with no JSON on stdout; "
            f"stderr tail: {stderr_tail!r}"
        )

    if data.get("is_error", False):
        api_status = data.get("api_error_status")
        subtype = data.get("subtype", "<no subtype>")
        detail = str(data.get("result", ""))[:500]
        msg = f"claude -p error (api_status={api_status}, subtype={subtype}): {detail}"
        if _is_retryable_cli_error(api_status):
            raise _ClaudeCliTransientError(msg)
        raise RuntimeError(msg)

    text = data.get("result", "")
    response_session_id = data.get("session_id", "")
    if not text:
        raise RuntimeError(
            f"claude -p returned empty result; keys: {sorted(data.keys())}"
        )
    if not response_session_id:
        # Resume needs this; surface immediately rather than failing on turn 2.
        raise RuntimeError(
            f"claude -p returned no session_id; keys: {sorted(data.keys())}"
        )
    return text, response_session_id


def call_claude_cli(
    *,
    model: str,
    user_message: str,
    system: Optional[str] = None,
    resume_session_id: Optional[str] = None,
    timeout_s: int = DEFAULT_REQUEST_TIMEOUT_S,
) -> tuple[str, str]:
    """Invoke `claude -p` and return (response_text, session_id).

    First turn: pass `system` to install the language-specific system
    prompt (replaces Claude Code's default preamble). The wrapper
    generates a fresh `uuid4` per attempt and pins it via
    `--session-id` so resume on the next turn binds to a uuid we
    chose (not whatever Claude Code's session-persistence layer
    would have named); the actually-used `session_id` is returned
    so the caller can pass it as `resume_session_id` on turn 2.

    Subsequent turns: pass `resume_session_id` from turn 1's return
    value to continue the session — the system prompt and prior turns
    persist server-side, so the spec isn't resent on turn 2.

    Auth is whatever Claude Code is configured with (subscription OAuth
    via `claude /login` or `CLAUDE_CODE_OAUTH_TOKEN` env). Wraps the
    once-helper in a 3-attempt exponential-backoff loop on detectably
    transient failures (HTTP 429 / 5xx, process-level timeout, session-id
    collision). Hard failures — auth errors, 4xx-other, rate-limit
    *exhaustion* on the 5-hour/weekly subscription windows — bubble up
    to the caller, which records them as cell failures rather than
    busy-waiting for hours."""
    if (system is None) == (resume_session_id is None):
        raise ValueError(
            "call_claude_cli: pass exactly one of `system` (new session) "
            "or `resume_session_id` (continuation)"
        )

    # Write system prompt to a tempfile and pass via --system-prompt-file
    # to dodge the argv-length ceiling (macOS ARG_MAX = 1 MB; the Sigil
    # spec is on a growth path that would breach this within a few
    # release cycles at the current rate).
    system_prompt_path: Optional[str] = None
    tmp_handle: Optional[tempfile._TemporaryFileWrapper] = None
    if system is not None:
        tmp_handle = tempfile.NamedTemporaryFile(
            mode="w",
            suffix=".system-prompt.md",
            prefix="comp-",
            delete=False,
            encoding="utf-8",
        )
        try:
            tmp_handle.write(system)
            tmp_handle.flush()
            tmp_handle.close()
            system_prompt_path = tmp_handle.name
        except Exception:
            try:
                tmp_handle.close()
            except Exception:
                pass
            if tmp_handle and tmp_handle.name:
                try:
                    os.unlink(tmp_handle.name)
                except FileNotFoundError:
                    pass
            raise

    try:
        last_exc: Optional[Exception] = None
        for attempt in range(CLI_RETRY_ATTEMPTS):
            if attempt > 0:
                time.sleep(CLI_RETRY_BACKOFFS_S[attempt - 1])
            # Rotate session_id per attempt for new sessions so a
            # collision on attempt 1 (Claude Code already has that
            # uuid reserved server-side somehow) doesn't recur on
            # retry. For resume, the id is fixed by definition.
            attempt_session_id: Optional[str] = (
                str(uuid.uuid4()) if system is not None else None
            )
            try:
                return _call_claude_cli_once(
                    model=model,
                    user_message=user_message,
                    system_prompt_path=system_prompt_path,
                    session_id=attempt_session_id,
                    resume_session_id=resume_session_id,
                    timeout_s=timeout_s,
                )
            except _ClaudeCliTransientError as e:
                last_exc = e
                continue
        raise RuntimeError(
            f"claude -p failed after {CLI_RETRY_ATTEMPTS} attempts: {last_exc}"
        )
    finally:
        if system_prompt_path is not None:
            try:
                os.unlink(system_prompt_path)
            except FileNotFoundError:
                pass


# ---------------------------------------------------------------------------
# Ollama HTTP client (OpenAI-compat /v1/chat/completions)
# ---------------------------------------------------------------------------
#
# Why a separate path from Claude Code:
#   - Ollama has no server-side session persistence. Each request is
#     stateless. We synthesise a `session_id` (uuid4) at first turn
#     and keep the message history client-side in
#     `_OLLAMA_SESSIONS`, then send the full history on resume.
#   - No subscription/auth in Sigil's typical Ollama deployment
#     (nginx-fronted local endpoint); no env-scrubbing dance.
#   - Latency is ~10-100× Claude's: 30s-2min vs ~1-5s on a small
#     prompt. Hence `DEFAULT_OLLAMA_REQUEST_TIMEOUT_S = 600` instead
#     of 180 used for Claude.
#
# Retry shape mirrors `call_claude_cli`: 3 attempts, 2s/4s backoffs,
# only on network-transient errors (timeout, connection refused,
# 5xx). 4xx (bad request) and JSON parse failures bubble up as hard
# errors.

_OLLAMA_HOST: Optional[str] = None
_OLLAMA_SESSIONS_LOCK = threading.Lock()
_OLLAMA_SESSIONS: dict[str, list[dict]] = {}


class _OllamaTransientError(RuntimeError):
    """Ollama transient failure: connection refused, timeout, 5xx.
    Caller's retry loop catches this and re-attempts."""


def _set_ollama_host(host: Optional[str]) -> None:
    """Configure the module-level Ollama endpoint. Called once at
    `main()` time from the CLI flag or `OLLAMA_HOST` env var. Trailing
    slash stripped so callers can concatenate `/v1/...` cleanly."""
    global _OLLAMA_HOST
    if host is None:
        _OLLAMA_HOST = None
        return
    _OLLAMA_HOST = host.rstrip("/")


def _call_ollama_once(
    *,
    model: str,
    messages: list[dict],
    timeout_s: int,
) -> str:
    """One POST to `${OLLAMA_HOST}/v1/chat/completions`. Returns the
    assistant content string. Raises `_OllamaTransientError` on
    timeout/connection/5xx; raises `RuntimeError` on hard failures
    (4xx, malformed JSON, missing fields)."""
    if _OLLAMA_HOST is None:
        raise RuntimeError(
            "Ollama host not configured; pass --ollama-host or set "
            "OLLAMA_HOST"
        )
    url = f"{_OLLAMA_HOST}/v1/chat/completions"
    payload = json.dumps({
        "model": model,
        "messages": messages,
        "stream": False,
    }).encode("utf-8")
    req = urllib.request.Request(
        url,
        data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout_s) as resp:
            body = resp.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as e:
        if 500 <= e.code < 600:
            raise _OllamaTransientError(
                f"ollama HTTP {e.code} at {url}: {e.reason}"
            ) from e
        raise RuntimeError(
            f"ollama HTTP {e.code} at {url}: {e.reason}"
        ) from e
    except urllib.error.URLError as e:
        # Network-level: DNS failure, connection refused, TLS handshake
        # failure, timeout. All transient — backoff + retry.
        raise _OllamaTransientError(
            f"ollama URL error at {url}: {e.reason}"
        ) from e
    except TimeoutError as e:
        raise _OllamaTransientError(
            f"ollama request timed out after {timeout_s}s at {url}"
        ) from e

    try:
        data = json.loads(body)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"ollama returned non-JSON from {url}: {body[:500]!r}"
        ) from e
    try:
        return data["choices"][0]["message"]["content"]
    except (KeyError, IndexError, TypeError) as e:
        raise RuntimeError(
            f"ollama response missing choices[0].message.content at "
            f"{url}: {body[:500]!r}"
        ) from e


def call_ollama(
    *,
    model: str,
    user_message: str,
    system: Optional[str] = None,
    resume_session_id: Optional[str] = None,
    timeout_s: int = DEFAULT_OLLAMA_REQUEST_TIMEOUT_S,
) -> tuple[str, str]:
    """Mirror of `call_claude_cli` for Ollama. Returns
    `(response_text, session_id)`. On a first turn (`system` set) we
    mint a uuid4 session id and stash the [system, user, assistant]
    triple in `_OLLAMA_SESSIONS`. On resume, we look up the existing
    history, append [user, assistant], and POST the full message
    list (Ollama has no server-side state)."""
    if (system is None) == (resume_session_id is None):
        raise ValueError(
            "call_ollama: pass exactly one of `system` (new session) "
            "or `resume_session_id` (continuation)"
        )

    if system is not None:
        session_id = str(uuid.uuid4())
        messages = [
            {"role": "system", "content": system},
            {"role": "user", "content": user_message},
        ]
    else:
        with _OLLAMA_SESSIONS_LOCK:
            history = _OLLAMA_SESSIONS.get(resume_session_id)
        if history is None:
            raise RuntimeError(
                f"call_ollama: resume_session_id {resume_session_id!r} "
                f"not found in client-side history (was the first turn "
                f"this process, same run?)"
            )
        session_id = resume_session_id
        messages = history + [{"role": "user", "content": user_message}]

    last_exc: Optional[Exception] = None
    for attempt in range(CLI_RETRY_ATTEMPTS):
        if attempt > 0:
            time.sleep(CLI_RETRY_BACKOFFS_S[attempt - 1])
        try:
            content = _call_ollama_once(
                model=model, messages=messages, timeout_s=timeout_s,
            )
        except _OllamaTransientError as e:
            last_exc = e
            continue

        # Success: store the updated history (system+user+assistant for
        # new sessions; old_history+user+assistant for resumes) so a
        # subsequent edit-loop turn against this session_id sees the
        # full context.
        updated = messages + [{"role": "assistant", "content": content}]
        with _OLLAMA_SESSIONS_LOCK:
            _OLLAMA_SESSIONS[session_id] = updated
        return content, session_id

    raise RuntimeError(
        f"ollama failed after {CLI_RETRY_ATTEMPTS} attempts: {last_exc}"
    )


# ---------------------------------------------------------------------------
# Model dispatcher — picks Claude vs Ollama based on `model` prefix
# ---------------------------------------------------------------------------


def call_model(
    *,
    model: str,
    user_message: str,
    system: Optional[str] = None,
    resume_session_id: Optional[str] = None,
) -> tuple[str, str]:
    """Dispatch a model call to the right provider. Models with the
    `ollama:` prefix go to the local Ollama endpoint; everything else
    goes through Claude Code's headless CLI. Per-provider timeouts
    are applied automatically (Claude 180s, Ollama 600s)."""
    if model.startswith(OLLAMA_MODEL_PREFIX):
        ollama_model = model[len(OLLAMA_MODEL_PREFIX):]
        return call_ollama(
            model=ollama_model,
            user_message=user_message,
            system=system,
            resume_session_id=resume_session_id,
        )
    return call_claude_cli(
        model=model,
        user_message=user_message,
        system=system,
        resume_session_id=resume_session_id,
    )


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
    suffix_by_lang = {
        "sigil": ".sigil",
        "python": ".py",
        "go": ".go",
        "rust": ".rs",
    }
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
    # `call_model` dispatches to Claude Code's headless CLI or the
    # Ollama HTTP adapter based on the `model` prefix
    # (`ollama:<tag>` → Ollama; everything else → Claude). For
    # Claude, the wrapper pins a fresh `--session-id <uuid4>` per
    # attempt internally and returns the actually-used id; for
    # Ollama, the wrapper mints a uuid4 and stashes the message
    # history client-side. Either way the edit-loop turn resumes
    # against the same id.
    try:
        first_response, session_id = call_model(
            model=model, system=system, user_message=prompt.prompt_text,
        )
    except Exception as e:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=None, edit_attempt=None, final_pass=False,
            error=f"model call (first turn) failed: {e}",
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
    try:
        edit_response, _ = call_model(
            model=model, user_message=followup, resume_session_id=session_id,
        )
    except Exception as e:
        return CellResult(
            prompt_id=prompt.id, language=language, model=model, run_idx=run_idx,
            first_attempt=first_attempt, edit_attempt=None, final_pass=False,
            error=f"model call (edit-loop turn) failed: {e}",
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
        # Strip regex metacharacters so the filename stays readable; cap length.
        sanitized = re.sub(r"[^A-Za-z0-9_-]+", "", filter_expr)[:32]
        if sanitized:
            parts.append(sanitized)
    if all_langs:
        parts.append("cross")
    if full:
        parts.append("full")
    tier_default = resolve_runs(tier=tier, explicit_runs=None) if tier else 1
    if runs > 1 and runs != tier_default:
        # Include r<N> when runs diverges from the tier default (or there's
        # no tier). Omit it when the tier already implies the count, to
        # avoid redundant baseline-r10 slugs.
        parts.append(f"r{runs}")
    if no_edit_loop:
        parts.append("noedit")
    return "-".join(parts)


def main() -> int:
    parser = argparse.ArgumentParser(description="Cross-language LLM authorship comparison.")
    parser.add_argument(
        "--langs",
        default=None,
        help=f"Comma-separated language list. Default for iteration: "
             f"{','.join(DEFAULT_LANGUAGES)}. Pass --all-langs to use "
             f"{','.join(FULL_LANGUAGES)} instead.",
    )
    parser.add_argument(
        "--all-langs",
        action="store_true",
        help=f"Run the full language set ({','.join(FULL_LANGUAGES)}). "
             f"Reserve for cross-language thesis comparisons — sigil-only "
             f"is the default for grammar-change iteration. Mutually "
             f"exclusive with --langs.",
    )
    parser.add_argument(
        "--models",
        default=None,
        help=f"Comma-separated model IDs. Default for iteration: "
             f"{','.join(DEFAULT_MODELS)}. Pass --full to use "
             f"{','.join(FULL_MODELS)} instead. Ollama-hosted models "
             f"use the `ollama:<model_tag>` prefix (e.g. "
             f"`ollama:qwen2.5-coder:7b`); set --ollama-host or "
             f"OLLAMA_HOST to point at the server.",
    )
    parser.add_argument(
        "--ollama-host",
        default=os.environ.get("OLLAMA_HOST"),
        help="Base URL of the Ollama endpoint (e.g. "
             "https://ollama.summercamp.eastharbor.casa). Required "
             "when --models contains any `ollama:<tag>` entries. "
             "Defaults to the OLLAMA_HOST env var.",
    )
    parser.add_argument(
        "--full",
        action="store_true",
        help=f"Run the full model set ({','.join(FULL_MODELS)}). "
             f"Reserve for before/after grammar-change comparisons — "
             f"the Opus runs are the rate-limit bottleneck. Mutually "
             f"exclusive with --models.",
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
        default=None,
        help="Number of independent runs per (prompt, lang, model). >1 enables "
             "K/N aggregation. Default depends on --tier (baseline=10, iteration=5, "
             "else 1).",
    )
    parser.add_argument(
        "--no-edit-loop",
        action="store_true",
        help="Disable the after-one-edit retry on first failure",
    )
    parser.add_argument(
        "--tier",
        default=None,
        choices=["baseline", "iteration"],
        help="Tiered run preset. baseline=10 runs/cell (authoritative measurement); "
             "iteration=5 runs/cell (cheap spot-check). Explicit --runs N overrides.",
    )
    parser.add_argument(
        "--results-dir",
        default=str(LOG_DIR),
        help=f"Directory for JSONL trace + comparison-log.md (default: {LOG_DIR})",
    )
    args = parser.parse_args()
    args.runs = resolve_runs(tier=args.tier, explicit_runs=args.runs)
    if args.tier and resolve_runs(tier=args.tier, explicit_runs=None) != args.runs:
        tier_default = resolve_runs(tier=args.tier, explicit_runs=None)
        print(
            f"compare.py: warning: --tier {args.tier} normally implies "
            f"--runs {tier_default}, but --runs {args.runs} was passed; the "
            f"tier label in the trace will be {args.tier!r} despite the "
            f"non-standard run count.",
            file=sys.stderr,
        )

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

    if args.full and args.models is not None:
        print("compare.py: --full and --models are mutually exclusive", file=sys.stderr)
        return 2
    if args.all_langs and args.langs is not None:
        print("compare.py: --all-langs and --langs are mutually exclusive", file=sys.stderr)
        return 2
    if args.full:
        models_spec = ",".join(FULL_MODELS)
    elif args.models is not None:
        models_spec = args.models
    else:
        models_spec = ",".join(DEFAULT_MODELS)
    if args.all_langs:
        langs_spec = ",".join(FULL_LANGUAGES)
    elif args.langs is not None:
        langs_spec = args.langs
    else:
        langs_spec = ",".join(DEFAULT_LANGUAGES)

    models_for_prereq_check = [s.strip() for s in models_spec.split(",") if s.strip()]
    has_claude_model = any(
        not m.startswith(OLLAMA_MODEL_PREFIX) for m in models_for_prereq_check
    )
    has_ollama_model = any(
        m.startswith(OLLAMA_MODEL_PREFIX) for m in models_for_prereq_check
    )
    # Claude binary check only fires when at least one non-ollama
    # model is requested — Ollama-only runs don't need claude on PATH.
    if has_claude_model and shutil.which("claude") is None:
        print(
            "compare.py: `claude` binary not on PATH. Install Claude Code "
            "and authenticate via `claude /login` or "
            "`claude setup-token` + export CLAUDE_CODE_OAUTH_TOKEN. "
            "(Alternatively, restrict --models to `ollama:*` entries to "
            "skip the Claude path entirely.)",
            file=sys.stderr,
        )
        return 2
    # Ollama host check only fires when at least one ollama: model is
    # requested. Configures the module-level endpoint for `call_ollama`.
    if has_ollama_model:
        if not args.ollama_host:
            print(
                "compare.py: --models contains `ollama:*` entries but no "
                "--ollama-host or OLLAMA_HOST is set. Pass the endpoint "
                "(e.g. --ollama-host https://your-ollama-host).",
                file=sys.stderr,
            )
            return 2
        _set_ollama_host(args.ollama_host)

    languages = [s.strip() for s in langs_spec.split(",") if s.strip()]
    models = [s.strip() for s in models_spec.split(",") if s.strip()]
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
          f"{len(models)} model(s) × {args.runs} run(s) = {total} `claude -p` "
          f"sessions; concurrency={args.max_concurrency}; edit_loop={edit_loop}",
          file=sys.stderr)

    work = [(p, lang, m, run_idx)
            for lang in languages
            for m in models
            for p in prompts
            for run_idx in range(args.runs)]
    results: list[CellResult] = []
    corpus_version = compute_corpus_version()

    started = time.time()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_concurrency) as pool:
        future_to_key = {
            pool.submit(
                run_one_cell,
                prompt=p, language=lang, model=m, run_idx=run_idx,
                spec_text=spec_text, edit_loop=edit_loop,
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
    run_slug = _build_run_slug(
        filter_expr=args.filter,
        full=args.full,
        all_langs=args.all_langs,
        runs=args.runs,
        no_edit_loop=args.no_edit_loop,
        tier=args.tier,
    )
    stem = f"comparison-log-{timestamp}" + (f"-{run_slug}" if run_slug else "")
    slug_suffix = f"-{run_slug}" if run_slug else ""
    jsonl_path = results_dir / f"comparison-results-{timestamp}{slug_suffix}.jsonl"
    md_latest_path = results_dir / "comparison-log.md"
    md_run_path = results_dir / f"{stem}.md"
    write_jsonl(results, jsonl_path, tier=args.tier, corpus_version=corpus_version)
    render_markdown_report(results, prompts, languages, models, args.runs, md_latest_path, jsonl_path)
    # Per-run report is a byte-identical copy of the latest pointer —
    # one render, one copy. Cheaper and guarantees the two files agree.
    shutil.copyfile(md_latest_path, md_run_path)
    print(f"compare.py: trace      -> {jsonl_path}", file=sys.stderr)
    print(f"compare.py: report     -> {md_latest_path}", file=sys.stderr)
    print(f"compare.py: run report -> {md_run_path}", file=sys.stderr)

    failed = sum(1 for r in results if not r.final_pass)
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
