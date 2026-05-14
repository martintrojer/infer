# infer-rs

Rust implementation of Infer's Pulse analysis for Textual/SIL programs. The goal is to
match Infer's OCaml Pulse semantics while making the analysis easy to test, profile, and
iterate on in Rust.

## Current status

The authoritative dashboard is [`docs/STATUS.md`](docs/STATUS.md). At a glance:

- Store-textual C Pulse sweep: `52 OK / 0 FAIL_ANALYZE / 0 TIMEOUT`. NPE
  `expected 131, found 140` (deltas documented in STATUS); LEAK `20/20` and
  UAF `7/7` match.
- C-suite OCaml↔Rust Pulse summary parity: `86 matching / 51 diffs` (`+36/-36`
  vs original `50/87` baseline). Per-pass narrative and per-file breakdown in
  [`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](docs/triage/c_pulse_summary_mismatches_2026_05_11.md).
- Latest OpenSSL partial-corpus performance dashboard lives in
  [`docs/STATUS.md`](docs/STATUS.md).
- Active work/backlog lives in `mu` tasks, not this file:

  ```sh
  mu state -w infer-rs
  mu task list -w infer-rs --status OPEN
  ```

## Build and test

From repository root:

```sh
cd infer-rs
cargo fmt -p pulse
cargo test -p pulse --lib
cargo test -p pulse --test end_to_end
cargo test -p infer-rs
INFER_BIN=../infer/bin/infer make check
```

Useful narrower loops:

```sh
cargo test -p pulse --lib formula:: -- --nocapture
cargo test -p pulse --lib term_value -- --nocapture
cargo test -p pulse --test end_to_end -- test_e2e_null_deref_fixture --nocapture
```

## CLI usage

Analyze by capturing with Infer, exporting Textual, then running Pulse:

```sh
cargo run -p infer-rs -- --pulse-only -- clang -c examples/hello.c
```

Analyze an existing Infer capture in `infer-out/`:

```sh
cargo run -p infer-rs -- --pulse-only
cargo run -p infer-rs -- --pulse-only --results-dir path/to/infer-out
```

Analyze direct `.sil`/Textual files:

```sh
cargo run -p infer-rs -- --pulse-only path/to/file.sil
cargo run -p infer-rs -- --pulse-only --source-override foo.c path/to/file.sil
```

Common flags:

```text
--pulse-only                         run Pulse only
--quiet                              suppress progress logs
--trace-ondemand                     scheduler/progress tracing
--procedures-filter <regex>          restrict analyzed procedures
--pulse-max-heap-mb <N>              per-procedure heap cap; 0 disables
--pulse-max-wall-secs <N>            per-procedure wall cap; 0 disables
--pulse-intermediate-formula-gc      opt-in memory-headroom formula cleanup
--pulse-report-issues-for-tests      include suppressed reports in test output
--debug-level-analysis <0|1|2>       analysis debug/trace detail
```

`infer-rs` discovers the OCaml `infer` binary from `INFER_BIN`, a sibling
`../infer/bin/infer`, `PATH`, or the current Infer source checkout.

## Repository layout

```text
crates/
  absint/         abstract-interpretation framework
  cli/            infer-rs binary and pipeline driver
  config/         CLI/config compatibility
  diagnostics/    issue reporting and traces
  liveness/       liveness/dead-store analysis
  pulse/          Pulse domain, transfer, summaries, models
  sil/            SIL IR
  textual/        Textual parser/transform/lowering
  test-harness/   OCaml comparison and compliance helpers

docs/             current status, architecture, testing, and archive docs
scripts/          benchmark/debug helpers
```

## Documentation

Start here:

- [`docs/STATUS.md`](docs/STATUS.md) — current correctness/performance dashboard.
- [`docs/TESTING.md`](docs/TESTING.md) — test and benchmark methodology.
- [`docs/README.md`](docs/README.md) — documentation index.
- [`AGENTS.md`](AGENTS.md) — development rules for coding agents.

Stable references:

- [`docs/PULSE.md`](docs/PULSE.md)
- [`docs/SIL.md`](docs/SIL.md)
- [`docs/TEXTUAL.md`](docs/TEXTUAL.md)
- [`docs/STORE_TEXTUAL.md`](docs/STORE_TEXTUAL.md)
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- [`docs/FRONTEND.md`](docs/FRONTEND.md)
- [`docs/BACKEND.md`](docs/BACKEND.md)
- [`docs/CHECKERS.md`](docs/CHECKERS.md)

Historical investigations live under [`docs/plans/`](docs/plans/). They are
archives; current work should be represented as `mu` tasks.
