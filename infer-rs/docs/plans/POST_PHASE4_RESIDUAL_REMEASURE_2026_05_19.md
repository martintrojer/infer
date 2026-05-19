# Post-phase4 residual remeasure — C-suite scout

Task: `scout_post_phase4_residual_remeasure`  
Workstream: `infer-rs`  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak`  
HEAD: `2671a5af0f`  
Date: 2026-05-19

Scope guard: **read-only triage**. No source edits, no commits, no sweeps. Only ran scoped C triage, wrote this `/tmp` report, and filed task-graph follow-ups.

## Command / artifact

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak/infer-rs
ulimit -v 8388608
RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 \
INFER_RS_C_TRIAGE_FILES=arithmetic.c,specialization.c,latent.c,memory_leak.c,funptr.c,interprocedural.c \
timeout 300 cargo test -p pulse --release --test end_to_end \
  test_summary_comparison_c_triage -- --ignored --nocapture \
  2>&1 | tee /tmp/post_phase4_csuite_triage_2026_05_19.log
```

## Confirmed current state

| File | Matching | Diffs | Notes |
|---|---:|---:|---|
| `arithmetic.c` | 6 | 5 | plus Rust-only `random` declaration/stub |
| `specialization.c` | 21 | 0 | perfect |
| `latent.c` | 10 | 4 | unchanged count after latent phases 1-4 |
| `memory_leak.c` | 42 | 4 | plus 5 Rust-only allocation/free declaration summaries |
| `funptr.c` | 25 | 3 | unchanged count |
| `interprocedural.c` | 16 | 1 | plus Rust-only `random` declaration/stub |
| **TOTAL** | **120** | **17** | requested total confirmed |

Rust-only declaration/stub surfaces are not counted in the 17 shared-procedure diffs: `arithmetic.c::random`, `interprocedural.c::random`, and `memory_leak.c::{a_malloc,my_free,my_malloc,my_realloc,realloc}`.

## Rebaseline deltas vs original classification docs

Compared against:

- `infer-rs/docs/plans/MEMORY_LEAK_FUNPTR_RESIDUAL_CLASSIFICATION_2026_05_18.md`
- `infer-rs/docs/plans/LATENT_CYCLE_CURSOR_PORT_DAYPLAN_2026_05.md`
- `infer-rs/docs/plans/NONDISJDOMAIN_PORT_DAYPLAN_2026_05.md`

Notable shifts:

- `memory_leak.c` moved from the 2026-05-18 40/6 classification to 42/4. The prior tight residuals `alias_ptr_free_ok` and `alloc_ref_counted_arith_ok` are closed by today's fixes.
- `specialization.c` is now 21/0 and has no residuals.
- `latent.c` remains 10/4 even after latent phase 3 sideband and phase 4 hidden-nondisj work. Cycle rows are still cursor/row-shape issues; `latent_use_after_free` now looks newly tractable because explicit sideband storage exists and the remaining mismatch is row canonicalization/import ordering.
- `arithmetic.c` remains 6/5, matching the NonDisjDomain dayplan expectation before worker-2's in-flight phase 5 / phase 6 validation completes.
- `funptr.c` remains 25/3. Dynamic-specialization rows remain blocked on NonDisj phase 5/6; `funptr_conditional_call_bad` is still an isolated imported-zero invalidation candidate.
- `interprocedural.c` remains 16/1; its lone shared diff is the historical random/equality-prune `Invalid(ConstantDereference(5))` materialization surface.

## Per-residual classification

### `arithmetic.c` — 5 diffs, all still tied to NonDisjDomain phase 5/6

| Procedure | Current diff shape at HEAD | Original classification comparison | Classification | Follow-up |
|---|---|---|---|---|
| `FN_call_if_negative_then_crash_with_negative_bad` | Row 1 kind differs (`OCaml ContinueProgram`, `Rust ExitProgram`); Rust has `cond:neg(x.*) < 0`, `atom:0 < x.*`, `atom:lin(-1*x.*) < 0`; OCaml has temp equalities around `if_negative_then_crash_latent(x.*)` and `x.* = lin(-1*lin(-1*x.*,const=-1),const=-1)`. Row 2 has `cond:0 <= v1` vs `cond:0 <= neg(x.*)` plus unary-neg phi presentation mismatch. | `NONDISJDOMAIN_PORT_DAYPLAN` predicted this remains blocked until hidden non-disj call application/force-continue, with possible leftover unary-neg presentation. | **waits-on-NonDisjDomain-phase-5/6** | Existing: `nondisj_phase5_call_apply_and_force_continue` → `nondisj_phase6_arithmetic_validation_and_cleanup` → `cluster_arithmetic_residuals_after_nondisjdomain_port`. |
| `call_if_negative_then_crash_with_local_bad` | Pre/post count differs (`OCaml 3`, `Rust 2`). OCaml row 1 is `ContinueProgram`, Rust row 1 is `ExitProgram`; OCaml has an extra empty `ExitProgram` row missing in Rust. | This is the canonical arithmetic hidden-continue / force-continue residual called out in the NonDisj dayplan. | **waits-on-NonDisjDomain-phase-5/6** | Existing NonDisj phase chain. |
| `if_negative_then_crash_latent` | Kinds/count align, but condition/phi presentation differs: OCaml uses `cond:v1 < 0` and `cond:0 <= v1`; Rust uses `cond:neg(x.*) < 0`, `cond:0 <= neg(x.*)` plus `atom:lin(-1*x.*) ...`. | Dayplan says this may be a formula-presentation cleanup after NonDisj validation. | **waits-on-NonDisjDomain-phase-6**; likely tight unary-neg alpha only after phase 6 split. | Existing `cluster_arithmetic_residuals_after_nondisjdomain_port`; no duplicate task filed. |
| `return_non_negative` | Rust has extra `phi` atom `0 <= return.*`; OCaml does not expose it in the same row. | Dayplan says NonDisj port should allow removing/avoiding Rust visible non-negative compensation later. | **waits-on-NonDisjDomain-phase-6** | Existing arithmetic-after-NonDisj cluster. |
| `return_non_negative_float` | OCaml has `eq:return.*=[DivF,[Var,v1],[Const,{den:1,num:28}]]`; Rust has extra `atom:0 <= return.*` and lacks the DivF equality. | Dayplan expected float DivF/formula presentation as likely leftover after NonDisj cleanup. | **waits-on-NonDisjDomain-phase-6** | Existing arithmetic-after-NonDisj cluster. |

### `specialization.c` — 0 diffs

No residuals. Current state is `matching=21 diffs=0`.

### `latent.c` — 4 diffs

| Procedure | Current diff shape at HEAD | Original classification comparison | Classification | Follow-up |
|---|---|---|---|---|
| `FN_crash_after_six_nodes_bad` | Multiple row-kind mismatches remain: OCaml `ContinueProgram`/`LatentInvalidAccess` rows become Rust `AbortProgram`/`ContinueProgram`; Rust still exports root self-cycle shapes like `q.* -*-> q.*`, `q.*.next -*-> q.*`, extra `q.*:[Initialized, WrittenTo]`, and manifest NPE diagnostics where OCaml has latent rows. OCaml keeps longer cursor paths such as `q.*.next.*...next -*-> ...`. | Latent dayplan phases 1-4 targeted this; phase 4 landed but count did not move. The residual is still the cursor row-key/representative path surface, not now a pure force-continue gate. | **waits-on-cycle-cursor** | No new tight task; existing phase findings say deeper imported equality/pre-materialization/summary ordering is needed. |
| `crash_after_one_node_bad` | Extra Rust self-cycle/post edge `q.* -*-> q.*`; extra `q.*.*` edge on another row; pre-attrs differ (`OCaml MustBeValid`, Rust `MustBeInitialized, MustBeValid`). | Same cycle-cursor family from latent dayplan; phase2 attempted cursor replay and found issue earlier than apply_post direct cell replay. | **waits-on-cycle-cursor** | No new tight task. |
| `crash_after_two_nodes_bad` | OCaml path edge `q.*.next -*-> q.*.next.*` missing; Rust has `q.* -*-> q.*`, `q.*.next -*-> q.*`; row 1 kind `OCaml ContinueProgram`, `Rust AbortProgram`; row 2 kind `OCaml LatentInvalidAccess`, `Rust ContinueProgram`; MustBeInitialized/MustBeValid parity differs. | Same cycle-cursor/latent publication family. | **waits-on-cycle-cursor** | No new tight task. |
| `latent_use_after_free` | Four rows differ in formal/base pairing and kind: OCaml keeps `x -*-> x.*` / `x.* -*-> x.*.*` on row 0 while Rust pairs to `b.*`; OCaml `LatentAbortProgram` row becomes Rust `ContinueProgram`; OCaml `LatentInvalidAccess` row becomes Rust `LatentAbortProgram`. `Invalid(ConstantDereference(42))` is preserved but lands on different row/value pairings; CFree/null cleanup row ordering diverges. | Original dayplan said missing exact `Summary.of_post` provenance; phase3 now added an explicit sideband, but row still differs, so the remaining surface has shifted to row-key/canonicalization/import ordering. | **tight-fix-candidate** | New task filed: `cluster_latent_uaf_sideband_row_canonicalization_rebase`. |

### `memory_leak.c` — 4 diffs

| Procedure | Current diff shape at HEAD | Original classification comparison | Classification | Follow-up |
|---|---|---|---|---|
| `free_all_in_array` | Four loop rows still differ. OCaml uses `array.* -[v3]-> v4`, `v4 -*-> v3` style index/pointee pairing; Rust uses `array.* -[v4]-> v5`, `v5 -*-> v6`. Attrs differ: OCaml keeps `Invalid(ConstantDereference(0/1))` on constants/index reps and `CFree` on actual freed pointee; Rust often puts `CFree` on null-ish reps, adds `cond:0 < ...`, and swaps `eq:v1=0/1`, `eq:v4=1/0`. | Prior combined null/free task was deferred after a broad attempt. With `alias_ptr_free_ok` and `alloc_ref_counted_arith_ok` now closed, this is newly isolated and narrower than the 2026-05-18 surface. | **tight-fix-candidate** | New task filed: `cluster_memory_leak_free_all_array_free_invalidation_rebase`. |
| `interproc_mutual_recusion_leak` | Rust has extra `x.* -*-> x.*.*` pre/post edges and `x.*.*:[Initialized, WrittenTo]`. Branch rows differ around `x.*.data.*`: OCaml has initialized/direct data attrs; Rust sometimes adds `Allocated(CMalloc), Uninitialized`, `atom:0 < x.*.data.*`, or flips `x.*.data.* == 0` vs `!= 0`. | Same as original memory/funptr classification: recursive-call fallback visible summary shape, blocked by NonDisj phase 5 call application/force-continue rebaseline. | **waits-on-NonDisjDomain-phase-5/6** | Existing `cluster_memory_leak_mutual_recursion_fallback_shape`, blocked by `nondisj_phase5_call_apply_and_force_continue`. |
| `mutual_recursion` | Single row: Rust extra `x.* -*-> x.*.*` pre/post and `x.*.*:[Initialized, WrittenTo]`. | Same recursive known-call/unknown fallback shape as original. | **waits-on-NonDisjDomain-phase-5/6** | Existing blocked cluster. |
| `mutual_recursion_2` | Same as `mutual_recursion`: Rust extra `x.* -*-> x.*.*` pre/post and `x.*.*:[Initialized, WrittenTo]`. | Same recursive fallback shape. | **waits-on-NonDisjDomain-phase-5/6** | Existing blocked cluster. |

Closed since the original memory/funptr classification: `alias_ptr_free_ok` and `alloc_ref_counted_arith_ok`.

### `funptr.c` — 3 diffs

| Procedure | Current diff shape at HEAD | Original classification comparison | Classification | Follow-up |
|---|---|---|---|---|
| `apply_funptr_with_intptrptr_and_after` | Only the specialized row `dynamic_types: {*after: dereference_dereference_ptr, *funptr: assign_NULL}` differs. Rust has extra `ptr.*.* -*-> ptr.*.*.*` and `is_int(ptr.*.*.*)`. | Same as 2026-05-18 classification: dynamic-specialized callee summary trailing read/is-int edge, blocked on NonDisj phase 5/6 dynamic specialization rebaseline. | **waits-on-NonDisjDomain-phase-5/6** | Existing `cluster_funptr_dynamic_specialization_residuals`, blocked by `nondisj_phase5_call_apply_and_force_continue`. |
| `conditionnaly_apply_funptr_with_intptrptr` | Rust has extra specialized summary `dynamic_types: {*funptr: assign_NULL}`; OCaml has no matching specialized summary. | Same as original. May eventually become accepted benign over-specialization, but should be rechecked after NonDisj phase 5/6 because dynamic-type summary request/continue policy is shared. | **waits-on-NonDisjDomain-phase-5/6** | Existing dynamic-specialization cluster. |
| `funptr_conditional_call_bad` | Single main row: OCaml post_attrs include `x.*:[Initialized, Invalid(ConstantDereference(0))]`; Rust has only `x.*:[Initialized]`. | Same null/zero invalidation family as original, but now separable from memory_leak `free_all_in_array` because the prior broad task was deferred and this is a single imported-zero funptr row. | **tight-fix-candidate** | New task filed: `cluster_funptr_conditional_zero_invalidation_import_rebase`. |

### `interprocedural.c` — 1 diff

| Procedure | Current diff shape at HEAD | Original classification comparison | Classification | Follow-up |
|---|---|---|---|---|
| `test_modified_value_then_error_bad` | Single row: Rust has extra post_attr `x.*.*:[Invalid(ConstantDereference(5))]`; OCaml has no such attr. Source writes `*x = random(); if (*x == 5) { NULL deref }`. | Historical `scout_interprocedural_summary_triage_drill` classified this as the remaining branch/equality-prune constant-invalidation materialization surface after the broader 11/6 → 15/2 fix. `bug_continue_zero_formal_drops_null_invalidation` later restored interprocedural to 16/1; this row is still the final shared diff. | **tight-fix-candidate** | New task filed: `cluster_interproc_random_equality_const_invalidation`. |

## New tight-fix tasks filed

1. `cluster_memory_leak_free_all_array_free_invalidation_rebase`
   - Scope: isolate `memory_leak.c::free_all_in_array` loop ArrayAccess index/pointee pairing plus `free(array[i])` null/success invalidation placement.
   - Rationale: count has shifted to 42/4; this is now the only non-recursive memory_leak residual.

2. `cluster_funptr_conditional_zero_invalidation_import_rebase`
   - Scope: isolate `funptr.c::funptr_conditional_call_bad` imported-zero invalidation on `x.*` through function-pointer specialization.
   - Rationale: single-row diff; avoid repeating broad null/free attempt that perturbed latent rows.

3. `cluster_interproc_random_equality_const_invalidation`
   - Scope: distinguish random/unknown equality-prune from real literal invalidation / recursive unknown arithmetic so `x.*.* Invalid(ConstantDereference(5))` is not exported spuriously.
   - Rationale: lone shared interprocedural residual at 16/1; historical trace already narrowed root family.

4. `cluster_latent_uaf_sideband_row_canonicalization_rebase`
   - Scope: after explicit latent sideband landed, rebase `latent_use_after_free` on row-key/canonicalization/import ordering rather than sideband absence.
   - Rationale: newly tractable angle after phase3; must avoid the known bad heuristic based only on payload `Invalid(42)`.

No new tasks filed for arithmetic, cycle-cursor rows, recursive memory fallback, or dynamic funptr specialization because appropriate existing tasks/edges already cover them and/or they remain gated on `nondisj_phase5_call_apply_and_force_continue` / phase 6 validation.

## Bottom line

Confirmed requested C-suite baseline: **120/17**. The residual map after today's wave is:

- **Tight-fix-candidate:** 4 procedures (`free_all_in_array`, `funptr_conditional_call_bad`, `test_modified_value_then_error_bad`, `latent_use_after_free`). Four new narrow tasks filed.
- **waits-on-NonDisjDomain-phase-5/6:** 10 procedures (5 arithmetic, 3 memory recursion, 2 funptr dynamic-specialization).
- **waits-on-cycle-cursor:** 3 procedures (`FN_crash_after_six_nodes_bad`, `crash_after_one_node_bad`, `crash_after_two_nodes_bad`).
- **accepted-known-limit:** none newly accepted at this rebaseline. `conditionnaly_apply_funptr_with_intptrptr` may become accepted benign over-specialization only after NonDisj phase 5/6 rebaseline.
