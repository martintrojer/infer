# infer-rs

Rust implementation of Infer's Pulse analysis for Textual/SIL programs. The goal is to
match Infer's OCaml Pulse semantics while making the analysis easy to test, profile, and
iterate on in Rust.

## Current status

The authoritative dashboard is [`docs/STATUS.md`](docs/STATUS.md). At a glance:

- Store-textual C Pulse sweep: `52 OK / 0 FAIL_ANALYZE / 0 TIMEOUT`. NPE
  found `134`; LEAK `20/20` and UAF `7/7` match exactly.
- C-suite OCaml↔Rust Pulse summary parity across 22 tested files:
  `199 matching / 62 diffs` (`76%` match), with **12 perfect-parity files**
  (arithmetic, specialization, memory_leak, interprocedural,
  array_out_of_bounds, assert, compound_literal, dangling_deref, enum,
  frontend, getcwd, issues_abort_execution). Per-file breakdown in
  [`docs/STATUS.md`](docs/STATUS.md).
- Latest OpenSSL partial-corpus performance dashboard lives in
  [`docs/STATUS.md`](docs/STATUS.md). Wave 10/11 landed eight perf/cap fixes
  plus a Textual `DeclEnv` enhancement; the latest full-corpus checkpoint exits
  `0` with `445/445` procs (`391.98s`, `9.44 GiB`, `11` aborts, max visit `4`).
  Focused wins: `sha512_block_data_order` `~29s -> 26.0s`,
  `md4_block_data_order` RSS `2.49 GiB -> 0.43 GiB`, and `passwd_main` wall-cap
  evasion fixed (`3h+ -> 1m01s`).
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
