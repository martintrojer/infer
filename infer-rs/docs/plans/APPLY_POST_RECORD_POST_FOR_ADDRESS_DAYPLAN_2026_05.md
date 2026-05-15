# apply_post / record_post_for_address Rust port day plan

Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak`
HEAD while scoping: `5745f79996`

Umbrella: `cluster_latent_record_post_for_address_porting`.

## OCaml reference surface

Primary reference: `infer/src/pulse/PulseInterproc.ml`:

- `visit` / `hist_map` construction during pre-materialization.
- `subst_find_or_new`, `call_state_subst_find_or_new`, `translate_access_to_caller`.
- `materialize_pre_from_address`, `materialize_pre_from_actual`, globals/array-index materialization, and `materialize_pre`.
- `delete_edges_in_callee_pre_from_caller`.
- `record_post_cell` and `record_post_for_address`.
- `apply_post_from_callee_pre`, `apply_post_from_callee_post`, `apply_post`.
- `AliasingWithAllAliases` conversion after successful materialization discovered heap-path aliases.

Supporting reference:

- `infer/src/pulse/PulseAbductiveDomain.ml`: `filter_for_summary`, `Summary.of_post_`, `incorporate_new_eqs`, and the EqZero → `PotentialInvalidAccessSummary` flow. This is adjacent to, but should not be duplicated with, worker-2's `scout_eqzero_potential_invalid_access_summary_sideband`.
- `infer/src/pulse/PulseValueHistory.ml`: `CellId`, `FromCellIds`, `from_cell_id`, `get_cell_ids`, `of_cell_ids_in_map`, and cell-id-preserving history constructors.

## Rust current surface

Primary files:

- `infer-rs/crates/pulse/src/interproc.rs`: current `apply_summary_with_aliasing`, `materialize_pre`, flat pre/post `apply_post_cell` loops, formula import, return/diagnostic translation.
- `infer-rs/crates/pulse/src/value_history.rs`: eager path-set history without OCaml `CellId` / `FromCellIds` equivalent; recently bounded by commit `1319de4f19`.
- `infer-rs/crates/pulse/src/base_memory.rs`: edge payload histories are available via `ValueWithHistory`; alias-contradiction pruning recently exists in `subst_var_or_unsat`.

## Residuals gated

- `latent.c` cycle-cursor cases: `crash_after_one_node_bad`, `crash_after_two_nodes_bad`, `FN_crash_after_six_nodes_bad` and surrounding cursor-cycle summary shape.
- `specialization.c`: only current residual `may_double_free_if_alias` / `call_may_double_free_if_alias_bad`.
- Possibly `latent.c`: `FN_nonlatent_use_after_free_bad{,2}` second-order latent-invalid surface.
- Possibly `memory_leak.c`: mutual-recursion heap self-cycle shapes.

Hard guards for every phase:

- Preserve `specialization.c` summary harness baseline: `20/1`, only `may_double_free_if_alias` may remain until the full port plus EqZero sideband lands.
- Preserve/measure `latent.c` current C-triage baseline from refreshed docs: `10/4` unless a phase explicitly improves it.
- Preserve existing unit/e2e pins around direct cycle edges and latent cycle shapes: `test_apply_summary_preserves_direct_callee_cycle_post_edge`, `test_e2e_latent_cycle_summary_shapes_match_ocaml_subset`, direct-formal latent-invalid tests, and alias-specialization tests.
- Do not reintroduce eager history blow-ups; any `ValueHistory` cell-id work must remain bounded like `1319de4f19`.

## Filed phase tasks

### Phase 1 — `cluster_apply_post_phase1_hist_map_cell_id_restoration` (0.75d)

Scope:

- Rust: `infer-rs/crates/pulse/src/value_history.rs`, `base_memory.rs`, `interproc.rs` (history import/replay only).
- OCaml refs: `PulseValueHistory.CellId`, `FromCellIds`, `from_cell_id`, `get_cell_ids`, `of_cell_ids_in_map`; `PulseInterproc.visit` hist_map update; `record_post_cell` history selection; `read_return_value` history restoration.

Plan:

- Add an OCaml-like bounded cell-id provenance layer to Rust histories, or an equivalent sideband in `interproc.rs`, so histories attached to callee pre cells can be restored when post edges/return values are replayed.
- Record `cell_id -> caller ValueHistory` during pre materialization for each visited callee pre cell.
- In post replay, if callee post history references cell IDs, use the mapped caller history instead of falling back to fresh/epoch/formal history.

Expected impact:

- Makes recursive post replay trace-stable and unblocks cycle-cursor latent rows whose caller-facing history currently collapses to short roots.
- Foundation for `latent.c` cycle-cursor residuals and `specialization.c may_double_free_if_alias` trace/latent classification.

### Phase 2 — `cluster_apply_post_phase2_delete_edges_in_callee_pre_from_caller` (0.75d)

Scope:

- Rust: `infer-rs/crates/pulse/src/interproc.rs` (`translate_access`, `apply_post_cell`, caller edge deletion), possibly `base_memory.rs` helper for left-biased merge with histories.
- OCaml refs: `PulseInterproc.delete_edges_in_callee_pre_from_caller`, `translate_access_to_caller`, `record_post_cell`.

Plan:

- Replace/verify the current per-access removal in `apply_post_cell` with a direct OCaml model: translate all callee pre accesses to caller accesses with the same substitution semantics as `translate_access_to_caller`, delete exactly those accesses from caller post edges, and return the updated substitution plus `post_edges_minus_pre`.
- Preserve array-index translation behavior and recency semantics in `BaseMemory::Edges`.

Expected impact:

- Correct strong updates when a callee pre edge disappears or is overwritten in post.
- Unblocks heap shape parity for `latent.c` cursor cycles and self-cycle leak/mutual-recursion surfaces.

### Phase 3 — `cluster_apply_post_phase3_recursive_left_biased_edges` (1.0d)

Scope:

- Rust: `infer-rs/crates/pulse/src/interproc.rs` (`apply_summary_with_aliasing`, new/ported `record_post_for_address`, `apply_post_from_callee_pre`, `apply_post_from_callee_post`, `apply_post_cell`); `base_memory.rs` if an explicit `union_left_biased` helper is needed.
- OCaml refs: `PulseInterproc.record_post_for_address`, `record_post_cell`, `call_state_subst_find_or_new`, `apply_post_from_callee_pre`, `apply_post_from_callee_post`, `BaseMemory.Edges.union_left_biased`.

Plan:

- Restructure the current flat Rust pre.heap/post.heap loops into OCaml's recursive `record_post_for_address` traversal.
- Seed recursion from mapped callee-pre roots first, then traverse unmapped post roots.
- Preserve a visited set per apply-post traversal.
- For each post cell, combine `translated_post_edges` with `post_edges_minus_pre` using left bias for callee-translated post edges, with histories from phase 1.
- Ensure recursive traversal creates fresh caller values with OCaml-like `subst_find_or_new` defaults, without canonicalizing away direct callee cycle targets before replay.

Expected impact:

- Main unblocker for `latent.c` cycle-cursor crash_after_{one,two,six}_nodes_bad state-shape parity.
- Main apply-post half of `specialization.c may_double_free_if_alias` and possible `memory_leak.c` heap self-cycle parity.

### Phase 4 — `cluster_apply_post_phase4_materialize_pre_aliases` (0.75d)

Scope:

- Rust: `infer-rs/crates/pulse/src/interproc.rs` (`materialize_pre`, alias tracking, `find_aliasing_contradiction`, `ApplySummaryOutcome.alias_specialization`); `base_memory.rs` only if alias graph helpers are needed.
- OCaml refs: `PulseInterproc.visit`, `add_alias`, `AliasingWithAllAliases`, `apply_summary` alias rejection path, `materialize_pre_from_address` / globals / array indices.

Plan:

- Complete materialize-pre visit parity for supported heap-path aliases: track rev-subst caller-to-callee bindings like OCaml, record all supported heap-path alias groups instead of returning only a first conflict, and return `AliasingWithAllAliases`-equivalent specialization data.
- Keep current smaller contradiction pruning from worker-2, but ensure supported aliases are specialized rather than silently merged or rejected without complete alias groups.
- Validate against cycle aliases and specialization alias tests.

Expected impact:

- Finalizes alias-specialized caller contexts needed after phases 1-3.
- Improves `specialization.c may_double_free_if_alias` and guards against regressions in supported cycle aliases.

## Edges

Linear phase ordering filed:

```sh
mu task block cluster_apply_post_phase2_delete_edges_in_callee_pre_from_caller --by cluster_apply_post_phase1_hist_map_cell_id_restoration -w infer-rs
mu task block cluster_apply_post_phase3_recursive_left_biased_edges --by cluster_apply_post_phase2_delete_edges_in_callee_pre_from_caller -w infer-rs
mu task block cluster_apply_post_phase4_materialize_pre_aliases --by cluster_apply_post_phase3_recursive_left_biased_edges -w infer-rs
```

Coordination note: worker-2's EqZero sideband scout (`scout_eqzero_potential_invalid_access_summary_sideband`) is adjacent, not duplicated here. If worker-2 files an implementation task for `PulseAbductiveDomain.filter_for_summary` / `Summary.of_post_` EqZero sideband, edge that task with phase 3/4 rather than creating another apply-post phase.
