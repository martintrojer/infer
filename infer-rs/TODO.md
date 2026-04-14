# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (authoritative store-textual sweep: 52/55 files analyzed, 3 skipped for fixpoint exhaustion; NPE expected 131 found 134, Leaks expected 20 found 20, UAF expected 7 found 7)

Correctness note: keep the semantically correct specialization / latent-invalid-access /
specialized-summary fixes even when totals move temporarily. The current sweep reflects the real
remaining count gaps after removing the old basename-matching measurement bug and after wiring
OCaml-style suppressed-report handling into the ignored sweep.

**Documented count deltas, not workaround targets:**

1. **`nullptr.c`** (+1): current sweep is `14` vs expected `13`. Rust reports the real
   `FN_nullptr_deref_old_bad` null dereference, while OCaml intentionally misses it because of
   recency forgetting. Keep the Rust report; do not add imprecision just to match the OCaml false
   negative.

2. **`sizeof.c`** (+2): exported Textual drops `Sizeof.nbytes` / array-length information and
   emits array `sizeof(...)` expressions as `<int[]>`. Rust faithfully roundtrips that back to
   `Sizeof { typ = int[]; nbytes = None }`, so Pulse cannot fold the `sizeof(c) > 2` /
   `sizeof(c) / sizeof(c[0]) != 2` branches. Treat this as a `--store-textual` /
   `--export-textual` fidelity limit unless the interface preserves richer `Sizeof` data.

**Leak differences:** none in the authoritative sweep. `MEMORY_LEAK_C` parity is exact.

**Reporting parity note:**

- Default CLI/reporting now suppresses OCaml-style constant / compared-to-null dereferences that
  do not carry a matching invalidation event in their access history.
- The ignored store-textual sweep explicitly enables `--pulse-report-issues-for-tests`, so those
  same reports still appear as distinguished `*** SUPPRESSED ***` test-only issues and count
  toward `issues.exp` parity.

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

Semantic summary-parity note: `specialization.c` is currently clean in the semantic harness
(`21 / 21` main summaries, `Matching: 21` combined main+specialized summaries). Keep
`test_summary_comparison_specialization_main` green as a regression guard, but it is no longer an
active OCaml-parity gap.

**Active OCaml-backed correctness focus:**

3. **Direct-formal / by-ref / suppression regression lock-in**: keep the focused regressions green:
   `test_e2e_guarded_outparam_write_uses_matching_summary_branch`,
   `test_e2e_write_through_ptr`, `test_e2e_manifest_use_after_free_reports_only_uaf`,
   `test_e2e_access_use_after_free_keeps_manifest_npe_and_uaf`,
   `test_e2e_local_zero_proof_on_formal_keeps_null_deref_manifest`,
   `test_e2e_negated_actual_keeps_arithmetic_latent_summary`,
   `test_e2e_imported_pure_call_condition_keeps_precondition_violation_latent`,
   `test_e2e_latent_chain_stays_latent_until_manifest_callsite`,
   `test_e2e_callee_local_abort_is_not_republished_on_caller`, and
   `test_to_issue_log_filters_suppressed_null_deref_by_default`. Also keep
   the ignored documentation repro
   `checker::tests::test_normal_exit_keeps_non_exit_latent_abort`: it marks a
   still-missing OCaml behavior, but the obvious broad checker-side fix was
   proven wrong and must not be resurrected as a count-tuning workaround.
   These are correctness fixes, not optional count-tuning.

4. **Wrapper/cycle null paths** such as `traverse_and_crash_if_equal_to_root`: the headline file
   counts are now at the expected baseline, but Rust and OCaml can still diverge on which latent
   null paths survive call chains and reify as manifest reports.

5. **Exact trace/report parity**: the new minimal suppression + provenance layer is enough for
   dedup and `issues.exp`-style counting, but richer OCaml-style `PulseTrace` / publication detail
   is still incomplete.

Recent groundwork that should stay in place even though the current NPE total moved in the wrong direction:
- summary normalization now strips hidden formal/local stack-root `Initialized`
  attrs after `restore_formals_for_summary`, matching the OCaml exported
  summary surface while keeping caller-visible pointee/return attrs
- unresolved `__call_c_function_ptr` now dereferences the function-pointer
  value in the unspecialized path, records `UnknownEffect` on actual values,
  and preserves `is_int` on fresh integer returns
- interproc formula import now mirrors OCaml `PulseFormula.and_callee_formula` more closely
  (shared substitution across the whole callee formula, remembered `conditions` imported first)
- summary import now snapshots caller allocation state before `apply_post` and uses that snapshot
  when importing `EqZero`, instead of relying on the rejected broad formula-before-post reorder
- caller-side latent invalid-access rechecks now mark translated diagnostic addresses
  `must_be_valid` and reuse summary classification, which fixes
  `manifest_use_after_free` without regressing `access_use_after_free_bad`
- direct-formal constant-deref latentification now refuses to fire when the summary already has a
  local depth-0 `addr == 0` proof, matching the OCaml `create_null_path2_bad_FN` shape
- default reporting now suppresses OCaml-style constant/compared-to-null dereferences without a
  matching invalidation event in the access history, and test-reporting mode
  (`--pulse-report-issues-for-tests`) emits them as `*** SUPPRESSED ***`
- summary normalization now uses `simplify_for_summary(precondition_vocabulary, keep)` and includes
  OCaml-style `pre_heap_has_assumptions`
- condition recording / summary export now preserves imported linear guards
  even when the solver pivots them onto the opposite variable
  (`neg_x = -x` stored as `x = -neg_x`), and pure imported arithmetic must
  not trigger the local manifest+latent twin path for invalid accesses
- latent invalid-access classification is now narrower than "any caller-visible constant deref":
  pre-existing caller-controlled values can stay latent, true by-ref/outparam slot writes can stay
  latent, and ordinary callee-written field nulls stay manifest again
- this narrowing restored the direct `.sil` `store_bad` / `use_not_modeled_bad` regressions and
  keeps `funptr.c` at parity; do not undo it just to chase headline totals

Remaining active store-textual work is concentrated in the issue-set / trace-quality invalid-access
cluster above. Do not try to "fix" `sizeof.c` in Pulse or suppress `FN_nullptr_deref_old_bad`.

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
  wrapper/cycle null paths such as `traverse_and_crash_if_equal_to_root`, suppression/report-trace
  presentation detail, and richer OCaml-style latent issue typing/traces.

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
