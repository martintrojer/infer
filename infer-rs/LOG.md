# Debug Log

This file is for short-lived but important debugging context that should survive chat compaction.
Keep it current when the active line of investigation changes.

## Current Focus

The active line is the next correctness cluster after fixing latent invalid-access reification,
specialized-summary publication, and the sweep expectation basename bug.

Latest authoritative sweep:

- `NPE: expected 131, found 139`
- `LEAK: expected 20, found 22`
- `UAF: expected 7, found 7`

What improved this turn:

- `specialization.c` regained the missing `USE_AFTER_FREE` in
  `call_may_double_free_if_alias_bad` after adding a Rust analogue of OCaml's latent-invalid-access
  flow.
- `funptr.c` is now at parity (`11` issues) after publishing specialized-summary diagnostics back
  into the owning callee summary in the global ondemand store.
- `compound_literal.c` and `initlistexpr.c` already match OCaml; their previous sweep diffs were
  caused by `issues_for_file()` using basename suffix matching instead of exact basename matching.
- `specialization.c` NPE parity is now exact (`4`), after making alias specialization equalities
  part of `phi` rather than exported depth-1 conditions.
- UAF sweep parity is now exact.
- Rust now supports the OCaml-compatible config flags
  `pulse-model-free-pattern`, `pulse-model-malloc-pattern`, and
  `pulse-model-realloc-pattern`, including the OCaml `Str` grouping/alternation syntax used by
  shared `.inferconfig` files such as `\\(my\\|a\\)_malloc`.

What is still open:

- `cleanup_attribute.c` has `+2` leaks.
- `memory_leak.c` is short by `2` NPEs and over by `1` leak.
- `angelism.c` has `+1` NPE.
- `integers.c` has `+2` NPEs.
- `nullptr.c` has `+2` NPEs.
- `nullptr_more.c` has `+2` NPEs.
- `offsetof_expr.c` has `+1` NPE.
- `sizeof.c` has `+2` NPEs.

Current strongest diagnosis:

1. Specialized-summary request collection is now coming from the actual caller state during the
   fixpoint, not from the previous hand-rolled replay. That fix is correct and should stay.

2. The specialization work is now in a good state semantically:
   - requests are collected from the real fixpoint caller state
   - alias specialization equalities are applied in `phi`
   - latent invalid accesses reify in callers
   - specialized callee diagnostics are published once on the owner, not per caller

3. `call_test_alias_bad` / `call_test_unalias_bad` were staying latent because
   `specialization.rs` used `and_condition_direct(..., depth=1)` for alias groups. OCaml
   `PulseSpecialization.apply` uses `PulseArithmetic.prune_binop`, so the Rust side was corrected to
   use `state.and_equal(...)` instead. This fixed those callers.

4. Rust now has the minimal analogue of OCaml's `LatentInvalidAccess` /
   `PotentialInvalidAccessSummary` path:
   - caller-visible invalid accesses in summaries stay latent instead of being forced through the
     generic manifestness classifier
   - `apply_summary` reifies them only when the translated caller address is invalid after summary
     application
   - this is the correct fix for `may_double_free_if_alias`; do not revert it even if other totals
     move temporarily

5. Specialized-summary publication must be filtered:
   - manifest specialized diagnostics should merge into the owner summary
   - diagnostics already represented by latent specialized pre/posts must NOT be merged
   - otherwise we reintroduce false extra callee reports such as `may_double_free_if_alias`

6. The next real mismatch clusters are now:
   - `cleanup_attribute.c` / `memory_leak.c` for leak behavior
   - `nullptr*`, integer, `offsetof`, and `sizeof` over-reporting for NPE behavior

7. `memory_leak.c` is now known to include a config-driven component:
   - with `--inferconfig-path ../infer/tests/codetoanalyze/c/pulse/.inferconfig`, Rust recovers
     `user_malloc_leak_bad` and `test_config_options_no_free_bad`
   - this confirms part of the old gap was missing support for
     `pulse-model-{malloc,realloc,free}-pattern`, not a Pulse core transfer bug
   - `malloc_ptr_{leak,no_check_leak}_bad` remain separate and are not fixed by this config work
   - the current ignored sweep test still uses the library pipeline directly and does not yet load
     the per-suite `.inferconfig`, so this config support will not change sweep totals until that is
     threaded through

## Non-Negotiable Guidance

- Read and follow `CLAUDE.md` before changing Pulse/formula/interproc code.
- Cross-reference every analysis change against the OCaml source in `infer/src/pulse/`.
- Correctness over numbers: confirm the semantic fix first, then investigate totals.
- Run `make check` before closing work if possible.

## Files Touched In This Checkpoint

- `crates/ondemand/src/summary.rs`
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
  - now `152 passed`
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
      - `call_may_double_free_if_alias_bad` now reports `USE_AFTER_FREE`
      - `may_double_free_if_alias` itself stays issue-free while keeping an alias-specialized summary
  - `cargo test -p pulse --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
    - `NPE: expected 135, found 139`
    - `LEAK: expected 20, found 22`
    - `UAF: expected 7, found 7`

## Remaining Sweep Differences

Authoritative sweep command:

```bash
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

Current file-level diffs from the latest sweep:

- NPE under: `compound_literal.c` (-1), `initlistexpr.c` (-3), `memory_leak.c` (-2)
- NPE over: `angelism.c` (+1), `integers.c` (+2), `nullptr.c` (+2),
  `nullptr_more.c` (+2), `offsetof_expr.c` (+1), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2), `memory_leak.c` (+1)
- UAF parity is now exact

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
- `LatentInvalidAccess`

- `infer/src/pulse/PulseArithmetic.ml`
- `infer/src/pulse/PulseFormula.ml`
- `infer/src/pulse/PulseCallOperations.ml`
  - latent invalid access reification in callers

## Current Line Of Thinking

### What was just confirmed

- The latest `Formula::simplify` fix is correct and stable.
- Summary conditions now preserve caller-relevant atom shape instead of normalizing through `phi`.
- `interprocedural.c` is back to the expected 6 issues.
- `latent.c` count is back in line with OCaml.
- Specialization requests must be collected from the real fixpoint caller state; the previous
  replay-based request builder was wrong.
- Specialized alias equalities belong in `phi` (`and_equal`), not as imported depth-1 conditions.
- The missing `specialization.c` UAF was not a specialization bug; it was the missing latent
  invalid-access path.
- `funptr.c` is no longer missing its callee-side specialized diagnostic.

### Immediate next edits

- Keep the specialization-request collector that runs during the real fixpoint.
- When touching either path, cross-check against:
  - `infer/src/pulse/PulseSummary.ml`
  - `infer/src/pulse/PulseAbductiveDomain.ml`
  - `infer/src/pulse/PulseCallOperations.ml`

Largest current authoritative diffs:

- NPE under: `compound_literal.c` (-1), `initlistexpr.c` (-3), `memory_leak.c` (-2)
- NPE over: `angelism.c` (+1), `integers.c` (+2), `nullptr.c` (+2), `nullptr_more.c` (+2),
  `offsetof_expr.c` (+1), `sizeof.c` (+2)
- LEAK over: `cleanup_attribute.c` (+2), `memory_leak.c` (+1)
- UAF parity is exact

### Still-open structural hypothesis

Rust summary application still stores `PrePost.formals` as formal stack addresses and uses the
explicit `Step 1a` dereference workaround in `interproc.rs`.

This may still be correct enough for current parity work, but it remains the likeliest deeper
interproc mismatch if the next targeted bug points back into summary materialization.

## Most Likely Next Steps

1. Keep the correctness-first fixes even where totals are still worse than OCaml.
2. Revisit the remaining summary/materialization mismatches behind `initlistexpr.c`,
   `compound_literal.c`, and the `nullptr*` / `integers.c` over-reports.
3. Re-run the ignored sweep after each correctness change and update `TODO.md` / `STATUS.md` only
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
