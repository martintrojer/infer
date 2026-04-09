# Debug Log

This file is for short-lived but important debugging context that should survive chat compaction.
Keep it current when the active line of investigation changes.

## Current Focus

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
