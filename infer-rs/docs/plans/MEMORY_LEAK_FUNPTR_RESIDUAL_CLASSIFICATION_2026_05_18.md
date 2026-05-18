# memory_leak.c + funptr.c residual classification (2026-05-18)

Scout task: `scout_memory_leak_funptr_residuals_combined_dayplan`  
Workstream: `infer-rs`  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak`  
Code baseline used for classification: `origin/infer-rs` / `671f0528f6` (`docs: status after EqZero Phase C + specialization 21/0 + deep-port scoping kickoff`).

Note: the freshly recreated workspace initially sat at `11ef79a399` and reproduced the stale `memory_leak.c` 38/8 surface. I fetched/checked out `origin/infer-rs` read-only; that matches the requested current 40/6 + 25/3 baseline.

## Commands / artifacts

Scoped triage:

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak/infer-rs
ulimit -v 8388608
RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 \
INFER_RS_C_TRIAGE_FILES=memory_leak.c,funptr.c \
timeout 240 cargo test -p pulse --release --test end_to_end \
  test_summary_comparison_c_triage -- --ignored --nocapture \
  2>&1 | tee /tmp/memory_leak_funptr_triage_origin_infer_rs.log
```

Result:

- `memory_leak.c`: `matching=40 diffs=6 ocaml_only=0 rust_only=5`
- `funptr.c`: `matching=25 diffs=3 ocaml_only=0 rust_only=0`

OCaml direct artifacts and per-proc Rust traces:

- OCaml summaries: `/tmp/mlfunptr_scout/{memory_leak,funptr}/ocaml-out/all_summaries.json`
- Textual: `/tmp/mlfunptr_scout/{memory_leak,funptr}/{memory_leak,funptr}.sil`
- Rust `--debug-level-analysis 2` per-proc traces: `/tmp/mlfunptr_scout/rust_traces/*.err`

Rust trace command shape:

```sh
timeout 45 target/release/infer-rs --pulse-only -j 1 \
  --pulse-max-heap-mb 2048 --pulse-max-wall-secs 30 \
  --debug-level-analysis 2 --procedures-filter '<proc>' \
  --capture-textual /tmp/mlfunptr_scout/<file>/<file>.sil
```

OCaml cross-reference files read during classification:

- `infer/src/pulse/PulseModelsC.ml` (`alloc_common_dsl`, `realloc_common`, `call_c_function_ptr`)
- `infer/src/pulse/PulseCallOperations.ml` (`iter_call`, dynamic-type specialization, `on_recursive_call`, `pulse_force_continue` + `NonDisjDomain.astate_is_bottom` gate)
- `infer/src/pulse/PulseSpecialization.ml` (`initialize_heap_path`, dynamic type application)
- prior scout docs: `ARRAY_ACCESS_CONST_NULL_COALESCING_2026_05.md`, `CONST_ZERO_REPR_DESIGN_EVIDENCE_2026_05.md`, and current NonDisj/cycle phase task notes.

## Current diffs by procedure

### memory_leak.c (40/6)

1. `alias_ptr_free_ok`
2. `alloc_ref_counted_arith_ok`
3. `free_all_in_array`
4. `interproc_mutual_recusion_leak`
5. `mutual_recursion`
6. `mutual_recursion_2`

Rust-only declaration summaries remain unchanged and are not counted among the 6 canonical procedure diffs: `a_malloc`, `my_free`, `my_malloc`, `my_realloc`, `realloc`.

### funptr.c (25/3)

1. `apply_funptr_with_intptrptr_and_after`
2. `conditionnaly_apply_funptr_with_intptrptr`
3. `funptr_conditional_call_bad`

The earlier callback closure rows and top-level specialized caller rows are closed at this baseline.

## Per-diff classification

| File | Procedure | Current diff shape | OCaml/Rust trace signal | Classification | Follow-up |
|---|---|---|---|---|---|
| `memory_leak.c` | `alias_ptr_free_ok` | Two continuing rows differ in branch/path-condition pairing. OCaml rows keep `out -*-> out.*`, `out.* != 0`, `flag.* = 0`, plus conditions like `0 < v1` / `out.* != v1`; Rust pairs `flag.* != 0` / `is_int(flag.*)` against the other row and drops the `out` pre-edge in one row. | Local trace follows the `flag ? malloc : out` split and the `if (y && y != out) free(y)` prune. The residual is not array, funptr, NonDisj, or latent cycle cursor. It looks like summary-condition / branch-prune alpha pairing for aliased pointer locals. | **(b) needs deeper scout**: small, single-proc scout before editing because this could be comparator pairing or real summary-condition export. | `cluster_memory_leak_alias_branch_prune_summary` |
| `memory_leak.c` | `alloc_ref_counted_arith_ok` | Only affine phi presentation differs: OCaml uses temp equalities (`return.* = v2 + 2`, `size.* = v3 - 4`, `0 < v1`); Rust keeps equivalent inline forms (`0 < return.* - 1`, `v3 = return.* - 1`). Heap/attrs are otherwise aligned. | Source is malloc of `size + sizeof(int)`, `*p++ = 1`, return adjusted pointer. This is summary alpha/isograph over linear arithmetic temps, not a semantic memory leak difference. | **(a) tight fix**: comparator/summary canonicalization for affine temps; 0.5d scope. | `cluster_summary_affine_temp_alpha_alloc_ref_counted` |
| `memory_leak.c` | `free_all_in_array` | Four loop rows still differ in ArrayAccess index/pointee pairing and null/free invalidation attrs. OCaml keeps `Invalid(ConstantDereference(0/1))` on the constant/index representatives and `CFree` on actual freed pointees; Rust still swaps some index/value roles and adds `cond:0 < ...` on freed pointees. | After worker-1's constant coalescing wave `allocate_all_in_array` closed; `free_all_in_array` is the residual that adds `free(array[i])` invalidation and loop fixpoint pairing. This is the same array/null representative family, now narrowed to free invalidation. | **(a) tight fix**: focus on `free()` null/success branch invalidation + ArrayAccess constant index/pointee canonicalization after const-zero coalescing; 0.75-1d. | `cluster_memory_leak_null_free_array_and_funptr_invalidation` |
| `memory_leak.c` | `interproc_mutual_recusion_leak` | Rust has extra visible `x.* -*-> x.*.*` pre/post edge and `x.*.*:[Initialized, WrittenTo]`; OCaml does not. Branch rows also differ on `x.*.data.*` null/non-null pairing. | Rust trace shows `mutual_recursion_2` calls `_mutual_recursion_` with no summary and treats it as an unknown call; `interproc_mutual_recusion_leak` then applies that fallback summary. OCaml `PulseCallOperations.on_recursive_call` plus call fallback has similar `UnknownEffect(SkippedKnownCall)` but a different visible summary heap shape. This is a recursive-call fallback surface, distinct from worker-1's latent cycle-cursor linked-list work. | **(c) gated on NonDisjDomain port/re-baseline**: recheck after `nondisj_phase5_call_apply_and_force_continue` because hidden NonDisj continuation/fallback state may change which unknown-call effects become visible; then do a narrow recursive-call fallback fix if still present. | `cluster_memory_leak_mutual_recursion_fallback_shape` blocked by `nondisj_phase5_call_apply_and_force_continue` |
| `memory_leak.c` | `mutual_recursion` | Rust exports extra `x.* -*-> x.*.*` edge and `x.*.*:[Initialized, WrittenTo]`; OCaml exports only the direct formal shape plus skipped-known-call attrs. | Same trace as above: recursive call fallback materializes/refreshes an extra dereference cell in Rust. | **(c) gated on NonDisjDomain port/re-baseline** with the `interproc_mutual_recusion_leak` cluster. | `cluster_memory_leak_mutual_recursion_fallback_shape` |
| `memory_leak.c` | `mutual_recursion_2` | Same extra `x.* -*-> x.*.*` edge and `x.*.*:[Initialized, WrittenTo]` as `mutual_recursion`. | Same recursive-call fallback surface. | **(c) gated on NonDisjDomain port/re-baseline** with the `interproc_mutual_recusion_leak` cluster. | `cluster_memory_leak_mutual_recursion_fallback_shape` |
| `funptr.c` | `apply_funptr_with_intptrptr_and_after` | Only the specialized summary for `dynamic_types: {*after: dereference_dereference_ptr, *funptr: assign_NULL}` differs: Rust has extra `ptr.*.* -*-> ptr.*.*.*` and `is_int(ptr.*.*.*)`. | Prior abort-propagation cascade is closed. The remaining row is the dynamic-type-specialized callee summary surface after applying `assign_NULL` then `dereference_dereference_ptr`. OCaml direct does not keep the trailing read/is-int edge in the same specialized summary row. | **(c) gated on NonDisjDomain phase 5 / dynamic-specialization re-baseline**: this area is tied to known-call force-continue and dynamic-type specialized stopped/continue policy. Recheck after hidden NonDisj pre/post application, then fix trailing read-edge export if still isolated. | `cluster_funptr_dynamic_specialization_residuals` blocked by `nondisj_phase5_call_apply_and_force_continue` |
| `funptr.c` | `conditionnaly_apply_funptr_with_intptrptr` | Rust has an extra specialized summary `dynamic_types: {*funptr: assign_NULL}`; OCaml only keeps the `do_nothing` dynamic-type specialization. | Source calls `funptr` only under `if (x)` then unconditionally writes `*ptr = NULL`. The extra Rust specialization is benign for issue reporting, but still counts in summary parity. It likely comes from requesting specialization from a branch-local need that OCaml suppresses/delays differently. | **(c) gated on NonDisjDomain phase 5 / then likely accept-or-tiny-fix**: do not spend before hidden continuation/specialization request policy lands. | `cluster_funptr_dynamic_specialization_residuals` |
| `funptr.c` | `funptr_conditional_call_bad` | Continuing row `post_attrs` misses OCaml `x.*:[Initialized, Invalid(ConstantDereference(0))]`; Rust keeps `x.*:[Initialized]`. | Rust trace shows the `assign_NULL` branch creates a separate zero/invalid value for `ptr`'s target; `x.*` is also known zero via the branch, but the invalidation does not coalesce onto the exported `x.*` representative. This resembles the remaining null-invalidation side of the const-zero work, not callback closure dispatch. | **(a) tight fix**: share with the `free_all_in_array` null/free invalidation follow-up; ensure equal zero values carry OCaml-shaped `Invalid(ConstantDereference(0))` attrs without reintroducing unsafe global zero unification. | `cluster_memory_leak_null_free_array_and_funptr_invalidation` |

## Follow-up tasks filed

1. `cluster_memory_leak_alias_branch_prune_summary`
   - Scope: single-proc branch/prune summary-condition pairing for `alias_ptr_free_ok`.
   - Classification: deeper scout before edit.

2. `cluster_summary_affine_temp_alpha_alloc_ref_counted`
   - Scope: affine-temp canonicalization for `alloc_ref_counted_arith_ok`.
   - Classification: tight fix, likely comparator/summary alpha normalization.

3. `cluster_memory_leak_null_free_array_and_funptr_invalidation`
   - Scope: `free_all_in_array` plus `funptr_conditional_call_bad` null/free invalidation attr coalescing.
   - Classification: tight fix.

4. `cluster_memory_leak_mutual_recursion_fallback_shape`
   - Scope: `mutual_recursion`, `mutual_recursion_2`, `interproc_mutual_recusion_leak` recursive known-call/unknown fallback visible summary shape.
   - Edge: blocked by `nondisj_phase5_call_apply_and_force_continue`.

5. `cluster_funptr_dynamic_specialization_residuals`
   - Scope: `apply_funptr_with_intptrptr_and_after` trailing read/is-int edge and `conditionnaly_apply_funptr_with_intptrptr` extra dynamic-type specialization.
   - Edge: blocked by `nondisj_phase5_call_apply_and_force_continue`.

No residual here is directly gated on worker-1's latent cycle-cursor phase tasks (`cluster_latent_cycle_phase*`): the memory recursion rows are callgraph mutual recursion, not linked-list/cursor latent invalid-access publication.

## Accepted/no-action items

- The 5 `memory_leak.c` Rust-only declaration summaries (`a_malloc`, `my_free`, `my_malloc`, `my_realloc`, `realloc`) remain a harness/declaration-summary surface outside the requested 6 procedure diffs. No new task from this scout.
- `conditionnaly_apply_funptr_with_intptrptr` may ultimately be accepted as benign over-specialization if it persists after NonDisj phase 5 and still has no issue/report impact, but keep it attached to the dynamic-specialization follow-up until that re-baseline.
