# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (store-textual sweep: 52/55 files, NPE expected 131 found 139, Leaks expected 20 found 22, UAF expected 7 found 7)

Correctness note: keep the semantically correct specialization / latent-invalid-access /
specialized-summary fixes even when totals move temporarily. The current sweep reflects the real
remaining count gaps after removing the old basename-matching measurement bug.

**NPE Over-detection (FPs, +10 gross / +8 net):**

1. **Nullptr + integer-ish cluster** (+9): `integers.c` (+2), `nullptr.c` (+2), `nullptr_more.c` (+2), `offsetof_expr.c` (+1), `sizeof.c` (+2).
2. **angelism.c** (+1): likely still an interprocedural/reporting mismatch.

**NPE Under-detection (FNs, -2 gross):**

3. **memory_leak.c** (-2): remaining function-pointer-wrapper / null-path issue-set mismatch.

**Leak differences (+2 FPs in sweep):**

4. **cleanup_attribute.c** (+2): `__attribute__((cleanup()))` GCC extension still not modeled closely enough.

**Config / harness follow-up:**

5. **Thread per-suite `.inferconfig` into the ignored sweep harness**: support for
   `pulse-model-{free,malloc,realloc}-pattern` is implemented, but the ignored store-textual sweep
   still does not load suite-local `.inferconfig` files. Until that is fixed, config-driven
   correctness changes will not affect the published sweep totals.

**Masked direct-run issue-set gaps:**

6. **memory_leak.c leak parity is still not truly done**: with
   `infer/tests/codetoanalyze/c/pulse/.inferconfig`, Rust now recovers `user_malloc_leak_bad` and
   `test_config_options_no_free_bad`, but pointer-arithmetic / array reachability still produces
   false positives that happen to cancel in the sweep counts.

**Skipped files (3):** `infinite.c` (106 procs with infinite loops/Ackermann), `recursion.c`,
`recursion2.c` — fixpoint exhaustion.

**Notable recent correctness wins:** `funptr.c` is now at parity (`11` issues), `specialization.c`
is back to the direct OCaml issue set, and `compound_literal.c` / `initlistexpr.c` already match
OCaml after fixing the sweep expectation helper to use exact basenames.

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
- **Latent issues parity / report timing**: latent/base publishing is now routed through summary classification, prune conditions now carry OCaml-style call-depth provenance, callee AbortProgram summaries now propagate again, and Rust now has a caller-side latent-invalid-access path. Remaining: `pre_heap_has_assumptions` parity in `is_manifest`, latent issue type reporting, and OCaml-aligned publication timing for any still-propagated abort over-reports.

## Debugging tools

- **Per-instruction tracing**: `--debug-level-analysis 1` (debug) or `2` (trace). Also `RUST_LOG=pulse=debug`. Log lines prefixed with `[proc_name]` for parallel-safe filtering.
- **Comparison script**: `scripts/compare_traces.py` — parses OCaml `--debug` HTML and Rust log, side-by-side per-instruction with disjunct counts.
- **Compliance recipe**: see CLAUDE.md "Step-by-step tracing for compliance debugging".

## Code issues

- **`find_return_value` fallback heuristic**: Skips void procs (correct for leak detection). For non-void, takes last Load/Call across ALL nodes.
- **All call arg types set to `void`** in to_sil: formal types available via PulseSummary.formal_types for havoc decisions.
- **Prune `is_then_branch` hardcoded to `true`** in to_sil.
- **`DeclEnv` uses `format!()` as HashMap keys**: Needs location-insensitive key types.

## Test improvements

## Code improvements (low priority)

- **`AnnotItem::empty()` etc. duplicate `Default`**
- **`Procdesc` succs/preds HashMap**: Vec would be more cache-friendly.
- **`Tenv::get_supers` clones**: Use references or intern.
- **`DUMMY_LOCATION` LazyLock**: Could be `const`.
