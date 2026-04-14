# infer-rs: Rust Implementation of Infer's Pulse Analysis

Rust port of [Infer](https://fbinfer.com/)'s Pulse analysis engine for memory safety checking (null dereferences, use-after-free, memory leaks).

Current authoritative store-textual sweep: 52/55 C files. NPE: expected 131, found 134. Leaks: expected 20, found 20. UAF: expected 7, found 7.

Exact summary-equality work now uses a semantic driver instead of raw JSON diffs:
`crates/test-harness/src/summary_compare.rs` canonicalizes both
`main.pre_post_list` and specialized summaries from `specialization.c`.
Current verified checkpoint:

- main summaries: `21 / 21` procedures match
- combined per-procedure harness: `Matching: 21`

A newer OCaml-backed latent-summary fix also preserves imported arithmetic guards through summary
recording and export, including reverse-pivoted linear equalities such as `neg_x = -x` that the
solver stores as `x = -neg_x`. Summary condition recording now keeps the caller-visible `-x`
shape instead of collapsing it to a dead temp or `0 == 0`, `simplify_for_summary(...)` rewrites
those dead arithmetic temps before pruning phi facts, and local invalid accesses only keep the
manifest+latent twin on non-manifest paths when the caller-sensitive signal comes from heap shape
or imported call-side validity rather than pure imported arithmetic. This restores
`if_negative_then_crash_latent` / `test_e2e_negated_actual_keeps_arithmetic_latent_summary`
without regressing the earlier cyclic-field-write latent-null behavior.

The latest OCaml-backed specialization pass now mirrors OCaml's dynamic-type path more closely:
the abductive domain tracks known dynamic types directly, `PulseSpecialization.apply` now seeds
dynamic-type constraints (the Rust analogue of `PulseArithmetic.and_dynamic_type_is_unsafe`)
instead of exporting `Closure(...)` attrs, specialization keys use `TypeName::CFunction(...)`,
and `__call_c_function_ptr` resolves known dynamic types before falling back to direct closure
attrs. `invoke` now matches.

Unknown-call fallback and summary export were also tightened again: bare pointer and bare `Tfun`
actuals materialize their missing pointee cell before havoc, unknown-call returns record
`ReturnedFromUnknown(actuals)`, specialized latent abort diagnostics are cached sideband and
rehydrated on apply, and summary normalization recreates caller-visible non-zero
`Invalid(ConstantDereference(k))` attrs when a value is only known constant through phi. Together
these changes bring `may_double_free_if_alias` to parity and restore the missing specialized
recursive invalidation surface in `two_pointers_recursion_bad`.

`specialization.c` is now fully clean in the semantic harness. The last analyzer-side fix keeps
OCaml's summary-import behavior for missing callee pre-edges: imported pre cells are now abduced
onto the caller with `read_heap(...)`, except for callee formal-stack bookkeeping cells for
value-style actuals, which still must not be replayed or the old `v -*-> v` self-edge bug comes
back. On the comparator side, `summary_compare.rs` now collapses witness atoms before anchored
affine rewrites, derives `is_int(...)` through exact-RHS, inverse-scaling, and eq-closure over
exported equalities, and drops redundant formula-only integer witnesses once an anchored closure is
available. The focused regressions around `invoke_itself_bad` and `two_pointers_recursion_bad` are
now green without widening the raw Rust summaries.

Recent OCaml-backed parity work also restored `traces.c`: Rust now preserves branch-conditioned
null provenance from both real `Prune` instructions and model-generated
`free(NULL)` / `free(non-null)` splits, so locally branch-proven direct-formal null dereferences
stay manifest/suppressed while caller-controlled ones stay latent.

The latest interproc correctness pass restored two OCaml-backed behaviors that the previous
summary-parity work had regressed: leaf `MustBeValid` preconditions are now checked/imported even
when the callee pre value has no outgoing pre-heap edges, and summary replay now skips callee
formal-stack bookkeeping cells for value-style actuals while still replaying true lvalue/by-ref
actuals. This fixes the lost latent precondition regressions and removes the old bogus
`invoke(id)` / `add_one` / `add_two` `v -*-> v` self-edge without papering over the remaining
semantic mismatches.

The latest specialization/summarization cleanup tightened three more OCaml-backed surfaces:
summary normalization now strips post-summary `Initialized` attrs from hidden formal/local stack
roots after `restore_formals_for_summary`, unresolved `__call_c_function_ptr` now
conservatively initializes values reachable from the function pointer and actual roots before
model / unknown-call handling, and normal integer literal evaluation now reuses existing formula
representatives like OCaml `PulseFormula.absval_of_int`. Summary normalization also now filters
pre/post attrs with the same suitability rules as OCaml `PulseAttribute`, which drops exported
post-summary `ComparedToNullInThisProcedure` invalidations while keeping real caller-visible
invalidations such as `ConstantDereference(0)`. Together these fixes remove the old unresolved-call
`Initialized` gap from `invoke`-style summaries, bring `test_unalias` to parity, and stop
over-exporting the wrapper shape in `call_may_double_free_if_alias_bad`.

A newer OCaml-backed model/export pass now also conservatively initializes actual roots before
entering known models, matching the `OCamlModel` path in `Pulse.ml`, and exports
continue-derived latent invalid-access summaries with `diagnostic=None`, reconstructing the
diagnostic again during summary import. That aligned `may_double_free_if_alias` with OCaml on
latent diagnostic shape and caller-visible pointee `Initialized` attrs; the later visible-constant
invalidation fix and exact-RHS `is_int` anchoring closed the last remaining summary-surface delta
there.

The config surface now also supports OCaml's `pulse-force-continue` flag through both
`.inferconfig` and CLI override. Rust summaries now retain OCaml-style
`has_dropped_disjuncts` metadata, and known-callee calls only fall back to unknown-call semantics
when the selected summary is empty or marked incomplete for the same reason. That narrow
force-continue path is correct and tested. The latest checker pass also restores the missing
skipped-call continue for selected alias-specialized latent-invalid-access summaries with no
`ContinueProgram`, which is the OCaml-backed shape behind
`call_may_double_free_if_alias_bad`. Together with the newer integer-literal interning and
post-summary attr filtering, those fixes remain part of the current
`21 / 21` main-summary and `Matching: 21` widened-summary checkpoint.
`invoke`, `invoke_itself_bad`, `call_may_double_free_if_alias_bad`,
`may_double_free_if_alias`, `test_unalias`, and `two_pointers_recursion_bad`
now match.

`memory_leak.c` is also back at parity after history-aware invalid-access provenance/dedup. The
remaining published NPE delta is:

- `nullptr.c` (+1): accepted correctness-positive divergence. Rust reports the real
  `FN_nullptr_deref_old_bad` null dereference that OCaml intentionally misses because of recency
  forgetting in that test.
- `sizeof.c` (+2): accepted `--store-textual` / `--export-textual` fidelity limitation. Exported
  Textual lowers array `sizeof(...)` expressions to `<int[]>` without `nbytes` or array length, so
  the Rust roundtrip cannot constant-fold those branches.

The latest correctness pass also keeps recoverable invalid-access paths from continuing after the
error has already been classified: transfer-side load/store recoverable errors and C-model
recoverable errors now stop instead of exporting `ContinueProgram + AbortProgram`, with focused
regressions for null-formal stores and double-free. That cleanup is correct and stays, but it does
not explain the now-clean `specialization.c` comparator by itself. A broader checker-side attempt to recover
non-exit latent invalid accesses when another path reaches exit was cross-checked against OCaml and
reverted because it over-published latent summaries in `test_alias`, `test_unalias`, and wrapper /
recursion cases. The ignored `test_normal_exit_keeps_non_exit_latent_abort` now exists only to
document that still-missing direct-formal-read case.

The latest direct-formal ordering fix closes the main `may_double_free_if_alias` shape bug without
cheating the counts: Rust now stamps `MustBeValid` / `MustBeInitialized` summary attrs with real
monotonic per-state timestamps instead of hardcoding `0`, and latent-invalid-access summary
shaping now orders direct-formal accesses by `(timestamp, location)` rather than raw `.sil`
location alone. That removes the extra latent branch and brings the raw main summary down to the
OCaml shape of `x == 0`, `x > 0 && y == 0`, and `x > 0 && y > 0`. After the later
integer-literal interning, summary-import fix, and eq-closure comparator pass, `specialization.c`
is now fully matched; the remaining parity work is outside this harness.

See [docs/STATUS.md](docs/STATUS.md) for detailed compliance data and
[docs/STORE_TEXTUAL.md](docs/STORE_TEXTUAL.md) for the accepted exported-Textual limitation.

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

When multiple `.sil` files are passed, `infer-rs` parses them in parallel, merges the resulting
`Cfg`/`Tenv`, and then runs analysis once over the unified program so cross-file calls can resolve
to in-memory summaries.

### CLI flags

| Flag | Description |
|------|-------------|
| `--pulse-only` | Run only the Pulse checker |
| `--liveness-only` | Run only the liveness/dead store checker |
| `--pulse-report-issues-for-tests` | Emit suppressed Pulse issues as distinguished `*** SUPPRESSED ***` test reports |
| `--pulse-force-continue BOOL` | Override OCaml-compatible force-continue fallback for incomplete known callees |
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
| `--pulse-model-abort PROCNAME` | Model an exact procname as non-returning (repeatable) |
| `--pulse-model-unreachable PROCNAME` | Model an exact procname as unreachable (repeatable) |
| `--pulse-model-free-pattern REGEX` | Model matching functions as wrappers to `free(3)` |
| `--pulse-model-malloc-pattern REGEX` | Model matching functions as wrappers to `malloc(3)` |
| `--pulse-model-realloc-pattern REGEX` | Model matching functions as wrappers to `realloc(3)` |
| `--pulse-model-return-nonnull REGEX` | Model matching functions as returning a non-null value |
| `--pulse-model-return-this REGEX` | Model matching methods as returning `this` / `self` |
| `--pulse-model-return-first-arg REGEX` | Model matching methods as returning the first source-language arg |
| `--pulse-model-return-nullable REGEX` | Model matching methods as returning null-or-non-null |
| `--pulse-model-skip-pattern REGEX` | Skip matching functions and treat them as unknown calls |
| `--pulse-model-unknown-pure REGEX` | Model matching functions as unknown pure calls (repeatable) |

`pulse-model-{free,malloc,realloc}-pattern`,
`pulse-model-return-{nonnull,this,first-arg,nullable}`, `pulse-model-skip-pattern`, and
`pulse-model-unknown-pure` are compatible with OCaml Infer's shared `.inferconfig` files,
including the `Str.regexp` syntax used in test suites such as `\\(my\\|a\\)_malloc`.
`pulse-force-continue` is also compatible with shared `.inferconfig`; the Rust default remains
`true` to match OCaml.
`pulse-model-{abort,unreachable}` follow OCaml's exact-procname list semantics.
`pulse-model-returns-copy-pattern` is still intentionally unsupported because Rust does not yet
implement OCaml's non-disjunctive unnecessary-copy tracking.

### infer binary discovery

The `infer` binary is found automatically in this order:

1. `--infer-bin <path>` CLI flag
2. `INFER_BIN` environment variable
3. `../infer/bin/infer` relative to the workspace root (in-repo builds)
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
# authoritative compliance sweep used for STATUS.md:
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

`make check-full` exercises the older `capture --dump-textual` path as a secondary regression check.
The published compliance numbers in [docs/STATUS.md](docs/STATUS.md) come from the
`--store-textual` + `--export-textual` sweep because that matches the CLI pipeline.
That ignored sweep now invokes `infer-rs` once per exported `.sil` from the originating source
directory, so OCaml-style upward `.inferconfig` discovery is part of the published totals. The
sweep helper also rebuilds `infer-rs` once per test process so the published numbers do not
silently reuse a stale `target/{debug,release}/infer-rs` binary.

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
    test-harness/    Test infra: OCaml runner, semantic summary comparison
    config/          Configuration (.inferconfig, CLI flags)
    cli/             CLI binary (infer-rs)
  docs/              STATUS, PULSE, architecture docs
  scripts/           Trace comparison tools
  test-data/         Test fixtures (.sil files)
```

## How It Works

1. OCaml's `infer capture --store-textual` captures C/C++ source and stores textual SIL in `capture.db`
2. `infer debug --export-textual` exports `.sil` files + `manifest.json` mapping source→sil→procedures
3. `infer-rs` parses textual SIL files in parallel, transforms them, and converts each module to SIL
4. The per-file `Cfg`/`Tenv` results are merged into one in-memory program
5. Pulse analysis runs: WTO fixpoint with disjunctive domain, biabduction, interprocedural summary application
   and OCaml-backed latent/manifest invalid-access classification
6. Reports NULLPTR_DEREFERENCE, USE_AFTER_FREE, MEMORY_LEAK_C issues with original source file paths

## Key Design Decisions

- **Textual as bridge**: human-readable SIL serialization is the interop boundary between OCaml and Rust
- **Cross-reference OCaml**: analysis logic follows OCaml's approach, cross-referenced against source in `infer/src/pulse/`
- **Correctness over counts**: keep semantically correct OCaml-backed behavior even when sweep totals move temporarily; accepted divergences are documented instead of hidden
- **Test through comparison**: compare against OCaml's `issues.exp` for compliance
- **Per-instruction tracing**: `--debug-level-analysis` + `scripts/compare_traces.py` for debugging divergences

## Documentation

- [docs/STATUS.md](docs/STATUS.md) — Compliance data, crate map, migration phases
- [docs/PULSE.md](docs/PULSE.md) — Pulse engine architecture
- [TODO.md](TODO.md) — Remaining gaps and backlog
- [CLAUDE.md](CLAUDE.md) — Development rules, build requirements, rebase recipe
