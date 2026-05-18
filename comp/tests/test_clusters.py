"""Tests for the cluster taxonomy + classifier in clusters.py."""
from __future__ import annotations

import pytest

import clusters  # type: ignore


def _row(eval_category="", eval_detail="", eval_raw_output=""):
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
    row = _row(eval_category="stdout",
               eval_detail="expected 42 got 24",
               eval_raw_output="diff: expected 42 got 24")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "wrong-output"


def test_no_code_block_category():
    row = _row(eval_category="no-code-block",
               eval_detail="model produced no extractable program")
    cluster = clusters.classify_failure(row, attempt="first")
    assert cluster.id == "no-code-block"


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


def test_classify_returns_none_when_eval_passed_is_none():
    """Attempt present but eval failed to run (eval_passed is None) — skip it."""
    row = {
        "first_attempt": {
            "eval_passed": None,
            "eval_category": None,
            "eval_detail": "",
            "eval_raw_output": "",
        },
        "edit_attempt": None,
    }
    assert clusters.classify_failure(row, attempt="first") is None


def test_classify_returns_none_when_edit_attempt_missing():
    row = {
        "first_attempt": {
            "eval_passed": False,
            "eval_category": "compile",
            "eval_detail": "error[E0010]",
            "eval_raw_output": "",
        },
        "edit_attempt": None,
    }
    assert clusters.classify_failure(row, attempt="edit") is None
