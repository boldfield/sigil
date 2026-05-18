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


def test_failure_detail_surfaces_harness_errors(tmp_path):
    # Row with error set and no attempts — simulates run_one_cell catching
    # a claude-cli exception before eval ever ran.
    harness_row = {
        "prompt_id": "C01", "language": "sigil",
        "model": "claude-haiku-4-5", "run_idx": 0,
        "tier": "baseline",
        "corpus_version": {"git_sha": "abc", "teaching_hash": "111"},
        "first_attempt": None, "edit_attempt": None,
        "final_pass": False,
        "error": "claude -p (first turn) failed: 429 Too Many Requests",
    }
    trace = _make_trace(tmp_path, "trace.jsonl", [harness_row])
    out = dashboard.render(dashboard.load_traces([trace]))
    assert "Harness errors" in out
    assert "C01" in out
    assert "429" in out


def test_failure_detail_no_failures_when_no_buckets_and_no_harness(tmp_path):
    trace = _make_trace(tmp_path, "trace.jsonl", [_row(first_passed=True)])
    out = dashboard.render(dashboard.load_traces([trace]))
    # Walk the section and check the empty-state message is present.
    assert "Failure detail by cluster" in out
    detail = out.split("## Failure detail by cluster", 1)[1].split("## ", 1)[0]
    assert "No failures in the loaded traces" in detail
