# Debug Log

This file is for short-lived but important debugging context that should survive chat compaction.
Keep it current when the active line of investigation changes.

## Current Focus

- Latest stable checkpoint:
  - `crates/pulse/src/abductive.rs`
    - added `conservatively_initialize_args(...)`
    - cross-ref: OCaml `PulseOperations.conservatively_initialize_args`
  - `crates/pulse/src/checker.rs`
    - `exec_call_c_function_ptr(...)` now:
      - evaluates actual args once up front
      - conservatively initializes values reachable from the funptr and actual
        roots before model / unknown-call handling
      - reuses those evaluated actuals on the unresolved-call path
    - focused regression:
      - `checker::tests::test_exec_call_c_function_ptr_unknown_derefs_funptr_and_marks_unknown_effect`
        now also asserts `Initialized` on both the funptr value and actual arg
        value, while keeping `UnknownEffect`
  - validations on the current tree:
    - `make check`
    - `cargo test -q -p pulse --lib test_exec_call_c_function_ptr_unknown_derefs_funptr_and_marks_unknown_effect`
    - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
  - current `specialization.c` comparator result:
    - `Matching: 13`
    - `Differences: 8`
  - remaining diff set:
    - `add_more_bad`
    - `add_two`
    - `alias_recursion`
    - `call_may_double_free_if_alias_bad`
    - `invoke_itself_bad`
    - `may_double_free_if_alias`
    - `test_unalias`
    - `two_pointers_recursion_bad`
  - important conclusions:
    - `add_one` and `invoke` now match; the unresolved-funptr `Initialized`
      gap was real
    - do not retry the broad checker-side non-exit latent-invalid-access
      recovery; it explodes wrapper/alias diffs
    - the next likely real fault line is in `crates/pulse/src/summary.rs`
      latent/export behavior for the alias / double-free cluster, plus smaller
      comparator-side arithmetic normalization for `add_more_bad` / `add_two`
      / parts of `invoke_itself_bad`

- Latest stable checkpoint:
  - `crates/pulse/src/summary.rs`
    - abort-state latent-invalid-access recovery now skips imported callee
      `MustBeValid` obligations whose access location does not belong to a
      real local access in the current proc
    - focused regression:
      - `summary::tests::test_of_proc_does_not_recover_imported_call_must_be_valid_from_local_abort`
    - important observed effect:
      - `call_test_alias_bad` / `call_test_unalias_bad` no longer export the
        extra Rust-only `LatentInvalidAccess` wrapper summary; debug output is
        back to a single `AbortProgram`, matching OCaml at the coarse level
  - `crates/test-harness/src/summary_compare.rs`
    - fixed `parse_ocaml_value_id()` to handle both OCaml JSON shapes:
      - stack-style `["Unknown","v1","_"]`
      - heap-target style `["v3","_"]`
    - phi canonicalization now resolves `is_int(...)` through explicit `eq:`
      bindings and drops trivial `is_int(constant)` noise
    - added focused parser/canonicalization regressions:
      - `summary_compare::tests::test_parse_ocaml_abort_wrapper_shape`
      - `summary_compare::tests::test_canonicalization_matches_alias_wrapper_abort_shape`
      - `summary_compare::tests::test_phi_normalization_resolves_is_int_through_equalities`
    - this parser bug explained a large chunk of the earlier bogus graph
      diffs in alias-wrapper / specialization summaries
- Validations on the current tree:
  - `cargo fmt --all`
  - `cargo test -q -p test-harness --lib`
  - `cargo test -q -p pulse --lib`
  - `cargo test -q -p pulse --test end_to_end test_debug_specialization_summary -- --nocapture`
  - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
- Current `specialization.c` comparator result:
  - before this pass:
    - `Matching: 5`
    - `Differences: 16`
  - after this pass:
    - `Matching: 11`
    - `Differences: 10`
- Remaining highest-value clusters after the parser/exporter cleanup:
  - real latent-summary/export behavior still looks open in
    `may_double_free_if_alias` / `call_may_double_free_if_alias_bad`
  - comparator/formula normalization is still underpowered for:
    - `add_one`
    - `add_two`
    - `add_more_bad`
    - parts of `invoke`, `invoke_itself_bad`, and `two_pointers_recursion_bad`
  - `alias_recursion` still looks like a genuine summary-set divergence

- Active line of investigation is still `specialization.c` main-summary
  parity, but the latest OCaml-backed state-canonicalization pass confirmed
  that the remaining `Matching: 5` / `Differences: 16` are not caused by
  stale equalities left unreplayed in the exported Rust state.
- Latest stable checkpoint:
  - `crates/pulse/src/abductive.rs`
    - added `canonicalize_with_current_path_condition()`
    - cross-ref: OCaml `PulseAbductiveDomain.canonicalize` before
      `filter_for_summary`
  - `crates/pulse/src/summary.rs`
    - `PrePost::normalize()` now canonicalizes the abductive state before
      `restore_formals_for_summary()`
    - focused unit coverage:
      - `summary::tests::test_normalize_canonicalizes_return_root_to_formula_repr`
- Validations on the current tree:
  - `make check`
  - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
- Important conclusion from this pass:
  - the new canonicalization step is correct and should stay, but the
    `specialization.c` comparator output is unchanged:
    - `Matching: 5`
    - `Differences: 16`
  - this narrows the next fault line further:
    - comparator-side semantic normalization is likely underpowered for the
      arithmetic/representative cluster (`add_one`, `add_two`, `add_more_bad`,
      `id`)
    - alias/main-summary shape differences are still real candidates in
      `test_alias`, `test_unalias`, `call_test_alias_bad`,
      `call_test_unalias_bad`, and `may_double_free_if_alias`
  - fresh raw-summary evidence from OCaml:
    - `add_one` exports `pre: v1 -*-> v2`, `post: return(v14) -*-> v13`, and
      phi `v2 = v13 - 1`
    - `add_two` exports `pre: v1 -*-> v3`, `post: return(v30) -*-> v29`, and
      phi `v3 = v29 - 2`
    - `add_more_bad` exports `pre` branch conditions and term-eqs in the same
      style (`v2 = a1 + 1`, function-app result tied to return minus one)
  - implication:
    the next pass should inspect raw Rust summaries for the same procs and
    decide whether to improve the semantic comparator or the exported summary
    shape; canonicalizing the state itself was not enough
- Follow-up comparator work on the same tree:
  - `crates/test-harness/src/summary_compare.rs` now prefers stable
    stack-root path labels (`i`, `i.*`, `return`, `return.*`, `x.*.*`, ...)
    over arbitrary `vN` ids for values reachable from the summary stack/heap
  - focused unit coverage:
    - `summary_compare::tests::test_canonicalization_prefers_stack_paths_for_reachable_values`
  - the headline count is still unchanged:
    - `Matching: 5`
    - `Differences: 16`
  - but the remaining diffs are now more interpretable:
    - arithmetic/representative cluster now reads in stack-root terms
      (`add_one`: OCaml still effectively ties `return` to the input path
      differently from Rust; `id`: pure `is_int(i.*)`-style mismatch)
    - alias-wrapper cluster now clearly shows the extra Rust latent
      `x.* = 0` / `y.* = 0` summaries on top of the manifest/local wrapper
      behavior
  - implication:
    the next likely high-value fix is in abort-state latent-invalid-access
    recovery for wrapper callers (`call_test_alias_bad`,
    `call_test_unalias_bad`, `may_double_free_if_alias`), while the
    arithmetic cluster may still need comparator-side formula normalization
    rather than analysis edits

- Active line of investigation is still `specialization.c` main-summary
  parity, but the latest pass narrowed two more exporter/model gaps without
  changing the headline `Matching: 5` / `Differences: 16`.
- Latest stable checkpoint:
  - `crates/pulse/src/summary.rs`
    - summary normalization now strips post-summary `Initialized` attrs from
      hidden formal/local stack roots after `restore_formals_for_summary`,
      while keeping caller-visible pointee/return attrs
    - focused unit coverage:
      - `summary::tests::test_normalize_drops_initialized_on_formal_stack_root`
  - `crates/pulse/src/checker.rs`
    - unresolved `__call_c_function_ptr` now:
      - forces the OCaml-style dereference of the function-pointer value in the
        unspecialized path
      - records `UnknownEffect` on actual values
      - marks fresh integer returns with `is_int`
    - focused unit coverage:
      - `checker::tests::test_exec_call_c_function_ptr_unknown_derefs_funptr_and_marks_unknown_effect`
  - validations on the current tree:
    - `cargo test -q -p pulse --lib`
    - `cargo test -q -p pulse --test end_to_end`
    - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
- Current `specialization.c` comparator result is still:
  - `Matching: 5`
  - `Differences: 16`
- Important conclusions from this pass:
  - the old formal-root `Initialized` export noise was real and is now gone
    from simple summaries such as `add_one`, `add_two`, `id`, and from the
    alias-call wrappers
  - unresolved funptr summaries now expose more of the OCaml unspecialized
    surface (`UnknownEffect`, extra dereference / `MustBe*`, integer return
    typing), which changed the shape of `invoke` / `invoke_itself_bad` in the
    right direction even though the comparator count did not move
  - the next fault line is now concentrated in:
    - representative / formula normalization (`add_one`, `add_two`, `id`,
      `add_more_bad`, parts of `invoke_itself_bad`, `two_pointers_recursion_bad`)
    - alias-shape / latent-vs-continue main-summary mismatches
      (`test_alias`, `test_unalias`, `call_test_alias_bad`,
      `call_test_unalias_bad`, `may_double_free_if_alias`)

- Active line of investigation is back on `specialization.c` main-summary
  parity after restoring two correctness regressions that the previous
  parity pass introduced.
- Latest stable checkpoint:
  - `crates/pulse/src/interproc.rs`
    - `materialize_pre` now checks `MustBeValid` obligations for leaf pre
      values too, not only for pre cells with outgoing edges
    - post-cell replay now skips callee formal-stack cells only for
      value-style actuals (`Var`, constants, computed expressions), while
      preserving replay for true lvalue-style actuals (`Lvar` / `Lfield` /
      `Lindex`)
  - focused new unit coverage:
    - `interproc::tests::test_apply_summary_keeps_leaf_precondition_violation_latent_when_flag_depends_on_caller`
    - `interproc::tests::test_apply_summary_does_not_replay_formal_stack_cell_onto_value_actual`
  - correctness regressions fixed:
    - `test_e2e_imported_pure_call_condition_keeps_precondition_violation_latent`
    - `test_e2e_latent_chain_stays_latent_until_manifest_callsite`
  - authoritative validations on the current tree:
    - `cargo test -q -p pulse --lib`
    - `cargo test -q -p pulse --test end_to_end`
    - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
- Current `specialization.c` comparator result is still:
  - `Matching: 5`
  - `Differences: 16`
- Important new conclusion from this pass:
  - the old `invoke(id)` / `add_one` self-edge bug (`v -*-> v`) was real and is
    now fixed
  - the remaining `add_one` / `add_two` / `add_more_bad` diffs are no longer
    about bogus self-dereference cells
  - the next fault line is narrower:
    return/result representative choice and formula normalization still differ
    from OCaml, and Rust still leaves extra `Initialized` attrs on caller/formal
    roots that OCaml does not export
- Most useful OCaml cross-refs for the next pass:
  - `infer/src/pulse/PulseInterproc.ml`
    - `materialize_pre_from_actual`
    - `apply_post`
  - `infer/src/pulse/PulseAbductiveDomain.ml`
    - `restore_formals_for_summary`
    - `filter_for_summary`

- Active step on the main-summary comparator path was the OCaml-style
  read/write access bookkeeping fix.
- Stable checkpoint from this pass:
  - `crates/pulse/src/operations.rs` and `crates/pulse/src/abductive.rs`
    now mirror the `PulseOperations.check_addr_access` split more closely:
    - successful reads abduce `MustBeInitialized` and mark the accessed
      address `Initialized`
    - successful writes mark the written address `Initialized` before the
      existing `WrittenTo` summary marker
  - focused unit coverage added:
    - `operations::tests::test_read_access_abduces_must_be_initialized_and_marks_initialized`
    - `operations::tests::test_write_access_marks_written_address_initialized`
  - important OCaml cross-ref from this pass:
    - `PulseInterproc.materialize_pre_from_actual` starts PRE
      materialization from the dereferenced formal value, not the formal stack
      cell itself
    - consequence for Rust:
      do **not** naively propagate formal-stack `MustBeValid` /
      `MustBeInitialized` through the current substitution map
    - current Rust interproc hook keeps the new pre-attr propagation limited
      to non-formal derived addresses, and still accepts the older
      `must_be_valid` post-set used by some Rust unit fixtures
- Current `specialization.c` main-summary comparison result after this pass:
  - `Matching: 5`
  - `Differences: 16`
  - the targeted attr gap improved locally:
    simple summaries such as `add_one`, `add_two`, and `id` no longer miss
    `MustBeInitialized` on the formal root in `pre_attrs`
  - the dominant remaining clusters are now:
    - heap/root shape mismatches
    - formula representative/normalization mismatches
    - latent invalid-access duplication / classification mismatches
    - extra `Initialized` attrs that are likely downstream of the remaining
      shape mismatch rather than a standalone access-mode bug
- Validations completed on the current tree:
  - `cargo test -q -p pulse operations::tests::test_read_access_abduces_must_be_initialized_and_marks_initialized --lib`
  - `cargo test -q -p pulse operations::tests::test_write_access_marks_written_address_initialized --lib`
  - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
  - `make check`

- Active line of investigation has moved from count-level parity back to
  fine-grained summary equivalence.
- New summary-comparison checkpoint:
  - `crates/test-harness/src/summary_compare.rs` now has a canonical
    main-summary comparator that:
    - parses OCaml `all_summaries.json`
    - builds a fine-grained `PrePost` model (stack / heap / attrs /
      conditions / phi / diagnostic)
    - alpha-renames abstract values
    - compares canonicalized main summaries per procedure
  - `crates/pulse/tests/end_to_end.rs` now has the first gold-file harness
    `test_summary_comparison_specialization_main`
  - current scope is intentionally step 1 only:
    - compare `main.pre_post_list`
    - ignore specialized summaries for now
    - ignore OCaml-only `type_constraints`
    - ignore interval-only noise and filter builtin-only Rust summaries
    - dedup simplified attrs so location/timestamp multiplicity does not
      overwhelm the semantic diff
- Focused validation:
  - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`
  - `make check`
- Current `specialization.c` main-summary comparison result:
  - `Matching: 5`
  - `Differences: 16`
  - no `ocaml_only` / `rust_only` procedures after builtin filtering
- The remaining diff clusters now look actionable:
  - **Precondition attr parity**: OCaml often exports
    `MustBeInitialized + MustBeValid` on formal roots while Rust often exports
    only `MustBeValid`
  - **Heap/root shape parity**: several procedures still disagree on whether a
    formal/root introduces an extra dereference layer (`v3 -*-> v2` vs
    `v2 -*-> v3`-style mismatches)
  - **Formula parity**: Rust still exports more normalized atom/linear forms in
    places where OCaml exports `term_eqs` / `is_int`-shaped facts
  - **Execution-kind parity**: `may_double_free_if_alias` still has
    `LatentInvalidAccess` vs `ContinueProgram` differences
- Realistic next step from this checkpoint:
  - use the new `specialization.c` diff as the driver for main-summary parity
    fixes before extending the comparator to `specialized`

- Active line of investigation has now moved from the `traces.c` gap back to
  general parity / documentation cleanup.
- Current tree status:
  - Pulse branch-condition parity fix is implemented and documented
  - `make check` is green
  - ready to commit once the tracked files are staged
- Stable checkpoint from the latest pass:
  - `traces.c` is fixed in the ignored store-textual sweep
  - current authoritative ignored sweep is:
    - `NPE: expected 131, found 134`
    - `LEAK: expected 20, found 20`
    - `UAF: expected 7, found 7`
    - file diffs:
      - `nullptr.c: expected 13, found 14`
      - `sizeof.c: expected 0, found 2`
  - interpretation remains:
    - `nullptr.c +1` is the accepted real `FN_nullptr_deref_old_bad`
      divergence
    - `sizeof.c +2` is the accepted exported-Textual fidelity limitation
- Root cause and fix for the old `traces.c` miss:
  - OCaml exports two summary disjuncts for `access_use_after_free_bad`:
    a suppressed `ConstantDereference` on the `v4 == 0` branch and a manifest
    `CFree` / UAF branch
  - Rust was missing the suppressed null report because two OCaml parity
    signals were absent or underused:
    - the `free(NULL)` / `free(non-null)` split in `models/c.rs` was not
      recording depth-0 prune conditions, only formula equalities
    - prune handling in `transfer.rs` was not recording
      `UsedAsBranchCond`, which OCaml uses to distinguish real local branch
      proofs from other caller-controlled direct-formal null paths
  - the stable fix set is:
    - `crates/pulse/src/models/c.rs`: record the `ptr == 0` / `0 < ptr`
      branch conditions in the free model
    - `crates/pulse/src/transfer.rs`: record `UsedAsBranchCond` on values used
      in prune expressions
    - `crates/pulse/src/summary.rs`: use `UsedAsBranchCond` plus the recorded
      local zero condition to keep branch-proven direct-formal null derefs
      manifest, while preserving `free(NULL)` direct-formal cases such as
      `latent.c:deref_then_free_then_deref_bad` as latent
- Latest focused validations on the current tree:
  - `make check`
  - `cargo test -q -p pulse --lib`
  - `cargo test -q -p pulse --test end_to_end`
  - `cargo test -q -p infer-rs --test cli_tests test_pulse_report_issues_for_tests_surfaces_suppressed_reports -- --nocapture`
  - `cargo test -q -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`

- Active line of investigation is now the latent-invalid-access recovery work
  for caller-controlled field writes when a procedure never exports a normal
  `ContinueProgram`/`ExitProgram` path.
- Stable checkpoint from the latest pass:
  - `crates/pulse/src/summary.rs` now recovers caller-controlled latent invalid
    accesses that survive into a local abort state, including the same-block
    case that previously dropped them entirely
  - focused regressions now cover both:
    - non-exit node-boundary recovery (`test_two_hop_field_write_keeps_local_null_derefs_latent`)
    - same-block abort-state recovery (`test_same_block_local_abort_keeps_earlier_null_derefs_latent`)
  - authoritative green validations on the current tree:
    - `cargo test -q -p pulse --lib`
    - `cargo test -q -p pulse --test end_to_end`
    - `cargo test -q -p infer-rs --test cli_tests test_pulse_report_issues_for_tests_surfaces_suppressed_reports -- --nocapture`
  - authoritative ignored sweep on the current tree:
    - `NPE: expected 131, found 133`
    - `LEAK: expected 20, found 20`
    - `UAF: expected 7, found 7`
    - file diffs:
      - `nullptr.c: expected 13, found 14`
      - `sizeof.c: expected 0, found 2`
      - `traces.c: expected 5, found 4`
- Important conclusion from the latest experiments:
  - the remaining `traces.c` miss is not caused by summary construction anymore;
    exported Textual summaries already contain the needed latent pre_posts for
    `access_use_after_free_bad`
  - the gap is in publication/reporting: the current CLI/test reporting path
    still does not surface the extra suppressed line-62 null report without
    destabilizing unrelated files
  - broad attempts to publish latent pre_posts from `to_issue_log()` were
    explicitly rejected; they fixed `traces.c` but exploded issue counts in
    `latent.c`, `integers.c`, `interprocedural.c`, `uninit.c`, and others
- Current tree state from this experiment:
  - `crates/pulse/src/checker.rs` now does a non-exit scan that synthesizes
    latent invalid accesses from `ContinueProgram` states only when the exit
    node has no normal path
  - `crates/pulse/src/summary.rs` no longer synthesizes latent invalid-access
    pre/posts directly from summarized `ContinueProgram` exit states
  - this narrowed sweep regressions in `abduce.c` and `dangling_deref.c`, but it
    also overcorrected and dropped focused latent summaries that should still be
    exported, including the local two-hop field-write repro and the latent NPE
    side of `access_use_after_free_bad`
- Immediate next step:
  - restore the correct summary-time latent invalid-access behavior without
    bringing back duplicate/sweep-side overreporting
- Important constraint discovered while validating the no-normal-exit path:
  - the current checker-side non-exit recovery only sees CFG-node boundary
    states from the fixpoint, not arbitrary intra-block states
  - a focused repro that creates caller-controlled `must_be_valid` obligations
    and then aborts in the same block is therefore not a valid test of the
    current mechanism
  - the focused unit/e2e repros were updated to split the field write and the
    local abort into separate basic blocks, which now correctly exercises the
    non-exit latent recovery path

- Active line of investigation has moved from headline count recovery to exact
  invalid-access publication/report parity.
- Latest concrete correctness fixes from this pass:
  - `crates/pulse/src/summary.rs`:
    `pre_post_has_direct_formal_constant_deref()` now refuses to latentify a
    direct-formal null dereference when the summary already contains a local
    depth-0 `addr == 0` proof
  - `crates/pulse/src/diagnostic.rs`,
    `crates/pulse/src/value_history.rs`,
    `crates/pulse/src/invalidation.rs`,
    `crates/pulse/src/checker.rs`:
    OCaml-style suppressed-report detection now exists; default reporting drops
    those issues, and `--pulse-report-issues-for-tests` surfaces them as
    `*** SUPPRESSED ***`
  - `crates/pulse/src/checker.rs` also filters out callee-local manifest aborts
    from the caller's non-exit scan when the diagnostic source range clearly
    belongs to the callee
- OCaml cross-refs used for this pass:
  - `PulseSummary.exec_summary_of_post_common`
  - `PulseLatentIssue.should_report`
  - `PulseReport.is_constant_deref_without_invalidation`
  - `PulseInvalidation.is_same_type`
- Focused coverage added this pass:
  - `summary::tests::test_classify_abort_kind_reports_direct_formal_null_manifest_when_locally_proven_zero`
  - `summary::tests::test_classify_abort_kind_keeps_write_through_pointee_null_deref_latent`
  - `checker::tests::test_to_issue_log_filters_suppressed_null_deref_by_default`
  - `end_to_end::test_e2e_local_zero_proof_on_formal_keeps_null_deref_manifest`
  - `end_to_end::test_e2e_deref_then_free_then_deref_keeps_npe_latent`
  - `end_to_end::test_e2e_callee_local_abort_is_not_republished_on_caller`
  - `cli_tests::test_pulse_report_issues_for_tests_surfaces_suppressed_reports`
- Latest authoritative validations on the current tree:
  - `cargo fmt --all`
  - `cargo test -q --manifest-path Cargo.toml -p config --lib`
  - `cargo test -q --manifest-path Cargo.toml -p pulse --lib`
  - `cargo test -q --manifest-path Cargo.toml -p pulse --test end_to_end`
  - `cargo test -q --manifest-path Cargo.toml -p infer-rs --test cli_tests`
  - `cargo test --manifest-path Cargo.toml -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
- Latest authoritative ignored sweep on the current tree:
  - `NPE: expected 131, found 134`
  - `LEAK: expected 20, found 20`
  - `UAF: expected 7, found 7`
  - file diffs:
    - `nullptr.c: expected 13, found 14`
    - `sizeof.c: expected 0, found 2`
- Interpretation of the current sweep:
  - `nullptr.c +1` is now the accepted real `FN_nullptr_deref_old_bad`
    divergence; the two OCaml-style `*** SUPPRESSED ***` reports are counted
    again in the ignored sweep but remain hidden in default CLI output
  - `sizeof.c +2` is still the accepted exported-Textual fidelity limitation
  - count parity is no longer blocked on `angelism.c` or `latent.c`
  - remaining meaningful parity work is exact issue-set publication on
    wrapper/cycle null paths such as `traverse_and_crash_if_equal_to_root`,
    plus richer `ValueHistory` / `PulseTrace` parity

Older notes below are historical checkpoints and may no longer describe the
active investigation.

Config-surface follow-up:

- Rust now also supports the OCaml-style generic config-driven model flags
  `pulse-model-unreachable`, `pulse-model-return-this`,
  `pulse-model-return-first-arg`, `pulse-model-return-nullable`, and
  `pulse-model-unknown-pure`, wired through both `.inferconfig` parsing and
  CLI overrides
- do not claim support for `pulse-model-returns-copy-pattern` or
  `pulse-model-cheap-copy-type`; both require the missing non-disjunctive
  unnecessary-copy tracker
- `crates/pulse/tests/end_to_end.rs` now serializes analysis behind a local
  mutex because the integration binary was flaky under parallel test execution;
  isolated runs and `--test-threads=1` were already green

Accepted correctness-positive divergence:

- `nullptr.c`: expected `13`, found `14` (`NULLPTR_DEREFERENCE`)
- root cause: Rust reports the real `FN_nullptr_deref_old_bad` null dereference, while OCaml's
  own source comment documents that it intentionally misses this because of recency forgetting
- policy: keep the Rust report; do not add imprecision just to match OCaml's false negative

Accepted store-textual limitation:

- `sizeof.c`: expected `0`, found `2` (`NULLPTR_DEREFERENCE`)
- root cause: exported Textual drops `Sizeof.nbytes` / array extent information and emits
  `<int[]>`, so Rust receives too little data to fold those branches without a workaround
- policy: accept and document this as a textual fidelity limit; do not add Pulse-side hacks for it

Latest authoritative sweep after the provenance/history change:

- `NPE: expected 131, found 134`
- `LEAK: expected 20, found 20`
- `UAF: expected 7, found 7`

Important checkpoint after validating the provenance work:

- `memory_leak.c` is now at parity (`16` issues total in the store-textual sweep).
- The previously missing duplicated `realloc_no_check_bad` null report is restored.
- New focused ignored regression:
  `cargo test -p pulse --test end_to_end test_e2e_memory_leak_realloc_reports_both_null_origins -- --ignored --nocapture`
  passes and proves both origin lines (`105`, `119`) survive dedup.
- `cargo test -p pulse --lib -- --nocapture` is green.
- `make check` is green.

Current authoritative file-level count diffs:

- `nullptr.c`: expected `13`, found `14` (`NULLPTR_DEREFERENCE`)
- `sizeof.c`: expected `0`, found `2` (`NULLPTR_DEREFERENCE`)

Latest known direct `nullptr.c` proc-set state before this checkpoint:

- gone: `unknown_from_parameters_latent` manifest `NULLPTR_DEREFERENCE`
- still extra: `FN_nullptr_deref_old_bad`

Latest structural correctness work:

- `crates/pulse/src/attribute.rs`
  now uses payload-sensitive ordering for `Allocator` / `Attribute` instead of
  variant-tag-only ordering.
- Supporting `Ord` derives were added on the SIL key types needed by the
  attribute set (`Location`, `Fieldname`, `IdentKind`, `Procname` family,
  `Pvar`, `Var`, etc.).
- New regression test:
  `attribute::tests::test_distinct_invalid_attributes_are_preserved`
  proves two distinct `Invalid(...)` attrs on one value are no longer dropped.
- Important outcome: this did **not** change the authoritative sweep and did
  **not** make `realloc_no_check_bad` report twice on a fresh direct
  `memory_leak.sil` run.
- Conclusion: the remaining `memory_leak.c` gap is still missing
  access/invalidation trace/value-history provenance, not just attribute-set
  collapsing.

Harness status:

- `crates/test-harness/src/infer_runner.rs` now builds `infer-rs` once per test
  process via `OnceLock`, so the ignored store-textual sweep no longer silently
  reuses a stale `target/{debug,release}/infer-rs`.
- Older `NPE 134` readings were partly stale-binary noise, but the current
  confirmed sweep on this tree after the recent null-materialization work is
  `NPE 133 / LEAK 20 / UAF 7`.
- `make check` is green on this tree.

Rejected experiment:

- Changing Rust diagnostic dedup to key on `(issue_type, invalidation_location, AbstractValue)`
  made direct `memory_leak.c` show both `realloc_no_check_bad` null reports, but it was wrong.
- The same change exploded the store-textual sweep to `NPE: expected 131, found 154`, with obvious
  clone reports in `latent.c`, `list_checks.c`, `integers.c`, `nullptr.c`, and others.
- This was reverted. The real missing piece is trace/message provenance, not raw address identity.

What improved this turn:

- `traces.c` is back to parity (`6` issues total, `5` manifest NPEs).
- The fix was not to weaken summary classification globally. That regressed
  `angelism.c` and `funptr.c`.
- The correct edit is narrower and only affects non-exit abort publication in
  `crates/pulse/src/checker.rs`:
  - build a temporary normalized abort pre/post
  - if the abort is on a value newly written into caller-owned memory through a
    true by-ref formal (`**`-style outparam), keep it latent
  - otherwise preserve the existing manifest/latent behavior
- Supporting regression/unit coverage now exists in `crates/pulse/src/summary.rs`
  for both sides of that boundary:
  - direct formal invalid accesses stay manifest for the non-exit reporter
  - post-written by-ref invalid accesses stay latent, including repr-canonicalized values
- Current focused validations after this fix:
  - `cargo test -p pulse classify_abort_kind --lib -- --nocapture`
  - `cargo test -p pulse caller_visible_invalid_access --lib -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`

- `nullptr.c` no longer has count parity, but the move is correctness-positive:
  `create_null_path2_bad_FN` is restored and only the old recency false
  positive remains.
- The fix was not another manifestness special-case. The real missing piece was
  pure-call dependency propagation through summaries:
  - `crates/pulse/src/interproc.rs`
    `translate_formula` now also replays remembered `fn_app_eqs` into the
    caller path condition when translating a callee formula
  - `crates/pulse/src/summary.rs`
    summary normalization now treats pure-call results as reachable from their
    caller-visible actual arguments
- This matches OCaml more closely: imported conditions such as
  `unknown(x) == 999` stay connected to the caller formal instead of being
  dropped as dead formula state during summary normalization.
- Focused regressions now pass:
  - `cargo test -p pulse --lib -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_imported_pure_call_condition_keeps_precondition_violation_latent -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
- Authoritative sweep progression for this checkpoint:
  - before the pure-call interproc fix: `NPE 133 / LEAK 20 / UAF 7`
  - after the pure-call interproc fix: `NPE 132 / LEAK 20 / UAF 7`

- `angelism.c` is now back at parity (`7` NPEs) after fixing the unknown by-ref slot semantics.
- The correct fix was not summary-level skipping. The first attempt marked formal slots with an
  `UnknownEffect`-style summary escape hatch, but that wrongly removed both:
  - the stale false positive `call_by_ref_actual_already_in_footprint_ok`
  - and the real expected report `call_by_ref_actual_already_in_footprint_bad`
- The correct edit is narrower and matches the intended semantics better:
  - keep the normal formal-value-to-actual mapping in interproc
  - at unknown/empty-body call sites, when the actual expression is an lvalue address
    (`Lvar` / `Lfield` / `Lindex`), ensure the root has a fresh post-state `Dereference`
    edge if one was missing
  - this makes later loads from `&param` observe an unknown rewritten slot value without
    erasing pre-call accesses that should still map back to the caller actual
- Focused regressions now pass:
  - `cargo test -p pulse test_apply_summary_removes_caller_edges_missing_from_callee_post --lib -- --nocapture`
  - `cargo test -p pulse test_apply_summary_materialize_pre_translates_array_indices --lib -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_unknown_call_havoc -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_unknown_call_havoc_on_by_ref_formal_slot -- --nocapture`
- Authoritative sweep progression for this checkpoint:
  - stale binary before the harness fix: `NPE 134 / LEAK 22 / UAF 7`
  - current sweep on this tree: `NPE 133 / LEAK 20 / UAF 7`

- `specialization.c` regained the missing `USE_AFTER_FREE` in
  `call_may_double_free_if_alias_bad` after adding a Rust analogue of OCaml's latent-invalid-access
  flow.
- `funptr.c` is now at parity (`11` issues) after publishing specialized-summary diagnostics back
  into the owning callee summary in the global ondemand store.
- `compound_literal.c` and `initlistexpr.c` already match OCaml; their previous sweep diffs were
  caused by `issues_for_file()` using basename suffix matching instead of exact basename matching.
- `specialization.c` NPE parity is now exact (`4`), after making alias specialization equalities
  part of `phi` rather than exported depth-1 conditions.
- UAF sweep parity is now exact.
- Rust now supports the OCaml-compatible config flags
  `pulse-model-free-pattern`, `pulse-model-malloc-pattern`, and
  `pulse-model-realloc-pattern`, including the OCaml `Str` grouping/alternation syntax used by
  shared `.inferconfig` files such as `\\(my\\|a\\)_malloc`.
- Rust now also supports the generic config-driven model flags `pulse-model-abort`,
  `pulse-model-unreachable`, `pulse-model-return-nonnull`, `pulse-model-return-this`,
  `pulse-model-return-first-arg`, `pulse-model-return-nullable`,
  `pulse-model-skip-pattern`, and `pulse-model-unknown-pure`, wired through both `.inferconfig`
  parsing and CLI overrides.
- The ignored store-textual sweep now invokes the `infer-rs` CLI per exported `.sil` from the
  originating source directory, with `--source-override` used only to preserve the original
  manifest source filename in reports.
- OCaml `.inferconfig` lookup was confirmed to walk upward from the starting working directory to
  filesystem root. The harness bug was the starting directory, not a `.git` / `.hg` boundary.
- Textual `Lvar` lowering now matches OCaml `TextualSil.ml`:
  - `crates/textual/src/to_sil.rs` threads `DeclEnv` through expression lowering
  - declared globals now become `Pvar::mk_global(...)` instead of always being local pvars
  - focused regression test added: `test_global_lvar_is_lowered_to_global_pvar`
- The synthetic global function-pointer initializer path now works end to end:
  - initializer summaries preserve the global stack binding for `fp`
  - the spec-loop driver now also seeds target summaries from `Closure(...)` attrs published by
    global initializer summaries
  - focused regression test now passes:
    `cargo test -p pulse --test end_to_end test_e2e_global_function_pointer_initializer_is_inlined -- --nocapture`
- `memory_leak.c` regained the previously missing wrapper-through-global leak reports:
  - `malloc_ptr_leak_bad`
  - `malloc_ptr_no_check_leak_bad`
  - and regained one missing NPE on the `malloc_via_ptr` path
- `memory_leak.c` array-wrapper false positives are now gone after two array-index fixes:
  - interproc post translation now canonicalizes translated constant array indices
  - summary normalization now preserves formula facts for array-index values used in retained heap
    accesses
- direct `memory_leak.c` is now down to a single proc-level mismatch:
  - missing one duplicated `NULLPTR_DEREFERENCE` on `realloc_no_check_bad`
  - leak proc set is now at parity
- `cleanup_attribute.c` is now back at parity after mirroring OCaml's
  cleanup-local metadata and store behavior:
  - CLI capture metadata recovery now restores local `has_cleanup_attribute`
    flags from `infer debug --procedures --procedures-attributes`
  - Pulse store handling now marks values stored into cleanup locals as
    `AlwaysReachable`
  - summary normalization now keeps the transitive closure of
    `AlwaysReachable` addresses out of leak reporting
- Correctness-first note: these fixes make the aggregate sweep numbers temporarily worse because
  they recover real issues before the remaining false positives are removed.
- The unknown pure-int call parity work is now split into two confirmed fixes:
  - `crates/pulse/src/formula/phi.rs`
    `add_linear_eq` now re-solves substituted linear equations through `add_linear_eq` instead of
    reinserting them raw. This is the Rust-side analogue of OCaml re-normalizing propagated linear
    equalities; it fixes the missed `IsInt` contradiction on indirect substitutions such as
    `x = sum / 2` then `sum = 1`.
  - `crates/textual/src/to_sil.rs`
    regular Textual calls now preserve procdecl-based return and formal argument types instead of
    hardcoding `Typ::void()`. This matches OCaml `TextualSil.ml` call lowering and fixes the
    straight-line regression where empty-body pure C calls returning `int` failed to contribute
    integer facts in Pulse.
- New focused regressions now pass:
  - `cargo test -p pulse --lib -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_empty_body_pure_int_call_preserves_integer_reasoning -- --nocapture`
  - `cargo test -p textual to_sil::tests::test_conversion_with_calls -- --nocapture`
- `crates/textual/src/to_sil.rs`
  general expression lowering now always maps `__sil_cast(<typ>, value)` to `Exp::Cast`,
  including zero constants. This matches OCaml `TextualSil.ml`; the previous Rust-only
  zero-cast workaround was incorrect and kept values like `__sil_cast(<int>, 0)` opaque.
- The cast correction fixed the remaining exported-textual `offsetof_expr.c` false positive:
  `FN_test_offsetof_expr_nonlit_bad` is now gone, and `offsetof_expr.c` dropped off the sweep
  diff list entirely.
- Authoritative sweep progression this turn:
  - before these fixes: `NPE 140 / LEAK 22 / UAF 7`
  - after unknown-call int + procdecl call typing: `NPE 138 / LEAK 22 / UAF 7`
  - after removing the zero-cast workaround: `NPE 137 / LEAK 22 / UAF 7`

What is still open:

- `memory_leak.c` is now short by `1` NPE and has leak count parity.
- `sizeof.c` has `+2` NPEs.
- `nullptr.c` no longer affects the aggregate NPE count, but its proc set is
  still off by one missing and one extra report.

Current strongest diagnosis:

1. Specialized-summary request collection is now coming from the actual caller state during the
   fixpoint, not from the previous hand-rolled replay. That fix is correct and should stay.

2. The specialization work is now in a good state semantically:
   - requests are collected from the real fixpoint caller state
   - alias specialization equalities are applied in `phi`
   - latent invalid accesses reify in callers
   - specialized callee diagnostics are published once on the owner, not per caller

3. `call_test_alias_bad` / `call_test_unalias_bad` were staying latent because
   `specialization.rs` used `and_condition_direct(..., depth=1)` for alias groups. OCaml
   `PulseSpecialization.apply` uses `PulseArithmetic.prune_binop`, so the Rust side was corrected to
   use `state.and_equal(...)` instead. This fixed those callers.

4. Rust now has the minimal analogue of OCaml's `LatentInvalidAccess` /
   `PotentialInvalidAccessSummary` path:
   - caller-visible invalid accesses in summaries stay latent instead of being forced through the
     generic manifestness classifier
   - `apply_summary` reifies them only when the translated caller address is invalid after summary
     application
   - this is the correct fix for `may_double_free_if_alias`; do not revert it even if other totals
     move temporarily

5. Specialized-summary publication must be filtered:
   - manifest specialized diagnostics should merge into the owner summary
   - diagnostics already represented by latent specialized pre/posts must NOT be merged
   - otherwise we reintroduce false extra callee reports such as `may_double_free_if_alias`

6. The Textual global lowering gap was real and is now fixed:
   - OCaml `TextualSil.ml` resolves `Lvar` via `TextualDecls.get_global`
   - Rust had been lowering every `Lvar` to a local `Pvar`
   - this prevented `var.is_global()` checks from ever firing in Pulse
   - the fix should stay; do not regress it to improve totals

7. The remaining `memory_leak.c` mismatch is now much narrower and more honest:
   - expected `NULLPTR_DEREFERENCE` procs:
     - `malloc_ptr_no_check_leak_bad`
     - `realloc_no_check_bad` (twice)
   - actual Rust `NULLPTR_DEREFERENCE` procs:
     - `malloc_ptr_no_check_leak_bad`
     - `realloc_no_check_bad` (once)
   - leak proc set now matches expected after:
     - global function-pointer initializer visibility
     - canonical post translation of constant array indices
     - preserving array-index formula facts in normalized summaries
   - OCaml-style last-chance pointer-arithmetic leak suppression
   - the remaining subproblem is the missing duplicated null-path on `realloc_no_check_bad`

8. The rejected dedup experiment clarified the actual diagnostic gap:
   - OCaml issue dedup is effectively keyed by issue kind plus message (`err_desc`) plus location
   - the two OCaml `realloc_no_check_bad` reports survive because their traces/messages differ
   - Rust `Diagnostic::AccessToInvalidAddress` currently only carries `{addr, invalidation,
     access_location, invalidation_location}`
   - without some analogue of OCaml trace/history/message provenance, Rust cannot distinguish the
     wanted `realloc_no_check_bad` duplicate from bogus loop/unroll clones
   - do not reintroduce raw-address-based dedup as a workaround

9. The next real mismatch clusters are now:
   - `memory_leak.c` for the one remaining duplicated `realloc` null path
   - `cleanup_attribute.c` for cleanup-attribute leak behavior
   - `nullptr*`, integer, `offsetof`, and `sizeof` over-reporting for NPE behavior

10. `.inferconfig` audit status:
   - for the current `infer/tests/codetoanalyze/c/pulse/.inferconfig`, there are no additional
     missing flags beyond the now-supported wrapper model keys
   - the remaining copy-specific `.inferconfig` gaps are `pulse-model-returns-copy-pattern`,
     which appears in `/.inferconfig` and the `pulse_messages_{c,cpp}` test configs, and
     `pulse-model-cheap-copy-type` in `infer/tests/codetoanalyze/cpp/pulse/.inferconfig`
   - `pulse-model-abort`, `pulse-model-unreachable`,
     `pulse-model-return-{nonnull,this,first-arg,nullable}`, `pulse-model-skip-pattern`, and
     `pulse-model-unknown-pure` are now implemented correctly as generic config-driven models
   - other missing Pulse config keys found in Infer test configs are mostly outside the current C
     null/UAF/leak parity scope: `pulse-specialization-partial`,
     `pulse-model-{release,deep-release}-pattern`, and taint-related config
   - several additional Pulse flags are still used directly in test Makefiles rather than
     `.inferconfig`, including `pulse-model-alloc-pattern` and
     `pulse-model-transfer-ownership`

11. Current exported-textual status after the latest fixes:

12. `sizeof.c` now looks like a Textual-roundtrip representation gap, not a
    straightforward Pulse bug:
    - exported `sizeof.sil` serializes the problematic conditions as raw type
      expressions such as `__sil_gt(<int[]>, 2)` and
      `__sil_divf(<int[]>, <int>)`
    - Rust currently lowers any stray `Exp::Typ` to `Exp::Sizeof`, but OCaml
      `TextualSil.ExpBridge.to_sil` does not allow raw `Typ` outside specific
      builtins
    - the exported Textual type also loses array-length information
      (`char c[2]` becomes `c: int[]`), so `sizeof(c)` cannot currently be
      reconstructed faithfully from the exported `.sil` alone
    - this means `sizeof.c` is unlikely to be fixed by a small Pulse-only edit;
      the eventual fix probably belongs in the Textual bridge or by threading
      additional capture metadata into the Rust pipeline
   - `sizeof.sil` is unchanged and still reports the same 2 false `NULLPTR_DEREFERENCE`s.
     This still looks like store-textual information loss around raw type expressions such as
     `<int[]>`, not a Pulse arithmetic bug.
   - `offsetof_expr.sil` now reports only:
     - `test_offsetof_expr_bad` (expected)
   - The zero-cast special-casing in Rust `Textual` lowering was the real root cause of the
     spurious `FN_test_offsetof_expr_nonlit_bad` report; do not reintroduce it as a workaround for
     null-path handling.

## Non-Negotiable Guidance

- Read and follow `CLAUDE.md` before changing Pulse/formula/interproc code.
- Cross-reference every analysis change against the OCaml source in `infer/src/pulse/`.
- Correctness over numbers: confirm the semantic fix first, then investigate totals.
- Run `make check` before closing work if possible.

## Files Touched In This Checkpoint

- `crates/ondemand/src/summary.rs`
- `crates/cli/src/main.rs`
- `crates/cli/tests/cli_tests.rs`
- `crates/config/src/lib.rs`
- `crates/pulse/src/checker.rs`
- `crates/pulse/src/formula/mod.rs`
- `crates/pulse/src/interproc.rs`
- `crates/pulse/src/models/c.rs`
- `crates/pulse/src/models/configured.rs`
- `crates/pulse/src/models/matching.rs`
- `crates/pulse/src/specialization.rs`
- `crates/pulse/src/summary.rs`
- `crates/pulse/src/transfer.rs`
- `crates/pulse/tests/end_to_end.rs`
- `crates/textual/src/to_sil.rs`
- `crates/test-harness/src/infer_runner.rs`
- `README.md`
- `docs/STATUS.md`
- `TODO.md`
- `LOG.md`

## Confirmed Changes Already Made

1. `crates/pulse/src/abductive.rs`
   `read_heap` now matches OCaml `SafeMemory.eval_edge` more closely:
   - return existing post edge without overwriting pre
   - only abduce into pre when the root is already present in pre
   - register new pre targets

2. `crates/pulse/src/summary.rs`
   `is_manifest` is now less naive:
   - collect formal-derived values from the pre heap, not the post heap
   - inspect atoms plus constant equalities
   - treat linear equalities as an undirected dependency graph

3. `crates/pulse/src/summary.rs`
   Added a first Rust port of OCaml `restore_formals_for_summary`.

4. `crates/pulse/src/base_memory.rs`
   Added `BaseMemory::remove`.

5. `CLAUDE.md`
   Added a `Correctness Over Numbers` section capturing the user's guidance.

6. `crates/pulse/src/base_memory.rs`
   Added `BaseMemory::retain_reachable` so summary filtering can drop dead heap cells.

7. `crates/pulse/src/summary.rs`
   Strengthened `PrePost::normalize()` to get closer to OCaml `filter_for_summary`:
   - trim unreachable pre/post heap cells, not just attrs
   - simplify the summary path condition to live values
   - retain only reachable `must_be_valid` and specialization values
   - keep leak checking on the pre-filter state
   - added unit tests for dead local heap/formula trimming

8. `crates/pulse/src/state_cmp.rs`
   Added alpha-equivalence canonicalization for Pulse states:
   - compare states modulo abstract-value renaming instead of raw IDs
   - keep disconnected attr-only values visible so leak-relevant states do not collapse
   - covers heap, attrs, formula, `must_be_valid`, and specialization values

9. `crates/pulse/src/execution_domain.rs` + `crates/absint/src/disjunctive.rs`
   Disjunctive join/leq now use semantic state comparison (`Comparable::leq`) instead of plain
   structural equality on raw abstract values. This is the direct Rust-side analogue of OCaml's
   `PulseExecutionDomain.leq` / `PulseAbductiveDomain.leq` being used during widening.

10. `crates/pulse/src/models/c.rs`
    `free()` now mirrors OCaml `Basic.free_or_delete` more closely:
    - only keep satisfiable `ptr == 0` and `ptr > 0` branches
    - regression test added for known-nonnull `free`
    - fixed the `lists.c` leak explosion (`delete_all` stabilizes at 4 disjuncts again)

11. `crates/pulse/src/checker.rs` + `crates/pulse/src/summary.rs`
    Latent-vs-manifest reporting is now decided by summary-style classification instead of publishing
    every raw `AbortProgram` seen during the checker scan.
    - non-exit aborts are still scanned, but only published if they are manifest
    - `of_proc` now emits manifest abort diagnostics itself after latent reclassification
    - `is_manifest` now ignores benign non-null constraints on allocated / must-be-valid /
      already-invalid values
    - `main` is treated as an entry point for this classification

12. `crates/pulse/src/formula/mod.rs` + `crates/pulse/src/interproc.rs` + `crates/pulse/src/transfer.rs` + `crates/pulse/src/summary.rs`
    Added OCaml-style prune-condition provenance:
    - local `Prune` conditions are recorded at depth `0`
    - imported callee conditions are translated at depth `depth + 1`
    - summary `is_manifest` now checks recorded conditions instead of raw atoms/equalities
    - formula simplification now trims dead conditions too
    - this restores `assert.c` and `ternary.c` without reintroducing the latent/base bug

## Validation Already Run

- `cargo fmt --all`
- `make check`
  - passed cleanly on 2026-03-30 after the specialization/interproc changes
- `cargo test -p pulse --lib -- --nocapture`
  - now `152 passed`
- Focused alpha-equivalence tests added and passing:
  - duplicate disjuncts with renamed abstract values collapse
  - disconnected leak-only state does not collapse away
- Targeted direct CLI checks:
  - `interprocedural.c` now matches the direct OCaml issue set again (6 issues)
  - `latent.c` dropped from 6 spurious UAFs to the expected 3 UAFs; one loop-depth NPE gap
    remains
  - `assert.c` now reports the expected 1 NPE again
  - `ternary.c` now reports the expected 3 NPEs again
- Latest focused validations after the condition-shape fix:
  - `cargo test -p pulse --test end_to_end test_debug_follow_ret -- --nocapture`
    - latent propagation remains latent through summaries and reifies at entry points
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --nocapture`
    - traversal conditions no longer collapse to tautologies like `x = x`
    - `manifest_use_after_free` / `deref_then_free_then_deref_bad` still emit both NPE and UAF
- `cargo test -p pulse --test end_to_end test_debug_specialization_summary -- --nocapture`
  - after the alias-specialization change:
      - `call_test_alias_bad` now reports `NULLPTR_DEREFERENCE`
      - `call_test_unalias_bad` now reports `NULLPTR_DEREFERENCE`
      - `call_may_double_free_if_alias_bad` now reports `USE_AFTER_FREE`
      - `may_double_free_if_alias` itself stays issue-free while keeping an alias-specialized summary
- `cargo test -p infer-rs --test cli_tests test_source_override_sets_reported_file -- --nocapture`
  - passed
- `cargo test -p test-harness --lib`
  - passed
- `cargo test -p textual test_global_lvar_is_lowered_to_global_pvar -- --nocapture`
  - passed
- `cargo test -p pulse --test end_to_end test_e2e_global_function_pointer_initializer_is_inlined -- --nocapture`
  - passed after the global-lowering and initializer-target fixes
- focused array / summary regressions:
  - `cargo test -p pulse apply_summary_canonicalizes_constant_array_indices_in_post --lib -- --nocapture`
    - passed
  - `cargo test -p pulse normalize_keeps_formula_for_reachable_array_index_constants --lib -- --nocapture`
    - passed
  - `cargo test -p pulse normalize_suppresses_leak_reachable_via_field_access --lib -- --nocapture`
    - passed
- latest authoritative sweep:
  - `cargo test -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
    - `NPE: expected 131, found 140`
    - `LEAK: expected 20, found 22`
    - `UAF: expected 7, found 7`

## Remaining Sweep Differences

Authoritative sweep command:

```bash
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

Current file-level diffs from the latest sweep:

- NPE under: `memory_leak.c` (-1)
- NPE over: `angelism.c` (+1), `nullptr.c` (+2), `nullptr_more.c` (+2), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2)
- UAF parity is now exact

## Relevant OCaml Cross-References

- `infer/src/pulse/PulseAbductiveDomain.ml`
  - `restore_formals_for_summary`
  - `filter_for_summary`
  - `discard_unreachable_`
  - `check_memory_leaks`

- `infer/src/pulse/PulseInterproc.ml`
  - `materialize_pre_from_actual`
  - `materialize_pre_from_address`
  - `apply_post`

- `infer/src/pulse/PulseSummary.ml`
  - `exec_summary_of_post_common`
- `LatentInvalidAccess`

- `infer/src/pulse/PulseArithmetic.ml`
- `infer/src/pulse/PulseFormula.ml`
- `infer/src/pulse/PulseCallOperations.ml`
  - latent invalid access reification in callers

## Current Line Of Thinking

### What was just confirmed

- The harness fix is semantically correct: published totals must reflect OCaml-style upward
  `.inferconfig` search from each source directory, not a process-global test config.
- The old `20 vs 22` leak total was partially masked by missing config-driven models in the sweep.
- `memory_leak.c` is now the real next leak target because config loading is already correct there.
- `interprocedural.c`, `latent.c`, `funptr.c`, and `specialization.c` stay fixed after the harness
  change.

### Immediate next edits

- Keep the correctness-first harness/config changes even though they made the published leak count
  worse.
- When touching the next leak path, cross-check against:
  - `infer/src/pulse/PulseSummary.ml`
  - `infer/src/pulse/PulseAbductiveDomain.ml`
  - `infer/src/pulse/PulseCallOperations.ml`
  - `infer/src/pulse/PulseModelsC.ml`

Largest current authoritative diffs:

- NPE under: `memory_leak.c` (-1)
- NPE over: `angelism.c` (+1), `nullptr.c` (+2), `nullptr_more.c` (+2), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2)
- UAF parity is exact

### Still-open structural hypothesis

Rust summary application still stores `PrePost.formals` as formal stack addresses and uses the
explicit `Step 1a` dereference workaround in `interproc.rs`.

This may still be correct enough for current parity work, but it remains the likeliest deeper
interproc mismatch if the next targeted bug points back into summary materialization.

## Most Likely Next Steps

1. Keep the correctness-first fixes even where totals are still worse than OCaml.
2. Compare Rust leak filtering/reachability against OCaml on `memory_leak.c`, especially
   pointer-arithmetic and array-free paths.
3. Re-run the ignored sweep after each correctness change and update `TODO.md` / `STATUS.md` only
   from the new authoritative counts.

## Useful Commands

Single-file Rust run on a C test:

```bash
cargo run -p infer-rs -- --pulse-only --results-dir /tmp/debug/infer-out -o /tmp/debug/out -- \
  clang -c /Users/mtrojer/infer-rs/infer/tests/codetoanalyze/c/pulse/lists.c
```

OCaml summary dump:

```bash
infer -j 1 --pulse-only -o /tmp/debug_out -- clang -c file.c
infer debug -j 1 --dump-json-summaries -o /tmp/debug_out
```

## Catch-Up Checklist

If resuming after compaction:

1. Read `CLAUDE.md`
2. Read this file
3. Check `git status`
4. Re-open:
   - `crates/pulse/src/summary.rs`
   - `crates/pulse/src/formula/mod.rs`
   - `crates/pulse/src/interproc.rs`
   - `crates/pulse/src/abductive.rs`
   - `infer/src/pulse/PulseAbductiveDomain.ml`
   - `infer/src/pulse/PulseInterproc.ml`
   - `infer/src/pulse/PulseFormula.ml`

## Active Work: Unified Cross-File Analysis

- Goal:
  parse multiple `.sil` files in parallel, then analyze one merged program so Pulse summaries flow
  across file boundaries.

- Implemented so far:
  - `Cfg::merge(&mut self, other: Cfg)`
  - `Tenv::merge(&mut self, other: Tenv)`
  - `ondemand::runner::run_inter_merged`
  - CLI split into `parse_file(...)` + merged analysis over successful parses
  - CLI regression test proving caller/callee cross-file Pulse propagation

- Important merge conclusion:
  OCaml's full merge machinery is broader because of its capture/database architecture, but the core
  need is not a SQLite artifact. Rust now also merges per-file `Cfg`/`Tenv` values in-memory after
  parallel parse, so duplicate typenames are real here too. Blind `HashMap::extend` makes duplicate
  type handling order-dependent.

- Current Rust stance:
  keep the semantic part that prevents information loss when multiple units contribute the same
  type; do not cargo-cult every OCaml merge corner unless Rust starts producing the same duplicate
  shapes.

- Validation status:
  - `cargo test -p sil --lib` passed after switching `Tenv::merge` away from raw overwrite
  - `cargo test -p ondemand --lib` passed
  - `cargo test -p infer-rs --test cli_tests test_multiple_files_unify_cross_file_pulse_analysis`
    passed

## 2026-04-09 Pulse Parity Checkpoint

- Current authoritative specialization comparator is still:
  - `Matching: 13`
  - `Differences: 8`
  - command:
    - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`

- Safe changes kept in the worktree:
  - `crates/pulse/src/transfer.rs`
    - recoverable load/store invalid-access paths now stop instead of exporting
      `ContinueProgram + AbortProgram`
    - targeted tests still pass, but this does not move `specialization.c`
  - `crates/pulse/src/models/c.rs`
    - recoverable C-model invalid accesses now stop instead of exporting a normal continue
    - added `test_double_free_stops_without_continue`
    - targeted tests still pass, but this also does not move `specialization.c`

- Important failed experiment:
  - broad checker-side recovery of non-exit latent invalid accesses when a normal exit path also
    exists
  - this did reproduce the missing direct-formal-read shape, but it badly regressed summary parity
    by surfacing extra latent summaries in `test_alias`, `test_unalias`, caller wrappers, and
    recursion cases
  - that experiment was reverted

- Focused regression kept as documentation:
  - `crates/pulse/src/checker.rs`
  - ignored test:
    - `test_normal_exit_keeps_non_exit_latent_abort`
  - purpose:
    - capture the still-missing case where a direct formal read should stay latent even though
      another path reaches the exit
  - current status:
    - ignored on purpose because the obvious checker-side fix is not correct yet

- OCaml cross-check from `/tmp/spec-ocaml.pzUKtI/out/all_summaries.json`:
  - `may_double_free_if_alias`
    - exactly 2 `LatentInvalidAccess` summaries from the read sites at lines `79` and `80`
    - plus 1 `ContinueProgram`
  - `test_alias`
    - only `ContinueProgram`
    - no latent invalid-access summaries from the store/write paths
  - `test_unalias`
    - same story as `test_alias`

- Best next step from here:
  - do not retry broad checker non-exit recovery
  - instead trace why Rust still keeps the `may_double_free_if_alias` null-read paths inside
    `ContinueProgram` summaries at all
  - likely places:
    - `crates/pulse/src/operations.rs`
    - `crates/pulse/src/transfer.rs`
    - `crates/pulse/src/summary.rs`
  - question to answer next:
    - where does the null read stop get lost before summary export, given that `load n6` /
      `load n4` should be fatal in Rust too

## 2026-04-10 PotentialInvalidAccessSummary Checkpoint

- Current authoritative specialization comparator is still:
  - `Matching: 13`
  - `Differences: 8`
  - command:
    - `cargo test -q -p pulse --test end_to_end test_summary_comparison_specialization_main -- --ignored --nocapture`

- Latest Rust changes on this line of investigation:
  - `crates/pulse/src/summary.rs`
    - ContinueProgram exit states can now be converted into
      `LatentInvalidAccess` during summary creation when a caller-controlled
      `must_be_valid` address is known zero in the normalized summary state
    - cross-ref:
      - OCaml `PulseAbductiveDomain.Summary.of_post`
      - OCaml `PulseSummary.exec_summary_of_post_common`
      - specifically the `PotentialInvalidAccessSummary` path
    - selected latent addresses now prefer source-location order over raw
      internal timestamps, since the desired behavior is "first source access
      wins"
    - selected latent addresses also drop their synthetic
      `Invalid(ConstantDereference(0))` attr on the chosen summary path
  - `crates/pulse/tests/end_to_end.rs`
    - `test_debug_specialization_summary` now prints raw
      `may_double_free_if_alias` pre/posts too, which was necessary to inspect
      the remaining extra latent summary shape directly

- What changed in behavior:
  - `may_double_free_if_alias` is no longer `4 x ContinueProgram`
  - current Rust raw main summary is now:
    - `LatentInvalidAccess` with `cond:v6 = 0`
    - `LatentInvalidAccess` with `cond:0 < v6` and `cond:v3 = 0`
    - `LatentInvalidAccess` with `cond:0 < v3` and `cond:v6 = 0`
    - `ContinueProgram` with `cond:0 < v3` and `cond:0 < v6`
  - OCaml still wants:
    - latent `x == 0`
    - latent `x > 0 && y == 0`
    - continue `x > 0 && y > 0`

- Important conclusion from the raw dump:
  - summary-side "look at the final normalized state and pick a zero
    `must_be_valid` address" gets closer, but it is not enough to reproduce
    OCaml's exact latent choice
  - the remaining extra latent is not just a dedup problem; it reflects that
    Rust still does not know which `EqZero` became newly derivable first during
    summary simplification
  - this now looks like a lower-level formula / new-equality issue, not
    something that should be papered over with more post-summary trimming

- Best next step from here:
  - inspect `crates/pulse/src/formula/mod.rs` and `crates/pulse/src/formula/phi.rs`
    with the OCaml cross-ref:
    - `infer/src/pulse/PulseAbductiveDomain.ml`
      - `filter_for_summary`
      - `incorporate_new_eqs`
    - goal:
      - expose the Rust analogue of OCaml's newly derived `EqZero` list during
        summary simplification, then drive potential-invalid-access selection
        from that ordered signal instead of from the final normalized state

## 2026-04-10 Direct-Formal Ordering Fix

- The previous "ordered new_eqs during summary simplify" hypothesis was wrong
  for the checked-in OCaml source in this workspace:
  - `infer/src/pulse/PulseFormula.ml` currently returns `RevList.empty` from
    `simplify`
  - the real active bug was lower-level and Rust-specific:
    - `MustBeValid` summary attrs were all being stamped with timestamp `0`
    - direct-formal latent shaping in `crates/pulse/src/summary.rs` then
      compared only raw `.sil` locations, which are not stable source-order
      proxies

- Correctness fix landed in Rust:
  - `crates/pulse/src/abductive.rs`
    - `mark_must_be_valid_at` and `mark_must_be_initialized_at` now allocate
      monotonic per-state timestamps instead of hardcoding `0`
  - `crates/pulse/src/summary.rs`
    - direct-formal latent normalization now compares
      `(MustBeValid timestamp, location)` rather than location alone
    - latent invalid-access pre/posts now:
      - require earlier direct-formal accesses to stay non-null on the latent
        path
      - forget later direct-formal pure constraints when exporting an earlier
        latent access
    - this normalization now runs for all latent invalid-access pre/posts, not
      only summary-synthesized `PotentialInvalidAccessSummary` cases
  - `crates/pulse/src/formula/mod.rs`
  - `crates/pulse/src/formula/phi.rs`
    - added a targeted "forget constraints involving these values" helper so
      summary shaping can drop later pure guards without lying about heap shape

- Verification:
  - unit tests added/passing:
    - `test_direct_formal_ordering_prefers_timestamp_over_location`
    - `test_potential_invalid_access_requires_earlier_direct_formals_nonzero`
    - `test_potential_invalid_access_forgets_later_direct_formal_constraints`
    - `test_forget_constraints_involving_drops_conditions_and_phi_facts`
  - specialization raw dump now shows the desired `may_double_free_if_alias`
    main summary shape:
    - latent `x == 0`
    - latent `x > 0 && y == 0`
    - continue `x > 0 && y > 0`

- Current authoritative comparator is still:
  - `Matching: 13`
  - `Differences: 8`
  - but the `may_double_free_if_alias` diff is narrower now:
    - the extra latent branch is gone
    - remaining delta in that procedure is down to latent diagnostic payload /
      attr / phi parity, not disjunct-count or guard-ordering parity

- Most realistic next step:
  - stay on the specialization cluster, but move from disjunct-ordering to
    payload parity:
    - latent-invalid-access diagnostic payload in summaries
    - `Initialized` attr retention on latent paths
    - positive-guard representation (`eq ... = 1` vs `0 < ...`)
