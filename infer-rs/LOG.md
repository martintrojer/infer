# Debug Log

This file is for short-lived but important debugging context that should survive chat compaction.
Keep it current when the active line of investigation changes.

## Current Focus

Keep the correctness-first latent/manifest classification fix in place. The latest OCaml-aligned
follow-up was adding call-depth provenance for prune conditions so local branch tests no longer
make issues latent. `assert.c` and `ternary.c` are now back. The next active target is the single
remaining UAF miss in `specialization.c`, specifically by making sure solver-discovered equalities
are incorporated back into the Rust abductive state the way OCaml does. That direct fix now
restores the expected five Rust issues for `specialization.c`, including the missing
`call_may_double_free_if_alias_bad` UAF. The active target has now moved to the new sweep
over-counts, especially `interprocedural.c` and `latent.c`, after restoring callee
`AbortProgram` propagation.

## Non-Negotiable Guidance

- Read and follow `CLAUDE.md` before changing Pulse/formula/interproc code.
- Cross-reference every analysis change against the OCaml source in `infer/src/pulse/`.
- Correctness over numbers: confirm the semantic fix first, then investigate totals.
- Run `make check` before closing work if possible.

## Current Dirty Files

- `CLAUDE.md`
- `crates/pulse/src/abductive.rs`
- `crates/pulse/src/execution_domain.rs`
- `crates/pulse/src/base_memory.rs`
- `crates/pulse/src/formula/mod.rs`
- `crates/pulse/src/formula/phi.rs`
- `crates/pulse/src/interproc.rs`
- `crates/pulse/src/state_cmp.rs`
- `crates/pulse/src/summary.rs`
- `crates/pulse/src/transfer.rs`
- `crates/absint/src/disjunctive.rs`
- `LOG.md`
- `TODO.md`
- `docs/STATUS.md`

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
- `cargo test -p pulse --lib`
  Latest result when last run: `131 passed`
- Focused alpha-equivalence tests added and passing:
  - duplicate disjuncts with renamed abstract values collapse
  - disconnected leak-only state does not collapse away
- Targeted direct CLI checks:
  - `interprocedural.c` now matches the direct OCaml issue set again (6 issues)
  - `latent.c` dropped from 6 spurious UAFs to the expected 3 UAFs; one loop-depth NPE gap
    remains
  - `assert.c` now reports the expected 1 NPE again
  - `ternary.c` now reports the expected 3 NPEs again
- Latest authoritative sweep:
  - `cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture`
  - NPE: expected `135`, found `128`
  - LEAK: expected `20`, found `24`
  - UAF: expected `7`, found `6`

## Regressions Seen After the Correctness Fixes

Authoritative sweep command:

```bash
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

Observed totals after the latest correctness-first latent + condition-depth fixes:

- NPE: expected `135`, found `128`
- LEAK: expected `20`, found `24`
- UAF: expected `7`, found `6`

Important interpretation:

- The big NPE drop is expected after stopping the incorrect publication of latent issues as manifest
  base reports.
- This is the correct direction under the user's guidance: keep the semantically correct latent fix,
  then work on recovering the now-unmasked true under-detections.

Current headline diffs:

- NPE under-detection: `funptr.c`, `initlistexpr.c`, `compound_literal.c`, `memory_leak.c`,
  `latent.c` (loop depth), `traces.c`
- NPE over-detection still present in `angelism.c`, `nullptr.c`, `nullptr_more.c`,
  `offsetof_expr.c`, `sizeof.c`
- LEAK over-detection remains only in `cleanup_attribute.c` (+2, GCC cleanup attr) and
  `memory_leak.c` (+2, funptr wrappers)
- UAF now only misses `specialization.c` (-1)

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

### What was just tested

The OCaml-style condition-depth port is internally consistent (`cargo test -p pulse --lib` passes)
and fixes the sharp manifestness regressions:

- `assert.c` direct Rust CLI now reports the expected 1 NPE
- `ternary.c` direct Rust CLI now reports the expected 3 NPEs
- the authoritative sweep improved from `121` to `128` NPEs without changing leak/UAF totals

### Current strongest open question

Why `specialization.c` still misses the aliased-actual UAF in
`call_may_double_free_if_alias_bad`.

### Latest active hypothesis

The alias-specialization builder itself is probably not the blocker anymore. The more likely gap is
that Rust has been learning `NewEq` facts in the formula solver without rewriting `pre` / `post`
/ attrs / heap-tracking sets. OCaml pushes those equalities back into the abductive state via
`PulseAbductiveDomain.incorporate_new_eqs`. The current implementation pass is to add the Rust
analogue in `crates/pulse/src/abductive.rs` and route mutating formula call sites through it.

### Latest confirmed outcome

That equality-incorporation pass did move the specialization target:

- `call_test_alias_bad` is now reported again
- `call_test_unalias_bad` is now reported again
- `call_may_double_free_if_alias_bad` UAF is now reported again

The follow-up OCaml parity fix was in `interproc.rs`: do not drop `AbortProgram` pre-posts during
summary application. OCaml `PulseCallOperations.apply_callee` preserves them, and specialized
summaries are otherwise not publishing their diagnostics anywhere else.

### New sweep baseline

Latest authoritative sweep after the specialization + abort-propagation fixes:

- NPE: expected `135`, found `138`
- LEAK: expected `20`, found `23`
- UAF: expected `7`, found `8`

Largest current diffs:

- NPE over: `interprocedural.c` (+3), `latent.c` (+3), `nullptr.c` (+2), `nullptr_more.c` (+2),
  `sizeof.c` (+2)
- NPE under: `funptr.c` (-5), `initlistexpr.c` (-3), `memory_leak.c` (-2)
- LEAK over: `cleanup_attribute.c` (+2), `memory_leak.c` (+1)
- UAF over: `interprocedural.c` (+1)

Working hypothesis: execution is now closer to OCaml, but abort/report publication timing is still
too eager after re-enabling `AbortProgram` propagation. Cross-check `PulseReport` /
`PulseSummary.exec_summary_of_post_common` before changing anything.

### Still-open structural hypothesis

Rust summary application still stores `PrePost.formals` as formal stack addresses and uses the
explicit `Step 1a` dereference workaround in `interproc.rs`.

This may still be correct enough for current parity work, but it remains the likeliest deeper
interproc mismatch if the next targeted bug points back into summary materialization.

## Most Likely Next Steps

1. Keep the latent-reporting fix; do not chase the old 135 NPE total by reintroducing latent/base
   confusion.

2. Next correctness target:
   - `specialization.c` missing UAF
   - then `memory_leak.c` funptr wrappers / leak FPs
   - then remaining NPE misses (`compound_literal.c`, `initlistexpr.c`, `funptr.c`, `latent.c`,
     `traces.c`)

3. Before any deeper interproc edit, re-check the OCaml source of truth in:
   - `PulseInterproc.ml`
   - `PulseSummary.ml`
   - `PulseAbductiveDomain.ml`

4. After the sweep, next target is abort/report publication parity:
   - inspect `interprocedural.c` and `latent.c`
   - compare Rust propagated abort reporting against OCaml `PulseReport.ml`
   - avoid undoing the now-correct specialization/UAF fix

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
