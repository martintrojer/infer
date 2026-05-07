# Next steps after the perf / scaling sessions

State after the B-track fixes through commit `a709280c22`:

- Per-procedure peak: `~16.7 GB` → `~0.5 GB` on `whirlpool_block`
  (`~97%` reduction).
- Multi-procedure 74-file OpenSSL: OOM-killed at `~30 / 570` procs
  before the perf/scaling sessions, now completes `570 / 570` clean.
- Latest no-explicit-cap 74-file OpenSSL `-j 4` rebaseline:
  `226.86s`, `~14.0 GB` max RSS (`~8.8 GB` peak footprint), `20 / 570` aborts, `max_visit_count=4`.
- Wall-time gap vs OCaml on the 74-file OpenSSL corpus: `~70×`/OOM-killed
  at the start of the sessions → out-of-box `~5.3×` now (`226.86s / 42.9s`).
- `OBJ_bsearch_ex_` `max_visit_count=10001` is no longer the dominant story
  in the latest convergence probe; the long tail has shifted to bounded-visit
  DES-family / `OBJ_obj2txt` large-state cost.

Default re-baseline is done; next step is to use the benchmark helper for
repeated runs/medians and then focus on DES-family large-state procedures
rather than more `OBJ_bsearch_ex_` convergence work.

## A. Close the remaining `~6×` wall-time gap

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

### B-pass 1 (commit `0a4cd8437b`): widen-flag bug fixed

Found and fixed two divergences in `DisjunctiveDomain::widen` vs
OCaml `AbstractInterpreter.MakeDisjunctiveTransferFunctions.widen`:

1. Over-iter branch was returning `prev.clone()` with
   `had_dropped_disjuncts = true` when `!next.leq(self)`. Our
   `leq` early-rejects on flag mismatch, so `widen.leq(prev)`
   returns false and the worklist kept re-scheduling.
2. Within-iter join had no post-join leq check, so even when the
   joined state was semantically equal to `prev`, we returned a
   structurally different object that failed `equal_fast`.

Fix returns `prev` exactly in both branches. Two regression tests
locked in.

**Per-procedure speedups (whole-program OpenSSL, same binary, same caps):**

| procedure       | before | after  | speedup |
|-----------------|--------|--------|---------|
| whirlpool_block | 1m25s  | 7s     | 12x     |
| fcrypt_body     | 1m11s  | 8.7s   | 8.5x    |
| DES_ofb_encrypt | 20m44s | 2m50s  | 7.4x    |
| OBJ_bsearch_ex_ | 10m35s | 3m50s  | 2.8x    |

**Whole-program wall:** `195s -> 484s` on a noisy host. The
regression is because the previous "fast" path relied on
`OBJ_bsearch_ex_` OOMing 4x and hitting empty-on-abort; with the
widen fix it stays below heap cap and burns wall budget instead.
The net per-procedure work is much cheaper, but the *separate*
`OBJ_bsearch_ex_` convergence bug now dominates.

### B-pass 2 (open): `OBJ_bsearch_ex_` still hits `max_widens = 10001`

Even after the widen fix, `OBJ_bsearch_ex_` racks up
`max_visit_count = 10001` (the safety cap). Live snapshot:

  nodes=26 revisited_nodes=15 max_visit_count=10001
  max_node_disjuncts=20 disj=311 kinds[c=11 li=300]
  formula[lin=285308 itv=285908 int=905 eq=300]
  sets[must=3647 dyn=311 spec=0 const=285300]

Observations:
- Most disjuncts (300/311) are `LatentInvalidAccess`, propagated
  unchanged through `exec_instr`.
- Per-disjunct shape is bounded (`max_node_disjuncts=20`,
  per-disjunct formula `lin~951` `itv~953`).
- `dyn=311` -- every disjunct has a distinct dynamic-type
  attribute. Likely from `__call_c_function_ptr` resolving the
  `cmp` callback to many concrete callees per outer
  re-analysis.

Likely root causes (need investigation):
- Non-determinism in fresh `AbstractValue::mk_fresh()` IDs across
  re-executions causing structurally-different but
  semantically-equal `LatentInvalidAccess` disjuncts -- our
  `equal_fast` returns false; semantic `leq` may also fail
  because `state_cmp::alpha_equivalent_value` doesn't fully
  handle dynamic-type / specialization context.
- Per-iteration accumulating dynamic-type bindings causing
  monotonic but slow growth that never stabilizes.

Next step: enable `RUST_LOG=absint::interp=debug` on a focused
bench run to see the actual convergence-check failures.

### B-pass 2 update: timing-dependent reproduction

Tried adding a `log::trace!` diagnostic in `exec_wto_node` that
fires when `old_state.visit_count >= 100`. With the diagnostic
compiled in (no actual logging output, just the
`log_enabled!(Trace)` short-circuit + the `>= 100` check), the
whole-program OpenSSL run completed in `324s` with `OBJ_bsearch_ex_`
converging at `max_visit_count = 4` (no convergence pathology).
With the diagnostic removed (back to baseline binary), the same
benchmark on the same host saw `OBJ_bsearch_ex_` rack up
`max_visit_count = 10001` (safety cap) again in `427s`.

That points at a **scheduling / timing-dependent** convergence
bug, not a deterministic semantic bug. Hypothesis: at `-j 4`,
summary-availability order across worker threads dictates
whether `OBJ_bsearch_ex_` sees a stable callee summary set on
its first interior loop iteration. If callee summaries flip
mid-iteration, dynamic-type bindings (`dyn=311` in the
snapshot) accumulate non-deterministically across re-executions,
breaking semantic `leq` convergence.

Single-procedure `OBJ_bsearch_ex_` runs always converge in
`1.3s`. Two-file (obj_dat + obj_xref) runs also converge in
`1.3s`. Three-file (+ obj_lib) also converges. The
reproduction needs the full corpus *and* concurrent worker
scheduling.

**Recommended next steps for B-pass 3 (when picked up):**

1. Reproduce at `-j 1` to remove worker-order non-determinism.
   If still pathological, the bug is in our analysis logic. If
   not, the bug is in scheduling / re-analysis triggers.
2. Add an `assert!` (off by default behind a debug flag) that
   the same procedure analyzed at the same dependency point
   produces the same canonical summary. Detect non-determinism
   in the summary store.
3. Look at whether `compute_specialization_heap_paths` is
   stable across re-executions when the callee summary set is
   stable.

### B-pass 2 follow-up: deterministic, NOT scheduling-related

Re-ran the same whole-program OpenSSL benchmark at `-j 1` to
factor out worker-order non-determinism. Result: same bug.
`OBJ_bsearch_ex_` is analyzed multiple times as callee
summaries change downstream. The first two re-analyses finish
in `~6s` each. The **third re-analysis** stalls and hits
`max_visit_count = 10001` in `4m56s`, with `hottest_node =
44:4760`.

Whole-program `-j 1` wall: **`9964.57s` (2h46m)**, vs `-j 4`
`~427s`. Both hit the same `OBJ_bsearch_ex_` pathology, just
with different overall scheduling.

Reframed hypothesis: each re-analysis with a richer callee
summary set generates new `AbstractValue` IDs and possibly
new `dynamic_types` bindings. Eventually the body's predecessor
posts vary structurally between iterations even though they're
semantically equivalent, breaking `equal_fast` and forcing the
slower `state_cmp::alpha_equivalent` path. If `alpha_equivalent`
itself fails (perhaps because `dynamic_types` is not part of
`canonicalize`), the worklist never converges.

`canonicalize` in `state_cmp.rs` includes `pre/post.{stack,heap,attrs}`
plus full `Phi` formula. It explicitly excludes `must_be_valid`
(noted as Rust-only helper). It also excludes
`dynamic_types` and `need_dynamic_type_specialization`, which
may or may not be a real semantic gap.

### B-pass 4 (commit `ab871f0b50`): root cause fixed

Found and fixed the deterministic cause: `state_cmp::canonical_attr`
was formatting `MustBeValid` / `MustBeInitialized` / `WrittenTo`
attributes with `{attr:?}`, which includes their `Timestamp`
field. Two iterations of the same procedure can assign different
timestamps to the same logical attribute (because per-state
`next_attr_timestamp` is bumped by intervening work), so iterated
states differ structurally even when semantically equivalent.
`state_cmp::alpha_equivalent` then returns false, the worklist
re-schedules, and convergence never fires.

OCaml has the same `Timestamp.t` fields but its `leq` relies on
`phys_equal` short-circuit through structural sharing -- when
nothing changes, the same object reference is reused. We don't
have that level of sharing yet, so timestamps must be ignored in
the canonical key.

Fix: drop the timestamp from `canonical_attr` for those three
attributes while keeping the location and reason fields.

**Whole-program OpenSSL impact (j=4):**

| metric                   | pre-fix | post-fix |
|--------------------------|---------|----------|
| max_visit_count across procs | 10001   | **4**    |
| wall                     | 484s    | 374s     |
| max RSS                  | 15.4 GB | 11.8 GB  |
| aborts                   | 16      | 14       |
| OBJ_bsearch_ex_ aborts   | 1 wall  | **0**    |

### B-pass 5 (open): timestamp fix is necessary but not sufficient

Re-running the same whole-program OpenSSL benchmark 3 times
reveals the timestamp fix gives **flaky** convergence:

| run | wall  | max RSS | aborts | max_visit_count |
|-----|-------|---------|--------|-----------------|
| 1   | 374s  | 11.8 GB | 14     | **4** (converged) |
| 2   | 432s  | 15.5 GB | 15     | 10001 (failed)    |
| 3   | 412s  | 19.5 GB | 20     | 10001 (failed)    |

Run 1 was lucky. The fix made the bug INTERMITTENT (vs
consistently failing pre-fix), so it's clearly relevant, but
there's a second non-determinism source.

Hypothesis: parallel scheduling at `-j 4` controls which
callee summaries are available when `OBJ_bsearch_ex_` is
re-analyzed, which changes the `dynamic_types` bindings, which
changes downstream abstract values. `dynamic_types` is *not*
in `canonicalize`, so identical-looking states are alpha-
equivalent, but downstream behavior differs. So the analysis
genuinely produces different per-iteration results, and the
fixpoint can't converge.

### B-pass 6 (commit `7bf86fd5c9`): dynamic types now participate in canonicalization

OCaml's corresponding dynamic-type constraints live in the path
condition and participate in `PulseAbductiveDomain.leq`. Rust kept
known dynamic types separately on `AbductiveDomain.dynamic_types`,
and `state_cmp::canonicalize` ignored them. That meant two states
that can resolve function pointers differently could be treated as
alpha-equivalent even though downstream transfer behavior differs.

Added `AbductiveDomain::iter_dynamic_types()` and included
canonical value->type bindings in `CanonicalState`, stable hashes,
`debug_canonical_dump`, and `DebugSignature`. New regression test:
`test_dynamic_types_participate_in_alpha_equivalence`:

- same dynamic-type binding under different raw `AbstractValue` IDs
  remains alpha-equivalent
- presence vs absence of a dynamic-type binding is not
  alpha-equivalent

One OpenSSL 74-file `-j 4` probe after this change:

| metric          | result |
|-----------------|--------|
| wall            | 301s   |
| max RSS         | 17.7GB |
| aborts          | 16     |
| max_visit_count | 10001  |

This is the best wall time seen on the B-track but still hits the
`OBJ_bsearch_ex_` safety cap in some runs. Treat as semantic
correctness + partial stabilization, **not** the final bsearch fix.

Next probe ideas for B-pass 7:

- Inspect what specifically changes between bad iterations of
  `OBJ_bsearch_ex_` -- dump canonical sections (`pre/post attrs`,
  `formula`, `dynamic_types`) for all 20 disjuncts at `node=44` and
  compare across iterations.
- Add a fixed-seed / deterministic scheduling mode for the 74-file
  benchmark so we can stop chasing host/scheduler noise.
- Check OCaml's behavior on `OBJ_bsearch_ex_` specifically: confirm
  whether its loop-head states include the same dynamic-type surface
  and whether `PulseAbductiveDomain.leq` reaches fixpoint via
  `phys_equal` or graph isomorphism.

Slowdown vs OCaml `42.9s`: best observed `~7.0x` (301s) but flaky;
more stable runs are `~8.7x-10x`. Was `~11.3x`, OOM-killed at
start of session.

Regression tests:
- `test_alpha_equivalent_states_ignore_attribute_timestamps`
- `test_dynamic_types_participate_in_alpha_equivalence`

### B-pass 7 (commit `c9876741d2`): identity-transfer shortcut for stopped states

Observation from bad `OBJ_bsearch_ex_` runs: hot nodes such as
node 44 are repeatedly visited with `20` `LatentInvalidAccess`
disjuncts and `0` `ContinueProgram` disjuncts. Instruction
transfer on such a domain is the identity: `exec_instr` simply
clones Abort/Latent/Exit/Exception disjuncts through every
instruction. Short-circuit `PulseTransferFunctions::exec_node` so
when no active `ContinueProgram` remains it returns
`current_post.join(input)` directly.

OpenSSL 74-file `-j 4` probe after this change:

| metric          | result |
|-----------------|--------|
| wall            | 460s   |
| max RSS         | 10.7GB |
| aborts          | 15     |
| max_visit_count | 10001  |

Interpretation: safe identity-transfer optimization and best RSS
seen on the B-track, but not a CPU/convergence fix. The remaining
problem is still that the WTO worklist keeps revisiting stopped
states; we are just doing less work per revisit.

### B-pass 8 (commit `3c1a720e78`): post-stable WTO convergence

The remaining failure pattern was pre-only churn: node pre-states
kept changing enough to fail `new_pre.leq(old_pre)`, but executing
the node and joining with the retained post produced no new outgoing
post-state. Since successors depend on a node's post-state, not its
stored pre-state, re-running the enclosing WTO component solely for
pre-only churn cannot affect downstream states.

Change `exec_wto_node` to update the invariant map with the new pre
(for future delta filtering) and the joined post, but report
`ReachedFixPoint` when `post.leq(old_post)` holds even if
`new_pre.leq(old_pre)` failed.

OpenSSL 74-file `-j 4` probe after this change:

| metric          | result |
|-----------------|--------|
| wall            | 486s   |
| max RSS         | 12.3GB |
| aborts          | 19     |
| max_visit_count | **4**  |

This removes the `OBJ_bsearch_ex_` `max_visit_count=10001` safety-cap
pathology in this probe. Wall time remains noisy/worse, dominated by
large DES-family states and abort behavior, but the WTO convergence
bug is gone on this run.


### DES-family first probe: uncapped `DES_ede3_cbcm_encrypt`

Ran a focused isolated probe with caps disabled:

```sh
RUST_LOG=warn,ondemand=info target/release/infer-rs   --pulse-only --quiet --trace-ondemand -j 1   --procedures-filter DES_ede3_cbcm_encrypt   --pulse-max-wall-secs 0 --pulse-max-heap-mb 0   ~/infer-rs-bench/openssl-20260501-084151/textual-out/*.sil
```

Stopped manually after `13m40s` to avoid a long uncapped run. Key snapshot:

- analyzed dependencies: `__infer_globals_initializer_DES_SPtrans`, `DES_encrypt1`,
  `DES_ede3_cbcm_encrypt`
- `max_visit_count=4`, `max_node_disjuncts=20` — convergence is bounded
- retained states: `3241` disjuncts across `201` nodes
- retained post total: `~28.1M` heap nodes / `~55.2M` heap edges
- retained post **dead** total: `~25.6M` heap nodes / `~50.3M` heap edges
- retained post live total: `~2.5M` heap nodes / `~4.85M` heap edges
- formula total: `~5.25M` linear equations / `~7.60M` intervals /
  `~6.18M` is-int facts / `~2.57M` equalities
- process RSS at manual stop: `~6.3GB`

Interpretation: the DES long tail is not a widen/WTO visit-count problem. It is
retained-state storage dominated by dead/unreachable post graph and formula volume.
`state_cmp` already ignores disconnected retained heap/attrs for comparison, but
we still physically store and clone them in invariant maps. The next real lever is
safe retained-state GC / storage shrinking that preserves leak reporting semantics.

Concrete next probe: compare OCaml's retained invariant storage for the same proc
(or inspect `PulseAbductiveDomain.get_unreachable_attributes` / leak filtering) to
understand when it is safe to drop dead post graph without losing leak diagnostics.

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

1. **Re-baseline defaults** — run the 74-file OpenSSL corpus without
   explicit `--pulse-max-*` flags now that `pulse-max-wall-secs=60` is
   the default. Confirm wall/RSS/abort/max-visit numbers and update the
   out-of-box docs.
2. **DES-family large-state investigation** — first probe done on
   `DES_ede3_cbcm_encrypt` with caps disabled. Visits are bounded
   (`max_visit_count=4`), but retained storage grows for minutes:
   after `13m40s`, retained post totals reached `~28.1M` heap nodes /
   `~55.2M` heap edges, of which `~25.6M` heap nodes and `~50.3M`
   edges were **dead/unreachable** from post stack. Formula also grew
   to `~5.25M` linear equations / `~7.60M` intervals. This points to
   retained-state storage/GC, not WTO convergence.
3. **Benchmark plumbing** — add a helper script that runs the 74-file
   benchmark N times and extracts wall, max RSS, abort count,
   max_visit_count, and slow-proc tables to stop chasing host noise.
4. **OBJ_obj2txt straight-line state explosion** — large retained totals
   with `max_visit_count=1`; likely formula/materialization rather than
   loop convergence.
5. **D/E** — per-disjunct value-count residue and OCaml parity/reporting
   gaps are deeper investments / different tracks.

## Done so far

### C — cap defaults landed (commit `b8d102ae72`)

`pulse-max-heap-mb` now defaults to `2048` (2 GB) and
`pulse-max-wall-secs` defaults to `60s`. The 74-file partial
OpenSSL run completes cleanly out of the box with no flag tuning
(`570 / 570` procs; out-of-box rebaseline: `226.86s`, `~14.0 GB` max
RSS, `~8.8 GB` peak footprint, `20` aborts, `max_visit_count=4`). Pass
`--pulse-max-heap-mb 0` / `--pulse-max-wall-secs 0`
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

### A third-pass attempt: FxHashMap on SummaryStore (no-go)

Tried switching `SummaryStore`'s `DashMap<Procname, ...>` from
`RandomState` to `BuildHasherDefault<FxHasher>` to bypass SipHash
setup overhead on `Procname` lookups (which transitively hash
`TemplateSpecInfo` per the previous profile, `~3.6%` self-time).

Measurements were not consistent. wp_block measurements over the
same binary varied between `~80s` and `~130s` across reruns,
attributable to host noise (background processes, thermal
throttling). The whole-program measurement dropped from `~195s` to
`~393s` once and we could not reliably attribute that to the change
vs noise. Reverted for now; the workspace dep was also removed.
Will revisit with a quieter host or after more reliable
benchmarking infrastructure.

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
