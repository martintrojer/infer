# Testing Strategy

## Test Tracks

| Command | What it runs | Speed |
|---------|-------------|-------|
| `make check` | fmt + clippy + all non-ignored tests | ~3s |
| `make check-full` | + C dump-textual sweep + #[ignore] tests | ~60s |

## Test Levels

**Unit tests** -- inline `#[cfg(test)]` modules in each crate. Run with `cargo test`.

**Compliance tests** -- ported from OCaml unit tests (`TextualParserTest.ml`, `abstractInterpreterTests.ml`, etc.). In `tests/compliance_tests.rs` per crate.

**OCaml SIL end-to-end** -- parse OCaml `.sil` test files directly and verify Pulse results. In `pulse/tests/end_to_end.rs`. Tests reference OCaml source files in `infer/tests/codetoanalyze/sil/pulse/`.

**C dump-textual sweep** -- the full pipeline: C source -> OCaml `infer capture --dump-textual` -> Rust parse -> Pulse analysis. Compares against OCaml's `issues.exp`. Run with `make check-full` (requires `infer` binary).

## Compliance Comparison

The sweep compares NULLPTR_DEREFERENCE and MEMORY_LEAK_C counts per file against OCaml's expected issues. See [STATUS.md](STATUS.md) for current numbers.

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
