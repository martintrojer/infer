# scout_eqzero_potential_invalid_access_summary_sideband scope note

Date: 2026-05-15
Workspace: /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-2
HEAD: da98dd6b09

## Baseline triage

Capped scoped triage command:

```bash
cd infer-rs
ulimit -v 8388608
INFER_RS_C_TRIAGE_FILES=latent.c,specialization.c \
RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 \
timeout 180 cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture
```

Result at scout HEAD: latent.c matching=10 diffs=4, specialization.c matching=20 diffs=1.
Target residuals observed:
- latent.c deref_then_free_then_deref_bad: Rust main[1] extra `x.*:[Initialized, Invalid(ConstantDereference(0)), WrittenTo]` plus diagnostic; OCaml has `x.*:[Initialized, WrittenTo]`.
- specialization.c may_double_free_if_alias: Rust 4 main rows vs OCaml 3; two null direct-formal paths survive as ContinueProgram / materialized Invalid(0) instead of OCaml PotentialInvalidAccessSummary -> LatentInvalidAccess.

## OCaml mechanism

OCaml separates formula EqZero discovery from invalidation materialization:

- `PulseFormula.ml`: `type new_eq = EqZero of Var.t | Equal of Var.t * Var.t`. Formula normalizer/interval incorporation returns `new_eqs`; `PulseFormula.simplify` also returns `(formula, live_via_arithmetic, new_eqs)`.
- `PulseAbductiveDomain.ml` inner `incorporate_new_eqs astate new_eqs` returns `Sat (astate, potential_invalid_access_opt)`:
  - `Equal(v1,v2)`: substitute or Unsat.
  - `EqZero v` + stack allocated: Unsat.
  - `EqZero v` + heap allocated: if pre attrs have MustBeValid, return sideband `Some(v, (trace, reason_opt))`; if not, Unsat/drop.
  - `EqZero` not allocated: continue.
  It does **not** add `Invalid(ConstantDereference(0))` in the MustBeValid case.
- ordinary outer `AbductiveDomain.incorporate_new_eqs new_eqs astate` maps the sideband to `Error(PotentialInvalidAccess ...)`.
- `filter_for_summary` returns `new_eqs` from `PulseFormula.simplify`; `Summary.of_post_` immediately calls inner `incorporate_new_eqs`, and a sideband becomes `Error(PotentialInvalidAccessSummary ...)`.
- `PulseSummary.ml`/`PulseReport.ml` convert PotentialInvalidAccessSummary with no existing Invalid attr into `Stopped (LatentInvalidAccess ...)`, replacing the Continue summary row. If an Invalid attr already exists, report AccessToInvalidAddress.
- `PulseInterproc.ml` uses the same `AbductiveDomain.incorporate_new_eqs` when importing callee formula and only updates `rev_subst` for `Equal`, not for `EqZero`.

`PotentialInvalidAccess` / `PotentialInvalidAccessSummary` are not `PulseAttribute` variants in OCaml; they are AccessResult/error sidebands.

## Rust current state

- `formula/phi.rs` already has `NewEq::EqZero` and `NewEq::Equal`.
- `abductive.rs::incorporate_new_eqs` currently materializes `Attribute::Invalid(Invalidation::ConstantDereference(0), dummy history)` for `EqZero` on any heap-allocated repr. This is the local/model bug.
- `abductive.rs` already has imported sideband `ImportedFormulaEffect::{Sat, PotentialInvalidAccess(AbstractValue)}` used only by `apply_formula_result_for_summary_import`.
- `interproc.rs::translate_formula` uses that imported sideband and snapshots `stack_allocated_before_call` / `heap_allocated_before_call` before post application to approximate OCaml's materialize_pre -> conjoin_callee_arith -> apply_post order.
- `summary.rs` has a heuristic `PotentialInvalidAccessSummaryCandidate` pipeline. `normalize_with_summary_info` scans `post.must_be_valid` with `Formula::is_known_zero_for_summary` before `simplify_for_summary_with_witness_targets`, then `potential_invalid_access_from_normalized_continue_pre_post` may convert Continue -> LatentInvalidAccess. This is not equivalent to OCaml's exact `new_eqs` return path.
- `models/c.rs::free` null branch calls `AbductiveDomain::and_condition_direct(x == 0)`, which uses ordinary EqZero and currently persists Invalid(0).

## Design conclusion

This is genuinely multi-day / multi-task, not one tight 1-day surgery:
1. local/model EqZero sideband is a contained small/medium task and directly fixes free(NULL) attr materialization.
2. summary `filter_for_summary`/`Summary.of_post` exact `new_eqs` sideband needs formula API changes and summary-row rewiring.
3. imported EqZero already exists but must be unified after the above to prevent divergent EqZero policies.
4. specialization.c may_double_free_if_alias also depends on existing apply_post/record_post_for_address phased work; do not subsume it into EqZero tasks.

## Filed tasks / edges

New tasks:
- `cluster_eqzero_local_potential_invalid_access_sideband` (0.75d)
- `cluster_eqzero_summary_of_post_new_eqs_sideband` (1.0d)
- `cluster_eqzero_interproc_sideband_unification` (0.75d)

Edges added:
- `cluster_eqzero_local_potential_invalid_access_sideband` -> `cluster_eqzero_summary_of_post_new_eqs_sideband`
- `cluster_apply_post_phase3_recursive_left_biased_edges` -> `cluster_eqzero_summary_of_post_new_eqs_sideband`
- `cluster_apply_post_phase4_materialize_pre_aliases` -> `cluster_eqzero_summary_of_post_new_eqs_sideband`
- `cluster_eqzero_summary_of_post_new_eqs_sideband` -> `cluster_eqzero_interproc_sideband_unification`
- `cluster_apply_post_phase4_materialize_pre_aliases` -> `cluster_eqzero_interproc_sideband_unification`
- `cluster_eqzero_summary_of_post_new_eqs_sideband` -> `cluster_specialization_may_double_free_summary_surface`
- `cluster_eqzero_interproc_sideband_unification` -> `cluster_specialization_may_double_free_summary_surface`

Existing `cluster_latent_record_post_for_address_porting` is not subsumed. It has already been split into apply_post phases; EqZero tasks edge into phase3/phase4 as adjacent dependents.
