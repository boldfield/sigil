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
#   - Linux uses GNU /usr/bin/time -v (apt: `time` package).
#   - macOS uses BSD /usr/bin/time -l (base system).
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
#     "boehm_gc_time_ms": <int or null, from boehm_gc_time_ms>
#     "precise_walker_ns": <int or null, from SIGIL_COUNTER_PRECISE_WALKER_NS>
#     "forced_gc_count":  <int or null, from SIGIL_COUNTER_FORCED_GC_COUNT>
#     "alloc_wrap_elided_count": <int or null, from SIGIL_COUNTER_ALLOC_WRAP_ELIDED_COUNT>
#   }
#
# Wall-clock / RSS are aggregated across runs; alloc counters are
# read from the last run only (they're deterministic — a workload
# allocates the same number of objects every run).

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

OS_KIND="$(uname -s)"
case "$OS_KIND" in
    Linux|Darwin) ;;
    *)
        echo "measure-throughput: unsupported OS $OS_KIND" >&2
        exit 1
        ;;
esac

# Parse the time-output file into shell-eval friendly key=value lines.
# Centralised here so both extraction and conversion live in one awk
# pass per OS — avoids regex / pipefail traps that were eating
# wall-clock values when the previous grep+sed implementation matched
# unexpected line shapes.
#
# Emits to stdout:
#   wall_ms=<int>
#   rss_kb=<int>
parse_time_log() {
    local file="$1"
    local kind="$2"
    if [[ "$kind" == "Linux" ]]; then
        awk '
            # GNU time -v: leading TAB, then label, then ": value".
            # Wall-clock line: "Elapsed (wall clock) time (h:mm:ss or m:ss): 0:00.42"
            # We only care about the last whitespace-separated field of
            # the matching line (the time itself), so use $NF.
            /Elapsed \(wall clock\) time/ {
                v = $NF;
                ms = 0;
                n = split(v, parts, ":");
                if (n == 3) {
                    ms = (parts[1] * 3600 + parts[2] * 60 + parts[3]) * 1000;
                } else if (n == 2) {
                    ms = (parts[1] * 60 + parts[2]) * 1000;
                } else {
                    ms = parts[1] * 1000;
                }
                printf "wall_ms=%d\n", ms + 0.5;
                wall_seen = 1;
            }
            /Maximum resident set size/ {
                # Last field is kB on GNU time.
                printf "rss_kb=%d\n", $NF;
                rss_seen = 1;
            }
            END {
                if (!wall_seen) printf "wall_ms=0\n";
                if (!rss_seen)  printf "rss_kb=0\n";
            }
        ' "$file"
    else
        awk '
            # macOS BSD time -l: "        0.42 real         0.00 user ..."
            # Wall-clock is the first field of the "real" line.
            $2 == "real" {
                ms = ($1 * 1000) + 0.5;
                printf "wall_ms=%d\n", ms;
                wall_seen = 1;
            }
            /maximum resident set size/ {
                # First field is bytes on macOS BSD time; normalise to kB.
                kb = ($1 / 1024) + 0.5;
                printf "rss_kb=%d\n", kb;
                rss_seen = 1;
            }
            END {
                if (!wall_seen) printf "wall_ms=0\n";
                if (!rss_seen)  printf "rss_kb=0\n";
            }
        ' "$file"
    fi
}

wall_ms_values=()
peak_rss_kb_values=()
last_alloc_count=""
last_alloc_bytes=""
last_gc_time_ms=""
last_precise_walker_ns=""
last_forced_gc_count=""
last_alloc_wrap_elided_count=""

for ((i = 1; i <= RUNS; i++)); do
    time_log="$(mktemp)"
    stderr_log="$(mktemp)"
    # `LC_ALL=C` pins the locale for /usr/bin/time's output. GNU
    # time hardcodes the format strings in English regardless of
    # locale (per `time(1)`'s source), but BSD time (macOS) is
    # less explicit; setting C unifies the output shape across
    # both hosts and protects against any future locale-driven
    # format drift on either side.
    if ! LC_ALL=C /usr/bin/time \
            $([[ "$OS_KIND" == "Linux" ]] && echo "-v" || echo "-l") \
            -o "$time_log" \
            env SIGIL_PRINT_STATS=1 "$BINARY" \
            > /dev/null 2> "$stderr_log"; then
        echo "measure-throughput: run $i failed" >&2
        echo "--- time log ---" >&2
        cat "$time_log" >&2
        echo "--- stderr log ---" >&2
        cat "$stderr_log" >&2
        rm -f "$time_log" "$stderr_log"
        exit 1
    fi

    # eval the awk-emitted key=value pairs into wall_ms + rss_kb.
    wall_ms=""
    rss_kb=""
    while IFS= read -r kv; do
        eval "$kv"
    done < <(parse_time_log "$time_log" "$OS_KIND")

    if [[ -z "$wall_ms" || -z "$rss_kb" ]]; then
        echo "measure-throughput: run $i — parser returned empty values:" >&2
        cat "$time_log" >&2
        rm -f "$time_log" "$stderr_log"
        exit 1
    fi

    # If the parser said wall_ms=0, dump the time log on the first
    # run so failures are diagnosable in CI output. (Subsequent runs
    # likely repeat the same failure mode; one dump is enough.)
    if [[ "$wall_ms" == "0" && "$i" -eq 1 ]]; then
        echo "measure-throughput: warning — wall_ms=0 on first run. time log content:" >&2
        cat "$time_log" >&2
    fi

    wall_ms_values+=("$wall_ms")
    peak_rss_kb_values+=("$rss_kb")

    # Counters from the child's stderr (last run wins). The
    # `tail -1` after each grep is defensive: if a future runtime
    # change ever emits the same counter line more than once per
    # exit (e.g., a wrapper that calls sigil_counter_print_all
    # twice), `tail -1` takes the last value rather than mixing
    # multiple readings via shell word-splitting.
    # `boehm_gc_time_ms` is a Plan E2 Phase 2 closeout probe that
    # doesn't exist on the pre-Phase-2 checkpoint; treat its absence
    # as `null` (the report's diff tool renders the delta as "n/a"
    # in that case).
    if [[ $i -eq $RUNS ]]; then
        last_alloc_count=$(grep '^SIGIL_COUNTER_BOEHM_ALLOC_COUNT=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        last_alloc_bytes=$(grep '^SIGIL_COUNTER_BOEHM_ALLOC_BYTES=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        last_gc_time_ms=$(grep '^boehm_gc_time_ms=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        # Plan E2 Phase 3 GC-time follow-up — precise-walker
        # cumulative ns. Counter is introduced post-Phase-3-followup
        # only, so pre-checkpoint builds will not emit this line.
        # Treat its absence as `null` (the diff tool renders "n/a").
        last_precise_walker_ns=$(grep '^SIGIL_COUNTER_PRECISE_WALKER_NS=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        # Plan E2 Phase 3 GC-time follow-up #2 — count of forced
        # `GC_gcollect()` injections fired by the
        # `SIGIL_FORCE_GC_EVERY_N_ALLOCS` cadence. Diagnostic-only:
        # the operator's sanity check is `forced_gc_count ≈
        # alloc_count / N`, distinguishing "injection fired,
        # `boehm_gc_time_ms` is genuinely 0" from "injection silently
        # didn't fire". The pre-checkpoint binary's cherry-picked
        # patch deliberately omits this counter slot (counters.rs
        # would need a sibling patch), so pre-side reads as `null`.
        last_forced_gc_count=$(grep '^SIGIL_COUNTER_FORCED_GC_COUNT=' "$stderr_log" \
            | tail -1 | cut -d= -f2 || true)
        # Plan E2 alloc-trampoline-elision Task 6 — count of allocs
        # that took the elided fast path (`SIGIL_ALLOC_ELIDE_WRAP=1`
        # AND thread not parked in `GC_do_blocking`). Counter is
        # introduced post-PR-#181, so pre-checkpoint builds will not
        # emit this line. The operator's sanity check for a green
        # Task 6 run is `alloc_wrap_elided_count > 0` on the post
        # side AND `null` on the pre side — disambiguates "elision
        # fires but TLS-read cost eats the win" from "elision never
        # fired because env didn't reach runtime", per the plan
        # body's Task 6 conclusion-branch criteria.
        last_alloc_wrap_elided_count=$(grep '^SIGIL_COUNTER_ALLOC_WRAP_ELIDED_COUNT=' "$stderr_log" \
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
        if [[ -z "$last_precise_walker_ns" ]]; then
            last_precise_walker_ns="null"
        fi
        if [[ -z "$last_forced_gc_count" ]]; then
            last_forced_gc_count="null"
        fi
        if [[ -z "$last_alloc_wrap_elided_count" ]]; then
            last_alloc_wrap_elided_count="null"
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
  "boehm_gc_time_ms": $last_gc_time_ms,
  "precise_walker_ns": $last_precise_walker_ns,
  "forced_gc_count": $last_forced_gc_count,
  "alloc_wrap_elided_count": $last_alloc_wrap_elided_count
}
JSON
