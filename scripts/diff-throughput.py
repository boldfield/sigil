#!/usr/bin/env python3
"""Plan E2 throughput-report diff tool.

Reads two `scripts/measure-throughput.sh` JSON outputs (pre /
post checkpoint) and emits a Markdown table comparing the
metrics, with absolute + percentage deltas. Sign convention:

  - wall_clock_ms: negative delta = faster (better).
  - peak_rss_kb:   negative delta = smaller (better).
  - alloc_count:   neutral (workload-determined; should be equal).
  - alloc_bytes:   neutral (workload-determined; should be equal).
  - boehm_gc_time_ms: negative delta = less GC time (better).

The diff tool is data-shape only: it does not interpret whether
"better" matches the plan body's hypothesis — that's the report
author's job. The tool produces the raw numbers for the report's
"Discussion" section to argue from.

Usage:

  scripts/diff-throughput.py <workload-name> <pre.json> <post.json>

Output: one Markdown subsection per call, suitable for
concatenation into the throughput report doc.
"""

import json
import sys
from pathlib import Path


def pct_delta(pre, post) -> str:
    # Either side may be `None` (the JSON `null` for
    # `boehm_gc_time_ms` on a pre-Phase-2 checkpoint that didn't
    # have the runtime probe). Render those as "n/a" so the report
    # surfaces the gap honestly rather than implying a 100% delta.
    if pre is None or post is None:
        return "n/a"
    if pre == 0:
        return "n/a"
    return f"{((post - pre) / pre) * 100:+.1f}%"


def abs_delta(pre, post) -> str:
    if pre is None or post is None:
        return "n/a"
    # `:+g` (vs `:+d`) gracefully handles any future probe that
    # emits a float — :+d would TypeError on non-int values.
    return f"{post - pre:+g}"


def fmt_scalar(v) -> str:
    return "n/a" if v is None else str(v)


def fmt_median_iqr(metric: dict) -> str:
    return f"{metric['median']} ± {metric['iqr']}"


def render(workload: str, pre: dict, post: dict) -> str:
    rows = []
    rows.append(f"### `{workload}`")
    rows.append("")
    rows.append("| Metric | Pre-Phase-2 | Post-Phase-2 | Δ abs | Δ % |")
    rows.append("|---|---|---|---|---|")

    # Wall-clock + RSS use median ± IQR.
    for key, unit in [("wall_clock_ms", "ms"), ("peak_rss_kb", "kB")]:
        rows.append(
            f"| {key} ({unit}) "
            f"| {fmt_median_iqr(pre[key])} "
            f"| {fmt_median_iqr(post[key])} "
            f"| {post[key]['median'] - pre[key]['median']:+d} {unit} "
            f"| {pct_delta(pre[key]['median'], post[key]['median'])} |"
        )

    # Counter-derived metrics are single values.
    for key, unit in [
        ("alloc_count", ""),
        ("alloc_bytes", "bytes"),
        ("boehm_gc_time_ms", "ms"),
    ]:
        rows.append(
            f"| {key}{(' (' + unit + ')') if unit else ''} "
            f"| {fmt_scalar(pre[key])} "
            f"| {fmt_scalar(post[key])} "
            f"| {abs_delta(pre[key], post[key])} "
            f"| {pct_delta(pre[key], post[key])} |"
        )

    rows.append("")
    rows.append(f"**Runs:** pre={pre['runs']}, post={post['runs']}.")
    return "\n".join(rows)


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        print(
            "usage: diff-throughput.py <workload-name> <pre.json> <post.json>",
            file=sys.stderr,
        )
        return 2

    workload, pre_path_s, post_path_s = argv[1], argv[2], argv[3]
    pre_path, post_path = Path(pre_path_s), Path(post_path_s)
    if not pre_path.is_file():
        print(f"diff-throughput: pre file not found: {pre_path}", file=sys.stderr)
        return 1
    if not post_path.is_file():
        print(f"diff-throughput: post file not found: {post_path}", file=sys.stderr)
        return 1

    pre = json.loads(pre_path.read_text())
    post = json.loads(post_path.read_text())
    print(render(workload, pre, post))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
