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

mkdir -p "$OUT_DIR"
SUMMARY="$OUT_DIR/summary.tsv"
printf 'run\texit\treal_s\tuser_s\tsys_s\tmax_rss_bytes\tpeak_footprint_bytes\taborts\tmax_visit_count\tanalyzed\tlog\n' > "$SUMMARY"

if (( RUNS <= 0 )); then
  echo "[bench] RUNS=$RUNS, wrote empty summary: $SUMMARY"
  exit 0
fi

if [[ ! -x "$BIN" ]]; then
  echo "error: binary not executable: $BIN" >&2
  echo "hint: cargo build --release -p infer-rs" >&2
  exit 2
fi

shopt -s nullglob
sil_files=("$TEXTUAL_DIR"/*.sil)
if (( ${#sil_files[@]} == 0 )); then
  echo "error: no .sil files under $TEXTUAL_DIR" >&2
  exit 2
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
    echo "[bench] run=$run exited with $code; continuing" >&2
  fi

done

echo "[bench] summary: $SUMMARY"
column -t -s $'\t' "$SUMMARY" 2>/dev/null || cat "$SUMMARY"
