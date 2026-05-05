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

## Update: per-procedure peak_rss heartbeats

Added `peak_rss=...` to the Pulse `done:` heartbeat (commit
`cacd0973cb`) and re-ran a 15-file OpenSSL slice with
`--trace-ondemand`. First three procedures to finish:

```
proc=private_AES_set_encrypt_key done: elapsed=49.0s ... peak_rss=252MB
proc=AES_encrypt                  done: elapsed=15.8s ... peak_rss=308MB
proc=AES_decrypt                  done: elapsed=15.8s ... peak_rss=378MB
```

That is `~+50-70 MB` per finished procedure, even on small / fast
procedures. Extrapolating to `571` procedures gives `~30 GB` of
cumulative growth, which matches the observed `~23 GB` whole-program
max RSS reasonably well.

The same trace run also showed a hard outlier: `DES_ede3_cfb_encrypt`
(in `cfb64ede.sil`, `276` nodes / `~1822` Procdesc::size) was still
running after `~17 minutes` with `4400+` retained disjuncts and
`~726k` total post stack entries / `~488k` total addrs in attrs / `~9k`
const_cache entries in retained sums. OCaml runs the entire same
74-file corpus in `42.9s`, so this procedure must take seconds in
OCaml. We have the same kind of byte-loop encryption pattern as
`whirlpool_block`, but the absolute disjunct count here
(`4400+` vs whirlpool_block's `8`) is dramatically worse.

So the picture splits into two distinct gaps, both real:

- **Per-procedure baseline accumulation**: ~`+50-70 MB` per finished
  procedure. Likely the SummaryStore (or some other long-lived cache)
  retaining heavy `AbductiveDomain`-shaped state per analyzed
  procedure.
- **Per-procedure outlier explosion**: a small number of procedures
  (`DES_ede3_cfb_encrypt`, plausibly the same family that includes
  `whirlpool_block`) generate `1000s` of retained disjuncts. These
  dominate total wall time *and* total memory, not the per-proc
  baseline. This is the same root cause as the deferred (A) follow-up
  on per-disjunct unique-value count vs OCaml.

Of the two, the outlier-explosion gap has the bigger lever: a single
procedure currently consumes more wall time than the entire 74-file
OCaml run.

## Update: dropping dead logical-var bindings is a per-procedure win

Follow-up: the missing-`ExitScope`-cleanup hypothesis turned out to be
right. Commit `1e2bf5cb9d` adds a per-node-exit pass that drops
`Var::LogicalVar(_)` post-stack bindings whose Ident is not live-out
of the node (driven by backward liveness, preserving the return-value
candidate). Measured impact:

- Filtered single-procedure `whirlpool_block` slice:
  `~3.93 GB` peak → **`~0.77 GB` peak** (`~5×` smaller), `4m33s`
  → `4m18s` wall (small win). We are now `~13×` below OCaml on memory
  for this one procedure (OCaml peaks at `~10 GB`, runs in `~120s`).
- 74-file whole-program corpus at `-j 1`: in a re-run with the drop
  pass on, the analyzer survived past the previous `~8 min` OOM
  point. RSS still climbed to `~24 GB` peak then started reclaiming
  to `~11.6 GB` at `54+ min`, where the run was killed manually.
  Wall time, not memory, is now the dominant blocker on the
  multi-procedure slice (OCaml: `42.9s` for the whole 74 files).

What that updates:

- **Per-procedure baseline accumulation gap** is largely closed for
  the per-procedure peak (the metric that was extrapolating to
  ~30 GB across 571 procs). The remaining whole-program RSS we still
  see at scale is likely a mix of the SummaryStore retention and
  per-disjunct cost still being too high on encryption-style outliers.
- The new top blocker on whole-program OpenSSL is **wall time**, not
  memory. The drop pass adds liveness-analysis CPU per procedure and
  per-disjunct cost remains high; together they make multi-procedure
  runs much slower than OCaml.
- The next-step queue from `CONVERGENCE_NEXT_STEPS.md` should now
  prioritize **wall-time CPU per disjunct** (the original (A)-reframe
  target: reduce per-disjunct unique-value count) over memory tracks.

## Update: OCaml also caps DES_ede3_cfb_encrypt at 20 disjuncts/node

Follow-up: re-checked OCaml's `--debug` HTML for `DES_ede3_cfb_encrypt`
specifically (in the partial corpus). OCaml retains up to **`20`**
disjuncts per node on this same procedure, hitting the same
`pulse_max_disjuncts = 20` cap as us. So the disjunct *count* per node
is not the differentiator.

What's different is then **per-disjunct cost**: each of our retained
disjuncts is much more expensive to manipulate than OCaml's. The
whirlpool_block comparison showed our per-disjunct unique-value count
at ~`1500-3000` vs OCaml's ~`487`, a `3-6×` gap. The same factor likely
applies here, multiplied by `~20` disjuncts per node `×` `~262` nodes
`×` per-iteration manipulation cost, which compounds into the observed
`~17 minutes vs <43 seconds` wall-time gap.

With this update, the "outlier explosion" framing collapses into the
original **(A) reframe** from `CONVERGENCE_NEXT_STEPS.md`: reduce
per-disjunct unique-value count by sharing abstract values across
chained field/array accesses (the way OCaml does). The single
outstanding investigative question is what specifically OCaml does
during `Load (Lfield ...)`, `Load (Lindex ...)`, and chained
`field-of-field` reads that we do not, and whether porting that yields
a proportional drop in per-disjunct value count on encryption-style
byte loops.

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

After the per-procedure `peak_rss` heartbeat data above, the picture
updates: there are *two* distinct gaps, not one.

- **Per-disjunct cost gap (top priority)**: a small number of
  procedures (`DES_ede3_cfb_encrypt`, plausibly the same family that
  includes `whirlpool_block`) sit at the `pulse_max_disjuncts = 20`
  cap on most nodes, just like OCaml does, but each of our retained
  disjuncts is `3-6×` more expensive to manipulate (more unique
  abstract values, more formula entries). That compounds across
  `~20 disj/node × ~262 nodes × fixpoint iterations` into the observed
  `~17 minutes vs <43 seconds` wall-time gap. Concrete first step:
  identify what OCaml does during `Load (Lfield ...)`,
  `Load (Lindex ...)`, and chained field-of-field reads that we do not,
  and port the equivalent value-sharing.
- **Per-procedure baseline accumulation (next priority)**: `~+50-70 MB`
  per finished procedure. Even with the per-disjunct cost fixed,
  multi-procedure runs need to release per-procedure transient state
  more aggressively (or shrink summary representation) to land near
  OCaml's `1.17 GB` whole-corpus peak.
- (C) (small remaining Arc candidates) is unchanged in priority.

## Update: pulse-max-heap-mb (commit `51b015d6dc`) unblocks scaling

Mirroring OCaml's `Pulse.exec_instr_with_oom_protection_and_path_update`
"AboutToOOM" early-exit, commit `51b015d6dc` adds an opt-in
`pulse-max-heap-mb` per-procedure heap cap. When a procedure's
analysis grows the process peak RSS beyond the configured budget, we
stop the transfer function and let the fixpoint converge on the
partial state already reached.

With `--pulse-max-heap-mb 1500` on the 74-file partial OpenSSL
corpus at `-j 1`:

- 292 / 570 procs completed in `~19 min` (vs `~30 / 570` killed at
  ~8 min OOM in the pre-cap baseline).
- 4 cap aborts fired as expected:
  `ripemd160_block_data_order`, `md5_block_data_order`,
  `md4_block_data_order`, `sha256_block_data_order`. The latter
  later completed despite the earlier abort (cap is per-`exec_node`
  check, not permanent skip).
- Per-procedure peak RSS for completed procs grows steadily to
  `~8.85 GB` over the run (still much higher than OCaml's `~1.17 GB`
  whole-corpus peak; the SummaryStore retention of completed
  procs is the residual). 
- Wall time still much slower than OCaml's `42.9s` (we spent
  `~19 min` on the same `292 / 570`-prefix workload that OCaml
  finishes in `~22s`).

Net effect: whole-program OpenSSL now completes the bulk of the
corpus where it previously ground to a halt on 3-4 outlier
procedures. The cap does not by itself make per-procedure analysis
fast — it just keeps runaway procedures from blocking the rest of
the pipeline.

## Update: AbductiveDomain.shrink_for_storage (commit `e103af207c`)

Drop apply-time-unused fields (`pre`, `const_cache`,
`need_dynamic_type_specialization`, `dynamic_types`) from each
PrePost.post before stashing the `PulseSummary` in the SummaryStore.
Measured single-procedure / 30-file impact: `~0` on per-procedure
peak. Whole-program impact requires a complete run to surface, but
the principle is correct: the cached summary should not carry
analysis-only working state.

## Headline: 74-file partial OpenSSL whole-program run completes

With `--pulse-max-heap-mb 1000 --pulse-max-wall-secs 60 -j 4` on the
74-file partial OpenSSL corpus, the run **completes cleanly** in
`~4m25s`:

| metric                       | OCaml (-j 1)    | Rust (now, -j 4)         | Rust (heap-only, -j 4) |
|------------------------------|-----------------|---------------------------|--------------------------|
| wall time                    | `42.9s`         | **`265s`** (`~4m25s`)     | `1703s` (`~28m23s`)      |
| user CPU                     | `~41s`          | `946s` (4 cores)          | `6545s` (4 cores)        |
| max RSS                      | `~1.17 GB`      | `~14.0 GB`                | `~16.8 GB`               |
| peak memory footprint        | `~1.10 GB`      | `~5.7 GB`                 | `~7.16 GB`               |
| procs analyzed               | `570 / 570`     | `570 / 570`               | `570 / 570`              |
| heap+wall aborts             | n/a             | `26 + 1 = 27 / 570`       | `21 / 570`               |
| exit                         | clean (`0`)     | clean (`0`)               | clean (`0`)              |

The `~6.4×` speedup over the heap-only run came from two changes
(commit `ae0589bc52`):

- New `--pulse-max-wall-secs` cap as a complement to the heap cap,
  for procedures whose fixpoint does not converge quickly but whose
  RSS stays low (e.g., bsearch-family with thousands of WTO revisits).
- Aborted `exec_node` returns `DisjunctiveDomain::empty(...)`
  instead of `pre.clone()`. Because the upstream WTO scheduler
  joins our return into `old_state.post`, and `empty.join(x) = x`,
  the existing post stays unchanged and the WTO loop converges in
  one or two more iterations instead of continuing to deep-clone
  heavy disjunctive domains forever after the abort flag is set.

We are now `~6.2×` slower than OCaml on wall time and `~12×` larger
on max RSS — down from `~40×` slower and `~14×` larger pre-caps,
and `~70×` slower / OOM-killed at the start of this session.

## Update: ValueSortKey landed (commit `25a67efd81`)

`samply` profile of the filtered single-procedure `whirlpool_block`
slice showed `Canonicalizer::partial_value_label` as the single
hottest function with `>20%` of self-time, driven by the
`sort_by_key` calls in `propagate_*` allocating a `String` purely
for `Ord` comparison. Replaced with `ValueSortKey` /
`AccessSortKey` / `EdgeSortKey` typed enums (commit `25a67efd81`).

Measured impact:

- Filtered single-procedure `whirlpool_block`: `202s` → `121s`
  (`~40%` wall-time reduction). We are now at OCaml parity on this
  slice (OCaml: `~120s`).
- 74-file partial OpenSSL whole-program at `-j 4` with default caps
  (`pulse-max-heap-mb=2048`, `pulse-max-wall-secs=120`):
  `545s` → `480s` (`~12%` wall-time reduction), `20 GB` → `~16.5 GB`
  max RSS (`~17%` reduction), aborts unchanged at `~19`.

We are now `~11.2×` slower than OCaml on the whole-program corpus,
down from `~12.7×` before this commit. The smaller relative win
on whole-program (vs the `~40%` win on the single procedure)
reflects that not every procedure spends most of its time in
canonicalization — it dominates only on the encryption-style
byte-loop family (whirlpool_block, des / md / sha block_data_order,
etc.).

## Update: remaining sort_by_key call sites converted (commit `d5d0488bd2`)

Follow-on profile after `25a67efd81` showed `partial_value_label`
still at the top of the profile (`~16%` of self-time). The
remaining hot call sites in `propagate_formula` were `equalities`,
`linear_eqs`, `intervals`, and `fn_apps` `sort_by_key` calls that
still went through the String-building helpers. Converted them all
to `partial_value_key`-based sorts.

Measured impact:

- whirlpool_block: `121s` → **`81.66s`** (`~33%` wall-time win,
  cumulative `~60%` from pre-`25a67efd81` `202s`). **We are now
  `~32%` faster than OCaml on this slice** (OCaml `~120s`).
- 74-file OpenSSL whole-program at `-j 4` with default caps:
  `480s` → **`195s`** (`~59%` wall-time win), `16.5 GB` → `19.3 GB`
  max RSS (slightly higher because more procs run to completion
  within budget). `0` wall-cap aborts (was `2`) — every procedure
  now finishes within `120s`. `17` heap aborts unchanged.

Whole-program slowdown vs OCaml: `11.2×` → **`4.5×`**. From `~70×`
slower and OOM-killed at the start of the perf sessions to `~4.5×`
slower with clean exits.

This is a major scaling milestone: pre-cap, the same run was
OOM-killed at `~30 / 570` procs after `~8 min`. With the cap +
parallelism, we now complete all 570 procs in `~28 min` of wall
time.

We are still `~40×` slower and use `~14×` more memory than OCaml.
Both gaps now look like first-order CPU efficiency issues on a
small set of pathological procedures (`DES_ofb_encrypt`,
`OBJ_bsearch_ln`, `sha256_block_data_order`, ...) rather than
categorical correctness or scaling problems.

Observed suspicious patterns to investigate next:
- `OBJ_bsearch_ln`: `26` nodes but `max_visit_count = 6450`. With
  `pulse_widen_threshold = 3` we should stop revisiting after `~3`
  widening iterations, so `6450` per-node visits suggests a
  fixpoint-convergence bug in the WTO scheduler or in our widen
  implementation. Likely the same root cause as the long-tail wall
  time on a few procedures.
- `DES_ofb_encrypt`: large retained state (`disj=1776`, `formula
  lin=544k`). Same family as `whirlpool_block` /
  `DES_ede3_cfb_encrypt` — we already cut per-proc peak `5×` on
  `whirlpool_block` so the per-disjunct cost is the residue.

This file is the artifact of the first whole-program OpenSSL pass.
Future passes should append findings here rather than overwriting.
