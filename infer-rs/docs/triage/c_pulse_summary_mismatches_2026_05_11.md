# C-suite OCaml↔Rust Pulse summary triage

Date: 2026-05-11
Branch: detached HEAD on 1608ab4 (after triage commit b62437c)
INFER_BIN: /Users/mtrojer/infer/infer/bin/infer (v1.2.0-e0d18cf1b6)

## Per-file totals

| File              | matching | diffs | ocaml_only | rust_only | wallclock |
|-------------------|---------:|------:|-----------:|----------:|----------:|
| arithmetic.c      |        4 |     7 |          0 |         1 |   ~25s    |
| funptr.c          |       10 |    18 |          0 |         0 |   ~25s    |
| interprocedural.c |        6 |    11 |          0 |         1 |   ~25s    |
| latent.c          |        1 |    13 |          0 |         0 |   ~25s    |
| memory_leak.c     |        9 |    37 |          0 |         5 |   ~25s    |
| specialization.c  |       20 |     1 |          0 |         0 |   ~25s    |
| nullptr.c         |        — |     — |          — |         — |  HANG     |

Notes
- "wallclock" is dominated by OCaml `infer capture` + analyze (~20s each).
- nullptr.c: Rust Pulse hangs (>240s). Already documented in
  infer-rs/docs/STATUS.md as a `1 TIMEOUT (known recursion hang)`.
- `rust_only` procs are mostly Pulse models that OCaml drops on the floor:
  `random` (declared via `__attribute__((__pure__))`), `realloc`/`a_malloc`/
  `my_free`/`my_malloc`/`my_realloc` (assigned via function pointers).
  Surface-only — they exist in Rust because we keep their summary store
  entries; OCaml prunes them.

## Recurring mismatch clusters (cross-file, ranked by frequency)

### Cluster A — `MustBeInitialized` annotation drift on `MustBeValid` formals  (~38 occurrences)
- Pattern: pre_attrs `missing=["x.*:[MustBeValid]"]` /
  `extra=["x.*:[MustBeInitialized, MustBeValid]"]` (or symmetric).
- Files: latent.c (16), memory_leak.c (20), funptr.c (6),
  interprocedural.c (1).
- Diagnosis: Rust attaches `MustBeInitialized` to dereferenced formals
  even when OCaml only records `MustBeValid`. Looks like a one-line
  policy diff in attribute construction in
  `infer-rs/crates/pulse/src/path_condition.rs` /
  `infer-rs/crates/pulse/src/abstract_state.rs` (vs OCaml's
  `PulseAbductiveDomain.AddressAttributes.add_one`).
- Effect: only changes pre_attr presentation; should not change reported
  bugs. High-leverage fix — clearing this would erase ~38 of ~87 diffs.

### Cluster B — `Initialized` post-attr leakage to formals (`x:[WrittenTo]` vs `x:[Initialized, WrittenTo]`)  (~10 occurrences)
- Pattern: post_attrs `missing=["x:[Initialized, WrittenTo]"]` /
  `extra=["x:[WrittenTo]"]`.
- Files: latent.c (traverse_and_crash_*), memory_leak.c
  (malloc_formal_leak_bad, report_leak_in_correct_line_bad,
  malloc_out_parameter_*).
- Diagnosis: Rust does not propagate `Initialized` to a formal pointer
  after a write through it; OCaml does. Likely missing
  `add_initialized` after `Store` of a formal address in Rust's
  store-instruction transfer function (`pulse/src/checker/store.rs` or
  similar). Worth confirming.

### Cluster C — `Allocated(CMalloc)` missing the `Uninitialized` companion attr  (~12 occurrences)
- Pattern: post_attrs `missing=[".*:[Allocated(CMalloc), Uninitialized]"]`
  / `extra=[".*:[Allocated(CMalloc)]"]`.
- Files: memory_leak.c (allocate_in_array, allocate_42_in_array,
  allocate_all_in_array, free_42_in_array, alloc_then_free_parameter_array_ok,
  malloc_returned_ok, malloc_out_parameter*, create_p, …).
- Diagnosis: Rust's malloc model emits `Allocated` but not the paired
  `Uninitialized` attribute that OCaml's
  `PulseAbductiveDomain.AddressAttributes.{set_uninitialized, add_one}`
  attaches. Surface-only divergence today, but if any caller reasons
  about uninitialized allocation, this could turn into a real FN.

### Cluster D — Function-pointer / closure summary surface  (~10 occurrences)
- Pattern: pre_attrs `missing=["malloc_func=malloc_func"]` (in pre_stack)
  + `phi missing=["atom:0 < malloc_func.*"]` /
  post_attrs `missing=[".*:[Initialized]"]` / `extra=[".*:[Closure(...), Initialized]"]`.
- Files: memory_leak.c (every `*_via_ptr`, every `malloc_ptr_*`,
  `__infer_globals_initializer_*`, `free_via_ptr`),
  funptr.c (`return_funptr`, `test_*_callback_*`, `apply_callback`).
- Diagnosis: Rust serialises captured globals (the function pointers)
  with the `Closure(<callee>)` attribute and elides the matching
  `0 < <addr>` strict-positivity atom; OCaml uses the inverse encoding
  (no Closure attr, just the formula atom + a stack entry for the
  global). This is a *whole encoding-strategy* diff, not a one-liner.
  Touches `pulse/src/closures.rs` and OCaml's
  `PulseSummary.add_pre_stack_for_globals` / how
  `PulseClosures.MakeClosure` serialises.

### Cluster E — `UsedAsBranchCond` over-attachment on Rust side  (~25 occurrences in latent.c)
- Pattern: pre_attrs `extra=[".*:[..., UsedAsBranchCond(<caller>)]"]`
  with no OCaml peer. Often paired with a kind flip
  `LatentInvalidAccess → AbortProgram` and an extra
  `AccessToInvalidAddress` diagnostic.
- Files: latent.c (`traverse_and_crash_*`,
  `crash_after_*`, `crash_if_different_addresses`,
  `equal_to_stack_address_test_then_crash_bad`).
- Diagnosis: Rust marks every formal that ever feeds a branch with
  `UsedAsBranchCond`, even when OCaml leaves it empty because the
  branch was discharged via the formula instead. The correlated
  kind-flip + extra diagnostic suggests the over-attachment is
  preventing the latent-promotion logic in
  `PulseSummary.summary_of_post`/`abductive_state_for_post` from
  recognising the constraint as already satisfied.
- This is the most semantically loaded cluster — worth a focused
  follow-up because it actually changes which procedures publish a
  Pulse diagnostic.

### Cluster F — Atom encoding: `cond:0 < x` (Rust) vs `phi:atom:0 < x` (OCaml)  (~5 occurrences in memory_leak.c)
- Pattern: conditions `extra=["cond:0 < v2"]` /
  phi `missing=["atom:0 < v2"]`.
- Files: memory_leak.c (alloc_then_free_parameter_array_ok,
  alloc_ref_counted_arith_ok, …), arithmetic.c (`return_non_negative*`).
- Diagnosis: Rust currently routes some witness atoms into
  `path_condition.conditions`; OCaml keeps them in `phi`. The
  comparator already canonicalises a lot of this, but a few survive.
  Worth a tiny canonicalizer extension OR fix at the producer site
  (`pulse/src/formula.rs::record_condition`).

### Cluster G — `is_int(...)` divergence (Rust adds, OCaml omits, or vice-versa)  (~10 occurrences)
- Files: funptr.c (`funptr_*_good`, `funptr_apply_*`,
  `funptr_conditional_call_bad`, `dereference_dereference_ptr`),
  memory_leak.c (`alloc_then_free_at_index_ok`, `interproc_mutual_recursion_leak`),
  specialization.c.
- Diagnosis: Rust adds `is_int(return.*)` to closure-call returns
  where OCaml has `eq:return.*=0` (a stronger, dependent fact). The
  comparator already strips redundant `is_int`, but here the OCaml
  side doesn't have it because it has a *better* fact. Indicates the
  closure-call modelling drops the equality fact in Rust.

### Cluster H — Linear-form encoding diffs surviving the canonicalizer (~6 occurrences)
- Pattern: phi `missing=["eq:x.*=lin(-1*v1,const=5)"]` with no rust peer.
- Files: interprocedural.c (`conditional_free*`), arithmetic.c
  (`assume_non_negative*`).
- Diagnosis: A handful of linear equalities are lost in the Rust
  formula because the call-summary substitution drops the affine
  link between the caller's local and the callee's formal. The
  canonicalizer cannot rescue this — it's a real summary-content
  loss.

## Cross-file procedure-level deltas worth tracking

| Procedure                                      | File              | Severity | Notes |
|------------------------------------------------|-------------------|----------|-------|
| `traverse_and_crash_if_equal_to_root`          | latent.c          | high     | 7 OCaml pre_posts → 10 Rust; kind flips ContinueProgram→AbortProgram on three of them with brand-new diagnostics. Cluster E. |
| `crash_after_two_nodes_bad`                    | latent.c          | high     | OCaml 4 → Rust 5; one ContinueProgram→AbortProgram flip + extra diagnostic. Cluster E. |
| `FN_call_if_negative_then_crash_with_negative_bad` | arithmetic.c  | high     | Rust raises `AccessToInvalidAddress(ConstantDereference(0))` that OCaml leaves silent. |
| `if_negative_then_crash_latent`                | arithmetic.c      | high     | Rust adds the same null-deref diagnostic. |
| `call_if_negative_then_crash_with_local_bad`   | arithmetic.c      | high     | OCaml has 3 pre_posts, Rust 2; an `ExitProgram` post-condition is missing in Rust. |
| `realloc_no_check_bad`, `realloc_no_free_bad`  | memory_leak.c     | medium   | OCaml has 4 pre_posts, Rust 1–3; multiple `ContinueProgram` cases collapsed. Likely realloc model surface diff. |
| `free_all_in_array`, `allocate_all_in_array`   | memory_leak.c     | medium   | Multiple `eq:vN=…` and `Invalid(ConstantDereference(N))` post-attrs missing on Rust side; index/value indirection through arrays. |
| `funptr_apply_funptr_with_intptrptr_and_after_*` | funptr.c        | medium   | OCaml has 2 pre_posts (one `AbortProgram` + diagnostic), Rust has 0 — Rust drops the bug entirely. |
| `apply_callback`                               | funptr.c          | low      | Specialization-key encoding mismatch: Rust emits two `dynamic_types: {**callback->f: assign_NULL/do_nothing}` keys; OCaml emits a single `⊥`. |
| `crash_if_different_addresses`                 | latent.c          | low      | OCaml 1 → Rust 2; an extra ContinueProgram in Rust where the post-state is reachable via formula equality OCaml prunes. |
| `may_double_free_if_alias`                     | specialization.c  | low      | The single still-failing case in specialization.c; OCaml 3 main / Rust 4; latent kind flip + post_attr drift. |

## Out-of-scope but observed

- nullptr.c hangs Rust Pulse (already tracked elsewhere); did NOT
  re-investigate the cause here.
- `infer/bin/infer` is empty in this checkout. INFER_BIN env points
  at the user's installed tree at `/Users/mtrojer/infer/infer/bin/infer`
  (Infer v1.2.0-e0d18cf1b6) and the harness handles that fine.

---

## Remeasure after initial cluster fixes (2026-05-12)

Branch: `infer-rs` at `0fbb99d9bb` plus the landed cluster-fix stack listed below.
INFER_BIN: `/Users/mtrojer/infer/infer/bin/infer`.

Requested repro command:

```sh
INFER_BIN=/Users/mtrojer/infer/infer/bin/infer \
  INFER_RS_C_TRIAGE_FILES=arithmetic.c,funptr.c,interprocedural.c,latent.c,memory_leak.c,specialization.c \
  cargo test -p pulse --test end_to_end test_summary_comparison_c_triage \
  -- --ignored --nocapture
```

Result: this six-file run aborts while entering `specialization.c` with a Rust
stack overflow (exit 101 / SIGABRT). The first five files complete and were
re-run without `specialization.c` to get a clean `TRIAGE SUMMARY` (exit 0). A
focused `specialization.c` run also stack-overflows, even with a larger
`RUST_MIN_STACK`, so the current specialization row is recorded as unmeasured.

### Per-file totals vs original baseline

| File              | baseline matching | baseline diffs | current matching | current diffs | ocaml_only | rust_only | delta matching | delta diffs | status |
|-------------------|------------------:|---------------:|-----------------:|--------------:|-----------:|----------:|---------------:|------------:|--------|
| arithmetic.c      |                 4 |              7 |                4 |             7 |          0 |         1 |             +0 |          +0 | remeasured |
| funptr.c          |                10 |             18 |               17 |            11 |          0 |         0 |             +7 |          -7 | remeasured |
| interprocedural.c |                 6 |             11 |                9 |             8 |          0 |         1 |             +3 |          -3 | remeasured |
| latent.c          |                 1 |             13 |                3 |            11 |          0 |         0 |             +2 |          -2 | remeasured |
| memory_leak.c     |                 9 |             37 |               18 |            28 |          0 |         5 |             +9 |          -9 | remeasured |
| specialization.c  |                20 |              1 |                — |             — |          — |         — |              — |           — | stack overflow |

Totals for the five completed files: original equivalent slice `30 matching / 86
diffs / 0 ocaml_only / 7 rust_only`; current `51 matching / 65 diffs / 0
ocaml_only / 7 rust_only`, for a verified slice delta of `+21 matching / -21
diffs`.

If `specialization.c` is carried forward at the original `20 matching / 1 diff`
(row not verified in this remeasure), the six-file suite-equivalent total would
be `71 matching / 66 diffs`, i.e. `+21 matching / -21 diffs` vs the original
`50 / 87` baseline.

### Per-cluster status snapshot

| Cluster | Initial fix status | Residual status |
|---------|--------------------|-----------------|
| A — formal `MustBeInitialized` drift | Landed `cluster_a_drop_spurious` (`b36da9543d`, rebased) and `cluster_a_taint_initial_formal_preeval_gap` (`f92c448b22`). | No open `cluster_a_residual_*` task. Some remaining pre-attr rows are mixed with D/G/array state-shape residuals. |
| B — `Initialized` after `Store` | Landed `cluster_b_propagate_initialized_after_store` (`2edf69ce06`). | No open `cluster_b_residual_*` task; remaining B-looking rows are broader summary/canonicalization cases. |
| C — `Allocated(CMalloc), Uninitialized` | Landed `cluster_c_pair_allocated_cmalloc_with` (`447ebff065`) and `cluster_c_tenv_struct_uninitialized_followup` (`edfa5f072f`). | No open `cluster_c_residual_*` task; array/realloc/index-shape rows remain outside the initial C fixes. |
| D — function-pointer/global summary surface | Landed `cluster_d_align_global_function_pointer` (`0fbb99d9bb`). | Open: `cluster_d_residual_global_pre_stack_initializer`. |
| E — `UsedAsBranchCond` / latent state shape | Landed `cluster_e_stop_over_attaching` (`8a52f5f5e0`) and narrow state-shape fix `cluster_e_residual_state_shape_self_cycle` (`2bcf498605`). | Open: `cluster_e_residual_cycle_eq_repr`; closed trace task `cluster_e_follow_up_trace_residual` was folded into it. |
| F — witness atoms in `conditions` vs `phi` | Landed `cluster_f_route_witness_atoms_through` (`ee3116407c`). | No open `cluster_f_residual_*` task. |
| G — closure-call return equality | Landed `cluster_g_preserve_closure_call_return` (`cdd901797f`) and partial residual fix `cluster_g_residual_funptr_return_export` (`563749a94a`). | Open: `cluster_g_residual_funptr_apply_post_canonical_edges`. |
| H — linear equalities across summaries | Landed `cluster_h_keep_linear_equalities_across` (`66fb6444d0`). | Open: `cluster_h_residual_inequality_witness_export`. |

### Remaining residuals

- `cluster_d_residual_global_pre_stack_initializer` — align global initializer
  pre-stack/pre-attrs and the `apply_callback` specialization-key surface.
- `cluster_e_residual_cycle_eq_repr` — fix latent traverse/crash cycle-equality
  representative choice so heap shape and latent kinds match OCaml.
- `cluster_g_residual_funptr_apply_post_canonical_edges` — preserve canonical
  read-only/post edges for `funptr_if_good` / `funptr_else_good` return loads.
- `cluster_h_residual_inequality_witness_export` — implement OCaml-style
  restricted/tableau witness export for inequality-derived affine facts.

### Current notable residual rows

- `funptr.c`: residuals are concentrated in `apply_callback`, callback update
  pre-attrs/closure attrs, `funptr_if_good` / `funptr_else_good`, and the
  `*_with_intptrptr_and_after_*` rows. Cluster D/G residual tasks cover the
  highest-leverage pieces.
- `latent.c`: still `3 matching / 11 diffs`; the large rows remain
  `traverse_and_crash_if_equal_to_root`, `FN_crash_after_six_nodes_bad`, and
  `crash_after_two_nodes_bad`, consistent with the open Cluster E representative
  task.
- `memory_leak.c`: now `18 matching / 28 diffs`; major residual families are
  array/index heap shape, malloc/realloc branch-count surface, global function
  pointer pre-stack, and recursion self-cycle/value-shape differences.
- `arithmetic.c` and `interprocedural.c`: counters are unchanged in this final
  stack; remaining rows are mainly arithmetic/inequality witness export and
  latent diagnostic/kind differences.
- `specialization.c`: current triage harness stack-overflows before producing a
  row. Earlier cluster notes recorded `20 matching / 1 diff`, but that number is
  not reverified at this final initial-cluster checkpoint.

---

## Remeasure after residual cluster fixes (2026-05-12, second pass)

Branch: `infer-rs` at `b0d55c8bd7` after the residual stack landed (full list
below). INFER_BIN: `/Users/mtrojer/infer/infer/bin/infer` (Infer
v1.2.0-e0d18cf1b6). Stack overflow seen in the previous remeasure has been
fixed (`bug_specialization_c_stack_overflow_lin_arith_of_q`,
`ed9ef8caeb`), so the full six-file run completes again.

Repro:

```sh
cd infer-rs
INFER_BIN=/Users/mtrojer/infer/infer/bin/infer \
  INFER_RS_C_TRIAGE_FILES=arithmetic.c,funptr.c,interprocedural.c,latent.c,memory_leak.c,specialization.c \
  cargo test -p pulse --test end_to_end test_summary_comparison_c_triage \
  -- --ignored --nocapture
```

### Per-file totals vs original baseline

| File              | baseline matching | baseline diffs | current matching | current diffs | ocaml_only | rust_only | delta matching | delta diffs |
|-------------------|------------------:|---------------:|-----------------:|--------------:|-----------:|----------:|---------------:|------------:|
| arithmetic.c      |                 4 |              7 |                6 |             5 |          0 |         1 |             +2 |          -2 |
| funptr.c          |                10 |             18 |               20 |             8 |          0 |         0 |            +10 |         -10 |
| interprocedural.c |                 6 |             11 |               10 |             7 |          0 |         1 |             +4 |          -4 |
| latent.c          |                 1 |             13 |                3 |            11 |          0 |         0 |             +2 |          -2 |
| memory_leak.c     |                 9 |             37 |               23 |            23 |          0 |         5 |            +14 |         -14 |
| specialization.c  |                20 |              1 |               20 |             1 |          0 |         0 |             +0 |          -0 |
| **total**         |            **50** |         **87** |           **82** |        **55** |          0 |         7 |        **+32** |     **-32** |

### New / amended commits in this pass

Residual-track fixes landed on top of the initial cluster pass:

- `cluster_d_residual_global_pre_stack_initializer` (`ebc2483271`) — dynamic
  CFunction/ObjcBlock pre-stack seeding plus `apply_callback` specialization-key
  surface alignment; memory_leak.c `18/28 → 22/24`, funptr.c `17/11 → 18/10`.
- `cluster_specialization_residual_post_overflow_fix` (`f822d97d5b`) — stop
  the recursive Cfun harness fallback that the cycle-break fix exposed;
  specialization.c restored from `18/3` back to `20/1`.
- `cluster_h_residual_inequality_witness_export` (`b8e4d41959`) — OCaml-style
  restricted/tableau witness export for inequalities; interprocedural.c
  `9/8 → 10/7`, arithmetic.c `4/7 → 6/5`.
- `cluster_e_residual_cycle_eq_repr` (`4f993fd916`) — align VarUF
  representative ordering with OCaml. Pin-down only at this point; latent.c
  counters unchanged because the remaining divergence is in apply_post / latent
  recovery (tracked by `cluster_e_residual_apply_post_cycle_edges`).
- `cluster_g_residual_funptr_apply_post_canonical_edges` (`98c5d37652`) —
  preserve no-op call summaries instead of going through unknown-call havoc;
  funptr.c `18/10 → 20/8`.
- `bug_h_residual_witness_regresses_specialization` (`24d2ff6e15`) — scope
  the H restricted witness export to direct summary roots; restored
  specialization.c from `19/2` back to `20/1` while preserving the H residual
  gains on interprocedural.c and arithmetic.c.

### Open residuals after this pass

- `cluster_a_taint_initial_formal_preeval_gap` follow-on residuals are folded
  into the broader memory_leak.c residual; no separate task.
- The single remaining specialization.c diff stays on `may_double_free_if_alias`
  (latent kind/post-attr drift), unchanged across this pass.

---

## Remeasure after secondary residual cluster fixes (2026-05-12, third pass)

Branch: `infer-rs` at `56f496f878` after the secondary residual stack landed:

- `cluster_d_residual_funptr_atom_repr` (`f4ead67353` / `f172c2fc95`) — prefer
  the global funptr pointee as the atom representative during summary
  comparison/normalization. Pure normalization fix; analysis semantics and
  Closure seeding unchanged. memory_leak.c improves; nothing else moves.
- `cluster_e_residual_apply_post_cycle_edges` (`0748df5dc3` / `56f496f878`) —
  remove eager subst-value canonicalization during imported formula
  application; restore direct pre/post cycle heap edges in latent summary
  shape. Unit-pinned; latent.c counters unchanged because the remaining drift
  is in broader latent classification, not the direct-edge mechanism.

### Per-file totals after the third pass

| File              | baseline matching | baseline diffs | current matching | current diffs | delta matching | delta diffs |
|-------------------|------------------:|---------------:|-----------------:|--------------:|---------------:|------------:|
| arithmetic.c      |                 4 |              7 |                6 |             5 |             +2 |          -2 |
| funptr.c          |                10 |             18 |               20 |             8 |            +10 |         -10 |
| interprocedural.c |                 6 |             11 |               10 |             7 |             +4 |          -4 |
| latent.c          |                 1 |             13 |                3 |            11 |             +2 |          -2 |
| memory_leak.c     |                 9 |             37 |               25 |            21 |            +16 |         -16 |
| specialization.c  |                20 |              1 |               20 |             1 |             +0 |          -0 |
| **total**         |            **50** |         **87** |           **84** |        **53** |        **+34** |     **-34** |

### Open residuals after this pass

- `cluster_a_taint_initial_formal_preeval_gap` follow-on residuals folded into
  the broader memory_leak.c residual; no separate task.
- `may_double_free_if_alias` is the only remaining specialization.c diff (latent
  kind/post-attr drift); accepted, no follow-up planned at this checkpoint.
- Remaining latent.c rows (`traverse_and_crash_if_equal_to_root`,
  `FN_crash_after_six_nodes_bad`, `crash_after_two_nodes_bad`) are a broader
  latent classification concern. Two further passes (atom normalization +
  classification + caller substitution) landed on top of this; see the next
  section.

---

## Remeasure after latent.c classification pass (2026-05-12, fourth pass)

Branch: `infer-rs` at `45c6b9004f`. Three new fixes landed:

- `cluster_atom_normalization_argc_lin_signed` (`1d26a295f5` /
  `b92bfefaa3`) — comparator-side normalization for cond ↔ phi.atom routing on
  zero (in)equalities and sign-normalized lin form
  `(<a-b> {!=,=} 0)` ↔ `(<a> {!=,=} <b>)`. Improves latent.c `3/11 → 5/9`.
- `cluster_use_after_free_caller_alpha_substitution` (`dd3f0f64e7` /
  `d43907a2e3`) — prefer caller-actual representative when canonicalizing
  imported substitution ranges in `apply_post`. Pin-down only at this point;
  latent.c `FN_nonlatent_use_after_free_bad` still differs on broader summary
  shape, but the alpha-rename mechanism is now OCaml-aligned.
- `cluster_latent_classification_extra_aborts` (`f9bb6319a1` /
  `45c6b9004f`) — remove the over-broad branch-controlled local manifest twin,
  classify recovered invalid accesses latent for non-entry procedures, strip
  recovered latent-invalid diagnostics from exported pre/posts, and replay
  stripped latent stopped diagnostics for specialized summaries. Pin-down only;
  latent.c counters unchanged at this layer.

### Per-file totals after the fourth pass

| File              | baseline matching | baseline diffs | current matching | current diffs | delta matching | delta diffs |
|-------------------|------------------:|---------------:|-----------------:|--------------:|---------------:|------------:|
| arithmetic.c      |                 4 |              7 |                6 |             5 |             +2 |          -2 |
| funptr.c          |                10 |             18 |               20 |             8 |            +10 |         -10 |
| interprocedural.c |                 6 |             11 |               10 |             7 |             +4 |          -4 |
| latent.c          |                 1 |             13 |                5 |             9 |             +4 |          -4 |
| memory_leak.c     |                 9 |             37 |               25 |            21 |            +16 |         -16 |
| specialization.c  |                20 |              1 |               20 |             1 |             +0 |          -0 |
| **total**         |            **50** |         **87** |           **86** |        **51** |        **+36** |     **-36** |

### Status after the fourth pass

No open `cluster_*` residual tasks remain in the C-suite triage track. Remaining
top-level diffs are concentrated in:

- latent.c `traverse_and_crash_if_equal_to_root` / `FN_crash_after_six_nodes_bad`
  / `crash_after_two_nodes_bad` — deeper latent classification + state-shape
  drift. The classification mechanism is OCaml-aligned via
  `cluster_latent_classification_extra_aborts`; the remaining drift is in heap
  shape during apply_post and per-step witness conditions.
- memory_leak.c — array/index heap shape, malloc/realloc branch-count surface,
  and recursion self-cycle/value-shape differences.
- specialization.c — single accepted residual on `may_double_free_if_alias`.
