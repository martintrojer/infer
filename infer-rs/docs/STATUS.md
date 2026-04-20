# infer-rs Status

## Summary

**~37,000 lines of Rust across 11 crates. 350+ tests. The latest authoritative store-textual sweep currently covers 52 of 55 C Pulse files (3 skipped for fixpoint exhaustion). NPE detection: expected 131, found 134. Leak detection: expected 20, found 20. UAF detection: expected 7, found 7. The remaining count deltas are the accepted `nullptr.c` `+1` real-bug divergence (`FN_nullptr_deref_old_bad`) and the accepted `sizeof.c` `+2` exported-Textual fidelity limit. Fine-grained summary parity is now driven by the semantic `specialization.c` harness in `crates/test-harness/src/summary_compare.rs`: all `21 / 21` procedures match on `main.pre_post_list`, and the widened harness now also compares specialized summaries with a current verified checkpoint of `Matching: 21` and no diffs. Recent OCaml-backed correctness work that stays in place now mirrors OCaml's dynamic-type specialization path (`PulseArithmetic.and_dynamic_type_is_unsafe`) instead of exporting `Closure(...)` attrs, routes resolved `__call_c_function_ptr` targets with no summary through the direct known-call unknown fallback, materializes missing imported callee pre-edges onto caller state while still skipping value-actual formal-stack bookkeeping cells, preserves latent-invalid-access export/import parity while rematerializing caller-visible non-zero `ConstantDereference(k)` invalidations at summary export, and extends the comparator only for true semantic noise (deterministic specialization keys, witness inequalities, syntax-first witness-atom collapse, unit-affine / exact-RHS / inverse-scaling / eq-closure `is_int(...)` normalization with redundant formula-only witness drop). `invoke`, `invoke_itself_bad`, `may_double_free_if_alias`, and `two_pointers_recursion_bad` now match. Exact issue-set parity work outside this summary cluster is now concentrated in broader wrapper/cycle null-path publication and richer trace/report parity; the filtered `traverse_and_crash_if_equal_to_root` latent-only repro is fixed again.**

## OpenSSL Benchmark

- The benchmark setup is now understood and repeatable on this host:
  use the repo clang on `PATH`, set `CC=clang`, append
  `-isysroot $(xcrun --show-sdk-path)`, and configure OpenSSL with
  `./Configure darwin64-x86_64-cc no-asm`.
- On the latest fresh shared capture
  (`/tmp/infer-rs-openssl-20260417-095315-rebase-j`), capture completed in
  `371.32s`, textual export completed in `0.35s`, and the exported corpus is
  `753` `.sil` files.
- Rust now parses the full exported Textual corpus from this benchmark:
  `753 / 753` `.sil` files with `0` parse failures. The traced `-j 8`
  direct-`.sil` run parsed them in `43.1s`, merged them into `8395`
  procedures / `683` types, and reached a real parallel round with
  `active=8`, so the old immediate macOS `-j > 1` startup failure is no
  longer the main story here.
- The OCaml baseline on the same shared capture completed with
  `infer analyze --pulse-only --results-dir infer-out -j 1` in `589.76s`
  with about `2.55 GB` max RSS.
- Whole-program Rust direct-`.sil` merged runs are still not stable on this
  benchmark: `-j 8` terminated abnormally after `190.81s` at about
  `24.5 GB` max RSS, and `-j 4` terminated abnormally after `690.77s` at
  about `33.2 GB` max RSS. Under `/usr/bin/time`, both runs ended as
  `command terminated abnormally` plus `signal: Invalid argument`, so the
  exact kill mechanism still needs a rerun without `time`.
- Focused traced `whirlpool_block` runs still show retained fixpoint state as
  the main memory multiplier, but the story is now narrower. The old Rust
  baseline reached `2995` retained post snapshots at `36.1s` while the live
  frontier still held only `20` disjuncts. After the latest OCaml-backed WTO
  revisit fix, the same narrowed probe reached `366` retained snapshots at
  `10.1s`, `466` at `30.3s`, `495` at `40.6s`, `520` at `50.8s`, `544` at
  `61.0s`, and `566` at `71.1s`; the live frontier was already down to `1`
  disjunct at `10.1s`, and the retained max per node was `3` then `4`.
- The matching OCaml `whirlpool_block` debug run on the same shared capture
  completed in `1m31s` and retained far less final state:
  `152` post snapshots across `178` CFG nodes, about `98727` post heap nodes,
  `53889` post heap edges, `13698` attr addresses, and `39663` attr entries.
  No final OCaml node retained more than `1` disjunct. Rust is now much
  closer than before, but it is still above the OCaml final shape and the
  remaining gap is the smaller set of loop heads that still keep up to `4`
  disjuncts.
- Rust Textual→SIL now lowers OCaml-exported `__sil_metadata_*` helper calls
  back to `Instr::Metadata`, matching
  `infer/src/textual/TextualOfSil.ml` `InstrBridge.of_sil_metadata`, and
  focused `to_sil` tests cover the supported metadata families.
- The local OCaml exporter in the sibling `infer/` repo now regenerates
  C/Java textual from loaded procdescs after preanalysis/WTO setup instead of
  dumping the raw stored textual string. A focused C export-textual regression
  now locks in `abstract` / `nullify` / `exit_scope` /
  `variable_lifetime_begins` helper emission on export.
- A fresh single-file OpenSSL `wp_block.c` export now carries those cleanup
  metadata helpers too, which removes the old export-boundary explanation for
  the hotspot. That correctness fix does not solve Rust by itself, though: the
  fresh filtered Rust `whirlpool_block` run on the regenerated export now
  finishes in `4m52s` with `1222` retained states, `max_node_disjuncts=8`,
  and top retained nodes `29,31,32,33,35,36,37,38`.
- Rust Pulse now also executes exported `Metadata::ExitScope` semantically
  instead of treating all metadata as a no-op. Cross-ref: OCaml
  `Pulse.ml` `Metadata (ExitScope ...)` and
  `PulseAbductiveDomain.Stack.remove_vars`. Focused tests now cover both
  surfaces: dead post-stack temp removal and preserved pre-rooted formals.
- That `ExitScope` fix materially improves the richer single-file hotspot's
  early retained-state curve but not its final fixpoint shape. A completed
  selected-node rerun on the same `wp_block.c` export now finishes in `4m58s`
  with `204` CFG nodes, `173` revisited nodes, `1222` retained states,
  `max_visit_count=4`, `max_node_disjuncts=8`, `pre_posts=2`, and the same
  top retained nodes `29,31,32,33,35,36,37,38`.
- The selected-node dump pins that remaining block to line `540`
  (`r++` / load / prune) and lines `752-755` (`S.q[...] = L*`). Matching
  OCaml HTML on those lines ends with `Got 1 disjunct back`, and the last
  visible OCaml PRE widths there stay smaller (`2` at node `29`, `4` at
  nodes `31/32`, `2` at nodes `35-38`). The remaining gap is therefore
  Rust-side retained-state convergence on that loop block, not exporter
  fidelity and not missing `ExitScope` semantics.
- `pulse-recency-limit` now exists through both CLI and `.inferconfig` for
  OCaml-style experiments, but Rust intentionally leaves it unset by default.
  Default-enabling the OCaml `32` cap reintroduced the real `nullptr.c`
  `FN_nullptr_deref_old_bad` false negative, and the focused
  `whirlpool_block --pulse-recency-limit 32` probe stayed essentially
  identical to the unbounded run.
- The first dominant hotspot, `ssl_set_client_disabled`, still improved from
  about `1m09s` to about `5.2s` after restoring the OCaml-style disjunctive
  `equal_fast` / semantic-`leq` split, with the same `173` transfer steps,
  `20`-disjunct cap, and hottest node `33:24`. That fix is real, but it is no
  longer enough to make the whole benchmark usable.
- We still do not claim a clean full-program apples-to-apples OCaml-vs-Rust
  timing number. The current blockers are retained invariant-map storage /
  sharing cost in hot procedures, the remaining residual loop-head
  convergence gap on `whirlpool_block`, merged-run abnormal termination,
  remaining heavy local Pulse procedures, and the exported-Textual
  proc-identity loss for some duplicate C names.

Recent correctness / robustness fixes:
- Loop-head convergence now mirrors OCaml more closely on two fronts.
  `state_cmp` / `PulseAbductiveDomain.leq` parity now compares only the
  stack-reachable heap / attr graph and ignores Rust-only helper caches, and
  `absint::TransferFunctions` gained `exec_node(...)` so Pulse can mirror
  OCaml `AbstractInterpreter.MakeDisjunctiveTransferFunctions.exec_node_instrs`
  on WTO revisits. Focused tests cover both surfaces. On `whirlpool_block`,
  the comparator cleanup was correct but not the main movement; the large
  retained-state drop came from the revisit fix that stops re-executing
  already-known pre disjuncts and joins only genuinely new post states into
  the retained node post.
- Latent UAF summary parity tightened again:
  summary pre/post dedup now keys invalid-access-shaped entries by the real
  diagnostic issue type instead of hard-coding NPE, which preserves
  `LatentAbortProgram(USE_AFTER_FREE)` alongside sibling null-style
  `LatentInvalidAccess` summaries at the same access site / heap path.
  Rust also now matches OCaml `PulseArithmetic.is_manifest` for benign
  imported `x != 0` / `0 < x` guards on `must_be_valid` values, so
  `latent_use_after_free` keeps its latent UAF path and
  `manifest_use_after_free` reifies only `USE_AFTER_FREE`. Verified with
  `cargo test -q -p pulse --lib` and `cargo test -q -p pulse --test end_to_end`.
- Direct-formal-load summary parity tightened too:
  Rust was inventing a latent invalid-access summary for the synthetic
  `formal_load_then_exit` shape via broad `summary_eq_zero` recovery on bare
  direct-formal values. A matching OCaml `infer --pulse-only` summary dump for
  `/tmp/formal_load_then_exit.c` shows one `ContinueProgram` only, and Rust
  now matches that behavior with the narrow
  `checker::tests::test_formal_load_then_exit_stays_continue_only` guard
  instead of changing the wider summary-formula simplification rules.
- Disjunctive Pulse execution now mirrors the OCaml split between cheap
  disjunct equality and semantic subsumption more closely. `absint` gained an
  explicit `Comparable::equal_fast(...)` hook, `DisjunctiveDomain::{join,dedup}`
  now use `equal_fast`, and loop widening still uses semantic `leq`
  (cross-ref: OCaml `PulseExecutionDomain.equal_fast` and
  `AbstractInterpreter.MakeDisjunctiveTransferFunctions.join_up_to` / widen).
  On the filtered OpenSSL hotspot `ssl_set_client_disabled`, this keeps the
  exact same execution shape (`173` transfer steps, `20` disjunct cap, hottest
  node `33:24`) while cutting runtime from about `1m09s` to about `5.2s`,
  which is strong evidence that the old cost was dominated by hot
  alpha-equivalence dedup/join comparisons rather than extra transfer work.
- Summary-comparison normalization now canonicalizes conditions through the
  same affine/equality closure already used for phi, and drops the redundant
  exact-one upper-bound artifact `add(-1, x) <= 0` when exported phi already
  fixes `x = 1`. That keeps OCaml's hidden recursive actual conditions like
  `0 < a1` aligned with Rust's visible affine form like `0 < add(-1, i.*)`,
  and restores the ignored `specialization.c` harness to `Matching: 21`
  immediately after the disjunctive `equal_fast` split. The current summary
  checkpoint therefore stays semantically clean while the performance fix
  remains in place.
- Textual ingestion and merged direct-`.sil` robustness improved again:
  parser name positions now accept exported identifiers tokenized as
  `Local(n)` plus `_` wildcard field names, empty exported
  `define { #node_0: @?; jmp @? }` stubs are now marked undefined during
  Textual→SIL lowering, and merged multi-file analysis now keeps a real body
  over an empty stub on duplicate procnames instead of blindly letting the
  later entry win. On the OpenSSL benchmark this closes the old parse blockers
  entirely (`753 / 753` exported `.sil` files now parse), while still keeping
  the remaining real limitation explicit: exported textual can drop OCaml's
  hashed proc UID for some duplicate C names, so real+real plain-name
  collisions remain an accepted `--export-textual` fidelity gap rather than a
  Rust-side renaming target.
- Textual metadata lowering now also mirrors OCaml's export surface for
  `Instr::Metadata`: `to_sil.rs` converts exported
  `__sil_metadata_{abstract,catch_entry,exit_scope,nullify,loop_*,skip,try_*,variable_lifetime_begins}`
  helper calls back into SIL metadata instructions instead of treating them as
  ordinary calls. The old OpenSSL export-boundary gap is therefore closed in
  the local OCaml exporter; the remaining hotspot is now on the Rust analysis
  side of that richer input.
- Pulse transfer now mirrors the OCaml `ExitScope` semantics too:
  `Instr::Metadata(ExitScope(...))` removes dead post-stack variables but keeps
  pre-rooted formals available for summary construction. This was a real Rust
  semantic gap on the richer exported Textual, and the focused `wp_block`
  rerun shows it reduces retained-state growth materially even though later
  revisits still hit the remaining `8`-way retained split.
- CLI/debugging gained OCaml-compatible `--procedures-filter` support through
  both CLI and `.inferconfig`. Rust mirrors OCaml's proc-only vs
  `source_regex:proc_regex` split semantics, and filtered interprocedural runs
  retain matching roots plus their transitive callees so focused hotspot
  debugging can still compute usable summaries.
- Long-running filtered Pulse runs now emit logger-based `pulse-progress`
  heartbeats under `--trace-ondemand`, which exposes elapsed time,
  transfer-step count, current node/instr, current-node revisit count, hottest
  node-so-far, and current/max disjunct counts. The first OpenSSL hotspot
  slice showed that
  `ssl_set_client_disabled` is not frozen on one transfer; it crawls through
  the CFG while staying saturated at `20` disjuncts. After restoring the
  OCaml-style `equal_fast` split, the same filtered run still takes the same
  `173` transfer steps and reaches the same hottest node, but now finishes in
  about `5.2s` instead of about `1m09s`. A wider OpenSSL sample with `-j 8`
  also confirmed that the Rust runner does use parallelism on the full
  benchmark (`active=8` in round 1 over a call graph with `8395` procedures /
  `175` logical waves / max logical wave size `2600`), so the remaining
  bottleneck is procedure-local Pulse cost in specific hot functions rather
  than a broken scheduler or thread-pool setup.
- Debugging now has a narrower retained-state dump surface too:
  `debug-fixpoint-nodes` is available through CLI/config for Rust-only hotspot
  work. With `RUST_LOG=pulse=debug` it logs final retained disjunct / visit
  counts for selected CFG nodes, and `--debug-level-analysis 2` upgrades that
  to full retained PRE/POST dumps. This is the surface that pinned the
  remaining `whirlpool_block` hotspot to nodes `18,20,21,22,24,25,26,27`.
- `pulse-recency-limit` is now wired through config/CLI and `BaseMemory::Edges`
  mirrors OCaml `RecencyMap` batching when that flag is set. Rust keeps the
  default unset, though: matching OCaml's default `32` cap by default would
  regress the real `nullptr.c` `FN_nullptr_deref_old_bad` report, and the
  focused `whirlpool_block` probe showed that recency alone does not change
  the problematic state shape materially.
- `absint` and Pulse now expose a low-frequency `live-fixpoint` heartbeat via
  `TransferFunctions::observe_fixpoint(...)`. On `whirlpool_block`, that made
  the real storage picture obvious: the current frontier state is large, but
  retained invariant-map state across CFG nodes is the dominant multiplier.
- The ondemand summary store now shares cached summaries through `Arc` handles
  instead of cloning large summaries per caller. That is still useful
  overhead reduction, but the new fixpoint heartbeat shows it is not the main
  OpenSSL memory fix.
- Imported arithmetic latent-summary parity is fixed again:
  `PulseFormulaPhi` condition normalization now preserves reverse-pivoted
  linear guards (for example, a stored `x = -neg_x` relation still records the
  imported condition as a caller-visible `-x == 0` shape),
  `Formula::simplify_for_summary(...)` rewrites dead arithmetic temps before
  phi pruning, and summary-side local-invalid-access manifest+twin export now
  stays reserved for caller-sensitive heap-shape / imported-call cases instead
  of pure imported arithmetic. Focused regressions pin down the new behavior
  (`test_simplify_for_summary_rewrites_dead_linear_guard_to_visible_operands`,
  `test_of_proc_keeps_imported_arithmetic_guarded_local_invalid_access_latent`,
  `test_e2e_negated_actual_keeps_arithmetic_latent_summary`), and
  `cargo test -q -p pulse` is green again.
- Function-pointer specialization now uses OCaml-style dynamic types end to
  end: `abductive.rs` tracks known dynamic types and rewrites them through
  equalities, `specialization.rs` applies `TypeName::CFunction(...)` /
  `TypeName::ObjcBlock(...)` bindings instead of seeding exported
  `Closure(...)` attrs, and `checker.rs` resolves `__call_c_function_ptr`
  through dynamic type first and treats an already-known dynamic type as
  satisfying `need_dynamic_type_specialization`. Focused regressions pin down
  the new shape
  (`test_and_equal_substitutes_heap_attrs_and_sets`,
  `test_apply_dynamic_type_specialization_sets_dynamic_type_without_closure_attr`,
  `test_make_specialization_from_caller_uses_dynamic_type_without_closure_attr`,
  `test_exec_call_c_function_ptr_dynamic_type_target_without_summary_uses_direct_unknown_call`),
  and `invoke` now matches OCaml again.
- Specialized-summary parity is now clean:
  unknown-call fallback still materializes missing pointee cells for bare
  pointer and bare `Tfun` actuals before havoc, unknown-call returns record
  `ReturnedFromUnknown(actuals)`, specialized latent abort diagnostics are
  cached sideband and reified on apply, summary normalization recreates
  caller-visible non-zero `Invalid(ConstantDereference(k))` attrs when a value
  is only known constant through phi, summary replay now abduces missing
  imported pre cells onto the caller while still skipping value-actual
  formal-stack bookkeeping cells, and the comparator now collapses witness
  atoms before anchored affine rewrites while deriving `is_int(...)` through
  exact-RHS, inverse-scaling, and eq-closure over exported equalities.
  Focused regressions pin down those behaviors
  (`test_unknown_call_function_value_materializes_missing_pointee_before_havoc`,
  `test_unknown_call_return_records_returned_from_unknown_actuals`,
  `test_add_specialized_summary_strips_latent_abort_diagnostic_from_cached_pre_post`,
  `test_normalize_materializes_nonzero_constant_invalid_for_visible_value`,
  `test_normalize_does_not_materialize_zero_constant_invalid_for_visible_value`,
  `test_apply_summary_materializes_missing_nested_pre_edge_for_value_actual`,
  `test_apply_summary_does_not_replay_formal_stack_cell_onto_value_actual`,
  `test_phi_normalization_derives_is_int_from_exact_rhs_equality`,
  `test_phi_normalization_derives_anchored_is_int_from_inverse_scaling_eq`,
  `test_phi_normalization_drops_formula_only_is_int_after_eq_closure`,
  `test_phi_normalization_drops_invoke_recursive_affine_atoms_with_actual_shape`),
  and the ignored `specialization.c` comparator now sits at `Matching: 21`
  with no differences.
- Summary-comparison normalization tightened again:
  `crates/test-harness/src/summary_compare.rs` now canonicalizes the OCaml
  restricted-witness inequalities emitted by
  `PulseArithmetic.solve_lin_ineq` / `PulseFormulaPhi`, treating
  `eq:x=lin(1*a1,const=1)` as `atom:0 < x`,
  `eq:x=lin(-1*a1)` as `atom:x <= 0`, and collapsing the corresponding
  `is_int(...)` witness terms onto `is_int(x)`. Focused tests pin down both
  shapes, and the ignored `specialization.c` comparator stays at
  `Matching: 16`, `Differences: 5`, which is the right outcome: the pass
  removes formula-presentation noise without hiding the remaining real
  arithmetic / recursion / attr gaps.
- Model-call / latent-summary export parity tightened again:
  known model calls now conservatively initialize actual roots before entering
  the model, matching the `OCamlModel` path in `Pulse.ml`, and
  continue-derived latent-invalid-access summaries now serialize with
  `diagnostic=None` and reconstruct the diagnostic during import. Focused
  regressions pin down both behaviors
  (`test_known_model_conservatively_initializes_actual_reachable_values`,
  `test_of_proc_drops_exported_diagnostic_for_continue_derived_latent_invalid_access`,
  `test_apply_summary_reconstructs_exported_latent_invalid_access_without_diagnostic`),
  and `test_debug_specialization_summary` confirms that
  `may_double_free_if_alias` now matches OCaml on latent diagnostic shape plus
  caller-visible `Initialized` attrs on the loaded pointees. The headline
  comparator stays `Matching: 16`, `Differences: 5` because the remaining
  delta there is formula shape, not attr/export surface.
- Unknown-call / summary-normalization parity tightened again:
  `PulseCallOperations.call_aux_unknown` parity now initializes direct unknown-call
  actuals before havoc/model handling, normal integer literals reuse existing
  formula representatives like OCaml `PulseFormula.absval_of_int`, and summary
  normalization now filters exported attrs with OCaml
  `PulseAttribute.is_suitable_for_{pre,post}_summary`. Focused regressions pin
  down all three behaviors, and the ignored `specialization.c` comparator moves
  from `Matching: 14`, `Differences: 7` to `Matching: 16`, `Differences: 5`.
  `call_may_double_free_if_alias_bad` and `test_unalias` now match; the
  remaining diff set is `add_more_bad`, `add_two`, `invoke_itself_bad`,
  `may_double_free_if_alias`, and `two_pointers_recursion_bad`.
- Callgraph scheduling now treats a single-node SCC with a self-edge as a real
  recursive cycle, so callers such as `alias_recursion` no longer get analyzed
  ahead of self-recursive callees such as `two_pointers_recursion_bad`. The
  focused regression `test_schedule_self_recursive_callee_before_caller`
  documents the bug, and the ignored `specialization.c` comparator moves from
  `Matching: 13`, `Differences: 8` to `Matching: 14`, `Differences: 7`.
- Direct self-recursive calls with no currently available summary now route
  through the same known-call unknown fallback as OCaml instead of preserving
  the state unchanged or falling through to the unresolved-funptr path. The
  focused regressions
  `test_exec_instr_direct_self_recursion_uses_unknown_call_fallback` and
  `test_exec_call_c_function_ptr_resolved_target_without_summary_uses_direct_unknown_call`
  pin down both the direct-call and resolved-funptr cases. This is correctness
  work that keeps pure-call / integer-return behavior aligned with OCaml even
  though the remaining summary diffs are elsewhere.
- Rust now supports OCaml's `pulse-force-continue` config/CLI flag and keeps
  `has_dropped_disjuncts` metadata on both main and specialized summaries.
  Known-callee calls now fall back to transfer-side unknown-call semantics only
  when the selected summary is empty or marked incomplete for that reason.
  Focused checker tests lock down both sides of that contract: empty/incomplete
  summaries regain a continue path, while precise abort-only summaries still do
  not get widened into unknown-call continues. The `specialization.c`
  comparator remains stuck beyond the scheduler fix, so this stays as
  correct groundwork rather than the final explanation for
  `call_may_double_free_if_alias_bad`.
- Selected alias-specialized summaries that contain
  `LatentInvalidAccess` but no `ContinueProgram` now also participate in the
  OCaml-style force-continue fallback. This is the narrower shape behind
  `specialization.c:call_may_double_free_if_alias_bad`: Rust now restores the
  missing empty skipped-call branch without reviving `alias_recursion`. The
  headline comparator stays `14 / 7`, but the remaining delta in
  `call_may_double_free_if_alias_bad` is now only the missing `Initialized`
  attr on `return.*`. The focused regression
  `test_exec_known_callee_summary_force_continue_for_alias_specialized_latent_invalid_summary_without_continue`
  pins down that behavior.
- Direct-formal latent summary shaping now uses real per-state summary-event
  order instead of fake location order. Rust previously stamped
  `MustBeValid` / `MustBeInitialized` attrs with timestamp `0`, so
  `summary.rs` fell back to comparing raw `.sil` locations and could export
  the wrong direct-formal latent branch ordering. `abductive.rs` now assigns
  monotonic timestamps, `summary.rs` orders direct-formal accesses by
  `(timestamp, location)`, and latent-invalid-access shaping now also drops
  later direct-formal pure constraints via a targeted formula helper. This
  removes the extra latent branch from `may_double_free_if_alias`: the raw
  main summary is now `x == 0`, `x > 0 && y == 0`, and the normal continue
  path. The overall comparator remains `Matching: 13`, `Differences: 8`, but
  the remaining delta in that procedure is now formula shape rather than raw
  disjunct count.
- Recoverable transfer-side invalid accesses now stop instead of exporting
  `ContinueProgram + AbortProgram`: recoverable `Load` / `Store` errors in
  `Pulse.ml`-style transfer now go through summary-side classification and
  produce a single stopped state. The focused regression
  `test_store_through_null_formal_stops_as_latent_without_continue` keeps the
  null-formal-store case pinned down. This is correct cleanup, but it does not
  move the current `specialization.c` comparator from `Matching: 13`,
  `Differences: 8`.
- Recoverable C-model invalid accesses now also stop instead of exporting a
  normal continue path. This keeps double-free / invalid model access behavior
  closer to OCaml reporting, and the focused regression
  `test_double_free_stops_without_continue` documents it. This also does not
  change the current summary comparator.
- A broader checker-side attempt to recover non-exit latent invalid accesses
  when another path also reaches exit was tried and reverted. OCaml summary
  dumps for `may_double_free_if_alias`, `test_alias`, and `test_unalias`
  showed that the broad recovery was wrong: it reproduced the missing
  direct-formal-read shape, but also surfaced bogus latent summaries in
  alias/wrapper/recursion cases. A later OCaml cross-check for the tiny
  `formal_load_then_exit` repro showed that this synthetic direct-formal-load
  shape should actually export a single `ContinueProgram`, and Rust now locks
  that down with
  `checker::tests::test_formal_load_then_exit_stays_continue_only` instead of
  keeping the old ignored test as a target.
- Exact summary-equality work now has a semantic driver instead of raw JSON diffs:
  `crates/test-harness/src/summary_compare.rs` canonicalizes OCaml and Rust
  `main.pre_post_list` state (stack / heap / attrs / conditions / phi /
  diagnostic, with alpha-renamed abstract values), and the ignored
  `test_summary_comparison_specialization_main` test uses `specialization.c`
  as the current gold file. This step intentionally compares `main` summaries
  first; `specialized` remains the next layer after the main-summary mismatches
  are reduced.
- Summary normalization now strips post-summary `Initialized` attrs from
  hidden formal/local stack roots after `restore_formals_for_summary`, while
  keeping caller-visible pointee/return attrs. This matches the OCaml exported
  summary surface more closely and removes the old formal-root `Initialized`
  noise from simple summaries such as `add_one`, `add_two`, and `id`.
- The unresolved `__call_c_function_ptr` path now mirrors
  `PulseModelsC.call_c_function_ptr` and
  `PulseOperations.conservatively_initialize_args` more closely: Rust
  dereferences the function-pointer value even on the unspecialized path,
  conservatively initializes values reachable from the function pointer and
  actual roots before model / unknown-call handling, records `UnknownEffect`
  on actual values, and preserves `is_int` on fresh integer returns. This
  removes the old unresolved-call `Initialized` gap from `invoke` and leaves
  `invoke_itself_bad` as the remaining funptr-adjacent comparator case.
- Local read/write access bookkeeping now mirrors OCaml
  `PulseOperations.check_addr_access` more closely: successful reads abduce
  `MustBeInitialized` and mark the accessed address `Initialized`, and
  successful writes mark the written address `Initialized` before the existing
  `WrittenTo` summary marker. This removed the old missing
  `MustBeInitialized` gap from simple summaries such as `add_one`, `add_two`,
  and `id`, and it stays foundational to the current `Matching: 13`,
  `Differences: 8` checkpoint.
- Summary import now honors leaf `MustBeValid` obligations even when the
  callee pre value has no outgoing pre-heap edges. This restores the lost
  caller-dependent latent precondition behavior that regressed after Rust
  stopped synthesizing the old incorrect write-time pre-read edge.
- Summary replay now skips callee formal-stack bookkeeping cells for
  value-style actuals (`Var`, constants, computed expressions) while still
  replaying them for true lvalue / by-ref actuals. This removes the old bogus
  `v -*-> v` self-edge on flows such as `invoke(id, n0)` without weakening
  by-ref materialization.
- Abort-state latent-invalid-access recovery now refuses to republish imported
  callee `MustBeValid` obligations unless the recorded access location belongs
  to a real local access in the current procedure. This removes the old
  wrapper-latent noise from `call_test_alias_bad` and
  `call_test_unalias_bad` without hiding true local aborts.
- The summary comparator now parses both OCaml value-id encodings from
  `all_summaries.json` (`["Unknown","v1","_"]` and heap-target `["v3","_"]`)
  and normalizes `is_int(...)` through equality bindings. That eliminates a
  chunk of bogus graph-shape differences that were not real semantic
  mismatches.
- After those correctness repairs, the remaining
  `test_summary_comparison_specialization_main` semantic mismatches are down to
  `Matching: 16`, `Differences: 5`: `add_more_bad`, `add_two`,
  `invoke_itself_bad`, `may_double_free_if_alias`, and
  `two_pointers_recursion_bad`. `alias_recursion`,
  `call_may_double_free_if_alias_bad`, and `test_unalias` now match after the
  self-loop scheduler fix plus the newer unknown-call / integer-literal /
  summary-filtering cleanup. The remaining themes are representative / formula
  normalization detail plus harder alias / double-free / recursion /
  attr-export mismatches. `may_double_free_if_alias` no longer has the old
  extra latent-disjunct bug or the old witness-equality presentation noise;
  what remains there is the last `is_int(y.*.*)` formula residue, and the
  recursion cluster still needs richer recursive-call bookkeeping than
  "missing summary means unknown call".
- Summary import now snapshots caller allocation state after `materialize_pre`
  but before `apply_post`, and imported `EqZero` handling consults those
  pre-call snapshots instead of the rejected broad formula-before-post reorder.
  This fixes the reduced guarded-outparam summary-import repro and keeps the
  direct `test_e2e_write_through_ptr` regression green without the old
  latent-vs-manifest fallout.
- Imported `LatentInvalidAccess` caller-side rechecks now mark the translated
  diagnostic address `must_be_valid` and reuse `summary::classify_abort_kind()`
  instead of a raw `check_valid && abort_is_manifest` test. This keeps
  direct-formal null dereferences latent at the next caller boundary, fixes
  `manifest_use_after_free`, and preserves the manifest field/null-after-free
  behavior in `access_use_after_free_bad`.
- Direct-formal constant-deref latentification now refuses to fire when the
  summary already has a local depth-0 `addr == 0` condition. This matches the
  OCaml `create_null_path2_bad_FN` / `malloc_then_call_create_null_path_then_deref_unconditionally_bad_FN`
  shape: the summary stays `AbortProgram`, then reporting decides whether it is
  suppressed.
- `free()` now records its `ptr == 0` and `0 < ptr` split as depth-0 branch
  conditions, and prune now records `UsedAsBranchCond` on values used in branch
  tests. Recovered-invalid-access classification uses those signals to separate
  locally branch-proven direct-formal null dereferences from caller-controlled
  `free(NULL)`-style cases. This restores the missing suppressed null report in
  `traces.c` without reviving the earlier broad latent-prepost publication bug.
- OCaml-style suppressed-report handling now exists in Rust reporting:
  default CLI output filters constant / compared-to-null dereferences without a
  matching invalidation event in the access history, while
  `--pulse-report-issues-for-tests` surfaces them as distinguished
  `*** SUPPRESSED ***` reports for `issues.exp`-style sweeps.
- Non-exit diagnostic collection now avoids republishing callee-local manifest
  aborts on wrapper callers when the source range clearly belongs to the callee
  itself. This keeps `bake`-style local NULLPTR reports on the callee instead
  of leaking them into the caller's manifest scan.
- Unspecialized summary application now rejects alias-collapsed callee heap roots when two
  distinct heap-backed callee addresses map to the same caller representative. This is the Rust
  analogue of the OCaml `PulseInterproc.ml` `AliasingWithAllAliases` rejection path, and it lets
  the existing alias-specialization machinery handle aliased actuals instead of forcing the
  unspecialized pre/post through.
- The end-to-end specialization driver now directly analyzes closure targets discovered from global
  initializer summaries when the ondemand store does not already contain them. This removes the
  old order-dependent `test_e2e_global_function_pointer_initializer_is_inlined` flake while also
  avoiding a store/mutex deadlock in the serialized integration harness.
- Invalid-access diagnostics now carry minimal value provenance histories (a reduced Rust analogue
  of `PulseValueHistory` / `PulseTrace`), and dedup keys now include history signatures. This
  restores the missing duplicated `realloc_no_check_bad` report in `memory_leak.c`, so
  `memory_leak.c` is back at parity in the authoritative sweep.
- Summary application now imports callee formula state with one shared substitution across the
  whole callee formula and replays remembered `conditions` before the rest of `phi`, matching
  OCaml `PulseFormula.and_callee_formula` / `PulseInterproc.ml` more closely.
- Summary normalization now uses
  `simplify_for_summary(precondition_vocabulary, keep)` and includes the
  OCaml `pre_heap_has_assumptions` manifestness check instead of the older
  reachability-only approximation.
- Latent invalid-access classification is now narrower and OCaml-backed:
  pre-existing caller-controlled invalid accesses can stay latent, true by-ref
  / outparam slot writes can stay latent, and ordinary callee-written field
  nulls stay manifest. This restored the direct `store_bad` / `use_not_modeled_bad`
  regressions while keeping the intended latent-invalid-access flow.
- Imported pure-call conditions now translate their remembered function-application dependencies
  through summary application, and summary normalization keeps those pure-call results reachable
  from caller-visible actuals. This removed the old `unknown_from_parameters_latent` manifest
  false positive; the pure-call dependency fix remains correct groundwork under the newer
  local-zero/suppression parity work.
- Capture metadata recovery now also restores `has_cleanup_attribute` on locals
  from `infer debug --procedures --procedures-attributes`, and Rust now mirrors
  OCaml `cleanup_attribute_store` by marking values stored into cleanup locals
  as `AlwaysReachable`. This restores `cleanup_attribute.c` parity in the
  authoritative store-textual sweep.
- Unknown by-ref call havoc now refreshes lvalue-root slots instead of weakening summary
  application. This keeps the by-ref unknown-call semantics aligned with OCaml without losing the
  real `call_by_ref_actual_already_in_footprint_bad` report. This remains
  correct groundwork for the current exact-issue parity work.
- Added a Rust analogue of OCaml's latent-invalid-access flow and caller-side reification, which restores the missing aliased UAF behavior in `specialization.c` without turning callee-only paths into manifest base reports.
- Specialized summaries are now published back into the owning ondemand summary store. That
  remains necessary groundwork for function-pointer parity, and `funptr.c` is back at parity in
  the current authoritative sweep.
- The store-textual sweep expectation helper now matches exact basenames instead of suffixes, which removed fake `compound_literal.c` / `initlistexpr.c` diffs caused by filename collisions.
- OCaml-compatible `pulse-model-free-pattern`, `pulse-model-malloc-pattern`,
  `pulse-model-realloc-pattern`, `pulse-model-abort`, `pulse-model-unreachable`,
  `pulse-model-return-{nonnull,this,first-arg,nullable}`, `pulse-model-skip-pattern`, and
  `pulse-model-unknown-pure` flags are now supported through `.inferconfig` and CLI overrides.
  Regex-based flags accept the shared `Str.regexp` syntax used by Infer test suites.
- The ignored store-textual sweep now invokes the `infer-rs` CLI once per exported `.sil` from the originating source directory, so the published totals include OCaml-style upward `.inferconfig` discovery.
- The ignored store-textual sweep now rebuilds `infer-rs` once per test process, eliminating stale
  binary noise from `target/{debug,release}/infer-rs` reuse.
- The `crates/pulse/tests/end_to_end.rs` integration binary now serializes analysis runs behind a
  local mutex. Parallel execution inside one test process was flaky (for example the global
  function-pointer initializer test), while serialized execution is stable and keeps `make check`
  deterministic.
- Accepted limitation: exported Textual currently loses `Sizeof.nbytes` / array extent information
  for cases such as `sizeof(c)` and `sizeof(c[0])`, so the authoritative store-textual sweep still
  over-reports `sizeof.c` by two NPEs. This is a capture/export fidelity limit, not a Pulse
  workaround target. See `docs/STORE_TEXTUAL.md`.
- OCaml-style `NewEq` incorporation is now wired back into the abductive state: formula equalities rewrite heap/attrs/tracking sets instead of staying solver-only. This restored the missing aliased-specialization behavior in `specialization.c`.
- Specialized-alias reasoning now affects actual heap semantics, not just formula representatives: `call_test_alias_bad`, `call_test_unalias_bad`, and `call_may_double_free_if_alias_bad` are all back in the direct `specialization.c` run.
- `apply_summary` now preserves `AbortProgram` summaries instead of dropping them, matching OCaml `PulseCallOperations.apply_callee` more closely.
- OCaml-style prune-condition depth tracking now distinguishes local conditions from callee-imported ones in manifest classification, which restores `assert.c` and `ternary.c` without reintroducing the latent/base reporting bug.
- `ExecutionDomain` / formula equality is now semantic, so `DisjunctiveDomain` subset checks and deduplication behave correctly.
- CLI infer autodiscovery now matches the repo layout and checks sibling `../infer/bin/infer`.
- Unsupported Textual `Closure` / `Apply` / residual `If` expressions now fail conversion explicitly instead of lowering to placeholder `0`.
- OCaml capture parity, CLI multi-file, and inline Pulse smoke tests now assert concrete behavior instead of mostly "did not crash".
- The multi-file CLI path now parses `.sil` inputs in parallel, merges their `Cfg`/`Tenv`, and runs
  Pulse once over the unified program so cross-file calls can reuse summaries across file
  boundaries.

Three CLI modes:
- **Full pipeline**: `infer-rs --pulse-only -- clang -c file.c` (capture + export + analyze)
- **Existing capture**: `infer-rs --pulse-only` (export from capture.db + analyze)
- **Direct .sil**: `infer-rs --pulse-only file.sil` (debugging)

Two analysis pipelines:
- **Liveness**: `.sil` → parse → transforms → to_sil → backward analysis → DEAD_STORE reporting
- **Pulse**: `.sil` → parse → transforms → to_sil → forward analysis → NULLPTR_DEREFERENCE / USE_AFTER_FREE detection

Source location remapping: `LineMap` maps `.sil` line numbers back to original C source via `@[line:col]` annotations (C/C++ frontend) and `// .line` directives (Rust frontend).

Pulse features:

- **WTO fixpoint with DisjunctiveDomain** matching OCaml's `MakeDisjunctive`
- **Multi-disjunct summaries** with PrePostKind (ContinueProgram / ExitProgram / AbortProgram /
  LatentAbortProgram / LatentInvalidAccess)
- **Biabduction**: pre-state tracking, pre-materialization with formal-value mapping, pre-condition violation detection
- **Access-mode parity**: successful reads now carry OCaml-style
  `MustBeInitialized` / `Initialized` side effects locally, while interproc
  still avoids naively propagating formal-stack `MustBe*` attrs until the Rust
  substitution model matches OCaml's dereferenced-formal materialization shape
- **Latent issue support**: `is_manifest` classification, `LatentAbortProgram` propagation through
  call chains, caller-side `LatentInvalidAccess` re-evaluation, and
  branch-condition provenance via `UsedAsBranchCond`
- **Summary specialization**: HeapPath-based dynamic type specialization for function pointer dispatch, recursive multi-level specialization, `needs_specialization` propagation
- **Summary normalization**: `simplify_for_summary(precondition_vocabulary, keep)` with
  reachability filtering matching OCaml `PulseSummary` more closely
- **Interprocedural path condition filtering**: translate callee formula atoms/equations to caller,
  reject inapplicable pre_posts when callee constraints contradict caller state, and reject
  unspecialized summaries when caller aliasing collapses callee-disjoint heap roots
- **Unknown call havoc**: type-aware, havocs memory reachable from pointer-typed args for C extern stubs
- **Formula solver**: union-find, linear arithmetic, atoms, term equalities, CItv integer intervals, `is_int` reasoning, LessThan implication checks, FunctionApplication tracking
- **Path-sensitive constant folding**: comparison ops, Mult/DivI/DivF/Mod, Shiftlt/Shiftrt, BAnd/BOr/BXor
- **`__sil_*` builtin conversion**: 23+ binops, 3 unops, allocate, cast, cfun
- **C models + generic configured models**: malloc/free/realloc, new/delete, exit/abort/\_\_assert\_rtn (noreturn), fopen/getcwd (null/non-null), memcpy/memmove, \_\_builtin\_expect, 18 stdio arg checks, config-driven malloc/free/realloc wrapper matching, and config-driven abort / unreachable / return-nonnull / return-this / return-first-arg / return-nullable / skip / unknown-pure modeling via OCaml-compatible `.inferconfig` flags
- **Memory leak detection**: unreachable allocated-not-freed addresses at summary creation, `find_return_value` void fix, `getcwd` conditional alloc, `is_known_nonzero` atom check, custom allocator tracking for config-driven wrappers
- **Function pointer dispatch** via `__call_c_function_ptr` + Closure attributes
- **Noreturn detection** propagated interprocedurally
- **Deterministic analysis**: thread-local counters + BTreeMap in core structures
- **History-aware invalid-access diagnostics**: minimal provenance paths, formal-to-actual history
  substitution, and history-sensitive dedup (restores duplicated reports such as
  `memory_leak.c:realloc_no_check_bad`)
- **Equality incorporation**: solver-discovered equalities now rewrite `pre`/`post`, heap access indices, attrs, `must_be_valid`, and specialization-tracking sets

## Migration Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 0: Project Setup | ✅ Done | Workspace, CI rules, crate structure |
| 1: Core SIL Types | ✅ Done | Typ, Exp, Instr, Procdesc, Cfg, Tenv, BuiltinDecl (1,777 lines) |
| 2: Textual Parser | ✅ Done | Lexer, parser, printer, transforms, to_sil (5,743 lines) |
| 3: Abstract Interpretation | ✅ Done | Domain traits, RPO + WTO fixpoint, forward/backward (1,503 lines) |
| 4: Liveness Checker | ✅ Done | Backward liveness, dead store reporter (994 lines) |
| 5: Database Layer | ⬜ TODO | rusqlite for capture.db |
| 6: Analysis Driver | ✅ Done | Parallel runner, call graph, blocking dedup, file callbacks (1,184 lines) |
| 7: Pulse | ✅ MVP | Formula, models, prune, interproc, WTO+DisjunctiveDomain, constant folding (4,683 lines) |
| 8: Additional Checkers | ⬜ TODO | SILValidation, BufferOverrun, RacerD, etc. |
| 9: Frontend Support | ⬜ TODO | Keep OCaml frontends, Rust reads Textual |

## Crate Map

```
infer-rs/
  Cargo.toml                    workspace root
  CLAUDE.md                     development rules (fmt, clippy -D warnings, test)
  test-data/                    test fixtures (.sil files)
  crates/                         (tokei: code / total lines)
    sil/          (1,877 / 2,335) core SIL types + BuiltinDecl registry + Specialization
    textual/      (5,743 / 6,613) Textual IR parser, printer, verification, transforms, to_sil
    absint/       (1,578 / 2,037) abstract interpretation framework (RPO + WTO fixpoint + DisjunctiveDomain)
    analyses/       (994 / 1,222) intraprocedural analyses (liveness, dead store reporter)
    test-harness/   (665 /   823) test infrastructure: Textual utils, OCaml infer runner, fixtures
    ondemand/     (1,184 / 1,480) parallel analysis runner with inter-procedural support
    diagnostics/    (214 /   267) issue types, severity, issue reporting
    config/         (200 /   250) configuration: .inferconfig, global OnceLock, OCaml flag compat, manifest parsing
    pulse/        (5,200 / 6,400) Pulse analysis engine (null deref, UAF, models, interproc, specialization, WTO+DisjunctiveDomain)
    cli/            (436 /   537) CLI binary (clap, ondemand integration, config wiring)
```

### sil crate
Core SIL intermediate representation types. All types derive `Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`.

| Module | Mirrors OCaml | Key types |
|--------|---------------|-----------|
| `typ.rs` | `Typ.ml/mli` | IKind, FKind, PtrKind, TypeQuals, TemplateArg, TemplateSpecInfo, TypeName (14 variants), TypeDesc, Typ |
| `exp.rs` | `Exp.ml/mli` | Exp (11 variants), Closure, SizeofData, LfieldObjData |
| `instr.rs` | `Sil.ml/mli` | Instr (Load/Store/Prune/Call/Metadata), InstrMetadata (11 variants), IfKind |
| `procdesc.rs` | `Procdesc.ml/mli` | Node, NodeKind, Procdesc (index-based CFG with BTreeSet edges) |
| `procname.rs` | `Procname.ml/mli` | Procname (10 language variants) with arity-based overload disambiguation for Hack/Python |
| `cfg.rs` | `Cfg.ml/mli` | Cfg (HashMap<Procname, Procdesc>) |
| `tenv.rs` | `Tenv.ml/mli` | Tenv (HashMap<TypeName, Struct>) with transitive super traversal |
| `strukt.rs` | `Struct.ml/mli` | Struct, Field, ClassInfo (Hack/Java class kinds), TenvMethod |
| `int_lit.rs` | `IntLit.ml/mli` | IntLit (arbitrary precision via num-bigint, custom serde) |
| `builtin_decl.rs` | `BuiltinDecl.ml/mli` | Builtin function registry (malloc, free, __new, __delete, etc.) with `is_declared()` and `match_builtin()` |
| `specialization.rs` | `IR/Specialization.ml` | HeapPath (Pvar/FieldAccess/Dereference), PulseSpecialization (dynamic_types map) |
| Others | Various | Ident, Pvar, Var, Fieldname, Binop, Unop, Const, CallFlags, CapturedVar, Mangled, QualifiedCppName, Location, SourceFile, Annot |

### textual crate
Textual IR parser, printer, transforms, and SIL conversion. Depends on `sil` and `logos`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `tokens.rs` | TextualMenhir tokens | Token enum shared between lexer and parser |
| `lexer.rs` | `TextualLexer.ml` | Three-stage pipeline: logos raw tokens → `::` ident merge → compound token adapter |
| `ast.rs` | `Textual.mli` | Full Textual AST: Location, Name, TypeName, Typ, Exp, BoolExp, Instr, Terminator, Node, ProcDesc, Module |
| `parser.rs` | `TextualMenhir.mly` | Recursive-descent parser. Handles `_ = expr` wildcard let-bindings and `<typ>` type expressions |
| `printer.rs` | `TextualOfSil.ml` | Pretty printer with structural roundtrip verification |
| `decls.rs` | `TextualDecls.ml` | Declaration environment (globals, structs, procs) |
| `verification.rs` | `TextualBasicVerification.ml` | Structural checks: unknown labels, unknown fields, wrong arg count |
| `type_check.rs` | `TextualTypeVerification.ml` | Type inference: fill `typ: None`, arrow deref insertion, SSA param registration, builtin return types |
| `transform.rs` | `TextualTransform.ml` | Complete transform pipeline: fix_closure_app → type_check → remove_effects → let_propagation → out_of_ssa |
| `to_sil.rs` | `TextualSil.ml` | Textual→SIL conversion with arity-aware procname construction |

### absint crate
Abstract interpretation framework. Depends on `sil`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `domain.rs` | `AbstractDomain.mli` | Comparable, AbstractDomain, WithBottom, WithTop traits. BottomLifted, TopLifted. Pair, BTreeSet, BTreeMap combinators. BooleanAnd, BooleanOr. |
| `transfer.rs` | `TransferFunctions.mli` | TransferFunctions trait |
| `wto.rs` | `WeakTopologicalOrder.ml` | Bourdoncle's WTO algorithm. Partition enum (Vertex/Component). Iterative DFS with SCC detection. |
| `interp.rs` | `AbstractInterpreter.ml` | RPO + WTO fixpoint engines. Forward/Backward via CfgDirection. WTO widens only at loop heads. |
| `disjunctive.rs` | `AbstractInterpreter.MakeDisjunctive` | DisjunctiveDomain: bounded list of disjuncts with join=union, widen=stop-after-N, leq=subset. |

### analyses crate
Intraprocedural analyses. Depends on `sil`, `absint`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `liveness.rs` | `checkers/liveness.ml` | Backward liveness. LiveVarSet domain. Gen on read, kill on write. Dead store reporter (DEAD_STORE issues). |

### pulse crate
Pulse analysis engine. Depends on `sil`, `diagnostics`, `num-rational`. See [PULSE.md](PULSE.md) for architecture.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `abstract_value.rs` | `PulseAbstractValue.ml` | Fresh symbolic addresses (newtype i64), thread-local counters, per-procedure reset |
| `access.rs` | `PulseAccess.ml` | FieldAccess, ArrayAccess, Dereference |
| `invalidation.rs` | `PulseInvalidation.ml` | How addresses become invalid (CFree, ConstantDereference, etc.) |
| `attribute.rs` | `PulseAttribute.ml` | Address attributes (~25 variants), Attributes set |
| `formula/` | `PulseFormula*.ml` | Constraint solver: union-find, linear arithmetic, atoms, term AST, term equalities (v = binop(x,y)), atom contradiction |
| `base_stack.rs` | `PulseBaseStack.ml` | Var → AbstractValue stack map |
| `base_memory.rs` | `PulseBaseMemory.ml` | AbstractValue → Edges heap graph |
| `base_attrs.rs` | `PulseBaseAddressAttributes.ml` | AbstractValue → Attributes map, check_valid |
| `base_domain.rs` | `PulseBaseDomain.ml` | Composite {stack, heap, attrs} |
| `abductive.rs` | `PulseAbductiveDomain.ml` | Post-state + formula, validity checking, OCaml-style `NewEq` incorporation, known dynamic-type tracking for specialization |
| `value_history.rs` | `PulseValueHistory.ml` + `PulseTrace.ml` | Minimal invalid-access provenance paths, formal-to-actual substitution, history-sensitive dedup support |
| `operations.rs` | `PulseOperations.ml` | eval, eval_deref, write_deref, check_addr_access, eval_or_fresh |
| `transfer.rs` | `Pulse.ml` | SIL instruction → state transition. Prune, UnOp folding (LNot/Neg/BNot), path sensitivity |
| `models/mod.rs` | `PulseModels*.ml` | Model dispatch: builtins first, then name-based. Models take priority over summaries |
| `models/c.rs` | `PulseModelsC.ml` | C models: malloc/free, new/delete, exit/abort (noreturn), fopen (null/non-null), 18 stdio arg-validity checks |
| `models/configured.rs` | `PulseModelsImport.ml` | Generic config-driven models: abort, unreachable, return-{nonnull,this,first-arg,nullable}, skip-pattern, unknown-pure |
| `summary.rs` | `PulseSummary.ml` | PulseSummary with Vec<PrePost> (multi-disjunct), specialized summaries, needs_specialization HeapPaths, is_noreturn flag |
| `specialization.rs` | `PulseSpecialization.ml` | apply() binds HeapPaths to dynamic-type constraints, make_specialization_from_caller(), eval_for_prune |
| `interproc.rs` | `PulseInterproc.ml` | apply_summary: callee→caller effect propagation, formal-value mapping for write-through-pointer, preserve abort summaries |
| `diagnostic.rs` | `PulseDiagnostic.ml` | History-aware AccessToInvalidAddress, MemoryLeak, RetainCycle |
| `execution_domain.rs` | `PulseExecutionDomain.ml` | ContinueProgram, AbortProgram, ExitProgram |
| `checker.rs` | `Pulse.ml` + `PulseCallOperations.ml` | analyze, analyze_with_specialization, select_pre_posts, dynamic-type-aware `__call_c_function_ptr` dispatch, propagate_specialization_need |

### ondemand crate
Parallel analysis runner. Depends on `sil`, `absint`, `rayon`, `dashmap`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `checker.rs` | `registerCheckers.ml` | `IntraChecker`, `InterChecker` (with `analyze_specialized`), `FileChecker` traits. `AnalysisContext` includes Cfg for specialization re-analysis. |
| `summary.rs` | `Summary.ml` | `SummaryStore<S>` with `DashMap<Procname, Arc<OnceLock<S>>>` for blocking dedup |
| `callgraph.rs` | `SyntacticCallGraph.ml` | Call graph from Cfg, bottom-up wave scheduling with SCC cycle detection |
| `runner.rs` | `ondemand.ml` | `run_intra`, `run_inter`, `run_inter_merged` (blocking dedup), `run_file_callbacks`, `run_parallel` |

### diagnostics crate
Issue reporting types. Depends on `sil`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `issue_type.rs` | `IssueType.ml` | Severity, Category, IssueTypeId enum (single source of truth for issue type strings matching OCaml), IssueType |
| `issue.rs` | `Errlog.ml` + `Reporting.ml` | Issue, IssueLog with sort, merge, JSON export, issues.exp format |

### test-harness crate
Shared test infrastructure. Depends on `sil`, `textual`, `serde_json`.

| Module | Description |
|--------|-------------|
| `textual_utils.rs` | `parse_and_convert()`, `parse_file_and_convert()`, `TestModule` with label→node_id lookup |
| `infer_runner.rs` | `InferRunner`: OCaml infer integration, `store_textual_and_export()`, `dump_textual_for_c()`, `analyze_pulse_c()`, report.json parsing, `compare_issues()` |
| `fixtures.rs` | `test_data_dir()`, `ocaml_c_test_dir()`, `parse_issues_exp()`, `issues_for_file()`, `load_fixture()` |
| `summary_compare.rs` | `parse_ocaml_summaries()`, `SummaryFacts`, `compare_summaries()`, `ComparisonReport` |

## Key Design Decisions

1. **Lexer**: logos + hand-written compound token adapter. Evaluated lalrpop but LALR(1) conflicts made it impractical.
2. **Parser**: Recursive descent mirroring TextualMenhir.mly. Supports `_ = expr` (OCaml prints but can't parse this).
3. **CFG**: Index-based (`Vec<Node>` + `HashMap<NodeId, BTreeSet<NodeId>>`). BTreeSet for edge sets.
4. **Transforms**: Complete pipeline matching OCaml's `TextualTransform.run`. NNF/DNF for boolean decomposition, iterative flattening with RemoveIf interleaving.
5. **Procname arity**: Hack/Python procnames encode arity (e.g. `C.f#2`) for overload disambiguation, matching OCaml behavior.
6. **Analysis runner**: No SQLite in the loop. `DashMap<Procname, Arc<OnceLock<S>>>` for blocking dedup — first thread computes, others wait. Bottom-up wave scheduling with SCC cycle detection. File-level callbacks for cross-procedure checkers.
7. **Backward analysis**: Same fixpoint engine as forward, parametrized by CfgDirection trait.
8. **Model dispatch**: `sil::builtin_decl` registry mirrors OCaml's `BuiltinDecl.ml`. Models match by identity via `match_builtin()`, not ad-hoc string comparison.
9. **Interprocedural via ondemand**: CLI wires Pulse as an `InterChecker` into the ondemand runner. Bottom-up call graph scheduling ensures callee summaries are available before callers. Parallel via rayon.
10. **Disjunctive interpreter**: `DisjunctiveDomain<D>` in absint implements `AbstractDomain` with join=union, widen=stop-after-N, leq=subset. Pulse checker uses `compute_fixpoint_wto` with this domain, matching OCaml's `MakeDisjunctive(PulseTransferFunctions)` exactly. No custom iteration loops.
11. **Configuration**: `config` crate with global `OnceLock<InferConfig>`. Set once at startup via `config::init()`, read anywhere via `config::get()`. Supports `.inferconfig` JSON (OCaml-compatible, unknown fields ignored). `#[serde(rename)]` is the single source of truth for flag names.
12. **Summary specialization**: `sil::specialization` (HeapPath, PulseSpecialization) mirrors `IR/Specialization.ml`. `pulse::specialization::apply()` now supports alias groups as well as dynamic types, so aliased actuals can be re-analyzed with the correct heap semantics before dispatch/reporting. Dynamic-type requests now follow OCaml's path-condition-driven shape instead of inventing exported `Closure(...)` attrs on specialized heap paths; callers can satisfy specialization either from known dynamic types or direct closure/Cfun values. Recursive specialization through multi-level call chains. `needs_specialization` propagation from callees to callers enables the ultimate caller to trigger the chain. `eval_for_prune` evaluates constants without Invalid marking for comparison contexts. Cross-ref: `PulseSpecialization.ml`, `PulseCallOperations.ml` iter_call, `Pulse.ml` analyze with specialization.
13. **Call graph Cfun scanning**: `CallGraph::from_cfg` scans ALL Cfun references in ALL expressions (Store values, Call args, Load expressions), not just Call.fun_exp. Captures function pointer targets for dependency scheduling.
14. **Biabduction formal-value mapping**: `apply_summary` maps each formal's loaded value (one deref from stack) to the actual value, ensuring write-through-pointer patterns propagate correctly. Without this, writes go one indirection level too deep.

## OCaml Test Porting Status

### Unit tests
51 of 78 OCaml unit tests ported (65%):

| OCaml file | Total | Ported | Remaining |
|---|---|---|---|
| `abstractInterpreterTests.ml` | 15 | 12 | 3 (try/catch) |
| `livenessTests.ml` | 25 | 17 | 8 (exceptions, closures, while) |
| `TextualParserTest.ml` | 10 | 9 | 1 (snapshot) |
| `TextualTransformTest.ml` | 12 | 7 | 5 (closure-to-obj, hackc, if-subexpr) |
| `TextualKeepGoingVerificationTest.ml` | 3 | 3 | 0 |
| `TextualSilTest.ml` | 10 | 3 | 7 (tenv annotations, instanceof) |
| `TextualTest.ml` | 2 | 0 | 2 (procname, linemap) |
| `TextualRestoreSSATest.ml` | 1 | 0 | 1 |

Remaining 27 tests are blocked on: exceptions (7), closure-to-object (3), tenv annotations (5), Hack-specific (2), while/loop_as_if (2), restore_ssa (1), instanceof (1), procname/linemap/snapshot (4), multi-module merge (1), if-in-subexpr (1).

### OCaml SIL Pulse end-to-end tests
10 of 18 OCaml `.sil` test files covered with assertion tests. Tests reference OCaml source files directly (no copies). Custom/merged fixtures stay in `test-data/pulse/`.

| OCaml file | Status | Notes |
|---|---|---|
| `alloc.sil` | ✅ Full pass | 2 procs, all OK |
| `npe.sil` | ✅ 9/10 | skip `external_call_and_npe_bad` (cross-file) |
| `npe_with_load_in_exp.sil` | ✅ 15/16 | skip `external_call_and_npe_bad` (cross-file) |
| `npe_without_types.sil` | ✅ 15/16 | skip `external_call_and_npe_bad` (cross-file) |
| `to_sil_bug.sil` | ✅ 1/3 | skip 2 deep interproc (pointer-to-pointer) |
| `ocaml_model.sil` | ✅ Full pass | 1 proc, unmodeled call handling |
| `static_types.sil` | ⚠️ 4/6 | skip 2 _bad: chained virtual calls in loads |
| `virt.sil` | ⚠️ 14/20 | skip 1 miss + 4 FP: devirt return values |
| `npe_external_oo.sil` | ✅ Full pass | merged fixture (5 procs, OO dispatch) |
| `externalObjOrientRetNull.sil` | ✅ helper | covered via merged fixture |
| `externals.sil` | — helper | defines `external_return_null` for npe.sil |
| `importedFunctions.sil` | — helper | defines funcs for typesAcrossFiles.sil |
| `npeWithExternalObjOrient.sil` | ✅ | covered via merged fixture |
| `basic.sil` | ⬜ N/A | taint analysis (not implemented) |
| `overload.sil` | ⬜ N/A | taint analysis (not implemented) |
| `overload_use.sil` | ⬜ N/A | taint analysis (not implemented) |
| `exncfg.sil` | ⬜ no issues | exception CFG; no-panic covered by bulk test |
| `textual_models.sil` | ⬜ no issues | Hack builtins; no-panic covered by bulk test |
| `typesAcrossFiles.sil` | ⬜ no issues | type edge cases; no-panic covered by bulk test |

**Skipped procs by root cause:**
- **Cross-file resolution** (3 procs): callee defined in companion `.sil` file, not available in single-file analysis
- **Deep interproc** (2 procs): pointer-to-pointer summary propagation not yet implemented
- **Virtual dispatch in loads** (2 procs): `n0.OO.get_null().B.f` chained method call resolution
- **Devirtualization** (5 procs): 1 miss (virtual dispatch through interprocedural call chain), 4 FP (return value not evaluated through prune conditions after devirtualized call)

### C source → store-textual → export → Rust pipeline (pulse)
The current authoritative sweep covers 52 of 55 C source files through the full pipeline:
C source → OCaml `infer --store-textual` → `infer debug --export-textual` → manifest.json →
Rust parse → Pulse analysis.
This is the authoritative compliance benchmark because it matches the CLI capture/export path.
For ad-hoc performance comparisons, keep using this path for correctness, but
time Rust analysis on already exported `.sil` files because `infer-rs --results-dir`
includes the export step.

Run with: `cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture`

The repo also keeps a separate `capture --dump-textual` sweep as a secondary regression test for the raw dumped `.sil` path. That sweep is useful for parser/to_sil debugging, but its numbers are not the published compliance baseline.

**Pipeline status (55 files):**

| Status | Count | Details |
|---|---|---|
| OK | 52 | parsed + analyzed, 509 procs |
| SKIP | 3 | infinite.c, recursion.c, recursion2.c (fixpoint exhaustion) |
| FAIL_PARSE | 0 | |
| TIMEOUT | 0 | |

**NULLPTR_DEREFERENCE comparison vs OCaml `issues.exp`: expected 131, found 134.**

Per-file differences:
- `nullptr.c`: expected `13`, found `14`
- `sizeof.c`: expected `0`, found `2` (accepted exported-Textual fidelity limitation)

Current interpretation of these deltas:
- `memory_leak.c` now matches OCaml again after the history-aware invalid-access provenance fix;
  `realloc_no_check_bad` once more reports both null origins (`105` and `119`).
- the latest interproc correctness fixes are intentionally correctness-first:
  the focused `manifest_use_after_free` and `access_use_after_free_bad`
  mismatches are fixed even though the overall NPE total moved in the wrong
  direction
- narrowing latent-invalid-access classification so ordinary callee-written
  field nulls stay manifest restored the direct `store_bad` /
  `use_not_modeled_bad` regressions and keeps `funptr.c` at parity
- the two OCaml-style suppressed `nullptr.c` reports are now counted again in the ignored sweep
  via `--pulse-report-issues-for-tests`, but they stay out of default CLI output by design
- the remaining `nullptr.c` delta is the accepted real `FN_nullptr_deref_old_bad` report. Keep
  the Rust report; do not add imprecision to match the OCaml false negative.
- `integers.c`, `nullptr_more.c`, and `offsetof_expr.c` are no longer on the sweep diff list.
- `compound_literal.c` and `initlistexpr.c` already match OCaml; their earlier sweep diffs were
  measurement bugs caused by basename suffix matching in the expectation helper.
- `assert.c` and `ternary.c` remain fixed by OCaml-style prune-condition depth tracking.
- count parity is no longer blocked on `angelism.c` or `latent.c`, though focused debug output can
  still show wrapper/cycle issue-set drift that count-only sweeps do not expose.
- `sizeof.c` is no longer considered an active Pulse parity task: the exported Textual path drops
  `Sizeof.nbytes` / array extents and emits `<int[]>`, so Rust receives too little information to
  fold those branches without adding a workaround. See `docs/STORE_TEXTUAL.md`.
- The remaining active work is concentrated in exact issue-set / reporting parity for broader
  wrapper/cycle null-path publication beyond the now-fixed filtered
  `traverse_and_crash_if_equal_to_root` repro, plus richer trace reconstruction.

**MEMORY_LEAK_C comparison vs OCaml `issues.exp`: expected 20, found 20.**

Leak sweep parity is now exact.

Direct issue-set note:
- `pulse-model-{free,malloc,realloc}-pattern` support is now implemented and reflected in the
  authoritative sweep because the harness runs `infer-rs` from each source file's directory.
- Additional `.inferconfig` model flags now supported: `pulse-model-abort`,
  `pulse-model-unreachable`, `pulse-model-return-{nonnull,this,first-arg,nullable}`,
  `pulse-model-skip-pattern`, and `pulse-model-unknown-pure`.
- The remaining `.inferconfig` gaps found in the current repo audit are copy-specific:
  root/build-system `pulse-model-returns-copy-pattern` and C++ `pulse-model-cheap-copy-type`,
  which depend on unnecessary-copy tracking that Rust does not implement yet.
- Language-specific `.inferconfig` gaps outside the current C null/UAF/leak scope remain:
  `pulse-model-{release,deep-release}-pattern`.

**USE_AFTER_FREE comparison vs OCaml `issues.exp`: expected 7, found 7.**

UAF sweep parity is now exact.

**Summary comparison (15 files, via `infer debug --dump-json-summaries`):**

The older summary-comparison snapshot is now stale relative to the latent-invalid-access,
specialized-summary publication, basename-fix, and config-support work above. Re-run it before
using any exact disjunct/null-attr mismatch counts.

## Known Issues / Gaps

1. **Type verification partial**: Type inference and hole-filling ported. Missing: DFS node ordering, terminator type checking (Ret/Jump SSA args/Throw), Store type-compatibility validation, restore_ssa on ident conflicts.
2. **to_sil partial lowering**: Expression conversion handles common cases. Unsupported Closure / Apply / residual If now fail conversion explicitly instead of lowering to placeholder values, but full lowering is still missing. OCaml ~1200 lines; ours ~600.
3. **Liveness simplified**: No exception handling. Dead store reporter lacks suppression heuristics.
4. **Pulse interprocedural gaps** vs OCaml:
   - Latent/base publishing now goes through summary classification instead of raw abort scans.
   - Summary application now has aliasing-contradiction rejection for unspecialized summaries, and
     callee formula import mirrors OCaml `PulseFormula.and_callee_formula` more closely.
   - Latent invalid access now exists for caller-derived invalid addresses, imported pure-call
     dependencies survive summary application, and `pre_heap_has_assumptions` is included in
     manifestness. The remaining active mismatch is narrower: exact publication/reporting parity
     on broader wrapper/cycle null-path publication beyond the now-fixed filtered
     `traverse_and_crash_if_equal_to_root` repro, plus richer suppression / trace presentation
     detail.
   - Minimal `ValueHistory` threading now exists for invalid-access provenance and dedup, but full
     OCaml `PulseValueHistory` / `PulseTrace` parity is still missing
   - No global variable handling in summary application
   - Specialization implemented for function pointers; dynamic type specialization for OO not yet done
5. **Pulse formula**: Union-find + linear arithmetic + atoms + term equalities + atom contradiction + CItv integer intervals + is_int integer reasoning. Missing: simplex tableau, non-linear terms.
6. **Pulse models**: C models cover malloc/free/realloc, config-driven malloc/free/realloc wrappers via `.inferconfig`, config-driven abort/unreachable/return-{nonnull,this,first-arg,nullable}/skip/unknown-pure models, new/delete, exit/abort/__infer_fail/__assert_rtn (noreturn), fopen (null/non-null), memcpy/memmove (dest+src validity), 18 stdio arg checks, `__call_c_function_ptr` (function pointer dispatch). Missing: list API, copy-tracker-driven `pulse-model-{returns-copy-pattern,cheap-copy-type}` support, and language-specific release/deep-release models. No Java, Hack, ObjC models.
7. **No summary persistence**: In-memory only. Optional disk persistence planned.
8. **Closure-to-object transform**: Closures left as-is rather than transformed to object allocations.
9. **Tenv annotations**: `.final`, `.abstract`, `.kind`, `.constant` struct attributes not handled in to_sil.

## What's Next (ranked by impact and tractability)

### 1. Finish exact invalid-access publication parity
The remaining correctness work is now mostly issue-set, not headline-count, work. Match OCaml on:

- broader wrapper/cycle null-path publication beyond the now-fixed filtered
  `traverse_and_crash_if_equal_to_root` repro
- latent-vs-manifest publication details that count-only sweeps can hide
- report/tracing presentation details around suppressed issues and caller reification

Use OCaml summary dumps and per-instruction traces first; do not tune counts directly.

### 2. Extend ValueHistory / trace parity
The new minimal provenance layer is enough to restore the duplicated `memory_leak.c`
`realloc_no_check_bad` reports and improve dedup correctness, but it is still reduced compared with
OCaml's full `PulseValueHistory` / `PulseTrace` stack.

### 3. Keep the accepted remaining count deltas documented
Do not add Pulse-side workarounds for these:

- `nullptr.c` `+1`: real `FN_nullptr_deref_old_bad` report that OCaml misses
- `sizeof.c` `+2`: exported-Textual fidelity limit (`Sizeof.nbytes` / array extents lost before
  Rust sees the SIL)

### Other latent/reporting follow-up
The current Rust implementation now has condition-depth tracking, latent invalid-access support,
specialized-summary filtering, imported pure-call dependency translation, and
`pre_heap_has_assumptions` in summary manifestness. The remaining general gap is narrower:

- precise latent-vs-manifest execution-kind selection for caller-visible invalid accesses
- wrapper/cycle null-path survival and reification through call chains
- latent issue type reporting / traces
- suppressed-report presentation / trace detail

### Other gaps

**Leak compliance (expected 20, found 20):**
- Leak sweep parity is now exact

**SIL test gaps (from skipped procs):**
- Virtual dispatch in loads (2 procs in static_types.sil)
- Devirtualization return values (5 procs in virt.sil)
- Deep interproc pointer-to-pointer (2 procs in to_sil_bug.sil)
- Cross-file resolution (3 procs across npe*.sil)

**Features:**
- Full latent/manifest publication parity — match OCaml's remaining `is_manifest` /
  abort-publication behavior on caller-visible invalid accesses and wrapper-cycle null paths
- Complete type verification — DFS ordering, terminator type checking, restore_ssa
- More Pulse models — Java, Hack, ObjC
- Summary persistence — serde+bincode to disk
- WidenThenNarrow — OCaml's default WTO mode
- Taint analysis — unblocks 3 SIL test files
- Tenv annotations — `.final`, `.abstract`, `.kind` in to_sil
- Database layer — rusqlite for capture.db
