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
- NPE 130/135
- Leaks 20/20
- UAF 10/7

See [STATUS.md](STATUS.md) for the per-file differences.

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

## Determinism

Analysis results are deterministic across runs:
- Thread-local `AbstractValue` counters (reset per procedure)
- `BTreeMap`/`BTreeSet` in analysis-critical structures
- Sorted wave ordering in call graph scheduling
