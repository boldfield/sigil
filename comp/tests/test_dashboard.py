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


def test_load_traces_detects_mixed_tiers(tmp_path):
    cv = {"git_sha": "aaa", "teaching_hash": "111"}
    a = _make_trace(tmp_path, "a.jsonl",
                    [_row(tier="baseline", corpus_version=cv)])
    b = _make_trace(tmp_path, "b.jsonl",
                    [_row(tier="iteration", corpus_version=cv)])
    loaded = dashboard.load_traces([a, b])
    assert loaded.mixed_tiers is True
    assert loaded.tier is None


def test_load_traces_single_tier_not_mixed(tmp_path):
    a = _make_trace(tmp_path, "a.jsonl", [_row(tier="baseline")])
    b = _make_trace(tmp_path, "b.jsonl", [_row(tier="baseline")])
    loaded = dashboard.load_traces([a, b])
    assert loaded.mixed_tiers is False
    assert loaded.tier == "baseline"


def test_render_header_warns_on_mixed_tiers(tmp_path):
    cv = {"git_sha": "aaa", "teaching_hash": "111"}
    a = _make_trace(tmp_path, "a.jsonl",
                    [_row(tier="baseline", corpus_version=cv)])
    b = _make_trace(tmp_path, "b.jsonl",
                    [_row(tier="iteration", corpus_version=cv)])
    out = dashboard.render(dashboard.load_traces([a, b]))
    assert "multiple tiers" in out.lower() or "mixed tier" in out.lower()
    assert "WARNING" in out


def test_load_traces_raises_on_malformed_json(tmp_path):
    p = tmp_path / "bad.jsonl"
    p.write_text('{"valid": "row"}\nnot valid json\n')
    with pytest.raises(ValueError, match="malformed JSON"):
        dashboard.load_traces([p])


def test_resolve_latest_trace_raises_on_empty_dir(tmp_path, monkeypatch):
    monkeypatch.setattr(dashboard, "LOG_DIR", tmp_path)
    with pytest.raises(FileNotFoundError, match="no trace files"):
        dashboard._resolve_latest_trace("baseline")
