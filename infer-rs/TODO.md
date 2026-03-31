# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (store-textual sweep: 52/55 files, NPE expected 131 found 134, Leaks expected 20 found 20, UAF expected 7 found 7)

Correctness note: keep the semantically correct specialization / latent-invalid-access /
specialized-summary fixes even when totals move temporarily. The current sweep reflects the real
remaining count gaps after removing the old basename-matching measurement bug.

**Accepted store-textual limitation (documented, not a Pulse workaround target):**

1. **`sizeof.c`** (+2): exported Textual drops `Sizeof.nbytes` / array-length information and
   emits array `sizeof(...)` expressions as `<int[]>`. Rust faithfully roundtrips that back to
   `Sizeof { typ = int[]; nbytes = None }`, so Pulse cannot fold the `sizeof(c) > 2` /
   `sizeof(c) / sizeof(c[0]) != 2` branches. Treat this as a `--store-textual` /
   `--export-textual` fidelity limit unless the interface preserves richer `Sizeof` data.

**Accepted correctness-positive divergence from OCaml:**

2. **`nullptr.c`** (+1): `create_null_path2_bad_FN` is restored and `unknown_from_parameters_latent`
   no longer inflates the manifest NPE count. The only remaining file-level mismatch is the extra
   `FN_nullptr_deref_old_bad`, which is a real bug that OCaml intentionally misses because of the
   recency limitation documented in the source comment.

**Leak differences:** none in the authoritative sweep. `MEMORY_LEAK_C` parity is exact.

Authoritative-sweep note: the ignored store-textual sweep now runs `infer-rs` from each source
file's directory, so the published totals already include suite-local `.inferconfig` behavior.

Separate config-surface note: `pulse-model-abort`, `pulse-model-unreachable`,
`pulse-model-return-nonnull`, `pulse-model-return-this`, `pulse-model-return-first-arg`,
`pulse-model-return-nullable`, `pulse-model-skip-pattern`, and `pulse-model-unknown-pure` are now
supported. The remaining `.inferconfig` gaps found in the current repo audit are copy-specific:
root/build-system `pulse-model-returns-copy-pattern` and C++ `pulse-model-cheap-copy-type`, both
of which need the OCaml unnecessary-copy pipeline rather than a simple model shim. Language-specific
`pulse-model-{release,deep-release}-pattern` remain out of the current C null/UAF/leak scope.

**Skipped files (3):** `infinite.c` (106 procs with infinite loops/Ackermann), `recursion.c`,
`recursion2.c` — fixpoint exhaustion.

**Notable recent correctness wins:** `cleanup_attribute.c` now matches OCaml again, `angelism.c`
is back at parity (`7` issues), `memory_leak.c` is back at full parity after the history-aware
diagnostic fix, `funptr.c` is at parity (`11` issues), `specialization.c` is back to the direct
OCaml issue set, and `compound_literal.c` / `initlistexpr.c` already match OCaml after fixing the
sweep expectation helper to use exact basenames.

There are no remaining active store-textual count fixes to pursue without either degrading
precision (`nullptr.c`) or patching over exported-Textual fidelity loss (`sizeof.c`).

### Textual pipeline gaps

- **DeclEnv enhancements** (`decls.rs`): Missing variadic proc tracking, generics status.
- Language-specific (defer): FixHackWrapper, FixHackInvokeClosure, TransformClosures, verify_variadic_position, SSA restoration.

### SIL test gaps (skipped procs)

- **Virtual dispatch in loads** (2 procs in static_types.sil)
- **Devirtualization return values** (5 procs in virt.sil)
- **Cross-file resolution** (3 procs across npe*.sil)

### Pulse gaps

- **Aliasing contradiction detection**: Caller aliasing callee's disjoint formals. Cross-ref: `PulseInterproc.ml` AliasingWithAllAliases.
- **Full ValueHistory / PulseTrace parity**: Rust now has minimal invalid-access provenance and
  history-sensitive dedup, but richer OCaml-style trace reconstruction is still incomplete.
- **Global variable handling** in summary application.
- **`sizeof` type evaluation**: scalar types are handled via `Typ::size_in_bytes()`. Accepted
  store-textual limitation: exported `<int[]>` arrives without array length or `nbytes`.
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
