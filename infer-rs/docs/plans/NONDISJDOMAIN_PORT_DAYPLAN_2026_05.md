# Pulse NonDisjDomain Rust port day plan

Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-2`
HEAD while scoping: `2dcccc1a41cd46ab6df2b9c0311b0eb2a434f56c`

Umbrella/scout: `scout_nondisjdomain_port_dayplan`.
Arithmetic edge: `cluster_arithmetic_residual_drill` classified all 5 `arithmetic.c` residuals as blocked on OCaml's non-disjunctive over-approx / force-continue summary sideband, with minor later formula-presentation cleanup.

## Objective

Port the OCaml Pulse non-disjunctive over-approximate summary path into Rust in a staged way. The minimal useful target is not the full OCaml `PulseNonDisjunctiveDomain` (which also powers unnecessary-copy / const-refable-parameter reporting and transitive-info bookkeeping) but the over-approximate `astate` sideband:

1. remember dropped `ContinueProgram` disjuncts as an over-approximate joined `AbductiveDomain`,
2. execute that joined over-approximate state through every subsequent instruction,
3. export it as a hidden summary pre/post (`summary.non_disj.astate` in OCaml), and
4. at call sites, apply that hidden pre/post separately from the visible disjunctive pre/post list, using its success when deciding whether `pulse_force_continue` is needed.

This is the missing mechanism behind the arithmetic fallback rows such as `call_if_negative_then_crash_with_local_bad` and `return_non_negative{,_float}`.

## OCaml reference surface

Primary files:

- `infer/src/pulse/PulseNonDisjunctiveDomain.mli`
- `infer/src/pulse/PulseNonDisjunctiveDomain.ml`
- `infer/src/pulse/Pulse.ml`
- `infer/src/pulse/PulseCallOperations.ml`
- `infer/src/pulse/PulseSummary.ml`
- `infer/src/pulse/PulseAbductiveDomain.ml`
- Secondary / mention-only: `infer/src/pulse/PulseTopl.ml`, `PulseTransitiveAccessChecker.ml`, `PulseSpecializedCallGraph.ml`.

### Key OCaml types and components

`PulseNonDisjunctiveDomain.t` contains four independent pieces:

```ocaml
type t =
  { intra: IntraDom.t
  ; inter: InterDom.t
  ; has_dropped_disjuncts: AbstractDomain.BooleanOr.t
  ; astate: OverApproxDomain.t }
```

For this port, the important pieces are:

- `has_dropped_disjuncts`: sticky bit set when the disjunctive interpreter under-approximates by dropping disjuncts.
- `astate: OverApproxDomain.t`: `BottomLifted (AbductiveDomain.t * PathContext.t)`, joined by `PulseJoin.join`, gated by OCaml config `pulse_over_approximate_reasoning`.
- `summary.astate`: hidden `AbductiveDomain.Summary.t bottom_lifted` produced from the over-approximate state via `AbductiveDomain.Summary.of_post`.

Important but not in the first minimal arithmetic port:

- `intra`: copy/const-ref/lifetime tracking (`copy_map`, `parameter_map`, destructor checks, captured vars, loads/stores, `passed_to`). Rust has no equivalent unnecessary-copy surface yet; do not include it in the arithmetic-focused implementation phases.
- `inter`: `TransitiveInfo` for transitive access / specialized direct callee graph. Rust has separate specialization mechanisms and no direct `TransitiveInfo` port; keep out of the minimal over-approx path.

### Key OCaml entry points

1. **Dropped disjunct production**
   - `PulseNonDisjunctiveDomain.remember_dropped_disjuncts`
   - `AbstractInterpreter.MakeDisjunctiveTransferFunctions.add_dropped_disjuncts`
   - `AbstractInterpreter.MakeDisjunctiveTransferFunctions.exec_instr` / `exec_node_instrs`

   When the disjunct limit causes dropped states, OCaml records:

   - `has_dropped_disjuncts = true` if the list is non-empty,
   - dropped `ContinueProgram astate` states joined into `non_disj.astate`, and
   - dropped states' `transitive_info` into `non_disj.inter`.

2. **Over-approx state execution after each instruction**
   - `PulseNonDisjunctiveDomain.exec`
   - `PulseTransferFunctions.exec_instr_non_disj`
   - `PulseTransferFunctions.exec_instr`
   - `PulseNonDisjunctiveDomain.for_disjunct_exec_instr`

   OCaml executes each normal disjunct with a cleared non-disj `astate` (`for_disjunct_exec_instr`) so ordinary disjunct execution cannot recursively consume the over-approx state. Separately, `exec_instr_non_disj` moves `non_disj.astate` into a singleton `ContinueProgram`, marks its `PathContext` as non-disjunctive, executes the same instruction with limit `1`, and joins any resulting `ContinueProgram` back into `non_disj.astate`.

3. **Summary export**
   - `PulseSummary.of_posts`
   - `PulseNonDisjunctiveDomain.make_summary`
   - `PulseNonDisjunctiveDomain.Summary.get_pre_post`
   - `PulseAbductiveDomain.Summary.of_post`

   Visible summary rows come from the ordinary `pre_post_list`. Separately, `make_summary` converts the hidden over-approx `astate` through `AbductiveDomain.Summary.of_post`; failures become `Bottom`. This gives `PulseSummary.summary = {pre_post_list; non_disj}`.

4. **Summary application / consumption**
   - `PulseCallOperations.call_aux`
   - `NonDisjDomain.apply_summary`
   - `NonDisjDomain.Summary.get_pre_post`
   - `NonDisjDomain.join_to_astate`
   - `PulseCallOperations.call` force-continue gate

   OCaml applies the hidden non-disj pre/post in two places:

   - If the caller path is itself non-disj (`PathContext.is_non_disj`), append `non_disj_callee`'s hidden pre/post to the callee visible rows being applied on that path.
   - Even from an ordinary disjunct, apply the hidden pre/post to the caller state and write the result directly into the caller's non-disj `astate` using `join_to_astate`.

   Then `has_continue_program` is true if either the visible call results contain `ContinueProgram` **or** the returned non-disj `astate` is non-bottom. The `pulse_force_continue` unknown-call fallback only fires when the selected summary was dropped/empty and still has no continue, including the hidden non-disj result.

5. **Secondary consumers**
   - `PulseTopl.ml`: tracks its own Topl disjunct dropping counters/limits; no direct `NonDisjDomain` dependency found in `PulseTopl.ml`, but Topl state is part of `AbductiveDomain` and therefore may be carried by any hidden over-approx summary.
   - `PulseTransitiveAccessChecker.ml` and `PulseSpecializedCallGraph.ml`: read `NonDisjDomain.Summary.get_transitive_info_if_not_top`. This is important for a full OCaml-equivalent domain, but not required for the arithmetic over-approx fallback.

## Rust current surface / delta

Primary files:

- `infer-rs/crates/absint/src/disjunctive.rs`
- `infer-rs/crates/absint/src/interp.rs`
- `infer-rs/crates/absint/src/transfer.rs`
- `infer-rs/crates/pulse/src/checker.rs`
- `infer-rs/crates/pulse/src/summary.rs`
- `infer-rs/crates/pulse/src/interproc.rs`
- `infer-rs/crates/pulse/src/transfer.rs`
- Adjacent sideband: `infer-rs/crates/pulse/src/abductive.rs` `PendingInvalidAccess`.

### What Rust already has

- `DisjunctiveDomain<ExecutionDomain>` has `had_dropped_disjuncts` and `bound()` sets it when the vector exceeds `pulse_max_disjuncts`.
- `PulseSummary` has a visible `pre_posts: Vec<PrePost>` and summary-level `has_dropped_disjuncts`.
- `checker.rs` already gates `maybe_force_continue_after_known_call` on `pulse_force_continue`, no visible continue, and `used_summary_was_empty || used_summary_has_dropped_disjuncts`.
- Unknown-call fallback in `transfer.rs::exec_call` already supports the important arithmetic shape: fresh return, conservative init, stable `FunctionApplication` for pure C unknown calls, pointer havoc for pointer args.
- `PendingInvalidAccess` is already a summary/application sideband in `AbductiveDomain` / `PrePost`: it is useful as precedent for carrying hidden analysis metadata across summary export, but it is **not** the same mechanism. `PendingInvalidAccess` lives inside visible `AbductiveDomain` / `PrePost`; NonDisj needs a summary-level hidden pre/post plus a per-node analysis domain.

### What is missing

1. **No `NonDisjDomain` / product domain.** Rust `PulseTransferFunctions::Domain` is only `DisjunctiveDomain<ExecutionDomain>`. There is no `NonDisjState { has_dropped_disjuncts, over_approx }` paired with it, so dropped states cannot be joined and re-executed.

2. **Dropped disjuncts are not recoverable.** `DisjunctiveDomain::bound()` drains dropped states and only sets a bool. Once drained, the actual `ContinueProgram` states are lost, so later summary export cannot produce OCaml's hidden over-approx pre/post. The OCaml interpreter records dropped state payloads at the join/bound point.

3. **No non-disj instruction execution pass.** `PulseTransferFunctions::exec_instr` loops ordinary disjuncts only. There is no `exec_instr_non_disj` analog that feeds the over-approx state through the same instruction with limit 1.

4. **No hidden summary pre/post.** `summary.rs::PulseSummary` has `has_dropped_disjuncts` but no `non_disj_pre_post: Option<PrePost>` (or equivalent). `of_proc_with_metadata` accepts only ordinary `exec_states` and a bool.

5. **Call application has only heuristics for the missing sideband.** `checker.rs` comments explicitly mention the missing hidden non-disj astate and use narrow workarounds for alias/dynamic-type stopped summaries. Arithmetic still needs the real hidden pre/post to distinguish “no visible continue but hidden over-approx continued” from “force unknown-call fallback now”.

6. **No true join of `AbductiveDomain` in Rust.** OCaml `OverApproxDomain.join` uses `PulseJoin.join`. Rust has semantic `alpha_equivalent` / `leq` but no implemented join for two `AbductiveDomain`s. A minimal first port can use a single-slot/first-over-approx state to get the mechanism wired, but parity needs at least a conservative/bounded join or a very small list with join-like bounding.

## Day-by-day phases

All implementation phases must be read/write tasks later, but this scout did no Rust source edits. Each phase below should fit 0.5-1.0 day and should be done linearly.

### Phase 1 — `nondisj_phase1_domain_scaffold` (0.75d)

Scope:

- `infer-rs/crates/pulse/src/non_disj.rs` (new module) or local module in `checker.rs` initially.
- `infer-rs/crates/pulse/src/lib.rs` module export if a new file is used.
- `infer-rs/crates/pulse/src/summary.rs` data shape only if needed for shared types.
- Unit tests in `checker.rs` / new `non_disj.rs` tests.

Plan:

- Introduce a minimal Rust `NonDisjDomain` focused on over-approx only:
  - `has_dropped_disjuncts: bool`,
  - `over_approx: Option<AbductiveDomain>` (or `Option<(AbductiveDomain, NonDisjContext)>` if a context enum is wanted),
  - `bottom`, `is_bottom`, `is_over_approx_bottom`, `join`, `remember_dropped_disjuncts`, `join_to_astate`, `for_disjunct_exec_instr`.
- Keep the first join implementation deliberately bounded. If full `PulseJoin` is not available yet, use deterministic single-state retention with clear TODO/tests, or a tiny `Vec<AbductiveDomain>` sideband behind the same API. Do not pretend this is full OCaml join parity.
- Add an internal product-domain type such as `PulseDomain { disjuncts: DisjunctiveDomain<ExecutionDomain>, non_disj: NonDisjDomain }` only if Phase 2 is ready to use it; otherwise keep the domain type isolated and unit-tested.

Expected impact:

- No end-to-end residual should be expected to move yet.
- Establishes compile-time/API surface needed by later phases.

Hard guards:

- Do not port `intra` copy/const-ref maps or `inter` transitive-info in this phase.
- Keep existing `PendingInvalidAccess` fields untouched.
- Unit tests only; no change to summary counts expected.

### Phase 2 — `nondisj_phase2_fixpoint_dropped_state_capture` (1.0d)

Scope:

- `infer-rs/crates/absint/src/disjunctive.rs` or new pulse-local bounded helper that can return dropped disjunct payloads.
- `infer-rs/crates/pulse/src/checker.rs` `PulseTransferFunctions::Domain`, `exec_instr`, `exec_node`, summary build's `has_dropped_disjuncts` computation.
- `infer-rs/crates/absint/src/interp.rs` / `transfer.rs` only if a generic product-domain hook is cleaner than pulse-local logic.

Plan:

- Pair ordinary disjuncts with the new `NonDisjDomain`, mirroring OCaml `Domain.t = disjunct list * non_disj`.
- At every place Rust bounds/dedups/joins disjuncts and currently only flips `had_dropped_disjuncts`, also pass the dropped `ExecutionDomain` payloads to `NonDisjDomain::remember_dropped_disjuncts`.
- In `exec_node`, preserve and join the non-disj component from old/current post, and keep the OCaml behavior where dropped old-pre information is not silently lost.
- Keep public summary bool `has_dropped_disjuncts` sourced from the non-disj domain (or at least ORed with it) during summary construction.

Expected impact:

- Mostly preparatory; may alter force-continue decisions only after Phase 4/5.
- Necessary before arithmetic can move because the dropped `ContinueProgram` states are currently irretrievably drained.

Hard guards:

- Preserve fixpoint convergence. Watch prior OpenSSL-style guardrails: do not mutate dropped metadata in a way that makes `leq` fail forever after widening.
- Preserve current C-suite scoped baselines unless a later phase intentionally changes arithmetic.
- Add tests for “dropped `ContinueProgram` enters non-disj over-approx” and “stopped-only drops set bit but do not create over-approx continue”.

### Phase 3 — `nondisj_phase3_exec_overapprox_per_instruction` (0.75d)

Scope:

- `infer-rs/crates/pulse/src/checker.rs` `PulseTransferFunctions::exec_instr` / `exec_node`.
- `infer-rs/crates/pulse/src/transfer.rs` unknown/model transfer compatibility.
- New helper analogous to OCaml `exec_instr_non_disj`.

Plan:

- Add a Rust equivalent of OCaml `NonDisjDomain.exec`:
  - take current `non_disj.over_approx`,
  - clear it before executing the instruction,
  - run the same instruction transfer on a singleton `ContinueProgram` with an “is non-disj” context if needed,
  - join only resulting `ContinueProgram` states back into `non_disj.over_approx`,
  - drop/ignore abort/exit from the hidden sideband for the minimal port.
- Prevent recursion: ordinary disjunct execution must not consume the over-approx state (`for_disjunct_exec_instr` behavior).
- Add a minimal context flag only if call application needs it in Phase 5; otherwise document that Rust's first implementation executes the hidden pre/post from ordinary disjuncts only at call boundaries.

Expected impact:

- Still may not move arithmetic until hidden summary export/application exists.
- Enables hidden over-approx states to survive past the instruction where disjuncts were dropped.

Hard guards:

- Limit hidden execution to one over-approx state / bounded sideband to avoid new path explosion.
- Do not publish diagnostics from hidden over-approx execution in this phase.
- Preserve `pulse_force_continue` existing behavior for visible summaries.

### Phase 4 — `nondisj_phase4_summary_export_hidden_prepost` (0.75d)

Scope:

- `infer-rs/crates/pulse/src/summary.rs` `PulseSummary`, `SpecializedSummary`, `of_proc_with_metadata`, `add_specialized_summary`, pretty/debug/equality helpers as needed.
- `infer-rs/crates/pulse/src/checker.rs` summary construction call site.
- Existing summary application tests that construct `PulseSummary` literals.

Plan:

- Add a hidden summary field, e.g. `non_disj_pre_post: Option<PrePost>`, to `PulseSummary` and specialized summaries if needed.
- At procedure exit, convert `non_disj.over_approx` to a `PrePost` by reusing `build_pre_post` + `normalize` / `normalize_with_summary_info` as close as possible to visible `ContinueProgram` export, but do not append it to visible `pre_posts`.
- Keep summary-level `has_dropped_disjuncts` independent and still serialized/printed as before; hidden pre/post is additional.
- If normalization finds leaks / latent invalid access, keep the first implementation conservative: drop hidden pre/post on normalization contradiction or potential diagnostic rather than publishing hidden diagnostics.

Expected impact:

- Callers can now see that a callee had a hidden over-approx pre/post, but no residual movement until Phase 5 consumes it.

Hard guards:

- Hidden pre/post must not affect visible summary row counts by itself.
- Do not include hidden pre/post in diagnostic publication or `pre_posts` dedup/sorting.
- Audit constructors/tests: avoid defaulting test summaries to a non-empty hidden field.

### Phase 5 — `nondisj_phase5_call_apply_and_force_continue` (1.0d)

Scope:

- `infer-rs/crates/pulse/src/checker.rs` `SelectedSummary`, `KnownCalleeResults`, `apply_pre_posts_with_specialization_loop`, `exec_known_callee_summary`, `maybe_force_continue_after_known_call`.
- `infer-rs/crates/pulse/src/interproc.rs` `apply_summary_with_aliasing` reused for hidden pre/post.
- `infer-rs/crates/pulse/src/transfer.rs` unknown-call fallback unchanged except tests.

Plan:

- Extend summary selection to carry the selected hidden non-disj pre/post alongside visible pre/posts.
- After applying visible rows, apply hidden pre/post to the caller's input state. Do **not** append the result to visible `results`; instead return it as a hidden continuation signal / `NonDisjDomain::join_to_astate` equivalent.
- Change `KnownCalleeResults` to track `hidden_non_disj_continued: bool` or carry the hidden state into the caller product domain if Phase 2 already introduced it.
- Match OCaml's force-continue gate: `has_continue_program = visible continue || hidden non-disj over-approx non-bottom`; only call `maybe_force_continue_after_known_call` when selected summary was empty/dropped and **no** visible or hidden continue exists.
- Preserve current alias/dynamic-type heuristic force-continue tests by either deleting now-obsolete heuristics with equivalent hidden-prepost tests or keeping them narrowly until hidden preposts cover all cases.

Expected impact:

- Main arithmetic unlock:
  - `call_if_negative_then_crash_with_local_bad`: should gain the OCaml hidden continue / force-continue behavior instead of visible Exit+Abort only.
  - `return_non_negative` and `return_non_negative_float`: allows removing/avoiding Rust's visible `0 <= return` compensation later; at minimum changes gate behavior safely.
  - `FN_call_if_negative_then_crash_with_negative_bad`: should gain the missing unknown-call/function-application continue row; remaining unary-neg presentation may remain.
- Possible latent/memory-leak surfaces may change where previous heuristics compensated for missing non-disj.

Hard guards:

- Do not expose hidden pre/post as a visible `ContinueProgram` row.
- Do not force unknown-call fallback when hidden apply succeeded.
- Focus testing on call-site behavior; keep `PendingInvalidAccess` and latent diagnostic sidebands independent.

### Phase 6 — `nondisj_phase6_arithmetic_validation_and_cleanup` (0.5d)

Scope:

- `infer-rs/crates/pulse/tests/end_to_end.rs` triage harness only as needed.
- `infer-rs/crates/pulse/src/summary.rs` / formula normalization only for removing prior arithmetic-specific compensations if now obsolete.
- Docs/status updates.

Plan:

- Run targeted arithmetic triage:
  - `RUST_TEST_THREADS=1 INFER_RS_C_TRIAGE_FILES=arithmetic.c cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture`
- Reclassify the five residuals:
  1. `FN_call_if_negative_then_crash_with_negative_bad`
  2. `call_if_negative_then_crash_with_local_bad`
  3. `if_negative_then_crash_latent`
  4. `return_non_negative`
  5. `return_non_negative_float`
- Remove any narrow Rust visible-summary compensation that is now superseded only if the hidden sideband proves the same caller behavior.
- File remaining formula-presentation work separately for unary neg / `DivF` term-eq if arithmetic is no longer NonDisj-gated.

Expected impact:

- Target: reduce or close `arithmetic.c` from `6/5` residuals.
- Likely leftovers after NonDisj port: formula presentation only (`if_negative_then_crash_latent` unary neg; float `DivF` term-eq).

Hard guards:

- Preserve global hard caps from scout note: current C-suite, 52/0/0 sweep, LEAK/UAF/NPE.
- No broad formula sweeps in this phase; only validate and remove obsolete compensation if tightly proven.

## Phase edges

Linear implementation order:

```sh
mu task block nondisj_phase2_fixpoint_dropped_state_capture --by nondisj_phase1_domain_scaffold -w infer-rs
mu task block nondisj_phase3_exec_overapprox_per_instruction --by nondisj_phase2_fixpoint_dropped_state_capture -w infer-rs
mu task block nondisj_phase4_summary_export_hidden_prepost --by nondisj_phase3_exec_overapprox_per_instruction -w infer-rs
mu task block nondisj_phase5_call_apply_and_force_continue --by nondisj_phase4_summary_export_hidden_prepost -w infer-rs
mu task block nondisj_phase6_arithmetic_validation_and_cleanup --by nondisj_phase5_call_apply_and_force_continue -w infer-rs
mu task block cluster_arithmetic_residuals_after_nondisjdomain_port --by nondisj_phase6_arithmetic_validation_and_cleanup -w infer-rs
```

## Edge to arithmetic residual cluster

`cluster_arithmetic_residual_drill` is closed and should remain as the historical classification. File a new follow-up cluster `cluster_arithmetic_residuals_after_nondisjdomain_port`, blocked by Phase 6, to re-run and split any residuals after the hidden non-disj summary sideband lands.

Expected arithmetic mapping:

- Phase 5 is the first phase expected to change control-flow/row-count residuals.
- Phase 6 validates and separates true NonDisj wins from remaining formula-summary presentation work.

## PendingInvalidAccess sideband comparison

`PendingInvalidAccess` is adjacent but not reusable directly:

- Similarity: both are hidden metadata crossing summary export/application boundaries without becoming ordinary visible pre/post rows.
- Difference: `PendingInvalidAccess` is stored inside `AbductiveDomain` and cloned into each visible `PrePost`; NonDisj needs a procedure-summary-level hidden pre/post plus a per-node analysis product domain that is executed every instruction.
- Recommendation: reuse style/patterns (summary constructors, default empty field, tests for not publishing diagnostics), not storage.

## Global hard guards for all future implementation phases

- Preserve existing C-suite caps noted by the scout: current `118/19` C-suite, `52/0/0` sweep, LEAK/UAF/NPE.
- Preserve targeted baselines unless a phase explicitly expects arithmetic movement: `funptr.c 24/4`, `interprocedural.c 16/1`, `latent.c 10/4`, `memory_leak.c 38/8`, `specialization.c 20/1`, `arithmetic.c 6/5`.
- Keep hidden non-disj pre/post invisible in ordinary summary row counts.
- Do not port unrelated OCaml `intra` copy/const-ref or `inter` transitive-info features as part of the arithmetic-focused port.
- Bound hidden over-approx state count; avoid reintroducing path explosion or non-convergent dropped-disjunct metadata behavior.
