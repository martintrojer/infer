# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (store-textual sweep: 52/55 files, NPE expected 131 found 132, Leaks expected 20 found 20, UAF expected 7 found 7)

Correctness note: keep the semantically correct specialization / latent-invalid-access /
specialized-summary fixes even when totals move temporarily. The current sweep reflects the real
remaining count gaps after removing the old basename-matching measurement bug.

**NPE Count Gaps:**

1. **`sizeof.c`** (+2): OCaml reports no NPEs; Rust still reports two. Likely tied to `sizeof`
   evaluation or report timing around type-only expressions.
2. **`memory_leak.c`** (-1): remaining duplicated `NULLPTR_DEREFERENCE` on
   `realloc_no_check_bad`.

**Issue-set parity still open even though the file count now matches:**

3. **`nullptr.c`**: `unknown_from_parameters_latent` is fixed and no longer inflates the NPE total,
   but the proc-level issue set is still off by one missing and one extra report:
   missing `create_null_path2_bad_FN`, extra `FN_nullptr_deref_old_bad`.

**Leak differences:** none in the authoritative sweep. `MEMORY_LEAK_C` parity is exact.

Authoritative-sweep note: the ignored store-textual sweep now runs `infer-rs` from each source
file's directory, so the published totals already include suite-local `.inferconfig` behavior.

Separate config-surface note: `pulse-model-abort`, `pulse-model-return-nonnull`, and
`pulse-model-skip-pattern` are now supported. The main remaining root-level `.inferconfig` gap is
`pulse-model-returns-copy-pattern`, which needs unnecessary-copy tracking rather than a simple
model shim.

**Skipped files (3):** `infinite.c` (106 procs with infinite loops/Ackermann), `recursion.c`,
`recursion2.c` — fixpoint exhaustion.

**Notable recent correctness wins:** `cleanup_attribute.c` now matches OCaml again, `angelism.c`
is back at parity (`7` issues), `memory_leak.c` leak parity is exact in the authoritative sweep,
`funptr.c` is at parity (`11` issues), `specialization.c` is back to the direct OCaml issue set,
`nullptr.c` count parity is exact again after translating pure-call function-application
dependencies through summary application, and `compound_literal.c` / `initlistexpr.c` already
match OCaml after fixing the sweep expectation helper to use exact basenames.

### Textual pipeline gaps

- **DeclEnv enhancements** (`decls.rs`): Missing variadic proc tracking, generics status.
- Language-specific (defer): FixHackWrapper, FixHackInvokeClosure, TransformClosures, verify_variadic_position, SSA restoration.

### SIL test gaps (skipped procs)

- **Virtual dispatch in loads** (2 procs in static_types.sil)
- **Devirtualization return values** (5 procs in virt.sil)
- **Cross-file resolution** (3 procs across npe*.sil)

### Pulse gaps

- **Aliasing contradiction detection**: Caller aliasing callee's disjoint formals. Cross-ref: `PulseInterproc.ml` AliasingWithAllAliases.
- **ValueHistory threading**: Error trace reconstruction. Cross-ref: `PulseValueHistory.ml`.
- **Global variable handling** in summary application.
- **`sizeof` type evaluation**: scalar types are handled via `Typ::size_in_bytes()`. Remaining gap: `<int[]>` without array length.
- **Latent issues parity / report timing**: latent/base publishing is now routed through summary classification, prune conditions now carry OCaml-style call-depth provenance, callee AbortProgram summaries now propagate again, Rust now has a caller-side latent-invalid-access path, and imported pure-call dependencies now survive summary application/normalization. Remaining: `pre_heap_has_assumptions` parity in `is_manifest`, latent issue type reporting, and OCaml-aligned publication timing for any still-propagated abort over-reports.

## Debugging tools

- **Per-instruction tracing**: `--debug-level-analysis 1` (debug) or `2` (trace). Also `RUST_LOG=pulse=debug`. Log lines prefixed with `[proc_name]` for parallel-safe filtering.
- **Comparison script**: `scripts/compare_traces.py` — parses OCaml `--debug` HTML and Rust log, side-by-side per-instruction with disjunct counts.
- **Compliance recipe**: see CLAUDE.md "Step-by-step tracing for compliance debugging".

## Code issues

- **`find_return_value` fallback heuristic**: Skips void procs (correct for leak detection). For non-void, takes last Load/Call across ALL nodes.
- **Prune `is_then_branch` hardcoded to `true`** in to_sil.
- **`DeclEnv` uses `format!()` as HashMap keys**: Needs location-insensitive key types.

## Test improvements

## Code improvements (low priority)

- **`AnnotItem::empty()` etc. duplicate `Default`**
- **`Procdesc` succs/preds HashMap**: Vec would be more cache-friendly.
- **`Tenv::get_supers` clones**: Use references or intern.
- **`DUMMY_LOCATION` LazyLock**: Could be `const`.
