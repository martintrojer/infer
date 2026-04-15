# Testing Strategy

## Test Tracks

| Command | What it runs | Speed |
|---------|-------------|-------|
| `make check` | fmt + clippy + all non-ignored tests | ~3s |
| `make check-full` | + C dump-textual sweep + #[ignore] tests | ~60s |
| `cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture` | authoritative Pulse compliance sweep via `--store-textual` + export | ~10s plus build |

## Test Levels

**Unit tests** -- inline `#[cfg(test)]` modules in each crate. Run with `cargo test`.

**Compliance tests** -- ported from OCaml unit tests (`TextualParserTest.ml`, `abstractInterpreterTests.ml`, etc.). In `tests/compliance_tests.rs` per crate.

**OCaml SIL end-to-end** -- parse OCaml `.sil` test files directly and verify Pulse results. In `pulse/tests/end_to_end.rs`. Tests reference OCaml source files in `infer/tests/codetoanalyze/sil/pulse/`.

**C store-textual sweep** -- the compliance benchmark reported in `STATUS.md`: C source -> OCaml `infer --store-textual` -> `infer debug --export-textual` -> Rust parse -> Pulse analysis. Run with `cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture` (requires `infer` binary).

**C dump-textual sweep** -- a secondary pipeline check: C source -> OCaml `infer capture --dump-textual` -> Rust parse -> Pulse analysis. Run with `make check-full` (requires `infer` binary).

## Why Two Sweeps?

The sweeps cover different seams:

- **Store-textual sweep** is the authoritative compliance benchmark. It exercises the same pipeline the CLI uses: batch capture with `infer --store-textual`, textual export via `infer debug --export-textual`, manifest handling, line-map remapping, then Rust analysis.
- **Dump-textual sweep** is a secondary ingestion-path regression test. It checks that raw `infer capture --dump-textual` output still parses, transforms, and analyzes correctly, which is useful for parser/to_sil regressions and single-file debugging.

In practice:

- Use the **store-textual** sweep to track parity numbers and update `STATUS.md`.
- Use the **dump-textual** sweep to catch breakage in the alternate `.sil` ingestion path.

## Compliance Comparison

The store-textual sweep compares NULLPTR_DEREFERENCE, MEMORY_LEAK_C, and USE_AFTER_FREE counts per file against OCaml's expected issues.

Current baseline:
- 52/55 files analyzed successfully
- NPE expected 131, found 134
- Leaks 20/20
- UAF 7/7

See [STATUS.md](STATUS.md) for the per-file differences.

## External Benchmark Comparison

For larger ad-hoc comparisons, keep OCaml Infer and `infer-rs` on the same
captured benchmark instead of rebuilding twice. Keep setup cost separate from
analysis cost:

- `infer-rs --results-dir ...` is the convenient end-to-end path for issue comparison and debugging.
- That path shells out to `infer debug --export-textual`, so it is not a fair Rust-only timing number.
- For apples-to-apples timing against `infer analyze`, export textual once, then time direct `.sil` analysis with the same `-j`.

### Shared Capture Setup

```bash
BENCH=/tmp/infer-rs-openssl-...
CLANG_BIN=/Users/mtrojer/infer-rs/facebook-clang-plugins/clang/install/bin
SDKROOT=$(xcrun --show-sdk-path)

cd "$BENCH/openssl-1.0.2d"

# Old OpenSSL on macOS/Apple Silicon: avoid the default i386 + asm path.
PATH="$CLANG_BIN:$PATH" CC=clang ./Configure darwin64-x86_64-cc no-asm

BASE_CFLAG=$(sed -n 's/^CFLAG= //p' Makefile)

# Shared capture for both analyzers.
PATH="$CLANG_BIN:$PATH" infer capture --keep-going --store-textual \
  --project-root .. \
  -o ../infer-out \
  -- make -k -j 1 CC=clang CFLAG="$BASE_CFLAG -isysroot $SDKROOT"

# Sanity-check that capture is real before comparing analyzers.
infer debug --results-dir ../infer-out --source-files | head
```

### Correctness Comparison On One Capture

Use the shared `infer-out` for issue comparison:

```bash
# Match jobs if you want a fair timing baseline.
infer analyze --pulse-only --results-dir ../infer-out -j 1

# Export the textual payload once for inspection/debugging and direct Rust runs.
infer debug --results-dir ../infer-out --export-textual ../textual-out

# Convenience Rust path on the same capture.db.
# Not a fair Rust-only timing number: this includes export-textual work.
/Users/mtrojer/infer-rs/infer-rs/target/release/infer-rs \
  --pulse-only \
  --trace-ondemand \
  --results-dir ../infer-out \
  -o ../infer-rs-out
```

### Fair Rust Timing

For timing, reuse the already exported `.sil` files and bypass `--results-dir`:

```bash
/Users/mtrojer/infer-rs/infer-rs/target/release/infer-rs \
  --pulse-only \
  --trace-ondemand \
  -j 1 \
  ../textual-out/*.sil
```

For the OpenSSL benchmark on this host, the fair Rust timing should be compared
to `infer analyze --pulse-only --results-dir ../infer-out -j 1`, not to an
OCaml run at a different `-j`.

Important macOS notes learned from the OpenSSL benchmark:

- Use the repo clang through `PATH=.../facebook-clang-plugins/clang/install/bin:$PATH`
  and `CC=clang`.
- Do not use `CC=/absolute/path/to/clang` for the benchmark build. That can let
  the build succeed while Infer captures nothing; `infer debug --source-files`
  will be empty.
- The repo clang does not automatically pick up the macOS SDK headers here, so
  append `-isysroot $(xcrun --show-sdk-path)` to the benchmark `CFLAG`/`CFLAGS`.
- `./config` on OpenSSL 1.0.2d picked `darwin-i386-cc` on this host and hit
  legacy asm capture failures. `./Configure darwin64-x86_64-cc no-asm` avoids
  that.
- `--keep-going` is useful on old benchmark builds: late link/app failures do
  not discard already captured translation units.

## Per-Instruction Tracing

For debugging analysis divergences between OCaml and Rust:

```bash
# OCaml: generate HTML debug traces
infer --pulse-only --debug -j 1 -- clang -c file.c

# Rust: generate log traces
infer-rs --debug-level-analysis 1 file.sil

# Compare side-by-side
python3 scripts/compare_traces.py \
    --ocaml-dir infer-out/captured/<hash>/nodes/ \
    --rust-log rust_trace.log \
    --proc function_name
```

See CLAUDE.md "Step-by-step tracing for compliance debugging" for details.

## Scheduler Tracing

For long merged interprocedural runs, enable OCaml-compatible scheduler tracing:

```bash
infer-rs --trace-ondemand --pulse-only --results-dir infer-out
```

If `RUST_LOG` is not already set, this defaults to `warn,ondemand=info`. The
runner logs:

- wave start/end lines
- a periodic snapshot every 10s while a wave is still running
- completed summaries vs total procedures
- throughput and coarse ETA

You can raise verbosity further with explicit logger filters, for example
`RUST_LOG=warn,ondemand=debug infer-rs --trace-ondemand ...` to include wave
member lists.

## Determinism

Analysis results are deterministic across runs:
- Thread-local `AbstractValue` counters (reset per procedure)
- `BTreeMap`/`BTreeSet` in analysis-critical structures
- Sorted wave ordering in call graph scheduling
