# Testing strategy

This document explains how to run tests and benchmarks. Current status numbers
live in [`STATUS.md`](STATUS.md); active benchmark follow-ups live in `mu`.

```sh
mu task list -w infer-rs --status OPEN
```

## Test tracks

| Command | What it runs |
|---|---|
| `make check` | fmt + clippy + non-ignored tests |
| `make check-full` | `make check` plus C dump-textual sweep and ignored tests |
| `cargo test -p pulse --lib` | Pulse unit tests |
| `cargo test -p pulse --test end_to_end` | Pulse end-to-end tests on SIL/Textual fixtures |
| `cargo test -p infer-rs` | CLI tests |
| `cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture` | authoritative C store-textual compliance sweep |

`make check` and `make check-full` intentionally run cargo tests with
`RUST_TEST_THREADS=1`; the Pulse end-to-end harness shares global analysis state.

## Test levels

- **Unit tests**: inline `#[cfg(test)]` modules in each crate.
- **Compliance tests**: ported OCaml unit-test behavior in per-crate
  `tests/compliance_tests.rs` files.
- **OCaml SIL end-to-end**: direct `.sil` tests in `pulse/tests/end_to_end.rs`.
- **C store-textual sweep**: C source → OCaml `infer --store-textual` →
  `infer debug --export-textual` → Rust parse/lower/analyze. This is the
  authoritative compliance sweep reported in `STATUS.md`.
- **C dump-textual sweep**: C source → OCaml `infer capture --dump-textual` →
  Rust parse/lower/analyze. This is a secondary ingestion-path regression test.

Use store-textual for parity numbers and dump-textual for parser/to_sil
regressions and single-file debugging.

## Compliance comparison

The store-textual sweep compares `NULLPTR_DEREFERENCE`, `MEMORY_LEAK_C`, and
`USE_AFTER_FREE` counts per file against OCaml expected issues. Since
`d9da630ae7`, `test_store_textual_sweep` also acts as a baseline regression
asserting 52 OK files, 0 analysis failures, and 0 timeouts. It pins
`MEMORY_LEAK_C` parity at 20/20 and `USE_AFTER_FREE` parity at 7/7, both in
total and per file, while `NULLPTR_DEREFERENCE` only asserts found >= expected
(currently 131) so over-reports remain allowed and tracked in `STATUS.md`.

```sh
cd infer-rs
cargo test -p pulse --release --test end_to_end \
  test_store_textual_sweep -- --ignored --nocapture
```

See [`STATUS.md`](STATUS.md) for current counts and accepted deltas.

## Shared benchmark capture methodology

For larger comparisons, keep OCaml Infer and `infer-rs` on the same captured
benchmark instead of rebuilding twice. Keep setup/export cost separate from
analysis cost:

- `infer-rs --results-dir ...` is convenient for issue comparison/debugging.
- That path shells out to `infer debug --export-textual`, so it is not a fair
  Rust-only timing number.
- For apples-to-apples timing against `infer analyze`, export Textual once, then
  time direct `.sil` analysis with the same `-j`.

### OpenSSL shared capture setup

```bash
BENCH=/tmp/infer-rs-openssl-...
CLANG_BIN=/Users/mtrojer/infer-rs/facebook-clang-plugins/clang/install/bin
SDKROOT=$(xcrun --show-sdk-path)

cd "$BENCH/openssl-1.0.2d"

# Old OpenSSL on macOS/Apple Silicon: avoid the default i386 + asm path.
PATH="$CLANG_BIN:$PATH" CC=clang ./Configure darwin64-x86_64-cc no-asm

BASE_CFLAG=$(sed -n 's/^CFLAG= //p' Makefile)

PATH="$CLANG_BIN:$PATH" infer capture --keep-going --store-textual \
  --project-root .. \
  -o ../infer-out \
  -- make -k -j 1 CC=clang CFLAG="$BASE_CFLAG -isysroot $SDKROOT"

infer debug --results-dir ../infer-out --source-files | head
```

macOS/OpenSSL notes:

- Put repo clang on `PATH` and set `CC=clang`.
- Avoid `CC=/absolute/path/to/clang`; it can let the build succeed while Infer
  captures nothing.
- Append `-isysroot $(xcrun --show-sdk-path)` to benchmark `CFLAG`/`CFLAGS`.
- OpenSSL 1.0.2d `./config` can pick `darwin-i386-cc`; prefer
  `./Configure darwin64-x86_64-cc no-asm`.
- `--keep-going` preserves captured translation units even if old benchmark
  builds fail late.

### Correctness comparison on one capture

```bash
infer analyze --pulse-only --results-dir ../infer-out -j 1
infer debug --results-dir ../infer-out --export-textual ../textual-out

/Users/mtrojer/infer-rs/infer-rs/target/release/infer-rs \
  --pulse-only \
  --trace-ondemand \
  --results-dir ../infer-out \
  -o ../infer-rs-out
```

### Fair Rust timing

```bash
/Users/mtrojer/infer-rs/infer-rs/target/release/infer-rs \
  --pulse-only \
  --trace-ondemand \
  -j 1 \
  ../textual-out/*.sil
```

For repeated OpenSSL partial-corpus runs, use:

```bash
cd infer-rs
OUT_DIR="$(pwd)/bench-out/current-head-openssl-$(date +%Y%m%d-%H%M%S)" \
  RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh
```

`scripts/bench_openssl_partial.sh` runs preflight checks before the first
iteration:

- the release binary must exist and not be older than any tracked workspace
  source file (`SKIP_FRESHNESS=1` to bypass, `REBUILD=1` to auto-rebuild);
- every flag the script passes (built-ins, anything in `EXTRA_ARGS`, anything
  listed in `REQUIRED_FLAGS`) must show up in `$BIN --help`
  (`SKIP_FLAG_CHECK=1` to bypass).

Failure semantics:

- default: exit 0 if at least one run succeeds, nonzero if every run fails;
- `STRICT=1`: exit nonzero if any run fails;
- `PERMISSIVE=1`: legacy "always exit 0" mode for exploratory bisects.

Use `DRY_RUN=1` or `--help` to inspect the resolved configuration without
starting the benchmark.

## Per-instruction tracing

```bash
# OCaml: generate HTML debug traces
infer --pulse-only --debug -j 1 -- clang -c file.c

# Rust: generate log traces
infer-rs --debug-level-analysis 1 file.sil

# Rust: narrow a large corpus to one procedure
infer-rs --debug-level-analysis 1 --procedures-filter 'target_proc' *.sil

# Compare side by side
python3 scripts/compare_traces.py \
  --ocaml-dir infer-out/captured/<hash>/nodes/ \
  --rust-log rust_trace.log \
  --proc function_name
```

See [`../AGENTS.md`](../AGENTS.md) for the compliance-debugging recipe.

## Scheduler and retained-state tracing

```bash
infer-rs --trace-ondemand --pulse-only --results-dir infer-out
```

If `RUST_LOG` is unset, `--trace-ondemand` defaults to
`warn,ondemand=info`. It logs:

- wave start/end lines,
- periodic wave snapshots,
- completed summaries vs total procedures,
- throughput and coarse ETA,
- `pulse-progress` heartbeats for long-running procedures, and
- `live-fixpoint` heartbeats for retained invariant-map state.

Use `pulse-progress` and `live-fixpoint` together to separate active-frontier
cost from retained-state storage cost.

## Resource hazards

- In-process summary harnesses such as `test_summary_comparison_c_triage` do
  not honor `--pulse-max-heap-mb` or `--pulse-max-wall-secs`, because they run
  analysis in-process rather than through the capped `infer-rs` CLI. During the
  2026-05-14 Linux session on devvm36499 (235 GB RAM), repeated measurements
  climbed past 160 GB RSS before orchestrator SIGTERM.
- `nullptr.c` is explicitly skipped in the triage harness (commit `363f07abdc`)
  because its in-process analysis grows to 44+ GB. Use the capped CLI for
  `nullptr.c` analysis instead.
- Scope these tests to one file and add external caps, for example:
  `ulimit -v 8388608; INFER_RS_C_TRIAGE_FILES=<one_file>
  RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 timeout 180 cargo test
  test_summary_comparison_c_triage`.
- Whole-corpus `infer-rs --pulse-only -j N <textual-out>/*.sil` runs can also
  bypass practical per-procedure protection when `-j` is high. Raw invocations
  should mirror the `scripts/bench_openssl_partial.sh` defaults:
  `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`.
- Incident note: worker `scout_openssl_corpus_capture_linux` scope-crept into a
  full-corpus `-j4` run that reached about 160 GB RSS and was SIGTERM-killed.
  Prefer single-file invocations or the bench script for corpus experiments.

## Profiling tools (Linux)

Audit from devvm36499 (CentOS Stream 9) on 2026-05-14. Profile only one
`.sil` file at a time and keep the same safety caps as
[`scripts/bench_openssl_partial.sh`](../scripts/bench_openssl_partial.sh):
`--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 -j 1`. The orchestrator
killed two exploratory runs around 160 GB RSS, so do not profile full corpora.
If `/proc/sys/kernel/perf_event_paranoid` is greater than `2`, `perf`-based
profilers need root or `sudo sysctl kernel.perf_event_paranoid=1`; devvm36499
currently has `1`, which is usable by unprivileged users.

Set a shell helper before running the examples:

```bash
BIN=target/release/infer-rs
ARGS="--pulse-only --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 -j 1 textual-out/one.sil"
```

| Tool | devvm36499 status | Quick command, output, and hint |
|---|---|---|
| `perf` | installed: `perf version 6.19.0-rc6` | `perf record -g --call-graph dwarf -o perf.data -- $BIN $ARGS`; open with `perf report -i perf.data`; sort by children/self time for wall-CPU hotspots. |
| `cargo flamegraph` | not installed | Install with `cargo install flamegraph`; then `cargo flamegraph --output flamegraph.svg -- $BIN $ARGS`; output SVG shows wide stacks as hot paths. |
| `heaptrack` / `heaptrack_print` | not installed | Install with `sudo dnf install heaptrack` (or `sudo apt install heaptrack`); run `heaptrack --output heaptrack.gz -- $BIN $ARGS`; inspect with `heaptrack_print heaptrack.gz` or GUI for allocation hot spots. |
| `valgrind --tool=massif` | installed via `valgrind-3.22.0` | `valgrind --tool=massif --massif-out-file=massif.out -- $BIN $ARGS`; output is heap snapshots; inspect peaks with `ms_print massif.out` where available. |
| `valgrind --tool=callgrind` | installed via `valgrind-3.22.0` | `valgrind --tool=callgrind --callgrind-out-file=callgrind.out -- $BIN $ARGS`; very slow; inspect inclusive instruction counts with `callgrind_annotate`/KCachegrind. |
| `dhat` crate | Cargo dependency, not a system tool | Add a feature-gated dep such as `dhat = "0.3"`, install `#[global_allocator] static ALLOC: dhat::Alloc = dhat::Alloc;`, and guard `let _profiler = dhat::Profiler::new_heap();`; output JSON/terminal heap profile when the feature is enabled. |
| `bytehound` | not installed | Build from source (`git clone https://github.com/koute/bytehound && cd bytehound && cargo build --release`) or use distro packages if available; run with `LD_PRELOAD=.../libbytehound.so $BIN $ARGS`; view recorded allocation data with the `bytehound` UI/server. |
| `samply` | not installed | Install with `cargo install samply`; run `samply record --save-only --output profile.json -- $BIN $ARGS`; open with `samply load profile.json` for a browser timeline/flame graph. |
| `/usr/bin/time -v` | installed: GNU Time 1.9 | `/usr/bin/time -v $BIN $ARGS 2>time.txt`; `Maximum resident set size` is max RSS and `Elapsed` is wall time. |
| `strace -c` | installed: `strace -- version 6.12` | `strace -c -o strace-counts.txt -- $BIN $ARGS`; output ranks syscall count/time/errors, useful for I/O or process-spawn suspicion. |

## Determinism

Analysis results are deterministic across runs:

- thread-local `AbstractValue` counters reset per procedure,
- `BTreeMap`/`BTreeSet` in analysis-critical structures, and
- sorted wave ordering in call graph scheduling.
