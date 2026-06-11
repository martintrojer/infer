# Final seven C-suite residuals classification

Task: `scout_final_seven_residuals_classification`  
Workstream: `infer-rs`  
Date: 2026-05-19  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1`  
Baseline classified: `origin/infer-rs` / `d0319ba4b4` (`pulse: close arithmetic force-continue and DivF residuals`).  
Scope: read-only scout; no source edits and no commits.

> **Historical task-name note (added 2026-06):** The task names cited below
> under "Follow-up filed" / "New tasks filed"
> (`cluster_arithmetic_unary_neg_summary_presentation`,
> `cluster_memory_leak_interproc_recursion_branch_havoc`,
> `cluster_latent_cycle_cursor_deep_port_revisit`) were planned names from this
> 2026-05-19 scout; they were never created as live `mu` tasks. The underlying
> work has mostly landed or been superseded by the final sweep recorded in
> `docs/STATUS.md`. Treat `docs/STATUS.md` and `mu task list -w infer-rs` as
> authoritative for current state; this doc is historical.

## Re-baseline

The freshly recreated worker-1 workspace initially sat one commit behind `origin/infer-rs`; I checked out `origin/infer-rs` read-only before measuring the requested final-seven surface.

Scoped C-triage command:

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1/infer-rs
INFER_RS_C_TRIAGE_FILES=arithmetic.c,funptr.c,interprocedural.c,latent.c,memory_leak.c,specialization.c \
RUST_TEST_THREADS=1 cargo test -p pulse --test end_to_end \
  test_summary_comparison_c_triage -- --ignored --nocapture
```

Confirmed shared-procedure summary state:

| File | Matching | Diffs | Extra non-shared stubs |
|---|---:|---:|---|
| `arithmetic.c` | 9 | 2 | Rust-only `random` |
| `funptr.c` | 27 | 1 | none |
| `interprocedural.c` | 17 | 0 | Rust-only `random` |
| `latent.c` | 11 | 3 | none |
| `memory_leak.c` | 45 | 1 | Rust-only `a_malloc`, `my_free`, `my_malloc`, `my_realloc`, `realloc` |
| `specialization.c` | 21 | 0 | none |
| **Total shared** | **130** | **7** | declaration/stub surfaces excluded from the 7 |

Artifacts:

- Scoped triage stdout/stderr was captured in the terminal run for this task.
- Capped per-procedure Rust traces: `/tmp/final_seven_2026_05_19/logs/rust_*.err`.
- OCaml direct summary JSON / debug artifacts: `/tmp/final_seven_2026_05_19/ocaml/*/out/all_summaries.json`; one full OCaml `--debug` HTML probe for arithmetic is under `/tmp/final_seven_2026_05_19/ocaml_debug/arithmetic_if/out/captured/.../nodes/`.
- C textual captures used for Rust traces: `/tmp/final_seven_2026_05_19/sil/{arithmetic,funptr,latent,memory_leak}.sil`.

Rust trace command shape used per residual:

```sh
timeout 60 target/debug/infer-rs --pulse-only -j 1 \
  --debug-level-analysis 2 --procedures-filter '<proc>' \
  -o /tmp/final_seven_2026_05_19/rust/<file>_<proc>_out \
  --capture-textual /tmp/final_seven_2026_05_19/sil/<file>.sil
```

OCaml direct command shape used per residual:

```sh
infer -j 1 --pulse-only --debug-level-analysis 2 \
  --procedures-filter '<proc>' \
  -o /tmp/final_seven_2026_05_19/ocaml/<file>_<proc>/out \
  -- clang -c infer/tests/codetoanalyze/c/pulse/<file>.c
infer debug -j 1 --dump-json-summaries \
  -o /tmp/final_seven_2026_05_19/ocaml/<file>_<proc>/out
```

OCaml source cross-referenced:

- Arithmetic/formula summary presentation: `PulseFormula.ml`, `PulseFormulaPhi.ml`, `PulseFormulaLinArit.mli`, `PulseFormulaVar.ml`, `PulseSummary.ml`, `PulseAbductiveDomain.filter_for_summary`.
- Cycle cursor / latent publication: `PulseInterproc.ml` (`materialize_pre_from_address`, `record_post_for_address`, `rev_subst`/aliases), `PulseAbductiveDomain.restore_formals_for_summary` / `filter_for_summary`, `PulseSummary.ml`, `PulseLatentIssue.ml`, `PulseCallOperations.ml`.
- Mutual recursion fallback: `PulseCallOperations.on_recursive_call`, `call_aux_unknown`, `SkippedKnownCall`, `PulseInterproc.apply_summary`.
- Function pointer specialization: `PulseModelsC.call_c_function_ptr`, `PulseCallOperations.iter_call` / `maybe_dynamic_type_specialization_is_needed`, `PulseSpecialization.ml`, `PulseAbductiveDomain.Summary.heap_paths_that_need_dynamic_type_specialization`.

## Per-residual classification

### 1. `arithmetic.c::if_negative_then_crash_latent`

**Diff shape:** row kinds/counts align; remaining delta is summary formula presentation only.

- OCaml conditions: `cond:v1 < 0`, `cond:0 <= v1`.
- Rust conditions: `cond:neg(x.*) < 0`, `cond:0 <= neg(x.*)` plus phi atoms such as `atom:lin(-1*x.*) < 0` / `atom:0 <= lin(-1*x.*)`.
- OCaml HTML for the callee shows the latent row simplified as `0 <= a3`, with term/linear facts like `[-a3]=v3`; Rust retains the caller-visible `Neg(Var(x.*))` syntax through summary canonicalization.

**Classification:** `tight-fix-candidate`.

**Why:** after worker-2's `c0e3007cbb` the force-continue and DivF parts are closed. This one is an isolated unary-neg alpha/presentation gap between equivalent formulas; no heap/diagnostic semantics are involved.

**Follow-up filed:** `cluster_arithmetic_unary_neg_summary_presentation`.

### 2. `arithmetic.c::FN_call_if_negative_then_crash_with_negative_bad`

**Diff shape:** same unary-neg presentation family plus one caller row ordering/count presentation delta.

- Rust has `main pre_post count: ocaml=3, rust=4`.
- OCaml has a `LatentAbortProgram` row with `cond:0 <= v1`, `atom:x.* <= 0`, and temp equality `v2=lin(1*v1,const=-1)`.
- Rust keeps an `ExitProgram`/extra `LatentAbortProgram` presentation involving `cond:neg(x.*) < 0`, `cond:0 <= neg(x.*)`, `atom:0 < x.*`, `atom:lin(-1*x.*) < 0`, and `eq:v1=lin(-1*v2,const=-1)`.

**Classification:** `tight-fix-candidate`.

**Why:** the hard interproc part already closed. This is the caller manifestation of the same unary-neg witness-temp normalization, with an extra summary row presentation/order artifact. It should be fixed or comparator-normalized in the arithmetic cluster, not accepted as a semantic limit.

**Follow-up filed:** covered by `cluster_arithmetic_unary_neg_summary_presentation`.

### 3. `latent.c::crash_after_one_node_bad`

**Diff shape:** row kinds mostly align, but cursor heap/attr shape still differs.

- Rust has extra post self-cycle edge `q.* -*-> q.*` and extra `q.*:[Initialized, WrittenTo]`.
- One row differs on `q.*:[MustBeValid]` vs `q.*:[MustBeInitialized, MustBeValid]`.
- OCaml direct summary preserves caller-visible `q->next` cursor obligations without publishing the same root self-cycle shape.

**Classification:** `scoped-deep-port-needed`.

**Why:** prior latent phase 2 already showed that fixing apply-post post-cell replay alone is too late. The residual lives in the deeper ordering between pre-materialization aliases/rev_subst, restored formal/cursor paths, and latent row-key publication.

**Follow-up filed:** `cluster_latent_cycle_cursor_deep_port_revisit` for all three cycle rows.

### 4. `latent.c::crash_after_two_nodes_bad`

**Diff shape:** full row-shape mismatch despite the Rust e2e subset guard being safe.

- OCaml row 0 keeps `q.*.next -*-> q.*.next.*`; Rust has `q.* -*-> q.*` and `q.*.next -*-> q.*`.
- OCaml row 1 is `ContinueProgram`; Rust row 1 is `AbortProgram` with manifest `AccessToInvalidAddress(ConstantDereference(0))`.
- OCaml row 2 is `LatentInvalidAccess`; Rust row 2 is `ContinueProgram` with a self-materialized cursor edge.
- MustBeInitialized/MustBeValid and condition/phi placement differ around `q.* != q.*.next.*` and `q.*.next.* = 0`.

**Classification:** `scoped-deep-port-needed`.

**Why:** this is not a small comparator issue; Rust's state-space row keys and manifest-vs-latent publication differ from OCaml. Requires the deeper cycle-cursor port described above.

**Follow-up filed:** covered by `cluster_latent_cycle_cursor_deep_port_revisit`.

### 5. `latent.c::FN_crash_after_six_nodes_bad`

**Diff shape:** the deepest member of the same cycle-cursor family.

- OCaml exports `ContinueProgram` plus `LatentInvalidAccess` rows over long cursor paths.
- Rust maps several rows to `AbortProgram` or `ContinueProgram`, with root self-cycle edges such as `q.* -*-> q.*`, `q.*.next -*-> q.*`, and `q.*.next.*...next -*-> q.*`.
- Rust has manifest null-deref diagnostics where OCaml's summary surface remains latent/caller-sensitive.

**Classification:** `scoped-deep-port-needed`.

**Why:** phase 4 hidden NonDisj/force-continue is already in place and did not move this. The remaining gap is cursor representative/path and latent publication ordering, not a final-mile force-continue tweak.

**Follow-up filed:** covered by `cluster_latent_cycle_cursor_deep_port_revisit`.

### 6. `memory_leak.c::interproc_mutual_recusion_leak`

**Diff shape:** only wrapper/caller of the mutual-recursion cluster remains; the two recursive procedures themselves now match.

- OCaml row 0 post attrs include `x.*.data.*:[Initialized]`.
- Rust row 0 keeps `x.*.data.*:[Allocated(CMalloc), Uninitialized]` plus `atom:0 < x.*.data.*`.
- Rows 1/2 flip the null/non-null presentation around `x.*.data.* == 0` vs `!= 0` and differ on whether `x.*.data:[Initialized, WrittenTo]` or `x.*.data.*:[Initialized]` is exported.
- Rust trace shows `interproc_mutual_recusion_leak` calls `mutual_recursion` through a matched summary on three caller rows; OCaml direct rows carry `UnknownEffect(SkippedKnownCall(mutual_recursion))` and different field-pointee attrs after the `malloc` branch.

**Classification:** `scoped-deep-port-needed`.

**Why:** worker-leak's `929bfc6928` correctly removed the broad extra struct-pointee fallback materialization and closed `mutual_recursion{,_2}`. This last wrapper row is narrower but still interproc recursive-call/unknown-effect plus branch/null/malloc attr ordering. A tight single-line tweak is unlikely; it needs a scoped port against OCaml `on_recursive_call` / `call_aux_unknown` / summary export ordering.

**Follow-up filed:** `cluster_memory_leak_interproc_recursion_branch_havoc`.

### 7. `funptr.c::conditionnaly_apply_funptr_with_intptrptr`

**Diff shape:** Rust-only specialized summary:

```text
specialized extra in rust: dynamic_types: {*funptr: assign_NULL}
```

OCaml has no matching `assign_NULL` specialization for this callee; it keeps only the useful `do_nothing` dynamic specialization surface. The source calls `(*funptr)(ptr)` only under `if (x)`, then unconditionally writes `*ptr = NULL`. At the currently analyzed call sites, the `assign_NULL` specialization is semantically redundant/benign:

- the unspecialized bad caller passes `x = 0`, so the funptr branch is not taken;
- the specialized bad caller passes `x = 1` with `funptr = do_nothing`, and the useful OCaml specialization is the `do_nothing` row;
- the unconditional `*ptr = NULL` dominates the issue surface either way.

**Classification:** `accepted-known-limit`.

**Why:** the residual is an over-specialization artifact, not an issue-count or semantic summary-row defect. Rust's summary surface records that `*funptr` may need dynamic type specialization before branch feasibility eliminates the `assign_NULL` caller path; OCaml's `iter_call` / `maybe_dynamic_type_specialization_is_needed` requests dynamic specialization from contradictions at call application time and does not publish this unused `assign_NULL` specialization. Removing it precisely would require a branch-feasible specialization-demand analysis and risks regressing the recently closed dynamic-specialization chain. The harness can treat this exact extra specialized summary as benign.

**Harness cleanup note:** future comparator cleanup can accept exactly `conditionnaly_apply_funptr_with_intptrptr` with one Rust-only specialized key `dynamic_types: {*funptr: assign_NULL}` and no main-summary differences. Do not generalize to arbitrary extra function-pointer specializations.

## Summary table

| File | Procedure | Classification | Task / action |
|---|---|---|---|
| `arithmetic.c` | `if_negative_then_crash_latent` | `tight-fix-candidate` | `cluster_arithmetic_unary_neg_summary_presentation` |
| `arithmetic.c` | `FN_call_if_negative_then_crash_with_negative_bad` | `tight-fix-candidate` | `cluster_arithmetic_unary_neg_summary_presentation` |
| `latent.c` | `crash_after_one_node_bad` | `scoped-deep-port-needed` | `cluster_latent_cycle_cursor_deep_port_revisit` |
| `latent.c` | `crash_after_two_nodes_bad` | `scoped-deep-port-needed` | `cluster_latent_cycle_cursor_deep_port_revisit` |
| `latent.c` | `FN_crash_after_six_nodes_bad` | `scoped-deep-port-needed` | `cluster_latent_cycle_cursor_deep_port_revisit` |
| `memory_leak.c` | `interproc_mutual_recusion_leak` | `scoped-deep-port-needed` | `cluster_memory_leak_interproc_recursion_branch_havoc` |
| `funptr.c` | `conditionnaly_apply_funptr_with_intptrptr` | `accepted-known-limit` | document comparator acceptance only |

## New tasks filed

1. `cluster_arithmetic_unary_neg_summary_presentation` — tight final-mile candidate for the two arithmetic residuals.
2. `cluster_latent_cycle_cursor_deep_port_revisit` — scoped deep-port task for the three latent cycle rows.
3. `cluster_memory_leak_interproc_recursion_branch_havoc` — scoped deep-port task for the remaining wrapper-side memory leak row.

No task filed for the accepted funptr over-specialization; it is documented here for a future harness-known-limit cleanup.
