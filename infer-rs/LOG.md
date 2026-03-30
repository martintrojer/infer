# Debug Log

This file is for short-lived but important debugging context that should survive chat compaction.
Keep it current when the active line of investigation changes.

## Current Focus

The active line has moved from latent-condition cleanup to the remaining specialization/reporting
conflicts.

Latest authoritative sweep:

- `NPE: expected 135, found 138`
- `LEAK: expected 20, found 22`
- `UAF: expected 7, found 6`

What improved this turn:

- `funptr.c` recovered from `6` issues up to `10` after replacing the fake specialization-request
  replay with requests collected from the actual fixpoint state at each call.
- `memory_leak.c` improved from `15` to `14` issues.
- `specialization.c` NPE parity is now exact (`4`), after making alias specialization equalities
  part of `phi` rather than exported depth-1 conditions.
- `nullptr.c` also improved slightly (`16` vs previous `17`).

What is still open:

- `funptr.c` is still short by `1` NPE.
- `cleanup_attribute.c` still has `+2` leaks.
- `specialization.c` regressed to missing the single expected UAF
  (`call_may_double_free_if_alias_bad`).

Current strongest diagnosis:

1. Specialized-summary request collection is now coming from the actual caller state during the
   fixpoint, not from the previous hand-rolled replay. That fix is correct and should stay.

2. The remaining specialization conflict is now about **how specialized invalid-access summaries are
   classified and reported**, not about whether the specialization was requested.

3. `call_test_alias_bad` / `call_test_unalias_bad` were staying latent because
   `specialization.rs` used `and_condition_direct(..., depth=1)` for alias groups. OCaml
   `PulseSpecialization.apply` uses `PulseArithmetic.prune_binop`, so the Rust side was corrected to
   use `state.and_equal(...)` instead. This fixed those callers.

4. The remaining missing UAF strongly suggests the current Rust latent/manifest logic still lacks an
   OCaml-equivalent `LatentInvalidAccess` / `PotentialInvalidAccessSummary` path. After the alias
   equalities moved into `phi`, `may_double_free_if_alias` no longer has imported conditions to keep
   the invalid access latent, but OCaml still treats it as latent and reifies it in the caller.

5. `funptr.c` still misses the single expected `apply_funptr_with_intptrptr_and_after` issue
   because we do not yet have a clean way to report a specialized callee diagnostic exactly once.
   The previous caller-level propagation over-reported it twice (both callers). Stripping manifest
   diagnostics from cached specialized pre/posts avoids the duplicate callers, but without a proper
   specialized-diagnostic reporting path it under-reports by one.

## Non-Negotiable Guidance

- Read and follow `CLAUDE.md` before changing Pulse/formula/interproc code.
- Cross-reference every analysis change against the OCaml source in `infer/src/pulse/`.
- Correctness over numbers: confirm the semantic fix first, then investigate totals.
- Run `make check` before closing work if possible.

## Files Touched In This Checkpoint

- `crates/cli/src/main.rs`
- `crates/pulse/src/checker.rs`
- `crates/pulse/src/formula/mod.rs`
- `crates/pulse/src/interproc.rs`
- `crates/pulse/src/specialization.rs`
- `crates/pulse/src/summary.rs`
- `crates/pulse/src/transfer.rs`
- `crates/pulse/tests/end_to_end.rs`
- `LOG.md`

## Confirmed Changes Already Made

1. `crates/pulse/src/abductive.rs`
   `read_heap` now matches OCaml `SafeMemory.eval_edge` more closely:
   - return existing post edge without overwriting pre
   - only abduce into pre when the root is already present in pre
   - register new pre targets

2. `crates/pulse/src/summary.rs`
   `is_manifest` is now less naive:
   - collect formal-derived values from the pre heap, not the post heap
   - inspect atoms plus constant equalities
   - treat linear equalities as an undirected dependency graph

3. `crates/pulse/src/summary.rs`
   Added a first Rust port of OCaml `restore_formals_for_summary`.

4. `crates/pulse/src/base_memory.rs`
   Added `BaseMemory::remove`.

5. `CLAUDE.md`
   Added a `Correctness Over Numbers` section capturing the user's guidance.

6. `crates/pulse/src/base_memory.rs`
   Added `BaseMemory::retain_reachable` so summary filtering can drop dead heap cells.

7. `crates/pulse/src/summary.rs`
   Strengthened `PrePost::normalize()` to get closer to OCaml `filter_for_summary`:
   - trim unreachable pre/post heap cells, not just attrs
   - simplify the summary path condition to live values
   - retain only reachable `must_be_valid` and specialization values
   - keep leak checking on the pre-filter state
   - added unit tests for dead local heap/formula trimming

8. `crates/pulse/src/state_cmp.rs`
   Added alpha-equivalence canonicalization for Pulse states:
   - compare states modulo abstract-value renaming instead of raw IDs
   - keep disconnected attr-only values visible so leak-relevant states do not collapse
   - covers heap, attrs, formula, `must_be_valid`, and specialization values

9. `crates/pulse/src/execution_domain.rs` + `crates/absint/src/disjunctive.rs`
   Disjunctive join/leq now use semantic state comparison (`Comparable::leq`) instead of plain
   structural equality on raw abstract values. This is the direct Rust-side analogue of OCaml's
   `PulseExecutionDomain.leq` / `PulseAbductiveDomain.leq` being used during widening.

10. `crates/pulse/src/models/c.rs`
    `free()` now mirrors OCaml `Basic.free_or_delete` more closely:
    - only keep satisfiable `ptr == 0` and `ptr > 0` branches
    - regression test added for known-nonnull `free`
    - fixed the `lists.c` leak explosion (`delete_all` stabilizes at 4 disjuncts again)

11. `crates/pulse/src/checker.rs` + `crates/pulse/src/summary.rs`
    Latent-vs-manifest reporting is now decided by summary-style classification instead of publishing
    every raw `AbortProgram` seen during the checker scan.
    - non-exit aborts are still scanned, but only published if they are manifest
    - `of_proc` now emits manifest abort diagnostics itself after latent reclassification
    - `is_manifest` now ignores benign non-null constraints on allocated / must-be-valid /
      already-invalid values
    - `main` is treated as an entry point for this classification

12. `crates/pulse/src/formula/mod.rs` + `crates/pulse/src/interproc.rs` + `crates/pulse/src/transfer.rs` + `crates/pulse/src/summary.rs`
    Added OCaml-style prune-condition provenance:
    - local `Prune` conditions are recorded at depth `0`
    - imported callee conditions are translated at depth `depth + 1`
    - summary `is_manifest` now checks recorded conditions instead of raw atoms/equalities
    - formula simplification now trims dead conditions too
    - this restores `assert.c` and `ternary.c` without reintroducing the latent/base bug

## Validation Already Run

- `cargo fmt --all`
- `make check`
  - passed cleanly on 2026-03-30 after the specialization/interproc changes
- `cargo test -p pulse --lib -- --nocapture`
  - now `148 passed`
- Focused alpha-equivalence tests added and passing:
  - duplicate disjuncts with renamed abstract values collapse
  - disconnected leak-only state does not collapse away
- Targeted direct CLI checks:
  - `interprocedural.c` now matches the direct OCaml issue set again (6 issues)
  - `latent.c` dropped from 6 spurious UAFs to the expected 3 UAFs; one loop-depth NPE gap
    remains
  - `assert.c` now reports the expected 1 NPE again
  - `ternary.c` now reports the expected 3 NPEs again
- Latest focused validations after the condition-shape fix:
  - `cargo test -p pulse --test end_to_end test_debug_follow_ret -- --nocapture`
    - latent propagation remains latent through summaries and reifies at entry points
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --nocapture`
    - traversal conditions no longer collapse to tautologies like `x = x`
    - `manifest_use_after_free` / `deref_then_free_then_deref_bad` still emit both NPE and UAF
  - `cargo test -p pulse --test end_to_end test_debug_specialization_summary -- --nocapture`
    - after the alias-specialization change:
      - `call_test_alias_bad` now reports `NULLPTR_DEREFERENCE`
      - `call_test_unalias_bad` now reports `NULLPTR_DEREFERENCE`
      - `call_may_double_free_if_alias_bad` currently has no issue
  - `cargo test -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
    - `NPE: expected 135, found 138`
    - `LEAK: expected 20, found 22`
    - `UAF: expected 7, found 6`

## Remaining Sweep Differences

Authoritative sweep command:

```bash
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

Current file-level diffs from the latest sweep:

- NPE under: `compound_literal.c` (-1), `funptr.c` (-1), `initlistexpr.c` (-3),
  `memory_leak.c` (-2)
- NPE over: `angelism.c` (+1), `integers.c` (+2), `nullptr.c` (+2),
  `nullptr_more.c` (+2), `offsetof_expr.c` (+1), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2), `memory_leak.c` (+1)
- UAF under: `specialization.c` (-1)

## Relevant OCaml Cross-References

- `infer/src/pulse/PulseAbductiveDomain.ml`
  - `restore_formals_for_summary`
  - `filter_for_summary`
  - `discard_unreachable_`
  - `check_memory_leaks`

- `infer/src/pulse/PulseInterproc.ml`
  - `materialize_pre_from_actual`
  - `materialize_pre_from_address`
  - `apply_post`

- `infer/src/pulse/PulseSummary.ml`
  - `exec_summary_of_post_common`

- `infer/src/pulse/PulseArithmetic.ml`
- `infer/src/pulse/PulseFormula.ml`

## Current Line Of Thinking

### What was just confirmed

- The latest `Formula::simplify` fix is correct and stable.
- Summary conditions now preserve caller-relevant atom shape instead of normalizing through `phi`.
- `interprocedural.c` is back to the expected 6 issues.
- `latent.c` count is back in line with OCaml.
- Specialization requests must be collected from the real fixpoint caller state; the previous
  replay-based request builder was wrong.
- Specialized alias equalities belong in `phi` (`and_equal`), not as imported depth-1 conditions.

### Immediate next edits

- Keep the specialization-request collector that runs during the real fixpoint.
- Investigate an OCaml-aligned Rust analogue of `LatentInvalidAccess` /
  `PotentialInvalidAccessSummary` so `may_double_free_if_alias` becomes latent in the callee and
  reifies in `call_may_double_free_if_alias_bad`.
- Add a proper reporting path for specialized callee diagnostics so
  `apply_funptr_with_intptrptr_and_after` can be reported once without duplicating across callers.
- When touching either path, cross-check against:
  - `infer/src/pulse/PulseSummary.ml`
  - `infer/src/pulse/PulseAbductiveDomain.ml`
  - `infer/src/pulse/PulseCallOperations.ml`

Largest current authoritative diffs:

- NPE under: `compound_literal.c` (-1), `funptr.c` (-1), `initlistexpr.c` (-3),
  `memory_leak.c` (-2)
- NPE over: `angelism.c` (+1), `integers.c` (+2), `nullptr.c` (+2), `nullptr_more.c` (+2),
  `offsetof_expr.c` (+1), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2), `memory_leak.c` (+1)
- UAF under: `specialization.c` (-1)

### Still-open structural hypothesis

Rust summary application still stores `PrePost.formals` as formal stack addresses and uses the
explicit `Step 1a` dereference workaround in `interproc.rs`.

This may still be correct enough for current parity work, but it remains the likeliest deeper
interproc mismatch if the next targeted bug points back into summary materialization.

## Most Likely Next Steps

1. Keep the correctness-first fixes even where totals are still worse than OCaml.
2. Port the missing OCaml latent-invalid-access path so `specialization.c` regains the expected
   UAF without undoing the alias-specialization fix.
3. Add a proper single-publication path for specialized callee diagnostics so `funptr.c` can regain
   its missing issue without double-reporting.
4. Re-run the ignored sweep after each correctness change and update `TODO.md` / `STATUS.md` only
   from the new authoritative counts.

## Useful Commands

Single-file Rust run on a C test:

```bash
cargo run -p infer-rs -- --pulse-only --results-dir /tmp/debug/infer-out -o /tmp/debug/out -- \
  clang -c /Users/mtrojer/infer-rs/infer/tests/codetoanalyze/c/pulse/lists.c
```

OCaml summary dump:

```bash
infer -j 1 --pulse-only -o /tmp/debug_out -- clang -c file.c
infer debug -j 1 --dump-json-summaries -o /tmp/debug_out
```

## Catch-Up Checklist

If resuming after compaction:

1. Read `CLAUDE.md`
2. Read this file
3. Check `git status`
4. Re-open:
   - `crates/pulse/src/summary.rs`
   - `crates/pulse/src/formula/mod.rs`
   - `crates/pulse/src/interproc.rs`
   - `crates/pulse/src/abductive.rs`
   - `infer/src/pulse/PulseAbductiveDomain.ml`
   - `infer/src/pulse/PulseInterproc.ml`
   - `infer/src/pulse/PulseFormula.ml`
