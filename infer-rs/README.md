# infer-rs: Rust Implementation of Infer's Pulse Analysis

Rust port of [Infer](https://fbinfer.com/)'s Pulse analysis engine for memory safety checking (null dereferences, use-after-free, memory leaks).

See [docs/STATUS.md](docs/STATUS.md) for detailed compliance data.

## CLI Usage

### Full pipeline (recommended)

Capture C/C++ source with OCaml infer, then analyze with infer-rs:

```bash
infer-rs -j 4 --pulse-only -- clang -c file.c
infer-rs -j 4 --pulse-only -- make
infer-rs -j 4 --pulse-only -- gcc -c src/*.c
```

This shells out to `infer --store-textual` for capture, exports textual SIL from
`capture.db`, then analyzes all exported `.sil` files.

### Analyze existing capture

If you've already run `infer capture --store-textual`, just point infer-rs at the results:

```bash
# Uses ./infer-out by default
infer-rs --pulse-only

# Or specify results directory
infer-rs --pulse-only --results-dir /path/to/infer-out
```

### Direct .sil files (debugging)

Analyze textual SIL files directly, bypassing capture:

```bash
infer-rs --pulse-only file.sil
infer-rs --pulse-only *.sil
```

### CLI flags

| Flag | Description |
|------|-------------|
| `--pulse-only` | Run only the Pulse checker |
| `--liveness-only` | Run only the liveness/dead store checker |
| `-j N` / `--jobs N` | Number of parallel worker threads (default: all CPUs) |
| `-o` / `--output DIR` | Output directory for report.json (default: `infer-rs-out`) |
| `--results-dir DIR` | Infer results directory containing capture.db (default: `infer-out`) |
| `--infer-bin PATH` | Path to infer binary (default: auto-detect) |
| `-q` / `--quiet` | Suppress progress output |
| `--pulse-max-disjuncts N` | Max disjuncts per program point (default: 20) |
| `--pulse-intraprocedural-only` | Disable inter-procedural analysis |
| `--max-widens N` | Max widenings before fixpoint gives up (default: 10000) |
| `--debug-level-analysis N` | 0=quiet, 1=per-instruction, 2=full state dumps |
| `--inferconfig-path FILE` | Path to .inferconfig file |

### infer binary discovery

The `infer` binary is found automatically in this order:

1. `--infer-bin <path>` CLI flag
2. `INFER_BIN` environment variable
3. `../../infer/bin/infer` relative to the infer-rs binary (in-repo builds)
4. `infer` on `PATH`

### Output

- `<output>/report.json` — JSON report matching OCaml's format
- stdout — issues in `issues.exp` format for comparison with OCaml test expectations
- Exit code: 0 = no issues, 1 = error, 2 = issues found

## Building and Testing

```bash
cd infer-rs
make check          # fmt + clippy + unit/integration tests (~3s)
make check-full     # + C dump-textual sweep (~60s, needs infer binary)
```

## Project Structure

```
infer-rs/
  crates/
    sil/             Core SIL types (Typ, Exp, Instr, Procdesc, Cfg, Tenv)
    textual/         Textual IR parser, printer, transforms, to_sil
    absint/          Abstract interpretation (RPO + WTO fixpoint)
    analyses/        Liveness analysis + dead store reporter
    pulse/           Pulse engine (null deref, UAF, models, interproc)
    diagnostics/     Issue types and reporting
    ondemand/        Parallel analysis runner
    test-harness/    Test infra: OCaml runner, summary comparison
    config/          Configuration (.inferconfig, CLI flags)
    cli/             CLI binary (infer-rs)
  docs/              STATUS, PULSE, architecture docs
  scripts/           Trace comparison tools
  test-data/         Test fixtures (.sil files)
```

## How It Works

1. OCaml's `infer capture --store-textual` captures C/C++ source and stores textual SIL in `capture.db`
2. `infer debug --export-textual` exports `.sil` files + `manifest.json` mapping source→sil→procedures
3. `infer-rs` parses textual SIL, transforms, and converts to SIL
4. Pulse analysis runs: WTO fixpoint with disjunctive domain, biabduction, interprocedural summary application
5. Reports NULLPTR_DEREFERENCE, USE_AFTER_FREE, MEMORY_LEAK_C issues with original source file paths

## Key Design Decisions

- **Textual as bridge**: human-readable SIL serialization is the interop boundary between OCaml and Rust
- **Cross-reference OCaml**: analysis logic follows OCaml's approach, cross-referenced against source in `infer/src/pulse/`
- **Test through comparison**: compare against OCaml's `issues.exp` for compliance
- **Per-instruction tracing**: `--debug-level-analysis` + `scripts/compare_traces.py` for debugging divergences

## Documentation

- [docs/STATUS.md](docs/STATUS.md) — Compliance data, crate map, migration phases
- [docs/PULSE.md](docs/PULSE.md) — Pulse engine architecture
- [TODO.md](TODO.md) — Remaining gaps and backlog
- [CLAUDE.md](CLAUDE.md) — Development rules, build requirements, rebase recipe
