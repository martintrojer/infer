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
`USE_AFTER_FREE` counts per file against OCaml expected issues.

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

## Determinism

Analysis results are deterministic across runs:

- thread-local `AbstractValue` counters reset per procedure,
- `BTreeMap`/`BTreeSet` in analysis-critical structures, and
- sorted wave ordering in call graph scheduling.
