# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (authoritative store-textual sweep: 52/55 files analyzed, 3 skipped for fixpoint exhaustion; NPE expected 131 found 134, Leaks expected 20 found 20, UAF expected 7 found 7)

Correctness note: keep the semantically correct specialization / latent-invalid-access /
specialized-summary fixes even when totals move temporarily. The current sweep reflects the real
remaining count gaps after removing the old basename-matching measurement bug.

**Accepted store-textual limitation (documented, not a Pulse workaround target):**

1. **`sizeof.c`** (+2): exported Textual drops `Sizeof.nbytes` / array-length information and
   emits array `sizeof(...)` expressions as `<int[]>`. Rust faithfully roundtrips that back to
   `Sizeof { typ = int[]; nbytes = None }`, so Pulse cannot fold the `sizeof(c) > 2` /
   `sizeof(c) / sizeof(c[0]) != 2` branches. Treat this as a `--store-textual` /
   `--export-textual` fidelity limit unless the interface preserves richer `Sizeof` data.

**Current active NPE file diffs (correctness work, not workaround targets):**

2. **`angelism.c`** (+1): current sweep is `8` vs expected `7`. This regressed after the
   correctness-first interproc invalid-access fixes and needs localization without undoing the
   now-correct by-ref / direct-formal behavior.

3. **`latent.c`** (+1): current sweep is `6` vs expected `5`. This is a remaining
   latent-vs-manifest publication mismatch.

4. **`nullptr.c`** (-1): current sweep is `12` vs expected `13`. The earlier
   `unknown_from_parameters_latent` false positive is gone, but the file is no longer an accepted
   "+1 real bug" divergence under the current latent/manifest work.

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

**Active OCaml-backed correctness focus:**

7. **Direct-formal / by-ref regression lock-in**: keep the new focused regressions green:
   `test_e2e_guarded_outparam_write_uses_matching_summary_branch`,
   `test_e2e_write_through_ptr`, `test_e2e_manifest_use_after_free_reports_only_uaf`, and
   `test_e2e_access_use_after_free_keeps_manifest_npe_and_uaf`. These are correctness fixes,
   not optional count-tuning.

8. **Wrapper/cycle null paths** such as `traverse_and_crash_if_equal_to_root`: Rust and OCaml
   still diverge on which latent null paths survive call chains and reify as manifest reports.

9. **Current file-level drift after the correctness-first fix set**: `angelism.c`, `latent.c`,
   and `nullptr.c` now carry the remaining active NPE count gap outside the accepted `sizeof.c`
   textual limitation.

Recent groundwork that should stay in place even though the current NPE total moved in the wrong direction:
- interproc formula import now mirrors OCaml `PulseFormula.and_callee_formula` more closely
  (shared substitution across the whole callee formula, remembered `conditions` imported first)
- summary import now snapshots caller allocation state before `apply_post` and uses that snapshot
  when importing `EqZero`, instead of relying on the rejected broad formula-before-post reorder
- caller-side latent invalid-access rechecks now mark translated diagnostic addresses
  `must_be_valid` and reuse summary classification, which fixes
  `manifest_use_after_free` without regressing `access_use_after_free_bad`
- summary normalization now uses `simplify_for_summary(precondition_vocabulary, keep)` and includes
  OCaml-style `pre_heap_has_assumptions`
- latent invalid-access classification is now narrower than "any caller-visible constant deref":
  pre-existing caller-controlled values can stay latent, true by-ref/outparam slot writes can stay
  latent, and ordinary callee-written field nulls stay manifest again
- this narrowing restored the direct `.sil` `store_bad` / `use_not_modeled_bad` regressions and
  keeps `funptr.c` at parity; do not undo it just to recover `angelism.c`'s current extra count

Remaining active store-textual work is concentrated in the latent/manifest invalid-access cluster
above. Do not try to "fix" `sizeof.c` in Pulse.

### Textual pipeline gaps

- **DeclEnv enhancements** (`decls.rs`): Missing variadic proc tracking, generics status.
- Language-specific (defer): FixHackWrapper, FixHackInvokeClosure, TransformClosures, verify_variadic_position, SSA restoration.

### SIL test gaps (skipped procs)

- **Virtual dispatch in loads** (2 procs in static_types.sil)
- **Devirtualization return values** (5 procs in virt.sil)
- **Cross-file resolution** (3 procs across npe*.sil)

### Pulse gaps

- **Full ValueHistory / PulseTrace parity**: Rust now has minimal invalid-access provenance and
  history-sensitive dedup, but richer OCaml-style trace reconstruction is still incomplete.
- **`sizeof` type evaluation**: scalar types are handled via `Typ::size_in_bytes()`. Accepted
  store-textual limitation: exported `<int[]>` arrives without array length or `nbytes`.
- **Latent vs manifest invalid-access parity / report timing**: latent/base publishing is now
  routed through summary classification, prune conditions now carry OCaml-style call-depth
  provenance, callee `AbortProgram` summaries now propagate again, Rust now has a caller-side
  `LatentInvalidAccess` path, imported pure-call dependencies now survive summary
  application/normalization, and `pre_heap_has_assumptions` parity is implemented. Remaining:
  `angelism.c`, `latent.c`, `nullptr.c`, wrapper/cycle null paths such as
  `traverse_and_crash_if_equal_to_root`, and richer OCaml-style latent issue typing/traces.

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
