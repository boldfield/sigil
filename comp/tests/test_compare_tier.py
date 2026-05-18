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


def test_compute_corpus_version_handles_missing_teaching_files(tmp_path, monkeypatch):
    monkeypatch.setattr(compare, "SIGIL_CONTEXT_PATH", tmp_path / "nonexistent.md")
    monkeypatch.setattr(compare, "SPEC_PATH", tmp_path / "nonexistent.md")
    cv = compare.compute_corpus_version()
    assert cv["teaching_hash"] == "unknown"


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


def test_build_run_slug_keeps_r_suffix_when_tier_and_runs_override():
    slug = compare._build_run_slug(
        filter_expr=None, full=False, all_langs=False,
        runs=3, no_edit_loop=False, tier="baseline",
    )
    assert "r3" in slug
    assert "baseline" in slug


def test_build_run_slug_drops_r_suffix_when_runs_matches_tier_default():
    slug = compare._build_run_slug(
        filter_expr=None, full=False, all_langs=False,
        runs=10, no_edit_loop=False, tier="baseline",
    )
    assert "r10" not in slug
    assert slug == "baseline"


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
