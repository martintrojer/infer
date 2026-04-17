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

Current top-level OpenSSL status:

- setup/capture is stable on this host with repo clang on `PATH`, `CC=clang`,
  explicit SDK `-isysroot`, and `./Configure darwin64-x86_64-cc no-asm`
- the latest fresh shared-capture snapshot
  (`/tmp/infer-rs-openssl-20260417-095315-rebase-j`) captured in `371.32s`,
  exported in `0.35s`, and produced `753` `.sil` files
- the OCaml baseline on that shared capture completed in `589.76s` with about
  `2.55 GB` max RSS
- Rust now parses the full exported corpus (`753 / 753`), and the rebased
  macOS `-j > 1` path now really starts parallel analysis (`active=8` in the
  traced `-j 8` run), so the old immediate startup failure is no longer the
  main blocker
- focused `whirlpool_block` tracing now separates active frontier cost from
  retained invariant-map cost, and the dominant multiplier is the retained
  fixpoint map rather than the live frontier alone
- whole-program Rust merged direct-`.sil` runs are still unstable on this
  benchmark: `-j 8` died at `190.81s` / `24.5 GB` RSS and `-j 4` died at
  `690.77s` / `33.2 GB` RSS
- the filtered hotspot fix for `ssl_set_client_disabled` remains real
  (`1m09s` -> `5.2s`), but the current benchmark blocker is merged parallel
  memory growth plus a few remaining heavy local Pulse procedures, not
  merge/callgraph setup or a one-core scheduler bug
- `--pulse-recency-limit 32` is available as an experiment knob, but it is
  intentionally not the default because it reintroduces the real `nullptr.c`
  false negative and does not materially change the `whirlpool_block` shape

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
OCaml run at a different `-j`. Until whole-program merged `-j > 1`
direct-`.sil` runs are stable, use `-j 1` as the publishable apples-to-apples
baseline.

Current OpenSSL status from the latest direct-Textual spot-check:

- shared capture and export on the fresh benchmark dir completed in `371.32s`
  and `0.35s`, respectively, and produced `753` exported `.sil` files
- parse coverage is now `753 / 753` exported `.sil` files with `0` parse
  errors after accepting textual name positions tokenized as `Local(n)` plus
  `_` wildcard field names
- the OCaml baseline on the same shared capture completed in `589.76s` with
  about `2.55 GB` max RSS
- the traced Rust direct `-j 8` run parsed the corpus in `43.1s`, merged it to
  `8395` procedures / `683` types, entered round 1 with `active=8`, then
  terminated abnormally at `190.81s` with about `24.5 GB` max RSS
- a Rust direct `-j 4` run also terminated abnormally, later, at `690.77s`
  with about `33.2 GB` max RSS
- the new `live-fixpoint` heartbeat shows why the benchmark is still hard:
  on isolated `whirlpool_block`, the frontier at `36.1s` still held only about
  `9837` summed post heap nodes, but the retained invariant map already held
  `2995` disjunct snapshots with about `975641` post heap nodes,
  `1313138` edges, and `2464294` attr entries
- the matching narrowed OCaml debug run on the same shared capture completed
  in `1m31s` and ended with only `152` retained post snapshots across
  `178` CFG nodes, about `98727` post heap nodes, `53889` post heap edges,
  and `39663` post attr entries; no final OCaml node retained more than
  `1` disjunct
- the OCaml-style recency experiment is available for direct probes
  (`--pulse-recency-limit 32`), but the focused `whirlpool_block` run stayed
  essentially identical and default-enabling that cap would reintroduce the
  real `nullptr.c` `FN_nullptr_deref_old_bad` false negative
- restoring the OCaml-style disjunctive `equal_fast` / semantic-`leq` split
  still cut the filtered `ssl_set_client_disabled` hotspot from about `1m09s`
  to about `5.2s` while keeping the same `173` transfer steps, `20`-disjunct
  cap, and hottest node `33:24`
- current interpretation: the old immediate macOS `-j > 1` startup failure is
  no longer the main issue; the blocker is retained invariant-map growth plus
  semantic-convergence gaps in retained loop-head states, abnormal
  termination in whole-program runs, the remaining heavy local Pulse
  procedures, and exported-Textual proc-identity loss for some duplicate C
  names

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

# Rust: narrow a large corpus to one procedure (keeps transitive callees for interproc)
infer-rs --debug-level-analysis 1 --procedures-filter 'target_proc' *.sil

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
- `pulse-progress` heartbeats for long-running procedures, including elapsed
  time, transfer-step count, current node/instr, current-node revisit count,
  hottest node-so-far, and current/max disjuncts
- `live-fixpoint` heartbeats for retained invariant-map state, including
  retained CFG-node count, retained disjunct-snapshot count, and aggregate
  heap / attr / formula size counters

When a filtered hotspot still shows the same transfer-step count before and
after a performance change, treat that as evidence about comparison cost vs
semantic work. The current OpenSSL `ssl_set_client_disabled` spot-check is the
example here: after the `equal_fast` split, the run still executes `173`
transfer steps and saturates at `20` disjuncts, but its runtime drops from
about `1m09s` to about `5.2s`, which points squarely at hot disjunct
dedup/join comparison cost rather than a change in explored paths.

Use `pulse-progress` and `live-fixpoint` together. On the current
`whirlpool_block` probe, the active frontier stays around `10k` summed post
heap nodes while the retained fixpoint map grows toward `1M`, which is why the
next OpenSSL work is on invariant-map retention / storage, not just frontier
caps.

You can raise verbosity further with explicit logger filters, for example
`RUST_LOG=warn,ondemand=debug infer-rs --trace-ondemand ...` to include wave
member lists.

## Determinism

Analysis results are deterministic across runs:
- Thread-local `AbstractValue` counters (reset per procedure)
- `BTreeMap`/`BTreeSet` in analysis-critical structures
- Sorted wave ordering in call graph scheduling
