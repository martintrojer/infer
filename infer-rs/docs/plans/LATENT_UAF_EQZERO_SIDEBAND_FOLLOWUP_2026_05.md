# latent_use_after_free after EqZero sideband — scout follow-up

Date: 2026-05-18  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-2`  
Baseline HEAD: `56c98117b5` (`Carry local EqZero invalid accesses in sideband`)

## Question

Check whether the local EqZero sideband consume/strip logic from `56c98117b5`
accidentally clears the imported/stored `Invalid(ConstantDereference(42))`
attribute on the `latent_use_after_free` import path.

## Result

No. The residual is not an `Invalid(42)` stripping bug. The ordinary imported
post attribute path still preserves concrete non-zero constant invalidations:

- `interproc.rs::apply_summary_with_aliasing` Step 5 iterates callee post attrs
  and calls `caller_state.post.attrs.add_one(caller_addr,
  translate_attribute(...))`.
- `interproc.rs::translate_attribute` preserves `Attribute::Invalid` by cloning
  the invalidation and history.
- The EqZero sideband consume path in `models/c.rs::free` only clears
  `pending_invalid_accesses` on `free(NULL)`; it does not touch ordinary
  `Attribute::Invalid` entries.
- `summary.rs::drop_selected_null_invalidation` removes only null-deref invalids
  from the selected invalid address. It does not remove the stored payload's
  non-zero `Invalid(ConstantDereference(42))`.

Observed Rust debug summaries at `56c98117b5` confirm preservation:

- `deref_then_free_then_deref_bad` exports the desired local EqZero shape:
  `LatentInvalidAccess` has no `Invalid(0)` on `x.*`, while its payload value
  still has `Invalid(ConstantDereference(42))`.
- `manifest_use_after_free` imports a continuing callee path with the payload
  `Invalid(ConstantDereference(42))` intact.
- `latent_use_after_free` itself has continuing rows with payload
  `Invalid(ConstantDereference(42))` intact.

The remaining `latent_use_after_free` C-summary mismatch is instead that Rust
fails to export OCaml's `LatentInvalidAccess` row for the zero-cleanup shape and
keeps/matches it as a `LatentAbortProgram`/`ContinueProgram` shape. The
`Invalid(42)` attr is absent from the expected latent row because Rust did not
construct that row, not because an exported/imported row lost the attr.

## Attempted narrow fix and revert rationale

I tried one narrow summary-side relaxation: allow the
`latent_invalid_access_has_only_path_local_conditions` direct-formal cleanup
exception when the selected zero address's pointee has an ordinary `Invalid`
attr (the `42` payload), instead of requiring a visible selected-address
`Invalid(0)`.

That made the heuristic too broad. It also admitted the independent-branch
`FN_nonlatent_use_after_free_bad{,2}` shapes as `LatentInvalidAccess` because
those rows also store a `42` payload with `Invalid(ConstantDereference(42))`.
Scoped `latent.c` triage regressed from `10/4` to `9/5`, so the attempt was
reverted.

This distinguishes the root cause from attr stripping: the stored payload attr
is a necessary fact but not a sufficient discriminator for OCaml's
`PotentialInvalidAccessSummary` row.

## Actual root cause / needed surgery

The residual needs exact summary-of-post EqZero sideband provenance rather than
another attr-based heuristic.

OCaml's `PotentialInvalidAccessSummary` is driven by `filter_for_summary` /
`Summary.of_post_` consuming formula `new_eqs` and carrying a non-attribute
potential-invalid-access sideband. That sideband records which `EqZero +
MustBeValid` obligation produced the latent invalid access. Rust's current
summary export approximates this with:

- `AbductiveDomain::pending_invalid_accesses` for local EqZero;
- `summary_eq_zero_must_be_valid` discovered by scanning known-zero
  `must_be_valid` values before summary simplification;
- direct-formal condition-depth/path-local filters in `summary.rs`.

The approximation cannot currently distinguish the valid
`latent_use_after_free` zero-cleanup latent-invalid row from the invalid
`FN_nonlatent_use_after_free_bad{,2}` independent-branch rows using only the
surviving post attrs. Loosening the path-local filter based on payload
`Invalid(42)` catches both.

Follow-up should coordinate with `cluster_eqzero_summary_of_post_new_eqs_sideband`
(worker-1) and thread the exact summary `new_eqs`/`PotentialInvalidAccessSummary`
source through the candidate, then use that provenance in
`normalize_direct_formal_latent_invalid_access_shape` /
`latent_invalid_access_has_only_path_local_conditions` instead of consulting
ordinary `Invalid(42)` attrs.

## Measurements at no-fix close

Commands run from `infer-rs` with the documented in-process cap for triage:

```bash
ulimit -v 8388608
INFER_BIN=../infer/bin/infer INFER_RS_C_TRIAGE_FILES=latent.c \
  RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 timeout 180 \
  cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture
```

Result: `latent.c matching=10 diffs=4`.

Two e2e pins:

```bash
RUST_TEST_THREADS=1 cargo test -p pulse --test end_to_end \
  test_e2e_deref_then_free_then_deref_keeps_npe_latent -- --nocapture
RUST_TEST_THREADS=1 cargo test -p pulse --test end_to_end \
  test_e2e_latent_cycle_summary_shapes_match_ocaml_subset -- --nocapture
```

Both passed.

Scoped guard triage:

```bash
ulimit -v 8388608
INFER_BIN=../infer/bin/infer \
INFER_RS_C_TRIAGE_FILES=specialization.c,latent.c,memory_leak.c,funptr.c,interprocedural.c \
RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 timeout 240 \
cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture
```

Results held:

- `specialization.c`: `20/1`
- `latent.c`: `10/4`
- `memory_leak.c`: `38/8`
- `funptr.c`: `24/4`
- `interprocedural.c`: `16/1`

ValueHistory/cell-id provenance pins:

```bash
RUST_TEST_THREADS=1 cargo test -p pulse --lib \
  test_merge_caps_path_count_and_keeps_invalid_history -- --nocapture
RUST_TEST_THREADS=1 cargo test -p pulse --lib \
  test_apply_summary_restores_post_edge_history_from_callee_pre_cell_id -- --nocapture
```

Both passed.
