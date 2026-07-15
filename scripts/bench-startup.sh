#!/usr/bin/env bash
# Benchmark claw startup time.
#
# Runs `claw --version` N times (plus one discarded warm-up run) and reports
# min/mean/max wall-clock time in milliseconds. Fails (exit 1) when the mean
# exceeds the threshold.
#
# Usage:
#   scripts/bench-startup.sh
#
# Environment overrides:
#   CLAW_BENCH_MAX_MS   mean-time threshold in ms
#                       (default: 800 for release builds, 2000 for debug)
#   CLAW_BENCH_RUNS     number of measured runs (default: 5)
#
# Exit codes: 0 ok, 1 too slow, 2 no binary / setup problem.

set -euo pipefail

# Consistent numeric formatting (decimal point) for awk and printf,
# regardless of the user's locale.
export LC_ALL=C

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# --------------------------------------------------------------------------
# Locate the binary: prefer release, fall back to debug.
# --------------------------------------------------------------------------

if [ -x "${REPO_ROOT}/rust/target/release/claw" ]; then
    CLAW_BIN="${REPO_ROOT}/rust/target/release/claw"
    BENCH_PROFILE="release"
elif [ -x "${REPO_ROOT}/rust/target/debug/claw" ]; then
    CLAW_BIN="${REPO_ROOT}/rust/target/debug/claw"
    BENCH_PROFILE="debug"
else
    echo "error: no claw binary found." 1>&2
    echo "Looked for:" 1>&2
    echo "  ${REPO_ROOT}/rust/target/release/claw" 1>&2
    echo "  ${REPO_ROOT}/rust/target/debug/claw" 1>&2
    echo "Build one first: ./install.sh (debug) or ./install.sh --release" 1>&2
    exit 2
fi

if [ "${BENCH_PROFILE}" = "release" ]; then
    MAX_MS="${CLAW_BENCH_MAX_MS:-800}"
else
    MAX_MS="${CLAW_BENCH_MAX_MS:-2000}"
fi

RUNS="${CLAW_BENCH_RUNS:-5}"
case "${RUNS}" in
    ''|*[!0-9]*|0)
        echo "error: CLAW_BENCH_RUNS must be a positive integer (got '${RUNS}')" 1>&2
        exit 2
        ;;
esac

# --------------------------------------------------------------------------
# Timing helper: microsecond timestamps via bash 5's EPOCHREALTIME, falling
# back to GNU date's nanoseconds.
# --------------------------------------------------------------------------

if [ -n "${EPOCHREALTIME:-}" ]; then
    now_us() {
        local t="${EPOCHREALTIME}"
        # seconds.microseconds -> microseconds (locale may use ',' as separator)
        t="${t/,/.}"
        printf '%s%s' "${t%.*}" "${t#*.}"
    }
else
    NS_PROBE="$(date +%s%N)"
    case "${NS_PROBE}" in
        *[!0-9]*)
            echo "error: need bash >= 5 (EPOCHREALTIME) or GNU date with %N support" 1>&2
            exit 2
            ;;
    esac
    now_us() {
        local ns
        ns="$(date +%s%N)"
        printf '%s' "$((ns / 1000))"
    }
fi

run_once_ms() {
    local start end
    start="$(now_us)"
    if ! "${CLAW_BIN}" --version >/dev/null 2>&1; then
        echo "error: '${CLAW_BIN} --version' failed" 1>&2
        exit 2
    fi
    end="$(now_us)"
    awk -v s="${start}" -v e="${end}" 'BEGIN { printf "%.2f", (e - s) / 1000 }'
}

# --------------------------------------------------------------------------
# Benchmark
# --------------------------------------------------------------------------

echo "claw startup benchmark"
echo "  binary:    ${CLAW_BIN}"
echo "  profile:   ${BENCH_PROFILE}"
echo "  runs:      ${RUNS} (+1 warm-up, discarded)"
echo "  threshold: ${MAX_MS} ms (mean)"
echo

WARMUP_MS="$(run_once_ms)"
echo "  warm-up:  ${WARMUP_MS} ms (discarded)"

SAMPLES=()
for i in $(seq 1 "${RUNS}"); do
    ms="$(run_once_ms)"
    SAMPLES+=("${ms}")
    echo "  run ${i}:    ${ms} ms"
done

STATS="$(printf '%s\n' "${SAMPLES[@]}" | awk '
    NR == 1 { min = $1; max = $1 }
    { sum += $1; if ($1 < min) min = $1; if ($1 > max) max = $1 }
    END { printf "%.2f %.2f %.2f", min, sum / NR, max }
')"
MIN_MS="${STATS%% *}"
MAX_SEEN_MS="${STATS##* }"
MEAN_MS="${STATS#* }"
MEAN_MS="${MEAN_MS% *}"

echo
echo "  min:  ${MIN_MS} ms"
echo "  mean: ${MEAN_MS} ms"
echo "  max:  ${MAX_SEEN_MS} ms"
echo

if awk -v mean="${MEAN_MS}" -v max="${MAX_MS}" 'BEGIN { exit !(mean > max) }'; then
    echo "FAIL: mean startup ${MEAN_MS} ms exceeds threshold ${MAX_MS} ms (${BENCH_PROFILE} build)" 1>&2
    exit 1
fi

echo "OK: mean startup ${MEAN_MS} ms is within threshold ${MAX_MS} ms (${BENCH_PROFILE} build)"
