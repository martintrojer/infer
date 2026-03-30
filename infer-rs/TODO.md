# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (store-textual sweep: 52/55 files, NPE 138/135, Leaks 23/20, UAF 8/7)

Correctness note: the recent equality-incorporation + abort-propagation fixes intentionally moved
the totals. `specialization.c` is now correct again, but the new sweep exposed a different problem:
AbortProgram/report publication is still not aligned with OCaml, so some interprocedural files now
over-report. Keep the semantically correct execution fix; align report timing/publication next.

**NPE Over-detection (FPs, +14 gross / +3 net):**

1. **AbortProgram/reporting parity** (+6): interprocedural.c (+3), latent.c (+3). After restoring callee AbortProgram propagation, report publication is still too eager compared with OCaml.
2. **Nullptr family** (+4): nullptr.c (+2), nullptr_more.c (+2). Mix of write-through-pointer / biabduction/reporting mismatches.
3. **sizeof / offsetof** (+3): sizeof.c (+2), offsetof_expr.c (+1). `sizeof.c` remains partly blocked on missing `<int[]>` length info.
4. **angelism.c** (+1): remaining interproc/reporting issue.

**NPE Under-detection (FNs, -11 gross):**

5. **Function pointer dispatch** (-5): funptr.c (11→6). Direct dispatch + single-level specialization work. Remaining: returned funptr, struct callbacks, deeper specialization chains.
6. **Deep interproc / models** (-4): initlistexpr.c (4→1), compound_literal.c (2→1).
7. **Function-pointer wrappers** (-2): memory_leak.c (3→1 NPEs).

**Leak differences (+3 FPs):**

8. **cleanup_attribute.c** (+2): `__attribute__((cleanup()))` GCC extension not modeled. Cleanup function (free) called automatically at scope exit — not modeled.
9. **memory_leak.c** (+1): mixed bag of funptr wrapper misses and independent leak FPs.

**UAF differences (+1 FP):**

10. **AbortProgram/reporting parity** (+1): interprocedural.c now reports one extra UAF after preserving callee AbortProgram summaries. Likely the same structural publication issue as the NPE over-counts.

**Skipped files (3):** infinite.c (106 procs with infinite loops/Ackermann), recursion.c, recursion2.c — fixpoint exhaustion.

**Notable recent correctness wins:** `specialization.c` now matches the direct OCaml issue set again after wiring formula equalities back into the abductive state and preserving callee AbortProgram summaries; `assert.c` and `ternary.c` remain fixed by OCaml-style condition-depth tracking without reintroducing the latent/base bug.

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
- **Latent issues parity / report timing**: latent/base publishing is now routed through summary classification, prune conditions now carry OCaml-style call-depth provenance, and callee AbortProgram summaries now propagate again. Remaining: `pre_heap_has_assumptions` parity in `is_manifest`, `LatentInvalidAccess`, latent issue type reporting, and OCaml-aligned publication timing for propagated aborts.

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
