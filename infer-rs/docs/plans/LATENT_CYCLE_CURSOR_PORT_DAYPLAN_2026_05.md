# latent.c cycle-cursor / latent-invalid-access port day plan

Date: 2026-05-18  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1`  
Scout task: `scout_latent_cycle_cursor_deep_port_dayplan`  
Baseline HEAD while scoping: `905ec61662` (`pulse: pin array constant coalescing fixtures`)  
Scope guard: read-only scout; no Rust source edits.

## Residual surface scoped

Concrete `infer/tests/codetoanalyze/c/pulse/latent.c` residuals:

- `crash_after_one_node_bad`
- `crash_after_two_nodes_bad`
- `FN_crash_after_six_nodes_bad`
- `latent_use_after_free` after local EqZero sideband and summary EqZero sideband work

Scoped command run during scout:

```sh
ulimit -v 8388608
INFER_BIN=../infer/bin/infer INFER_RS_C_TRIAGE_FILES=latent.c \
  RUST_TEST_THREADS=1 RAYON_NUM_THREADS=1 timeout 180 \
  cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture
```

Result at `905ec61662`: `latent.c matching=10 diffs=4`.

Diff composition is now:

1. `FN_crash_after_six_nodes_bad`: Rust still has row-kind and shape divergences, including OCaml `LatentInvalidAccess` rows that Rust turns into `AbortProgram` or `ContinueProgram` rows.
2. `crash_after_one_node_bad`: kind surface is close, but heap/attr shape still differs (`q.* -> q.*`/extra initialized/written shapes, MustBeInitialized/MustBeValid parity).
3. `crash_after_two_nodes_bad`: the e2e subset pin passes, but full C triage still differs in row shape, path conditions, pre attrs, and one latent-vs-continue/abort row.
4. `latent_use_after_free`: still differs on exact OCaml `Summary.of_post`/`PotentialInvalidAccessSummary` provenance for the zero-cleanup row; worker-2 verified this is not loss of the imported `Invalid(ConstantDereference(42))` attr.

## OCaml mechanism breakdown

### 1. There is no standalone `PulseCycleCursor` module

Searches over `infer/src/pulse` found no named cycle-cursor module. The cursor behavior is emergent from these mechanisms:

- loop execution and bounded fixpoint in `Pulse.ml`;
- formal/cursor restoration during summary export in `PulseAbductiveDomain.ml`;
- recursive summary application in `PulseInterproc.ml`;
- summary-of-post `EqZero` sideband conversion to `LatentInvalidAccess` in `PulseAbductiveDomain.ml` + `PulseSummary.ml`;
- latent issue re-checking in `PulseCallOperations.ml` / `PulseLatentIssue.ml`.

The apparent “six-node cursor” behavior is not a hard-coded six-step traversal. OCaml's defaults are `--pulse-max-disjuncts=20` and `--pulse-widen-threshold=3`; loop back-edge termination is driven by `PulseLoopHeaderInfo.has_previous_iteration_same_path_stamp` when `pulse_eternal` is off. The six-node fixture is a stress case where the finite loop-summary frontier, restored formal cursor, and cyclic caller heap shape interact.

### 2. Loop/fixpoint cursor shape

Relevant OCaml code:

- `infer/src/pulse/Pulse.ml`:
  - `LoopEntry` initializes per-loop header info.
  - `LoopBackEdge` pushes a path stamp and stops when the same path stamp recurs.
  - `DisjunctiveAnalyzer = AbstractInterpreter.MakeDisjunctive(...)` uses:
    - `UnderApproximateAfter Config.pulse_max_disjuncts`
    - `UnderApproximateAfterNumIterations Config.pulse_widen_threshold`
- `infer/src/pulse/PulseLoopHeaderInfo.ml`:
  - stores `{timestamp; path_stamp}` stack per loop header.
  - terminates repeated path-stamp loop exploration.

For `traverse_and_crash_if_equal_to_root`, the local cursor `p` is advanced through `p = p->next` while `old_p` stays rooted at the original formal. Summary export restores the original formal view, so callers see conditions/heap paths relative to `q`, `q->next`, `q->next->next`, etc. rather than the callee-local final value of `p`.

### 3. Summary export restores formals and preserves cursor heap paths

Relevant OCaml code:

- `PulseAbductiveDomain.filter_for_summary`:
  1. canonicalizes the whole state;
  2. calls `restore_formals_for_summary` so formal stack cells use their initial pre-state values;
  3. discards unreachable state;
  4. calls `Formula.simplify ~precondition_vocabulary ~keep`, returning `new_eqs`.
- `restore_pre_var_value` uses a visited set to avoid cycling while restoring formal/global/return-visible subgraphs.

This is the first “cursor” point: after the loop mutates local `p`, summary export re-exposes the original formal root and reachable pre-heap paths. The cycle is terminated by visited sets, not by rewriting every field cursor back to the root.

### 4. Summary application recursively replays post heap

Relevant OCaml code: `infer/src/pulse/PulseInterproc.ml`:

- `call_state` carries:
  - `subst`: callee address -> caller address/history;
  - `rev_subst`: caller address -> callee address plus heap path;
  - `hist_map`: callee cell id -> caller history;
  - `visited`: callee addresses visited during pre/post walks;
  - `aliases`: supported heap-path alias groups.
- `materialize_pre_from_address` walks the callee pre graph, records cell histories, checks `MustBeValid`, and records heap-path aliases instead of blindly merging them.
- `apply_post` does:
  1. `apply_unknown_effects`;
  2. `apply_post_from_callee_pre`;
  3. `apply_post_from_callee_post`;
  4. `add_attributes \`Post ...`;
  5. recursive/transitive info and return value replay.
- `record_post_for_address` recursively traverses callee post edges with a visited set.
- `record_post_cell` translates post edges, restores histories through `hist_map`, deletes callee-pre accesses from caller post edges, and combines translated post edges with old caller edges via `BaseMemory.Edges.union_left_biased`.

The apply-post four-phase port has now landed in Rust, so the remaining cycle residuals are not “flat apply_post” anymore. They are the next layer: which caller representative/path should survive after a cursor value is proven equal to a cycle root, and which summary-of-post `EqZero + MustBeValid` obligation should be exported as `LatentInvalidAccess`.

### 5. Latent invalid-access provenance is a sideband, not `Invalid(0)`

Relevant OCaml code:

- `PulseAbductiveDomain.incorporate_new_eqs`:
  - `EqZero v + heap allocated + MustBeValid` returns `Some (v, must_be_valid)`;
  - it does **not** add `Invalid(ConstantDereference(0))` to `v`.
- `PulseAbductiveDomain.filter_for_summary` returns `new_eqs` from formula simplification.
- `PulseAbductiveDomain.Summary.of_post_` immediately consumes those `new_eqs`:
  - `Some(address, must_be_valid)` becomes `PotentialInvalidAccessSummary(summary, astate_before_filter, Decompiler.find address ..., must_be_valid)`.
- `PulseSummary.exec_summary_of_post_common` converts `PotentialInvalidAccessSummary` into `Stopped (LatentInvalidAccess {astate; address; must_be_valid; calling_context=[]})` when the address has no ordinary `Invalid` attr.
- `PulseCallOperations.apply_callee` re-checks `LatentInvalidAccess` at the caller boundary:
  - if the translated address is still not invalid, keep latent;
  - if it is invalid in the caller post, report/reify.

This exact provenance is also worker-2's `latent_use_after_free` finding: the remaining UAF divergence needs OCaml `Summary.of_post` provenance, not another attr-based heuristic over the stored payload `Invalid(ConstantDereference(42))`.

### 6. Force-continue / NonDisjDomain interaction

Relevant OCaml code:

- `PulseCallOperations.call` treats a call as having a continue path if either disjunctive results contain `ContinueProgram` **or** the hidden `PulseNonDisjunctiveDomain` state is non-bottom.
- With `Config.pulse_force_continue`, OCaml force-continues only when the selected summary has dropped/empty disjuncts and still has no continue result.

This can affect the `ContinueProgram` rows around latent-only cycle summaries. Rust has a narrow approximation in `checker.rs::maybe_force_continue_after_known_call` and special cases for alias/dynamic-type summaries, but does not have the full NonDisjDomain sideband. Coordinate with worker-2's `scout_nondisjdomain_port_dayplan` before changing this layer.

## Rust delta after apply_post + EqZero work

Primary Rust surfaces:

- `crates/pulse/src/interproc.rs`
  - Apply-post phases now exist: hist_map/cell-id restoration, pre-edge deletion, recursive `record_post_for_address`, left-biased edge merge, and alias groups.
  - Remaining gap: downstream canonicalization/rebasing can still collapse field-derived cursor targets back to root representatives or fresh self-cycles. Current C triage shows extra `q.* -> q.*` and missing longer `q.*.next...` chains.
- `crates/pulse/src/summary.rs`
  - Has `pending_invalid_accesses` and transient `summary_potential_invalid_access: Option<AbstractValue>` from summary simplification.
  - Remaining gap: the OCaml latent invalid-access export stores a precise `(address, must_be_valid)` sideband on the stopped summary. Rust reconstructs from diagnostics, `must_be_valid`, `pending_invalid_accesses`, and heap-path scans; this is insufficient for cycle cursor rows and `latent_use_after_free` vs `FN_nonlatent_use_after_free_bad{,2}` discrimination.
  - Existing helpers such as `latent_invalid_access_path_values`, `latent_invalid_access_heap_path`, and `recovered_invalid_access_pre_posts_from_abort_state` are useful but heuristic.
- `crates/pulse/src/formula/var_uf.rs`
  - Already documents the relevant invariant: field cursor representatives should not always be rewritten to roots when a cursor is proven equal to the root.
  - Remaining gap is likely not the basic union rule alone; it is the full path from formula simplification through summary canonicalization, post replay, and latent-invalid reporting keys.
- `crates/pulse/src/value_history.rs`
  - Cell-id restoration is now present and bounded. Keep it guarded; do not reintroduce history blowups.

## Why `crash_after_two_nodes_bad` e2e passes while C triage still calls it a diff

`test_e2e_latent_cycle_summary_shapes_match_ocaml_subset` is a Rust-side default-config subset pin. It asserts the current acceptable Rust kind surface for the textual fixture:

```text
crash_after_two_nodes_bad:
  ContinueProgram, LatentInvalidAccess, AbortProgram, AbortProgram
```

That test deliberately prevents diagnostic-less latent orphan rows and guards the default `pulse_force_continue=true` Rust subset behavior.

The C triage comparator is stricter and compares the single-file clang-captured `latent.c` summaries against OCaml summary JSON at the row-shape level: pre/post heap paths, attrs, path conditions, phi, and diagnostic presence. At `905ec61662`, `crash_after_two_nodes_bad` still differs on:

- OCaml latent/continue rows that Rust maps to continue/abort rows under different shape keys;
- extra Rust root self-cycle edges like `q.* -> q.*`;
- missing OCaml longer cursor paths like `q.*.next -> q.*.next.*`;
- `MustBeInitialized`/`MustBeValid` attr parity;
- condition/phi placement for `q.* != q.*.next.*` and `q.*.next.* = 0`.

So the e2e pass is real but intentionally weaker: it pins a safe Rust subset and regression guard, not full OCaml shape parity.

## Phase breakdown

### Phase 1 — Cycle cursor shape oracle and exact row-key probes (0.5d)

Task: `cluster_latent_cycle_phase1_shape_oracles`

Scope:

- Add focused, non-heuristic shape probes before changing semantics:
  - real `latent.c` reduced fixture for `traverse_and_crash_if_equal_to_root`, one-node, two-node, six-node;
  - per-row kind, latent report key, selected invalid address heap path, post heap edge list, and `MustBeValid` source timestamp/location;
  - explicit check of default Rust e2e subset vs C-triage OCaml shape.
- Candidate Rust files later: `crates/pulse/tests/end_to_end.rs`, debug-only helpers in `summary.rs`/`checker.rs` if needed.
- No behavior change in this phase.

Impact:

- Turns the current read-only observations into stable implementation guards.
- Prevents repeating the previous attempt-3 failure mode: diagnostic-less latent rows or duplicate latent twins.

Guards:

- Existing pins must keep passing:
  - `test_e2e_latent_cycle_summary_shapes_match_ocaml_subset`
  - `test_e2e_deref_then_free_then_deref_keeps_npe_latent`
  - cell-id provenance tests
- `latent.c` scoped triage should remain `10/4` unless a later phase deliberately improves it.

### Phase 2 — Preserve field-derived cursor representatives through summary/export/apply (1.0d)

Task: `cluster_latent_cycle_phase2_cursor_reprs`

Scope:

- Audit and port the OCaml representative/path invariant end-to-end:
  - `PulseFormulaVar.is_simpler_than` / canonical value choice;
  - `PulseAbductiveDomain.filter_for_summary` canonicalize + `restore_formals_for_summary`;
  - `PulseInterproc.record_post_for_address` lazy substitution lookup and direct cycle target replay.
- Rust candidate sites:
  - `formula/var_uf.rs` (only if the representative rule is incomplete);
  - `summary.rs::canonicalize_for_summary_or_unsat`, `restore_direct_cycle_edges_for_summary`, `restore_pre_var_value`;
  - `interproc.rs::canonicalize_imported_actual_value`, `resolve_for_post_with_history`, recursive post replay;
  - `state_cmp.rs` only for test/oracle normalization, not production semantics.
- Goal: keep cursor heap paths such as `q.*.next.*.next` distinct long enough for summary export/report keys, instead of collapsing them into `q.* -> q.*` self-cycle surfaces.

Expected impact:

- Reduces heap/attr/phi shape diffs in `crash_after_one_node_bad` and `crash_after_two_nodes_bad`.
- Prepares `FN_crash_after_six_nodes_bad` by preserving the deep caller path instead of losing the six-hop cycle to root coalescing.

Guards:

- Do not regress `apply_post` phase pins:
  - `test_apply_summary_preserves_direct_callee_cycle_post_edge`
  - `test_apply_summary_restores_post_edge_history_from_callee_pre_cell_id`
- Do not regress array/constant coalescing pins from current HEAD.
- Watch for history/path explosion; `ValueHistory` cap tests must hold.

### Phase 3 — Store OCaml-shaped `LatentInvalidAccess(address, must_be_valid)` sideband on PrePost rows (1.0-1.25d)

Task: `cluster_latent_cycle_phase3_latent_address_sideband`

Scope:

- Replace the remaining diagnostic/attr/heap-scan heuristic boundary with an explicit OCaml-shaped latent invalid-access sideband:
  - exact address selected by summary `new_eqs` / `PotentialInvalidAccessSummary`;
  - `must_be_valid` trace/location/reason;
  - calling context / import history sufficient for caller reification.
- Rust candidate sites:
  - `summary.rs::PrePost` metadata, `NormalizedSummaryInfo`, `potential_invalid_access_from_normalized_continue_pre_post`, `latent_invalid_access_diagnostic_from_summary_state`, `latent_invalid_access_report_key`, `recovered_invalid_access_pre_posts_from_abort_state`;
  - `interproc.rs` LatentInvalidAccess application branch and `summarize_stopped_state` handling;
  - `abductive.rs` summary `new_eqs` consumption only if the current `summary_potential_invalid_access` needs to carry more than an address.
- Preserve ordinary concrete `Attribute::Invalid` attrs. The sideband must not be expressed as `Invalid(ConstantDereference(0))`.

Expected impact:

- Fixes the core FN class where OCaml exports `LatentInvalidAccess(q/path)` and Rust currently exports `AbortProgram`/`ContinueProgram` or loses the diagnostic.
- Directly overlaps `latent_use_after_free`: the zero-cleanup row needs exact `Summary.of_post` provenance to distinguish it from `FN_nonlatent_use_after_free_bad{,2}`.

Guards:

- Keep `deref_then_free_then_deref_bad` OCaml attr invariant:
  - no `Invalid(ConstantDereference(0))` on the direct formal pointee;
  - stored `Invalid(ConstantDereference(42))` remains on the payload.
- Keep `latent_use_after_free` imported/stored `Invalid(42)` attr preservation.
- No diagnostic-less `LatentInvalidAccess` rows.
- Do not broaden path-local filters based only on payload `Invalid(42)`; worker-2 already proved that regresses `FN_nonlatent_use_after_free_bad{,2}`.

### Phase 4 — Force-continue / NonDisjDomain-aware cycle publication and six-node parity (0.75-1.0d)

Task: `cluster_latent_cycle_phase4_nondisj_force_continue`

Scope:

- Reconcile the cycle latent-only / stopped-only summary paths with OCaml's call boundary:
  - `PulseCallOperations.call` uses disjunctive results plus non-disj bottom/non-bottom to decide `has_continue_program`;
  - `pulse_force_continue` adds an unknown-call continue only in a narrow dropped/empty/no-continue case.
- Rust candidate sites:
  - `checker.rs::apply_known_callee_summary`, `maybe_force_continue_after_known_call`, alias-specialized latent-only logic;
  - whatever worker-2's NonDisjDomain scout identifies as the eventual Rust non-disj sideband boundary.
- Make `FN_crash_after_six_nodes_bad` publish the OCaml latent NPE rows without introducing manifest NPE reports or duplicate continue rows.

Expected impact:

- Closes or sharply reduces the six-node FN residual.
- Prevents false positives from converting latent cycle paths to manifest aborts.

Guards:

- Coordinate with `scout_nondisjdomain_port_dayplan` before implementation.
- Preserve current `FN_crash_after_six_nodes_bad` e2e guard that no manifest null dereference is published unless the expected OCaml issue surface is deliberately updated.
- Full guard triage after this phase:
  - `specialization.c` `20/1`
  - `latent.c` improvement from `10/4` expected; no new residual classes
  - `memory_leak.c` `38/8`
  - `funptr.c` current baseline
  - `interprocedural.c` `16/1`
  - store-textual sweep and LEAK/UAF/NPE pins

## Edges filed

Linear implementation ordering:

```sh
mu task block cluster_latent_cycle_phase2_cursor_reprs --by cluster_latent_cycle_phase1_shape_oracles -w infer-rs
mu task block cluster_latent_cycle_phase3_latent_address_sideband --by cluster_latent_cycle_phase2_cursor_reprs -w infer-rs
mu task block cluster_latent_cycle_phase4_nondisj_force_continue --by cluster_latent_cycle_phase3_latent_address_sideband -w infer-rs
```

Coordination edge is logical rather than a hard `mu task block` for now: phase 4 should read/consume worker-2's `scout_nondisjdomain_port_dayplan` output before implementation, because the force-continue row and hidden non-disj continue signal are shared mechanisms.

## Cross-worker coordination

- Worker-2 `cluster_latent_use_after_free_divergence_after_eqzero_sideband` finding is incorporated: residual is missing exact OCaml `Summary.of_post` provenance, not stripping of imported `Invalid(42)`.
- Worker-2 `scout_nondisjdomain_port_dayplan` may overlap phase 4. If their scout files a concrete NonDisjDomain sideband task, re-check whether `cluster_latent_cycle_phase4_nondisj_force_continue` should depend on that task or be folded into it.
- Worker-leak's memory leak/funptr scout should not be blocked by phases 1-3. Phase 4 force-continue changes can affect memory leak/funptr unknown-call/continue surfaces, so include their guard files before landing.

## Risks / edges

- **Over-canonicalization risk:** fixing cursor representatives in one place can regress constant/array access coalescing or alias-specialized apply_post. Keep phase 2 narrow and pinned.
- **Duplicate latent rows:** adding a sideband without dedup/report keys can reproduce the previous diagnostic-less orphan row in `crash_after_two_nodes_bad`.
- **Attr vs sideband confusion:** EqZero latent invalid accesses are sidebands; concrete stored constants and UAF invalidations remain ordinary `Invalid` attrs.
- **NonDisjDomain overlap:** force-continue changes before worker-2's scout lands may encode another heuristic. Prefer consuming their design output for phase 4.
- **Config mismatch:** default Rust e2e (`pulse_force_continue=true`) and C triage/OCaml JSON shape are not identical. Use the e2e subset as a regression guard and the C triage as parity target.
