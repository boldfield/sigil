#!/usr/bin/env bash
# peak-rss.sh — measure peak resident-set size of a command and its tree.
#
# Combines two signals:
#
#   1. `/usr/bin/time -l` on the root process. The kernel tracks
#      `ru_maxrss` for the entire life of the process, including
#      page-fault-warmup spikes. This is authoritative for the root
#      and bulletproof against short-lived commands.
#
#   2. Polling `ps -o rss=` over the descendant tree every N ms.
#      Needed for parallel builds (e.g. `cargo build -j 8`) where
#      several rustc workers overlap in time but the root (cargo)
#      itself stays small. `ru_maxrss` on cargo alone undercounts
#      the peak tree size in that case.
#
# The reported peak is max(root_time_l_peak, polling_tree_peak).
#
# Usage:
#   scripts/peak-rss.sh [--] <command> [args...]
#   scripts/peak-rss.sh -h | --help
#
# Output goes to stderr so the wrapped command's stdout is unaffected.
# Exit code mirrors the wrapped command's exit code.
#
# The polling interval defaults to 10ms (catches short commands well;
# ps fork overhead is still <1% of a cargo build's wall time). Override
# via PEAK_RSS_INTERVAL_SECONDS=<seconds> in the environment.
#
# macOS- and Linux-compatible. Not wired into CI or pod-verify.

set -eu

print_usage() {
  cat <<'__USAGE_EOF__' >&2
peak-rss.sh — measure peak resident-set size of a command tree.

USAGE
    scripts/peak-rss.sh [--] <command> [args...]
    scripts/peak-rss.sh -h | --help

EXAMPLES
    # Full parallel cargo build — the realistic whole-tree peak.
    cargo clean
    scripts/peak-rss.sh cargo build --release

    # Full workspace test suite — usually the heaviest workload
    # (many test binaries linked concurrently).
    cargo clean
    scripts/peak-rss.sh cargo test --workspace --no-fail-fast

    # Single-worker build — approximates what a constrained pod sees
    # per individual rustc. Useful for estimating pod-memory ceilings.
    cargo clean
    scripts/peak-rss.sh cargo build --release -j 1

    # Sigil compiling a closure-heavy example — the number that OOMs
    # a constrained pod.
    scripts/peak-rss.sh ./target/release/sigil examples/higher_order.sigil -o /tmp/ho

OUTPUT
    The wrapped command's stdout and stderr pass through unchanged.
    A summary block is written to stderr after the command exits:

        peak rss:
          root process (kernel ru_maxrss): 66519040 bytes (63 MiB)
          descendant tree (10ms polling):  64832 KiB (63 MiB)
          reported peak:                   66519040 bytes (63 MiB)

    The reported peak is max(root, tree). Short-lived commands rely on
    root; long parallel builds rely on tree.

    Exit code is the wrapped command's exit code.

ENVIRONMENT
    PEAK_RSS_INTERVAL_SECONDS
        Polling interval in seconds. Default: 0.01 (10ms). Lower values
        catch shorter spikes at higher ps-fork cost; values under ~0.005
        start to sample-skew due to ps overhead on macOS.

NOTES
    - Tool is developer-local. Not run by CI or scripts/pod-verify.sh.
    - Root is measured via /usr/bin/time -l (BSD time on macOS, also
      available on Linux).
    - Tree polling uses pgrep -P recursively. Processes that reparent
      to init via double-fork escape the tree; cargo and rustc do not.
    - RSS is physical memory. Swap is not counted.
__USAGE_EOF__
}

case "${1:-}" in
  -h|--help)
    print_usage
    exit 0
    ;;
  --)
    shift
    ;;
esac

if [ $# -eq 0 ]; then
  print_usage
  exit 64
fi

interval="${PEAK_RSS_INTERVAL_SECONDS:-0.01}"

# Pick a time(1) implementation that writes rusage-style output to
# stderr. On macOS and Linux this is /usr/bin/time (BSD/GNU time,
# respectively); the shell builtin `time` won't do. If /usr/bin/time
# is missing, fall back to polling-only.
time_bin=""
if [ -x /usr/bin/time ]; then
  time_bin=/usr/bin/time
fi

# Scratch file for capturing time -l's output. Keep outside the
# wrapped command's stderr so users still see their own errors.
rusage_file="$(mktemp -t peak-rss-rusage.XXXXXX)"
trap 'rm -f "$rusage_file"' EXIT

# Launch the target command. Tee /usr/bin/time's stderr so the user
# still sees any real stderr from the wrapped command, while also
# capturing it for rusage parsing.
if [ -n "$time_bin" ]; then
  # macOS BSD time -l and Linux GNU time -v both accept -l / -v and
  # both emit a "maximum resident set size" line we can grep for.
  # On macOS: bytes. On Linux: kilobytes. We normalize below.
  "$time_bin" -l "$@" 2> >(tee "$rusage_file" >&2) &
else
  "$@" &
fi
root=$!

peak_tree_kb=0

descendants() {
  local parent=$1
  echo "$parent"
  local child
  for child in $(pgrep -P "$parent" 2>/dev/null || true); do
    descendants "$child"
  done
}

while kill -0 "$root" 2>/dev/null; do
  pids=$(descendants "$root" 2>/dev/null | tr '\n' ' ' || true)
  if [ -n "${pids// /}" ]; then
    # shellcheck disable=SC2086
    total=$(ps -o rss= -p $pids 2>/dev/null | awk '{s+=$1} END {print s+0}')
    if [ "$total" -gt "$peak_tree_kb" ]; then
      peak_tree_kb=$total
    fi
  fi
  sleep "$interval"
done

rc=0
wait "$root" 2>/dev/null || rc=$?

# Parse ru_maxrss out of time -l / time -v output. macOS BSD time -l
# prints a line like:
#   66519040  maximum resident set size
# Linux GNU time -v prints:
#   Maximum resident set size (kbytes): 64832
#
# Normalize both to bytes.
root_peak_bytes=0
if [ -s "$rusage_file" ]; then
  # macOS first: one numeric field, then "maximum resident set size".
  macos_line=$(grep -E '[[:space:]]*[0-9]+[[:space:]]+maximum resident set size' "$rusage_file" || true)
  if [ -n "$macos_line" ]; then
    root_peak_bytes=$(echo "$macos_line" | awk '{print $1}')
  else
    # Linux GNU: "Maximum resident set size (kbytes): <N>"
    linux_line=$(grep -E 'Maximum resident set size' "$rusage_file" || true)
    if [ -n "$linux_line" ]; then
      linux_kb=$(echo "$linux_line" | awk -F': ' '{print $2}' | tr -d ' ')
      root_peak_bytes=$(( linux_kb * 1024 ))
    fi
  fi
fi

peak_tree_bytes=$(( peak_tree_kb * 1024 ))

# Reported peak is the larger of the two measurements.
reported=$peak_tree_bytes
if [ "$root_peak_bytes" -gt "$reported" ]; then
  reported=$root_peak_bytes
fi

fmt_bytes() {
  local b=$1
  local mib=$(( b / 1024 / 1024 ))
  local gib
  gib=$(awk -v x="$b" 'BEGIN { printf "%.2f", x / 1024 / 1024 / 1024 }')
  printf '%d bytes (%d MiB, %s GiB)' "$b" "$mib" "$gib"
}

{
  echo "peak rss:"
  if [ "$root_peak_bytes" -gt 0 ]; then
    echo "  root process (kernel ru_maxrss): $(fmt_bytes "$root_peak_bytes")"
  else
    echo "  root process (kernel ru_maxrss): unavailable (no /usr/bin/time)"
  fi
  if [ "$peak_tree_bytes" -gt 0 ]; then
    echo "  descendant tree (${interval}s polling): $(fmt_bytes "$peak_tree_bytes")"
  else
    echo "  descendant tree (${interval}s polling): 0 (command too short to sample)"
  fi
  echo "  reported peak:                   $(fmt_bytes "$reported")"
} >&2

exit "$rc"
