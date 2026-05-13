#!/usr/bin/env bash
# scripts/measure-throughput.sh — Plan E2 throughput-report driver.
#
# Runs a compiled binary N times (default 5), captures wall-clock +
# peak RSS + alloc counters + Boehm full-GC time, and emits a single
# JSON record on stdout with median + IQR over the runs. The diff
# script (`scripts/diff-throughput.py`) consumes pairs of these JSON
# records (pre/post checkpoint) and produces the report's delta
# tables.
#
# Usage:
#   scripts/measure-throughput.sh <binary> [runs]
#
# Reads:
#   - The binary at <binary> — must accept no arguments and exit 0.
#   - SIGIL_PRINT_STATS=1 is set on the child so the runtime dumps
#     counters + boehm_gc_time_ms to stderr at process exit (see
#     runtime/src/counters.rs::sigil_counter_print_all).
#
# Per-host:
#   - Linux uses GNU /usr/bin/time -v.
#   - macOS uses BSD /usr/bin/time -l.
# Both surface "maximum resident set size" which we normalise to kB.
#
# Emitted JSON shape:
#   {
#     "binary": "<path>",
#     "runs": <n>,
#     "wall_clock_ms": {"median": <ms>, "iqr": <ms>, "min": <ms>, "max": <ms>},
#     "peak_rss_kb":   {"median": <kb>, "iqr": <kb>, "min": <kb>, "max": <kb>},
#     "alloc_count":   <int, from SIGIL_COUNTER_BOEHM_ALLOC_COUNT>,
#     "alloc_bytes":   <int, from SIGIL_COUNTER_BOEHM_ALLOC_BYTES>,
#     "boehm_gc_time_ms": <int, from boehm_gc_time_ms>
#   }
#
# Wall-clock / RSS are aggregated across runs; alloc counters are
# read from the last run only (they're deterministic — a workload
# allocates the same number of objects every run).
#
# Exits non-zero if any run exits non-zero, if the binary doesn't
# exist, or if a counter line is missing from a run's stderr.

set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "usage: $0 <binary> [runs]" >&2
    exit 2
fi

BINARY="$1"
RUNS="${2:-5}"

if [[ ! -x "$BINARY" ]]; then
    echo "measure-throughput: $BINARY is not executable" >&2
    exit 1
fi

if ! [[ "$RUNS" =~ ^[1-9][0-9]*$ ]]; then
    echo "measure-throughput: runs must be a positive integer (got '$RUNS')" >&2
    exit 1
fi

case "$(uname -s)" in
    Linux)
        TIME_BIN="/usr/bin/time"
        TIME_ARGS=(-v)
        TIME_WALL_PATTERN='Elapsed (wall clock) time'
        TIME_RSS_PATTERN='Maximum resident set size'
        RSS_UNIT_KB=1
        ;;
    Darwin)
        TIME_BIN="/usr/bin/time"
        TIME_ARGS=(-l)
        TIME_WALL_PATTERN='real'
        TIME_RSS_PATTERN='maximum resident set size'
        # macOS time -l reports RSS in bytes; normalise to kB.
        RSS_UNIT_KB=0
        ;;
    *)
        echo "measure-throughput: unsupported OS $(uname -s)" >&2
        exit 1
        ;;
esac

# Parse Linux's "h:mm:ss" or "mm:ss.ss" wall-clock format → ms.
linux_wall_to_ms() {
    local raw="$1"
    awk -v t="$raw" 'BEGIN {
        n = split(t, parts, ":");
        ms = 0;
        if (n == 3) {
            ms = (parts[1] * 3600 + parts[2] * 60 + parts[3]) * 1000;
        } else if (n == 2) {
            ms = (parts[1] * 60 + parts[2]) * 1000;
        } else {
            ms = parts[1] * 1000;
        }
        printf "%d\n", ms + 0.5;
    }'
}

# Parse Darwin's "N.NNN real" format → ms.
darwin_wall_to_ms() {
    awk -v t="$1" 'BEGIN { printf "%d\n", (t * 1000) + 0.5 }'
}

wall_ms_values=()
peak_rss_kb_values=()
last_alloc_count=""
last_alloc_bytes=""
last_gc_time_ms=""

for ((i = 1; i <= RUNS; i++)); do
    time_log="$(mktemp)"
    stderr_log="$(mktemp)"
    # Run under `time`, redirect time's own stderr to time_log and
    # the child's stderr to stderr_log via a separate FD shuffle.
    if ! "$TIME_BIN" "${TIME_ARGS[@]}" -o "$time_log" \
            env SIGIL_PRINT_STATS=1 "$BINARY" \
            > /dev/null 2> "$stderr_log"; then
        echo "measure-throughput: run $i failed" >&2
        cat "$stderr_log" >&2
        rm -f "$time_log" "$stderr_log"
        exit 1
    fi

    # Extract wall-clock + RSS from the time-output file.
    case "$(uname -s)" in
        Linux)
            wall_raw=$(grep "$TIME_WALL_PATTERN" "$time_log" \
                | sed 's/.*time (h:mm:ss or m:ss): //')
            wall_ms=$(linux_wall_to_ms "$wall_raw")
            rss_raw=$(grep "$TIME_RSS_PATTERN" "$time_log" | awk '{print $NF}')
            rss_kb="$rss_raw"
            ;;
        Darwin)
            wall_raw=$(grep "real" "$time_log" | awk '{print $1}')
            wall_ms=$(darwin_wall_to_ms "$wall_raw")
            rss_raw=$(grep "$TIME_RSS_PATTERN" "$time_log" | awk '{print $1}')
            if [[ "$RSS_UNIT_KB" -eq 0 ]]; then
                rss_kb=$(awk -v b="$rss_raw" 'BEGIN { printf "%d\n", (b / 1024) + 0.5 }')
            else
                rss_kb="$rss_raw"
            fi
            ;;
    esac

    if [[ -z "$wall_ms" || -z "$rss_kb" ]]; then
        echo "measure-throughput: run $i — failed to parse time output:" >&2
        cat "$time_log" >&2
        rm -f "$time_log" "$stderr_log"
        exit 1
    fi

    wall_ms_values+=("$wall_ms")
    peak_rss_kb_values+=("$rss_kb")

    # Counters from the child's stderr (last run wins). `boehm_gc_time_ms`
    # is a Plan E2 Phase 2 closeout probe that doesn't exist on the
    # pre-Phase-2 checkpoint; treat its absence as `null` (the report's
    # diff tool renders the delta as "n/a" in that case).
    if [[ $i -eq $RUNS ]]; then
        last_alloc_count=$(grep '^SIGIL_COUNTER_BOEHM_ALLOC_COUNT=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        last_alloc_bytes=$(grep '^SIGIL_COUNTER_BOEHM_ALLOC_BYTES=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        last_gc_time_ms=$(grep '^boehm_gc_time_ms=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        if [[ -z "$last_alloc_count" || -z "$last_alloc_bytes" ]]; then
            echo "measure-throughput: run $i — missing alloc counter line in stderr:" >&2
            cat "$stderr_log" >&2
            rm -f "$time_log" "$stderr_log"
            exit 1
        fi
        # Probe-absence on pre-Phase-2 builds maps to JSON null.
        if [[ -z "$last_gc_time_ms" ]]; then
            last_gc_time_ms="null"
        fi
    fi

    rm -f "$time_log" "$stderr_log"
done

# Aggregate (median, IQR, min, max) via awk.
aggregate() {
    local label="$1"; shift
    printf '%s\n' "$@" | sort -n | awk -v label="$label" '
        { v[NR] = $1 }
        END {
            n = NR;
            mid = int((n + 1) / 2);
            if (n % 2 == 1) {
                median = v[mid];
            } else {
                median = (v[mid] + v[mid + 1]) / 2;
            }
            q1_idx = int((n + 3) / 4);
            q3_idx = int((3 * n + 1) / 4);
            iqr = v[q3_idx] - v[q1_idx];
            printf "\"%s\": {\"median\": %.0f, \"iqr\": %.0f, \"min\": %d, \"max\": %d}",
                label, median, iqr, v[1], v[n];
        }
    '
}

wall_json=$(aggregate "wall_clock_ms" "${wall_ms_values[@]}")
rss_json=$(aggregate "peak_rss_kb" "${peak_rss_kb_values[@]}")

cat <<JSON
{
  "binary": "$BINARY",
  "runs": $RUNS,
  $wall_json,
  $rss_json,
  "alloc_count": $last_alloc_count,
  "alloc_bytes": $last_alloc_bytes,
  "boehm_gc_time_ms": $last_gc_time_ms
}
JSON
