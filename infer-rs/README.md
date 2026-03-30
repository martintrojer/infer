# infer-rs: Rust Implementation of Infer's Pulse Analysis

Rust port of [Infer](https://fbinfer.com/)'s Pulse analysis engine for memory safety checking (null dereferences, use-after-free, memory leaks).

See [docs/STATUS.md](docs/STATUS.md) for detailed compliance data.

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

1. OCaml's `infer capture --dump-textual` converts C source to Textual IR (`.sil` files)
2. `infer-rs` parses, transforms, and converts Textual to SIL
3. Pulse analysis runs: WTO fixpoint with disjunctive domain, biabduction, interprocedural summary application
4. Reports NULL_DEREFERENCE, USE_AFTER_FREE, MEMORY_LEAK_C issues

## Key Design Decisions

- **Textual as bridge**: human-readable SIL serialization is the interop boundary between OCaml and Rust
- **Cross-reference OCaml**: analysis logic follows OCaml's approach, cross-referenced against source in `infer/src/pulse/`
- **Test through comparison**: compare against OCaml's `issues.exp` for compliance
- **Per-instruction tracing**: `--debug-level-analysis` + `scripts/compare_traces.py` for debugging divergences

## Documentation

- [docs/STATUS.md](docs/STATUS.md) -- Compliance data, crate map, migration phases
- [docs/PULSE.md](docs/PULSE.md) -- Pulse engine architecture
- [TODO.md](TODO.md) -- Remaining gaps and backlog
- [CLAUDE.md](CLAUDE.md) -- Development rules, build requirements, rebase recipe
