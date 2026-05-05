# Next steps after the perf / scaling sessions

State as of commit `88e02e7af9`:

- Per-procedure peak: `~16.7 GB` \u2192 `~0.5 GB` on `whirlpool_block`
  (`~97%` reduction).
- Multi-procedure 74-file OpenSSL: OOM-killed at `~30 / 570` procs
  before this session, now completes `570 / 570` in `~4m25s` clean.
- Wall-time gap vs OCaml on whole-program OpenSSL: `~40\u00d7` slower
  before this session \u2192 `~6.2\u00d7` slower now.

The remaining gaps are first-order CPU efficiency on per-disjunct
analysis work, not categorical scaling problems. The major candidate
tracks below are listed by leverage and concrete actionability.

## A. Close the remaining `~6.2\u00d7` wall-time gap

**Concrete next step:** profile a representative slow procedure (or
the whole-program run) with `cargo flamegraph` / `samply` to find
specific hotspots in our per-instruction work. Likely candidates:

- `Phi::propagate_equality` and `var_eqs` union-find ops
  (`eq=2074` per-disjunct on whirlpool_block).
- `LinArith` normalization (`is_int=1152`, `lin=180` per-disjunct
  dominate).
- `Edges::find_with_history_canonicalized` linear-scan fallback for
  `ArrayAccess` misses.

**Pros:** measurable, concrete; likely to yield `~2-3\u00d7` speedups
via small targeted changes once the hotspots are identified.
**Cons:** requires setting up profiling infrastructure that may not
survive future sessions.

## B. Investigate the bsearch-family fixpoint convergence bug

`OBJ_bsearch_ex_` shows `max_visit_count = 6450` \u2014 way past
`pulse_widen_threshold = 3`. With `widen` returning `prev` after 3
iterations, the outer fixpoint should converge in `~5` visits.

Likely a bug in either:
- our `widen` (missing convergence signal \u2014 perhaps the
  `had_dropped_disjuncts` flag dance not stabilizing),
- our `leq` (not detecting equivalence after widen returns prev).

**Pros:** clean fix would remove `6450 \u2192 ~5` iterations on
bsearch-family, removing a wall-time long tail.
**Cons:** debugging fixpoint convergence is fiddly; could be deep.

## C. Set sensible cap defaults

Currently both `--pulse-max-heap-mb` and `--pulse-max-wall-secs`
default to `None`. New users get the OOM-killed behavior of pre-caps.
Set OCaml-comparable defaults so the binary is usable out of the
box.

**Pros:** trivial to implement; users immediately benefit.
**Cons:** picks a policy; might hide real bugs.

## D. Tighten per-disjunct value-count residue (`+637 / tier`)

Per `CONVERGENCE_8D4V_FINDINGS.md`, candidates: per-iteration formula
GC, fold `mark_is_int` into `find_term_value` short-circuit, redesign
value minting around `LinArith` / `Term` interning.

**Pros:** structural correctness improvement; closes the last
identified semantic gap.
**Cons:** invasive; may not yield much wall-time win on its own
since the cap acts as a safety net.

## E. Sweep the OCaml parity gaps in `TODO.md`

Compliance / correctness items:
- `nullptr.c` `+1` false positive (recency forgetting).
- `sizeof.c` `+2` (textual export drops `nbytes`).
- Various `inferconfig` model gaps (copy-specific patterns).

**Pros:** moves the actual parity-with-OCaml ledger.
**Cons:** different track from the perf / scaling work the recent
sessions have been about.

## Recommended order

1. **C** \u2014 trivial change, immediate user benefit.
2. **A** \u2014 the single biggest remaining user-facing problem;
   profiling will likely uncover small fixes that compound into a
   `~2-3\u00d7` speedup.
3. **B** \u2014 surgical fixpoint fix; small risk of getting stuck in
   debugging.
4. **D** and **E** are deeper investments / different tracks.
