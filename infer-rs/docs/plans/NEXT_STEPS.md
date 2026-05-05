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

1. **C** — trivial change, immediate user benefit.
2. **A** — the single biggest remaining user-facing problem;
   profiling will likely uncover small fixes that compound into a
   `~2-3×` speedup.
3. **B** — surgical fixpoint fix; small risk of getting stuck in
   debugging.
4. **D** and **E** are deeper investments / different tracks.

## Done so far

### C — cap defaults landed (commit `b8d102ae72`)

`pulse-max-heap-mb` now defaults to `2048` (2 GB) and
`pulse-max-wall-secs` defaults to `120s`. The 74-file partial
OpenSSL run completes cleanly out of the box with no flag tuning
(`570 / 570` procs, `~9 min` wall, `~20 GB` max RSS, `~20`
aborts). Pass `--pulse-max-heap-mb 0` / `--pulse-max-wall-secs 0`
to disable each cap (escape hatch documented in CLI help).

### A second pass — remaining sort_by_key sites converted (commit `d5d0488bd2`)

Follow-on to `25a67efd81`. The remaining `propagate_formula`
`sort_by_key` call sites (`equalities`, `linear_eqs`, `intervals`,
`fn_apps`) were still building Strings via
`partial_*_label` helpers. Converted all four to
`partial_value_key`-based sorts.

Measured impact:

- whirlpool_block: `121s` → **`81.66s`** (`~33%` wall-time win,
  cumulative `~60%` from pre-`25a67efd81` `202s`). **We are now
  `~32%` faster than OCaml on this slice** (OCaml `~120s`).
- 74-file OpenSSL whole-program at `-j 4` with default caps:
  `480s` → **`195s`** (`~59%` wall-time win). `0` wall-cap
  aborts (was `2`); `17` heap aborts unchanged.

Whole-program slowdown vs OCaml: `11.2×` → **`4.5×`**.

### A first pass — ValueSortKey landed (commit `25a67efd81`)

`samply` profile of the filtered single-procedure `whirlpool_block`
slice (`target/profiling/infer-rs` build with `dsymutil`-resolved
symbols) showed `Canonicalizer::partial_value_label` as the single
hottest function with `>20%` of self-time, driven by
`sort_by_key` calls in `propagate_*` allocating a `String` purely
for `Ord` comparison.

Replaced with `ValueSortKey` / `AccessSortKey` / `EdgeSortKey`
typed enums. Variant order chosen to match the previous String
lexicographic order so iteration is unchanged. Measured impact:

- whirlpool_block: `202s` → **`121s`** (`~40%` wall-time win,
  **at OCaml parity** on this slice; OCaml takes `~120s`).
- 74-file OpenSSL whole-program: `545s` → **`480s`** (`~12%`
  wall-time win), `20 GB` → `~16.5 GB` max RSS.

Whole-program slowdown vs OCaml: `~12.7×` → `~11.2×`. The
remaining gap is concentrated outside the canonicalization-heavy
encryption procedures.

### A third-pass candidates (next)

With the canonicalize String-allocation hotspot resolved, the
profile re-balance shifts to the remaining suspects from the
`samply` profile (still to be re-confirmed with a fresh profile
run):

- `<TemplateSpecInfo as Hash>::hash` (`typ.rs:130`) accounted for
  `~3.6%` self-time pre-`d5d0488bd2`. The `NoTemplate` variant
  should hash to a single discriminant byte, so this much time
  suggests we hash `Procname` (which embeds `TemplateSpecInfo`) an
  enormous number of times via `HashMap` lookups. Switching the
  hot HashMaps to `rustc-hash::FxHashMap` is the next obvious
  lever.
- `Vec::clone` (`mod.rs:3749`) accounted for `~3.5%` self-time
  across multiple call sites — candidates include `ValueHistory`
  cloning, `Edges::recency_bindings_cloned`, and `Atom::all_vars`.
  Worth instrumenting individual sources after the FxHashMap pass
  re-balances the profile.
- String / format operations together accounted for another
  `~5%+` self-time — mostly `core::fmt::write` paths. Likely from
  log format-arg construction even when the message is below the
  log level threshold. Audit `log::debug!` / `log::trace!` call
  sites for non-trivial format-arg work that should be guarded by
  `log::log_enabled!`.
