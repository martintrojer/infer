# Whole-program OpenSSL findings (74-file partial capture)

First pass at track (B) from `CONVERGENCE_NEXT_STEPS.md`: validate the
Phase 1 Arc savings on whole-program OpenSSL.

## Bench

The `make -k` capture pipeline kept stalling silently mid-build on this
host, so the corpus is the 74-file partial export it had managed before
dying:

- Source tree: `~/infer-rs-bench/openssl-20260501-084151/openssl-1.0.2d/`
- Capture DB: `~/infer-rs-bench/openssl-20260501-084151/infer-out/`
  (74 source files, 571 procedures)
- Textual export:
  `~/infer-rs-bench/openssl-20260501-084151/textual-out/`
  (74 `.sil` files, ~3.9 MB / ~93k lines total)

This is much smaller than the historical 753-file shared corpus, but it's
sufficient to multiply the per-procedure cost by enough to surface
multi-procedure scaling problems.

## Headline result: per-procedure Arc gains do not survive scaling

Running both engines on the same 74-file corpus at `-j 1`:

| metric                | OCaml                   | Rust (post-Arc)                |
|-----------------------|--------------------------|--------------------------------|
| wall time             | `42.9s`                  | `494.75s` (`~11.5×` slower)    |
| max resident set size | `~1.17 GB`               | `~23.43 GB` (`~20×` more)      |
| peak memory footprint | `~1.10 GB`               | `~236.69 GB` (cumulative)      |
| exit                  | clean (`0`)              | terminated abnormally (`1`)    |
| specs / procs         | `447` summaries written  | terminated before completion   |

## Reframe vs the single-procedure picture

The single-procedure `whirlpool_block` filtered probe showed Rust
post-Arc using `~3.93 GB` peak vs OCaml's `~10 GB` peak — Rust was
`~2.6×` *more efficient* on that one procedure. The 74-file
multi-procedure run flips that completely: OCaml ends up using
`~20×` *less* peak memory than Rust.

What that means:

- Rust's per-procedure peak is comparable to OCaml's when run in
  isolation, but **across a 74-file corpus we accumulate state we never
  release**, and the cumulative resident set climbs to ~`23 GB`.
- OCaml's per-procedure analysis can hit `~10 GB` peak too (on
  `whirlpool_block`), but its whole-program peak stays `~1.17 GB`
  because it aggressively releases per-procedure transient state
  between procedures.

The Phase 1 Arc work was a real win on per-procedure peak (`16.7 GB`
\u2192 `3.93 GB` on the filtered slice). But at scale the dominant cost is
not per-state size; it is **per-procedure state retention across
procedure boundaries**. That is a different problem, and Phase 1 Arc by
itself does not address it.

## Hypotheses for the per-procedure retention gap

In rough priority order:

1. **Summary cache retention.** We may keep full
   `AbductiveDomain`-shaped summaries (heavy heap / attrs / formula)
   alive in the per-program summary cache for every analyzed procedure.
   OCaml normalizes summaries down to a much sparser external
   representation and frees the working state.
2. **Per-procedure invariant maps not dropped after fixpoint.** If we
   hold the per-procedure
   `interp::InvariantMap<DisjunctiveDomain<ExecutionDomain>>` after the
   fixpoint completes, the retained per-disjunct state for every node
   stays resident until the analysis run ends.
3. **Implicit dependency re-analysis.** If we re-analyze each callee
   from scratch at every call site instead of cached summaries, the
   per-call peak adds up on a 74-file corpus the way it did not on a
   single-procedure run.
4. **Worker / scheduler accumulating per-procedure caches.** Even at
   `-j 1`, retained caches outside the per-procedure scope (e.g.,
   global tenv, model cache, or a single ondemand worker holding stale
   `AbductiveDomain`s) would show up as continual growth.
5. **Top-up `Arc` clones across procs.** The Phase 1 Arc work shares
   the heap / attrs / stack / phi *within a single fixpoint*. Once one
   procedure finishes, those Arcs would naturally drop unless we copy
   them into a long-lived summary for callers. If we do, that explains
   why Arc helps per-procedure peak but not whole-program peak.

The expected fix is probably layered: shrink the per-procedure summary
representation we keep around (1, 5), then make sure invariant maps are
dropped at procedure-end (2), then revisit ondemand caches (4).

## What this changes in `CONVERGENCE_NEXT_STEPS.md`

- (B) is no longer "validate Arc savings on whole-program OpenSSL." The
  Arc savings did not survive scaling. (B) becomes "diagnose why
  per-procedure state isn't released across procedure boundaries."
- (A) and (C) remain as before, but the per-state CPU / unique-value
  question (the leftover from (A)'s reframe) and the smaller-Arc
  candidates of (C) are both lower-priority than this new track.

This file is the artifact of the first whole-program OpenSSL pass.
Future passes should append findings here rather than overwriting.
