#!/usr/bin/env bash
# Run the 74-file partial OpenSSL benchmark one or more times and extract
# stable summary metrics from each run.
#
# Defaults assume the durable corpus created during the perf sessions:
#   ~/infer-rs-bench/openssl-20260501-084151/textual-out/*.sil
#
# Examples:
#   scripts/bench_openssl_partial.sh
#   RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh
#   EXTRA_ARGS="--pulse-max-wall-secs 60" scripts/bench_openssl_partial.sh
#   BENCH_DIR=~/infer-rs-bench/openssl-20260501-084151 OUT_DIR=/tmp/bench scripts/bench_openssl_partial.sh
#
# Preflight knobs:
#   SKIP_FRESHNESS=1   skip the stale-binary check
#   REBUILD=1          run `cargo build --release -p infer-rs` if sources are
#                      newer than $BIN (or always, if SKIP_FRESHNESS=1 too)
#   REQUIRED_FLAGS=    extra flags to verify against `$BIN --help` (space-sep)
#                      built-ins + flags parsed from EXTRA_ARGS are always
#                      checked
#   SKIP_FLAG_CHECK=1  skip the --help flag preflight entirely
#
# Failure semantics:
#   default            exit 0 if at least one run succeeded; nonzero if every
#                      run failed (was: always 0 -- silent corpus regressions)
#   STRICT=1           exit nonzero if any run failed
#   PERMISSIVE=1       restore the legacy "always exit 0" behavior for
#                      exploratory runs (e.g. expected-fail bisects)
#
# Other knobs:
#   DRY_RUN=1          print resolved config + planned command and exit 0

set -euo pipefail

ROOT="${ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
BENCH_DIR="${BENCH_DIR:-$HOME/infer-rs-bench/openssl-20260501-084151}"
TEXTUAL_DIR="${TEXTUAL_DIR:-$BENCH_DIR/textual-out}"
BIN="${BIN:-$ROOT/target/release/infer-rs}"
RUNS="${RUNS:-1}"
JOBS="${JOBS:-4}"
OUT_DIR="${OUT_DIR:-$ROOT/bench-out/openssl-partial-$(date +%Y%m%d-%H%M%S)}"
EXTRA_ARGS="${EXTRA_ARGS:-}"
RUST_LOG_VALUE="${RUST_LOG_VALUE:-warn,ondemand=info}"
REQUIRED_FLAGS="${REQUIRED_FLAGS:-}"
STRICT="${STRICT:-0}"
PERMISSIVE="${PERMISSIVE:-0}"
SKIP_FRESHNESS="${SKIP_FRESHNESS:-0}"
SKIP_FLAG_CHECK="${SKIP_FLAG_CHECK:-0}"
REBUILD="${REBUILD:-0}"
DRY_RUN="${DRY_RUN:-0}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  awk '/^set -euo pipefail$/{exit} NR>1{sub(/^# ?/,""); print}' "$0"
  exit 0
fi

# ---------------------------------------------------------------------------
# Preflight 1: rebuild / freshness
# ---------------------------------------------------------------------------
need_build=0
if [[ "$REBUILD" == "1" ]]; then
  need_build=1
elif [[ ! -x "$BIN" ]]; then
  echo "[bench] $BIN missing; will build" >&2
  need_build=1
elif [[ "$SKIP_FRESHNESS" != "1" ]]; then
  # Are any tracked Rust sources newer than the binary?
  declare -a freshness_paths=()
  [[ -d "$ROOT/crates"     ]] && freshness_paths+=("$ROOT/crates")
  [[ -f "$ROOT/Cargo.toml" ]] && freshness_paths+=("$ROOT/Cargo.toml")
  [[ -f "$ROOT/Cargo.lock" ]] && freshness_paths+=("$ROOT/Cargo.lock")
  newer_count=0
  if (( ${#freshness_paths[@]} > 0 )); then
    newer_count=$(find "${freshness_paths[@]}" \
                    -type f \
                    \( -name '*.rs' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
                    -newer "$BIN" -print 2>/dev/null | wc -l | tr -d ' ')
  fi
  if [[ "$newer_count" != "0" ]]; then
    echo "error: $BIN is older than $newer_count workspace source file(s)." >&2
    echo "hint:  REBUILD=1 $0   # auto-rebuild" >&2
    echo "hint:  SKIP_FRESHNESS=1 $0   # benchmark current binary anyway" >&2
    echo "hint:  cargo build --release -p infer-rs" >&2
    exit 2
  fi
fi

if (( need_build )); then
  echo "[bench] cargo build --release -p infer-rs" >&2
  ( cd "$ROOT" && cargo build --release -p infer-rs )
fi

if [[ ! -x "$BIN" ]]; then
  echo "error: binary not executable after preflight: $BIN" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# Preflight 2: required flags
# ---------------------------------------------------------------------------
# Built-in flags the script always passes:
declare -a always_flags=(--pulse-only --quiet --trace-ondemand -j)
# Pull --foo / -x tokens out of EXTRA_ARGS so a typo'd flag fails before the
# benchmark spends an hour ignoring it.
declare -a extra_flag_tokens=()
# shellcheck disable=SC2206  # word-splitting is intentional here
extra_arg_words=( $EXTRA_ARGS )
for tok in "${extra_arg_words[@]:-}"; do
  case "$tok" in
    --*) extra_flag_tokens+=("$tok") ;;
  esac
done
declare -a explicit_required=()
# shellcheck disable=SC2206
required_words=( $REQUIRED_FLAGS )
for tok in "${required_words[@]:-}"; do
  explicit_required+=("$tok")
done

if [[ "$SKIP_FLAG_CHECK" != "1" ]]; then
  help_text=$("$BIN" --help 2>&1 || true)
  missing=()
  for flag in "${always_flags[@]}" "${extra_flag_tokens[@]}" "${explicit_required[@]}"; do
    [[ -z "$flag" ]] && continue
    # `-j` is positional-ish; clap prints it as `-j, --jobs <N>` etc., so a
    # plain substring match is enough.
    if ! grep -qE -- "(^|[[:space:]])${flag//\//\\/}([[:space:],=<>]|$)" <<<"$help_text"; then
      missing+=("$flag")
    fi
  done
  if (( ${#missing[@]} > 0 )); then
    echo "error: $BIN --help is missing flag(s): ${missing[*]}" >&2
    echo "hint:  rebuild ($BIN may predate the new flag): REBUILD=1 $0" >&2
    echo "hint:  or skip the check: SKIP_FLAG_CHECK=1 $0" >&2
    exit 2
  fi
fi

# ---------------------------------------------------------------------------
# Corpus + output setup
# ---------------------------------------------------------------------------
shopt -s nullglob
sil_files=("$TEXTUAL_DIR"/*.sil)
if (( ${#sil_files[@]} == 0 )); then
  echo "error: no .sil files under $TEXTUAL_DIR" >&2
  exit 2
fi

if [[ "$DRY_RUN" == "1" ]]; then
  echo "[bench] DRY_RUN config:"
  echo "  ROOT=$ROOT"
  echo "  BIN=$BIN"
  echo "  TEXTUAL_DIR=$TEXTUAL_DIR (${#sil_files[@]} .sil files)"
  echo "  OUT_DIR=$OUT_DIR (would be created)"
  echo "  RUNS=$RUNS JOBS=$JOBS"
  echo "  EXTRA_ARGS=$EXTRA_ARGS"
  echo "  RUST_LOG=$RUST_LOG_VALUE"
  echo "  STRICT=$STRICT PERMISSIVE=$PERMISSIVE"
  echo "  flags checked: ${always_flags[*]} ${extra_flag_tokens[*]:-} ${explicit_required[*]:-}"
  echo "[bench] would run (per iteration):"
  echo "  RUST_LOG=$RUST_LOG_VALUE /usr/bin/time -l \\"
  echo "    $BIN --pulse-only --quiet --trace-ondemand -j $JOBS $EXTRA_ARGS \\"
  echo "    <${#sil_files[@]} .sil files>"
  exit 0
fi

mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/summary.tsv"
printf 'run\texit\treal_s\tuser_s\tsys_s\tmax_rss_bytes\tpeak_footprint_bytes\taborts\tmax_visit_count\tanalyzed\tlog\n' > "$SUMMARY"

if (( RUNS <= 0 )); then
  echo "[bench] RUNS=$RUNS, wrote empty summary: $SUMMARY"
  exit 0
fi

extract_metrics() {
  local run="$1"
  local exit_code="$2"
  local log="$3"
  python3 - "$run" "$exit_code" "$log" "$SUMMARY" <<'PY'
import pathlib
import re
import sys

run, exit_code, log_path, summary_path = sys.argv[1:]
text = pathlib.Path(log_path).read_text(errors="replace")
lines = text.splitlines()

real_s = user_s = sys_s = ""
max_rss = peak = ""
for line in lines:
    m = re.search(r"\s*([0-9.]+)\s+real\s+([0-9.]+)\s+user\s+([0-9.]+)\s+sys", line)
    if m:
        real_s, user_s, sys_s = m.groups()
    m = re.search(r"\s*([0-9]+)\s+maximum resident set size", line)
    if m:
        max_rss = m.group(1)
    m = re.search(r"\s*([0-9]+)\s+peak memory footprint", line)
    if m:
        peak = m.group(1)

aborts = sum(1 for line in lines if "[pulse-progress] proc=" in line and " aborted at " in line)
visits = [int(m.group(1)) for m in re.finditer(r"max_visit_count=([0-9]+)", text)]
max_visit = max(visits) if visits else ""

analyzed = ""
for line in lines:
    m = re.search(r"checker=pulse done: analyzed=([0-9]+/[0-9]+)", line)
    if m:
        analyzed = m.group(1)

with open(summary_path, "a", encoding="utf-8") as out:
    out.write("\t".join(map(str, [run, exit_code, real_s, user_s, sys_s, max_rss, peak, aborts, max_visit, analyzed, log_path])) + "\n")

# Also emit a slow-proc table next to the log.
slow = []
for line in lines:
    m = re.search(r"slow proc done: (\S+) elapsed=([^ ]+)", line)
    if not m:
        continue
    elapsed = m.group(2)
    sec = 0.0
    rest = elapsed
    if "m" in rest:
        minutes, rest = rest.split("m", 1)
        sec += float(minutes) * 60
    if rest.endswith("s"):
        sec += float(rest[:-1])
    slow.append((sec, m.group(1), elapsed))

slow_path = pathlib.Path(log_path).with_suffix(".slow.tsv")
with slow_path.open("w", encoding="utf-8") as out:
    out.write("seconds\tproc\telapsed\n")
    for sec, proc, elapsed in sorted(slow, reverse=True):
        out.write(f"{sec:.3f}\t{proc}\t{elapsed}\n")
PY
}

failed_runs=0
ok_runs=0
for run in $(seq 1 "$RUNS"); do
  log="$OUT_DIR/run-$run.log"
  echo "[bench] run=$run/$RUNS jobs=$JOBS log=$log"
  set +e
  RUST_LOG="$RUST_LOG_VALUE" /usr/bin/time -l "$BIN" \
    --pulse-only --quiet --trace-ondemand -j "$JOBS" $EXTRA_ARGS \
    "${sil_files[@]}" > "$log" 2>&1
  code=$?
  set -e
  extract_metrics "$run" "$code" "$log"
  tail -n 1 "$SUMMARY"
  if [[ "$code" != "0" ]]; then
    failed_runs=$((failed_runs + 1))
    echo "[bench] run=$run exited with $code; continuing" >&2
  else
    ok_runs=$((ok_runs + 1))
  fi

done

echo "[bench] summary: $SUMMARY"
column -t -s $'\t' "$SUMMARY" 2>/dev/null || cat "$SUMMARY"
echo "[bench] runs ok=$ok_runs failed=$failed_runs total=$RUNS"

if [[ "$PERMISSIVE" == "1" ]]; then
  exit 0
fi
if [[ "$STRICT" == "1" && "$failed_runs" -gt 0 ]]; then
  echo "[bench] STRICT=1: $failed_runs/$RUNS run(s) failed" >&2
  exit 1
fi
if [[ "$ok_runs" -eq 0 ]]; then
  echo "[bench] all $RUNS run(s) failed" >&2
  exit 1
fi
exit 0
