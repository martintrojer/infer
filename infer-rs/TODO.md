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

Separate recency note: `pulse-recency-limit` is now available through CLI and
`.inferconfig` for OCaml-style experiments, but it is intentionally not the
default path. Matching OCaml's default `32` cap by default would reintroduce
the real `nullptr.c` `FN_nullptr_deref_old_bad` false negative, and the
focused `whirlpool_block` probe stayed essentially unchanged with the cap on.

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
   `test_to_issue_log_filters_suppressed_null_deref_by_default`, and
   `checker::tests::test_formal_load_then_exit_stays_continue_only`.
   OCaml exports the direct-formal-load synthetic repro as a pure
   `ContinueProgram`, so Rust must not resurrect the old broad
   `summary_eq_zero` latent-invalid-access workaround there. These are
   correctness fixes, not optional count-tuning.

4. **Broader wrapper/cycle null-path publication parity**: the real exported
   `latent.c` issue-set compare is exact again at `(procedure, line,
   issue-type)`, and the filtered `traverse_and_crash_if_equal_to_root`
   one-node caller/export path is fixed too, and the reduced one-step / local
   two-hop field-write publication fixtures are green again. The remaining
   direct-formal latent summary gap on the real exported `latent.c` fixture is
   fixed too: `FN_nonlatent_use_after_free_bad{,2}`,
   `latent_use_after_free`, `manifest_use_after_free`, and `main` now line up
   again on the validated compare. Keep the new shared-direct-formal import
   behavior in place: when one callee dereferenced-formal summary value maps to
   multiple caller actuals, summary import must conjoin those actuals instead
   of arbitrarily picking the first one, or the old manifest `main`
   `NULLPTR_DEREFERENCE` comes back.

5. **Exact trace/report parity**: the new minimal suppression + provenance layer is enough for
   dedup and `issues.exp`-style counting, serialized issue traces now append minimal
   invalidation/access history signatures, and `report.json` now carries a minimal structured
   `bug_trace` / `bug_trace_{length,max_depth}` payload plus flat `bug_type` / `severity` /
   `category` aliases, stable `key`, `node_key`, `hash`, `procedure_start_line`, and empty
   `extras`. The structured
   trace is also a bit closer to OCaml now on both access and invalidation paths (caller
   provenance before a synthetic call step, outer-formal synthesis before deeper callee formals,
   modelled-allocation call/return compression, and caller-side UAF qualifiers that mention the
   outer call when that provenance is available), but richer OCaml-style `PulseTrace` /
   publication detail is still incomplete.

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
- latent UAF summary export/import now keeps `USE_AFTER_FREE`
  `LatentAbortProgram` paths distinct from null-style `LatentInvalidAccess`
  siblings by deduping on the real issue type, and benign imported `x != 0` /
  `0 < x` guards on `must_be_valid` values now follow OCaml manifestness
  again; keep `latent_use_after_free` / `manifest_use_after_free` green
- bare direct-formal load/exit summaries no longer synthesize latent
  invalid-access pre/posts from `summary_eq_zero` alone; OCaml exports the
  matching `formal_load_then_exit` repro as one `ContinueProgram`, and Rust
  now locks that down with `checker::tests::test_formal_load_then_exit_stays_continue_only`
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
- the real exported `latent.c` compare is now exact again at the
  `(procedure, line, issue-type)` level: Rust restores OCaml's local
  `AbortProgram` + `LatentAbortProgram` twin publication for
  `traverse_and_crash_if_equal_to_root`, keeps recovered caller abort pre/posts
  in summaries without republishing their manifest diagnostics, dedups latent
  invalid accesses by caller-visible heap path with earlier-location
  preference, and filters mixed local+imported direct-formal latent-invalid
  publication from the report surface; keep
  `test_e2e_latent_cycle_summary_shapes_match_ocaml_subset` and
  `test_e2e_mixed_depth_direct_formal_latent_invalid_is_not_reported` green

Remaining active store-textual work is concentrated in the issue-set / trace-quality invalid-access
cluster above. Do not try to "fix" `sizeof.c` in Pulse or suppress `FN_nullptr_deref_old_bad`.

### Textual pipeline gaps

- **Exported proc identity for duplicate C names**: direct multi-file `.sil` analysis now preserves
  real bodies over empty exported stubs, but `infer debug --export-textual` /
  `manifest.json` can still drop OCaml's hashed proc UIDs and collapse distinct real C functions
  onto one plain procname. Treat this as an exported-Textual fidelity limitation unless upstream
  preserves the proc UID at the textual boundary.
- **Fresh preanalysis export rerun**: the local OCaml exporter in `../infer`
  now regenerates C/Java textual after preanalysis/WTO setup, and fresh
  `wp_block.c` exports now carry `abstract` / `nullify` / `exit_scope` /
  `variable_lifetime_begins`. Re-run the shared OpenSSL corpus through that
  exporter and keep debugging Rust on the richer input; the single-file
  `whirlpool_block` probe now finishes in `4m52s` with `1222` retained
  states, so the remaining blocker is not the old missing-metadata export bug.
- **DeclEnv enhancements** (`decls.rs`): Missing variadic proc tracking, generics status.
- Language-specific (defer): FixHackWrapper, FixHackInvokeClosure, TransformClosures, verify_variadic_position, SSA restoration.

### OpenSSL benchmark follow-ups

**Current state:** The whole-program OpenSSL run is no longer a
categorical scaling blocker. On the 74-file partial corpus at
`-j 4`, the latest no-explicit-cap out-of-box rebaseline completed
cleanly in `226.86s` (`570 / 570` procs, `~14.0 GB` max RSS,
`~8.8 GB` peak footprint, `20 / 570` heap+wall aborts,
`max_visit_count=4`). Whole-program slowdown
vs OCaml's `42.9s`: `~5.3×` out-of-box. The
`OBJ_bsearch_ex_` `max_visit_count=10001` pathology is no longer the
dominant story in the latest convergence probe; the remaining wall
time is bounded-visit DES-family / `OBJ_obj2txt` large-state cost.
See `docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md` and
`docs/plans/NEXT_STEPS.md` for the headline tables and B-track notes.

Immediate next benchmark task: use `scripts/bench_openssl_partial.sh`
for repeated runs and compare medians.

Below: historical `whirlpool_block` notes still useful as context.

- Keep the narrowed `whirlpool_block` checkpoint current on the regenerated
  export too: after the `ExitScope` fix, the completed selected-node rerun on
  the fresh single-file `wp_block.c` export still converges to
  `1222` retained states in `4m58s`, with `204` CFG nodes,
  `max_visit_count=4`, `max_node_disjuncts=8`, and top retained nodes
  `29,31,32,33,35,36,37,38`.
- Keep the new subtree finding in mind while narrowing further: the retained
  `31/35` tier growth is currently concentrated in the global `Cx` table
  subtree (about `+129` subtree nodes / array edges / initialized attrs per
  larger tier), while the local `K` / `S` / `H` loop-state subgraphs stay
  flat.
- The current `--procedures-filter whirlpool_block` hotspot run no longer
  omits `__infer_globals_initializer_Cx`: callgraph/filtering and
  pre-analysis summary collection now retain rooted global initializer deps.
- Rust now also supports OCaml's default `pulse-max-cfg-size = 15000`, so the
  default filtered repro keeps `__infer_globals_initializer_Cx` in the set but
  skips analyzing it as a large procedure. Treat that `4m42s` / `1222`
  retained-state checkpoint as the current OCaml-compatible single-file slice.
- Keep two additional interpretations available when debugging:
  (1) the older no-initializer slice is still useful for the pure loop-head
  convergence bug, and
  (2) the forced retained-initializer slice is the better upper bound for real
  whole-program `Cx` cost.
- Keep the forced retained-initializer cost surface documented even though the
  default path now skips it: on the fresh release repro,
  `__infer_globals_initializer_Cx` alone takes about `5m22s` and reaches
  ~`16k` live heap nodes before `whirlpool_block` even starts, and the first
  minute of `whirlpool_block` with that summary available jumps to about `33k`
  live heap nodes / `67k` edges per active state and multi-million retained
  totals. After ~`7m28s` of `whirlpool_block` itself, the fuller slice was
  still only at `max_node_disjuncts=4` but had already grown to ~`6.85M` live
  heap nodes / `13.68M` live heap edges in retained totals.
- Rust now executes exported `Metadata::ExitScope` semantically instead of as
  a no-op, with focused regressions for dead temp removal and preserved
  pre-rooted formals. Keep that correctness fix even though the full final
  selected-node run still reaches the same `1222` / `8` final shape; it
  improved the early trajectory, not the eventual convergence point.
- Rust now also executes exported `Metadata::VariableLifetimeBegins`
  semantically for non-global locals, with focused regressions for fresh-slot
  rebinding and structured-binding skip behavior. Keep that correctness fix
  too: the completed selected-node rerun still finishes at the same
  `1222` / `8` final shape in `4m56s`.
- Treat the old-vs-new `VariableLifetimeBegins` selected-node dump diffs as
  serialization noise until proven otherwise: representative hot blocks
  (`29:PRE`, `31:PRE`, `38:POST`) keep identical line counts, abstract-value
  counts, invalid / initialized attr counts, `must_be_valid` counts, and
  variable-name sets before vs after the fix, even though raw hashes differ.
- Next narrowed hotspot step: rerun the richer export with
  `--debug-level-analysis 2 --debug-fixpoint-nodes 29,31,32,33,35,36,37,38`
  and compare Rust's retained PRE/POST states to the OCaml line `540` /
  `752-755` block using location/instruction mapping rather than spending more
  time on Rust-vs-Rust raw dump hashes or assuming raw node-id parity across
  frontends.
- Keep the current root-cause split explicit while doing that work: the
  remaining `whirlpool_block` hotspot is primarily a convergence / OCaml-parity
  problem, while clone pressure is a secondary RSS amplifier. The latest
  selected-node alpha signatures still show `35:POST == 31:PRE` and `4`
  monotonic growth tiers per variant, so the next narrowing should explain why
  those tiers survive semantically before treating structural sharing as the
  main fix.
- Do not spend time on `Nullify` / `Abstract` metadata here unless new
  evidence appears: OCaml Pulse keeps them as no-op too.
- Keep the current OCaml comparison in view while doing that dump: the
  corresponding OCaml HTML nodes end with `Got 1 disjunct back`, and their
  last visible PRE widths are smaller (`2` at node `29`, `4` at `31/32`,
  `2` at `35-38`), so the remaining bug is before any exporter workaround
  discussion.
- Do not chase a Rust/Pulse workaround that tries to restore the old smaller
  state numbers by regressing export fidelity. The correct input now includes
  the cleanup metadata.
- Re-export the shared OpenSSL corpus with the patched OCaml exporter before
  any new `whirlpool_block` surgery so the next benchmark checkpoint uses the
  more faithful boundary end to end.
- First rerun the shared exported corpus at `-j 1` after the narrowed hotspot
  comparison is understood, and treat that as the only current publishable
  apples-to-apples Rust timing until merged `-j > 1` runs are stable.
- Then re-run the same shared corpus without `/usr/bin/time` at `-j 4` and
  `-j 8` so we can capture the real exit status and determine whether the
  current failures are external kills or some other runtime termination path.
- Do not treat clone-reduction as the primary `whirlpool_block` fix. The
  current selected-node alpha signatures show the remaining `8` hot disjuncts
  are semantically distinct, not duplicate retained copies, and the biggest
  hotspot wins so far came from OCaml-parity fixes rather than representation
  changes.
- If we pursue clone-reduction for OpenSSL memory / RSS, use component-level
  structural sharing rather than a borrow-heavy refactor. Prototype plan:
  `docs/plans/STRUCTURAL_SHARING_PROTOTYPE.md`. Phase 1 baby steps are now
  in (`Arc<Edges>`, `Arc<Attributes>`, outer `Arc<BTreeMap>` for both, plus
  `Arc<BaseStack.map>` and `Arc<Phi>`); on the filtered single-file
  `whirlpool_block` slice, peak memory is now `~3.93 GB` (down from
  `~16.7 GB`, `~76%` reduction) at unchanged wall time and unchanged
  analysis behavior.
- After the Phase 1 structural-sharing wins, the next steps are tracked in
  `docs/plans/CONVERGENCE_NEXT_STEPS.md`. Three live tracks (A: diagnose the
  `8d:4v` convergence gap; B: validate Arc savings on whole-program OpenSSL;
  C: mop up remaining smaller Arc candidates), with **A as the
  recommendation** since cheap structural sharing has hit clear diminishing
  returns and the remaining cost is now retained-state count, not per-state
  size.
- Keep the new `pulse-recency-limit` flag experimental only. It is useful for
  OCaml cross-checks, but it is not the current fix direction for OpenSSL.
- Keep profiling the remaining heavy procedures after the
  `ssl_set_client_disabled` fix. That hotspot improvement is real, but it is
  no longer the only thing blocking OpenSSL usability.
- Keep the duplicate-proc exported-Textual identity loss documented as an
  upstream fidelity limit unless `infer debug --export-textual` preserves the
  OCaml proc UID.

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
  broader wrapper/cycle null-path publication beyond the now-fixed filtered
  `traverse_and_crash_if_equal_to_root` repro, suppression/report-trace presentation detail, and
  richer OCaml-style latent issue typing/traces.

## Debugging tools

- **Per-instruction tracing**: `--debug-level-analysis 1` (debug) or `2` (trace). Also `RUST_LOG=pulse=debug`. Log lines prefixed with `[proc_name]` for parallel-safe filtering.
- **Scheduler tracing**: `--trace-ondemand` enables logger-based wave start/end and periodic progress snapshots (`RUST_LOG=warn,ondemand=info` by default when the flag is set).
- **Retained-state tracing**: `--trace-ondemand` now also emits `live-fixpoint` heartbeats so OpenSSL debugging can separate active frontier cost from retained invariant-map cost.
- **Selected-node retained dumps**: `--debug-fixpoint-nodes 18,20,22` (with `RUST_LOG=pulse=debug`) logs final retained disjunct / visit counts for chosen CFG nodes; add `--debug-level-analysis 2` to dump retained PRE/POST states too.
- **Comparison script**: `scripts/compare_traces.py` — parses OCaml `--debug` HTML and Rust log, side-by-side per-instruction with disjunct counts.
- **Retained-block comparison script**: `scripts/compare_fixpoint_blocks.py` — compares selected Rust retained PRE/POST dumps across two runs, reports coarse signatures, and shows the first normalized diff.
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
