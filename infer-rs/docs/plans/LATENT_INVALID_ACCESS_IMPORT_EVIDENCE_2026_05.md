# latent_invalid_access import/recovery reconciliation evidence

Date: 2026-05-15  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-2`  
OCaml debug results: `/tmp/infer-latent-scout` (generated read-only with `/home/mtrojer/infer/infer/bin/infer run --pulse-only --debug --debug-level 3 --results-dir /tmp/infer-latent-scout --project-root . -- clang -c infer/tests/codetoanalyze/c/pulse/latent.c -o /tmp/latent.o`).

## Procedures checked

The actual procedure name in `infer/tests/codetoanalyze/c/pulse/latent.c` is `latent_use_after_free` (not `nonlatent_use_after_free`; `FN_nonlatent_use_after_free_bad` is a separate later test case).

Scoped OCaml debug HTML:

- `latent_use_after_free`: `/tmp/infer-latent-scout/captured/latent.c.8b2bfb96330f167d/latent_use_after_free.a34478a09d99ce6d.html`
- `deref_then_free_then_deref_bad`: `/tmp/infer-latent-scout/captured/latent.c.8b2bfb96330f167d/deref_then_free_then_deref_bad.ee87bf63ca3dbce3.html`
- caller import evidence for the latent path: `/tmp/infer-latent-scout/captured/latent.c.8b2bfb96330f167d/manifest_use_after_free.8b3c25f732f0a563.html` and `main.fad58de7366495db.html`

## Concrete OCaml debug facts

### `latent_use_after_free` exports a latent-invalid summary with a concrete stored-constant invalid attr

In `latent_use_after_free.a34478a09d99ce6d.html`:

- `#1: LatentInvalidAccess(b)` (HTML text line ~150).
- Its current post has `b` and `x` coalesced to the same value (`v3`) and a post deref cell from that value to `v12`:
  - raw `mem`: `v1 -> * -> v3`, `v2 -> * -> v3`, `v3 -> * -> v12`.
  - raw `attrs`: `v3 -> { Initialized, WrittenTo(...) }`, `v12 -> { Initialized, Invalid ... ConstantDereference(is assigned to the constant 42) }`.
- Its inferred pre has `v3 -> { MustBeValid(... line 18 access ...), UsedAsBranchCond(...) }` and the path condition `x = 0@0` (`conditions: {v3 = 0@0}; term_eqs: 0=v3∧42=v12`).

So the concrete `Invalid(ConstantDereference(...))` on the stored constant is a normal post attribute of the exported latent-invalid summary. The latent invalid obligation is on the address `v3` (known null and must-be-valid); the invalid attr is on the value written through it (`v12`, constant 42).

`latent_use_after_free` also has Continue rows (`#2`/`#3`) that keep the same stored-constant invalid attr on `v11`/`v12`. That is expected OCaml behavior from normal constant evaluation and summary export.

### Applying/importing that latent-invalid summary preserves the attr

In `manifest_use_after_free.8b3c25f732f0a563.html`:

- `#0: ContinueProgram` has `Current post: x->...-value: v6` with `attributes: { Invalid ::(`latent_use_after_free`)line 25, column 40[assigned at line 18 ... ConstantDereference(is assigned to the constant 42)] }`.
- Raw attrs include `v6 -> { Invalid ::(`latent_use_after_free`)line 25, column 40[...] }`.
- The path condition includes `x = 0@2` / `term_eqs: 0=v2∧42=v6`.

This proves the imported callee post attribute is preserved as an ordinary `Invalid(ConstantDereference(42))` attr and wrapped with call history. It is not a sideband.

### `deref_then_free_then_deref_bad` has no concrete `Invalid(0)` on the local free(NULL) split

In `deref_then_free_then_deref_bad.ee87bf63ca3dbce3.html`:

- `#0: LatentInvalidAccess(x)` has:
  - current post `x -> v2 -> v3`, where `v2` has `{ Initialized, WrittenTo(...) }` only.
  - `v3` has `{ Initialized, Invalid assigned at line 28 ... ConstantDereference(is assigned to the constant 42) }`.
  - inferred pre has `x = 0@0` (`conditions: {v2 = 0@0}; term_eqs: 0=v2∧42=v3`).
- `#1: AbortProgram` has the manifest UAF path:
  - `v2` has `Invalid ... CFree(...)` and `WrittenTo(...)`.
  - `v3` still has the stored-constant `Invalid(ConstantDereference(42))`.

The local null/free split address (`v2`, `x.*`) does **not** have `Invalid(ConstantDereference(0))` in OCaml. The only concrete invalid attr in the latent null split is the stored constant (`42`) on the payload value (`v3`). This is the exact distinction the failed local sideband attempt regressed.

## OCaml source chain

### Producer of concrete constant invalid attrs

`infer/src/pulse/PulseOperations.ml`:

- `eval_to_value_origin` case `Const (Cint i)` (around line 304) calls `PulseArithmetic.absval_of_int`, then `AddressAttributes.invalidate ... (Invalidation.ConstantDereference i) ...`.
- `write_access` / `write_deref` (around lines 178-184) first `check_addr_access path Write` on the address, then writes the evaluated constant value into memory.
- `check_addr_access` (around line 17) calls `AddressAttributes.check_valid`; on `Write` it initializes the written address but does not strip attrs from the object value.

This is why stores of `42` create a concrete `Invalid(ConstantDereference(42))` attr on the stored value in both procedures.

### Imported/recovered latent-invalid attr preservation

`infer/src/pulse/PulseInterproc.ml`:

- `materialize_pre` (around line 801) reads the callee pre, then calls `conjoin_callee_arith`, and finally `add_attributes \`Pre` over `call_state.callee_pre.attrs`.
- `apply_post` (around line 1146) does `apply_unknown_effects`, `apply_post_from_callee_pre`, `apply_post_from_callee_post`, and then `add_attributes \`Post path (AbductiveDomain.Summary.get_post call_state.callee_summary).attrs`.
- `add_attributes` (around lines 767-797) maps each callee attr through `caller_attrs_of_callee_attrs`, then either `AddressAttributes.abduce_all` for pre attrs or `AddressAttributes.add_all` for post attrs.
- `caller_attrs_of_callee_attrs` (around lines 751-765) calls `Attributes.add_call_and_subst`.
- `infer/src/pulse/PulseAttribute.ml` `add_call_and_subst` for `Invalid (invalidation, trace)` (around lines 579-600) returns `Invalid (invalidation, add_call_to_trace trace)`.
- `infer/src/pulse/PulseAbductiveDomain.ml` `SafeAttributes.add_one` (around line 737) adds all post attrs (and initializes on `WrittenTo`); it has no special case stripping `Invalid`.

Therefore imported post attrs from a `LatentInvalidAccess` summary are preserved literally and only get call history wrapped. This is the OCaml mechanism that keeps the stored-constant invalid attr on `manifest_use_after_free` / `main` when `latent_use_after_free` is applied.

### Sideband path for local EqZero / summary creation

`infer/src/pulse/PulseAbductiveDomain.ml`:

- Inner `incorporate_new_eqs astate new_eqs` (around line 1105) handles `EqZero v`:
  - if stack allocated: Unsat;
  - if heap allocated and `SafeAttributes.get_must_be_valid` exists: return `Sat (astate, Some (v, (trace, reason_opt)))`;
  - it does **not** add `Invalid(ConstantDereference(0))`.
- Outer `incorporate_new_eqs new_eqs astate` (around line 2467) maps that sideband to `Error (\`PotentialInvalidAccess (astate, address, must_be_valid))` for normal/local analysis.
- `filter_for_summary` (around line 2086) calls `Formula.simplify ...` and returns `new_eqs`.
- `Summary.of_post_` (around lines 2243-2310) immediately calls the inner `incorporate_new_eqs astate new_eqs`; `Some(address, must_be_valid)` becomes `Error (\`PotentialInvalidAccessSummary (astate, astate_before_filter, Decompiler.find address ..., must_be_valid))`.
- `PulseSummary.ml` `exec_summary_of_post_common` (around lines 86-170) converts `PotentialInvalidAccessSummary` into `Stopped (LatentInvalidAccess ...)` when no existing `Invalid` attr is found on the invalid address; if an `Invalid` is present, it reports `AccessToInvalidAddress` instead.
- `PulseReport.ml` `summary_of_error_post` / `report_summary_error` (around lines 283-334) performs the same conversion for error-post summarization.

This is why `deref_then_free_then_deref_bad` can export a `LatentInvalidAccess(x)` for `x == 0` without attaching `Invalid(ConstantDereference(0))` to `x.*`.

## Rust source chain / delta

Rust already has parts of both mechanisms, but they are split and incomplete:

- `infer-rs/crates/pulse/src/operations.rs::eval_const` (around line 338) mirrors OCaml constant evaluation by invalidating every integer literal. This is correct for stored constants like `42`.
- `infer-rs/crates/pulse/src/interproc.rs::apply_summary_with_aliasing` applies post attrs only in Step 5 (around lines 392-412): for every callee post attr, `caller_state.post.attrs.add_one(caller_addr, translate_attribute(...))`.
- `interproc.rs::translate_attribute` (around line 1928) preserves `Attribute::Invalid(invalidation, history)` by cloning it. **But unlike OCaml `PulseAttribute.add_call_and_subst`, it does not wrap Invalid/MustBeValid/WrittenTo histories with call context.** This is a history parity gap, not the main attr-retention gap.
- `abductive.rs::apply_formula_result_for_summary_import` / `incorporate_new_eqs_for_summary_import` (around lines 691/738) already have an imported `EqZero` sideband: `ImportedFormulaEffect::PotentialInvalidAccess(AbstractValue)` and it explicitly does not persist a synthetic invalid attr.
- `summary.rs::materialize_visible_constant_invalidations` (around line 581) deliberately skips `constant == 0`, so summary export will never recreate a missing `Invalid(ConstantDereference(0))` from phi alone.
- `summary.rs::recovered_invalid_access_pre_posts_from_abort_state` (around line 1614) and `recovered_invalid_accesses_from_continue_state` (around line 1558) synthesize/recover latent-invalid preposts, but there is no OCaml `PotentialInvalidAccessSummary`-style import/recovery step that ensures the concrete imported post attrs from the source latent-invalid/continue state remain attached to the recovered latent row while also keeping local EqZero as a sideband.

The prior failed commit `c9cc32e7f4` showed the trap: it added a local `Attribute::PotentialInvalidAccess` sideband and then stripped that attr from all exported rows, including `recovered_invalid_accesses`. That fixed the local free(NULL) shape but also stripped/failed to preserve the concrete invalid attr that `latent_use_after_free` needs on recovered/imported latent-invalid paths.

## Exact reconciliation

- OCaml concrete constant invalid attrs are normal `Invalid` post attrs and are preserved by `PulseInterproc.add_attributes \`Post` through `PulseAttribute.add_call_and_subst`.
- OCaml `EqZero + heap allocated + MustBeValid` is not an attr. It is a sideband from `incorporate_new_eqs`, and `Summary.of_post_` converts it to `PotentialInvalidAccessSummary` / `LatentInvalidAccess` without writing `Invalid(0)` on the address.
- Therefore the Rust fix must not globally strip `Invalid(ConstantDereference(_))` from latent/recovered rows. It should only consume a local EqZero sideband marker (if introduced), while preserving real imported post attrs (`Attribute::Invalid`) and wrapping histories.

## Concrete fix sketch

Implement as a narrow import/recovery-aware sideband split:

1. Reintroduce a local EqZero sideband in `abductive.rs`, but keep it out of `Attribute::Invalid`. Prefer returning a `FormulaEffect::PotentialInvalidAccess { addr, must_be_valid }` from local formula application (or a non-exported marker that is consumed before summary export). Do **not** express it as a normal exported post attr.
2. Wire local consumers:
   - `operations`/`transfer` callers of local prune/equality application should stop/branch on the sideband, so `models/c.rs::free` null branch becomes an OCaml-style no-op/potential-invalid split without `Invalid(ConstantDereference(0))` on the freed address.
   - `summary.rs::normalize_with_summary_info` / `PulseSummary::of_proc` should consume the sideband before exporting, analogous to `Summary.of_post_` consuming `new_eqs`.
3. Add the missing imported/recovered preservation in `interproc.rs` and `summary.rs`:
   - `interproc.rs::translate_attribute` should take `callee_procname`/`loc` and wrap `Invalid`, `WrittenTo`, `MustBeValid`, `MustBeInitialized`, etc. histories/locations like OCaml `Attribute.add_call_and_subst` (or add a `translate_attribute_with_call` used by both pre and post imports).
   - `summary.rs::recovered_invalid_access_pre_posts_from_abort_state` and `recovered_invalid_accesses_from_continue_state` must preserve ordinary `Attribute::Invalid` attrs from the source normalized state on the recovered `LatentInvalidAccess` rows. If a local `PotentialInvalidAccess` marker is present, strip only that marker, not `Invalid`.
4. Add pinned tests:
   - `latent_use_after_free` summary/caller import keeps the stored-constant `Invalid(ConstantDereference(42))` on the imported/recovered latent-invalid row.
   - `deref_then_free_then_deref_bad` local null/free split has no `Invalid(ConstantDereference(0))` on `x.*`, while still retaining the stored-constant `Invalid(ConstantDereference(42))` on the payload.

Most likely edit sites: `infer-rs/crates/pulse/src/abductive.rs`, `operations.rs`/`transfer.rs`/`models/c.rs` for local sideband propagation, `summary.rs` for summary-of-post sideband consumption and recovered-row attr preservation, and `interproc.rs::translate_attribute` / Step 5 import for OCaml `add_call_and_subst` parity.

## Updated effort estimate

Follow-up implementation estimate: **1.25-1.75 days**.

- ~0.5d: local EqZero sideband reintroduction and `free(NULL)` consumption without exported `Invalid(0)`.
- ~0.5d: imported/recovered latent-invalid attr preservation plus call-history wrapping in `interproc.rs`.
- ~0.25-0.5d: summary export/recovery tests and latent.c/specialization.c scoped parity triage.
- Contingency: +0.25d if the existing heuristic `PotentialInvalidAccessSummaryCandidate` pipeline needs tighter replacement by an exact `simplify -> new_eqs` API.
