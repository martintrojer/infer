# OpenSSL Linux perf attack surface (2026-05)

Purpose: this is a doc-only synthesis of the historical OpenSSL performance
surface for the upcoming Linux wall/RAM profiling pass. It is not a benchmark
result, and it should be read as a map for the next worker to compare against
fresh Linux measurements.

Inputs were read in the requested order: `docs/STATUS.md`,
`docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`,
`docs/plans/CONVERGENCE_8D4V_FINDINGS.md`,
`docs/plans/CONVERGENCE_NEXT_STEPS.md`, and
`docs/plans/STRUCTURAL_SHARING_PROTOTYPE.md`. Specific historical numbers below
cite the commit+file that recorded them; where the current archive intentionally
points readers to commit messages for exact raw measurements, the direct perf
commit is cited as such.

## Section 1: Known wall-time hotspots (with historical fix attempts)

### Procedure-level map

| procedure | `.sil` file if known | historical wall/RSS signal | root cause class | historical fixes / attempts | current status |
|---|---|---|---|---|---|
| `OBJ_bsearch_ex_` | `obj_dat.sil` in the OpenSSL Textual corpus; the required docs name the proc but not the file | Old symptom: `max_visit_count=10001` in bsearch-family whole-program runs; later isolated wall was cut from `1.91s` to `~0.47s` after state-comparison work (`856a747291 docs/STATUS.md`). | Initially WTO/fixpoint convergence and unstable `state_cmp` equality; later mostly `state_cmp::canonicalize` CPU cost inside disjunctive `leq`. | B-track convergence fixes: OCaml widen semantics, timestamp stripping, dynamic-type canonicalization, stopped-state identity shortcut, and post-stable WTO convergence (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). Focused `state_cmp` fixes: cached sort keys, structural canonical formula/heap/attrs/stack/dynamic-types keys, cached propagation sort keys, flat-slab `CanonTerm`, and later hoisting canonicalization out of the `N*M` disjunctive `leq` cross-product (`2c4b11ca47`, `87e366b8d4`, `420a502eee`, `01ef6573f5`, `5d5a95bd1a`, `1608ab4eae` commit messages). | Fixes are in current HEAD. Still a must-profile sentinel because it was the canonical `state_cmp` hotspot, but the historical `max_visit_count=10001` blocker is no longer expected. |
| `DES_ede3_cfb_encrypt` | `cfb64ede.sil` (`5e07ca1a32 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`) | Early trace: still running after `~17 minutes`, with `4400+` retained disjuncts, `~726k` post-stack entries, `~488k` attr addrs, and `~9k` `const_cache` entries (`5e07ca1a32 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). Current dashboard says focused isolated wall fell from `~40.2s` after first structural fixes to `~21.8s` after cached propagation sort keys and flat-slab `CanonTerm` (`856a747291 docs/STATUS.md`). | DES-family byte-loop large-state cost: `pulse_max_disjuncts` fan-out plus per-disjunct formula/heap volume and `state_cmp::canonicalize`/`DisjunctiveDomain::leq` CPU. OCaml also hit `20` disjuncts/node, so count alone was not the differentiator (`5e07ca1a32 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Value sharing / formula work reduced related `whirlpool_block`; retained-state heap/attr GC helped DES-family storage; `state_cmp` sort-key/cache/CanonTerm fixes materially reduced focused DES wall (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`, `856a747291 docs/STATUS.md`, `01ef6573f5` and `5d5a95bd1a` commit messages). Stale `term_value_index` repair helped a selected DES target but was rejected for the default path because whole-program medians worsened and counters showed no repair hits (`11fa5b8649 docs/STATUS.md`, `edfa22d595 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Main fixes are in current HEAD; stale-key repair is parked/rejected; formula GC remains opt-in. This should be the first focused DES wall/RSS target on Linux. |
| `DES_ede3_cbcm_encrypt` | The required docs use a focused proc filter but do not record a `.sil` file; current OpenSSL corpora commonly contain it in `ede_cbcm_enc.sil` and/or `destest.sil`. | Focused uncapped probe before retained-state GC was manually stopped at `13m40s` with `~6.3 GB` RSS; after heap/attr GC it completed in `11m04s` at `~3.99 GB`; with opt-in formula GC it completed in `8m50s` at `~3.69 GB` (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Retained invariant-map storage and formula volume, not WTO convergence: `max_visit_count=4`, and retained post dead heap/edges dominated physical storage (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Large-state intermediate post heap/attr GC (`00f73c1e6d`) collapsed dead post heap from `~25.6M / ~50.3M` nodes/edges to `276 / 276`; formula GC reduced intervals/is-int but stayed opt-in because capped whole-program wall regressed on the host (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Heap/attr GC is in current HEAD. Formula cleanup is opt-in via `--pulse-intermediate-formula-gc`; full/stale repair variants are parked/rejected. Use as the focused RAM-pressure DES target. |
| `whirlpool_block` | Historical repro: `wp_block.sil` (`CONVERGENCE_8D4V_FINDINGS.md` records the full path); current OpenSSL corpora often store Whirlpool in `wp_dgst.sil`. | Phase 1 Arc baseline: `4m34s` / `~16.7 GB`; after Arc sharing: `4m33s` / `~3.93 GB`; after dead logical-var drop: `4m18s` / `~0.77 GB`; after BinOp `const_cache`: `3m45s`; after reverse `term_value_index`: `3m22s` / `~0.51 GB` (`d83d701542 docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). Later ValueSortKey work cut `202s` to `121s`, then remaining sort-key conversions cut `121s` to `81.66s` (`fb0e456dea` and `9c640a9624 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). | Originally suspected convergence gap (`8d:4v`), later reframed as per-disjunct CPU/value-count cost. OCaml retained more disjuncts at the inspected node (`10` vs Rust `8`) but with denser per-disjunct representation (`~487` unique values vs Rust `~1500-3000`, later up to `3914` by tier) (`d83d701542 docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). | Structural sharing (`Arc` around heap/attrs/stack/Phi), dead logical-var stack cleanup, BinOp/UnOp `const_cache` canonicalization, reverse `term_value_index`, and `state_cmp` sort-key work all landed. | Fixes are in current HEAD. It is no longer the primary whole-program blocker but remains the best historical microscope for per-disjunct representation and canonicalization cost. |
| `OBJ_bsearch_ln` | Likely same OBJ-family corpus area as `OBJ_bsearch_ex_`; required docs do not record a `.sil` file. | Suspicious earlier pattern: `26` nodes but `max_visit_count=6450` after the first major speedups (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`, in the pre-B-track narrative preserved there). | Bsearch-family fixpoint convergence / WTO scheduler interaction; state equality instability. | Same B-track convergence fixes as `OBJ_bsearch_ex_`. | Expected fixed in current HEAD via B-track; include in Linux per-proc wall distribution to confirm no recurrence under Linux scheduling. |
| `OBJ_obj2txt` | Likely `a_object.sil`; required docs name the proc/family but do not record the file. | Older archive headline called out DES-family and `OBJ_obj2txt`; current archive broadens this to OBJ-family procedures (`856a747291 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). Exact per-proc wall number is not preserved in the current required docs. | OBJ-family state comparison / canonicalization and possibly summary-surface size. | Covered indirectly by the same `state_cmp` structural canonicalization and B-track convergence fixes. | Current status unknown without Linux measurement; include in the per-procedure wall/RSS scan rather than assuming it is fixed. |
| `DES_ofb_encrypt` | `ofb_enc.sil` or `des_old.sil` in common OpenSSL Textual exports; required docs do not pin one file. | Earlier suspicious pattern: retained `disj=1776`, formula `lin=544k` (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`, in the pre-B-track narrative). | DES-family large retained state and formula volume, with `state_cmp` canonicalization cost. | Heap/attr retained-state GC, formula-GC experiments, and `state_cmp` fixes. | Fixes mostly in current HEAD; formula GC opt-in. Include as a secondary DES-family focused proc. |
| `sha256_block_data_order` | `sha256.sil` in common OpenSSL Textual exports; required docs mention the proc family but do not pin the file. | Listed among small-set pathological procedures after the early `480s -> 195s` whole-program speedup (`6104123dc4 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`, pre-B-track narrative). No exact per-proc wall is preserved in the required docs. | Encryption/hash block-loop large-state pattern similar to Whirlpool/DES. | Mostly indirect wins from value/sort-key/canonicalization fixes. | Unknown on current Linux corpus; include in distribution scan if present. |
| `fcrypt_body` | `fcrypt_b.sil` or `fcrypt.sil` in common OpenSSL Textual exports; required docs do not pin one file. | Dead logical-var drop made it finish at `1m25s` / `830 MB`; later reverse `term_value_index` made its peak rise slightly (`830 MB -> 918 MB`) (`d83d701542 docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). | Large-proc retained-state shape; not the final wall leader, but a useful regression sentinel for value-sharing changes. | Dead logical-var cleanup and reverse `term_value_index`; stale repair later rejected for default. | Fixes in current HEAD except rejected stale repair. Include only after DES/OBJ sentinels. |
| `private_AES_set_encrypt_key`, `AES_encrypt`, `AES_decrypt` | AES files vary by corpus; required docs do not pin files. | Early `peak_rss` heartbeat examples showed `252 MB`, `308 MB`, `378 MB`; after reverse `term_value_index`, small-proc peaks became `222 MB`, `273 MB`, `343 MB` (`5e07ca1a32` and `d83d701542 docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). | Small-procedure baseline accumulation / per-procedure RSS drift, not primary wall hotspots. | Structural sharing, dead logical-var cleanup, reverse term index. | Fixes in current HEAD. Use as low-cost RAM baseline sentinels in Linux runs. |
| `__infer_globals_initializer_Cx` | Captured as an implicit dependency in Whirlpool reproductions; `.sil` file varies by capture. | If analyzed, it materializes a very large single-disjunct heap and can push `whirlpool_block` into multi-million retained heap/edge totals, even with low node disjunct counts (`39b3f26434 docs/plans/STRUCTURAL_SHARING_PROTOTYPE.md`). | Large global-table materialization; structural sharing and retained-state storage pressure. | Default OCaml-compatible `pulse-max-cfg-size=15000` skipped it in the historical focused slice; structural sharing was framed as the physical-storage mitigation if such globals are analyzed (`39b3f26434 docs/plans/STRUCTURAL_SHARING_PROTOTYPE.md`). | Treat as a corpus/capture-shape hazard rather than an ordinary proc hotspot. Worker-1 corpus regrowth should note whether this initializer is present/analyzed. |

### Fix-status notes

- **Current HEAD fixes:** Phase 1 structural sharing, dead logical-var drop,
  BinOp/UnOp `const_cache` canonicalization, reverse direct
  `term_value_index`, B-track convergence fixes, retained post heap/attr GC,
  parser/perf cleanup, and focused `state_cmp` structural/cached-key fixes are
  all ancestors of current HEAD.
- **Opt-in / parked:** `--pulse-intermediate-formula-gc` remains opt-in because
  it was roughly neutral on the current dashboard corpus (`~2.5%` wall win,
  RSS within noise) and earlier formula-GC variants regressed capped
  whole-program wall (`11fa5b8649 docs/STATUS.md`, `6104123dc4
  docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). Stale `term_value_index`
  repair is rejected/parked for the default path (`11fa5b8649 docs/STATUS.md`).
- **Recently removed surface:** `BasedOn` provenance attributes are no longer in
  current HEAD after `43a55e6f1d` (`bug_summary_basedon_provenance_unwired`).
  That removal touched `attribute.rs`, `operations.rs`, `state_cmp.rs`,
  `summary.rs`, and `interproc.rs`, overlapping files that historical
  structural-sharing and summary-surface work cared about. Any Linux summary
  size measurement should therefore treat BasedOn-style attr proliferation as a
  historical risk, not a current expected contributor.

## Section 2: Known RAM-pressure shapes

### Retained invariant maps growth pattern

The first whole-program framing found two RAM modes: outlier procedures and
baseline accumulation. The `peak_rss=...` heartbeat showed small/fast procs
adding roughly `+50-70 MB` per finished procedure (`private_AES_set_encrypt_key`
`252 MB`, `AES_encrypt` `308 MB`, `AES_decrypt` `378 MB`), which extrapolated to
`~30 GB` across `571` procs and matched the observed `~23 GB` whole-program RSS
order of magnitude (`5e07ca1a32 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`).
That was before later summary shrinkage and retained-state cleanup, but it is
still the RAM shape the Linux worker should test for: monotonic resident growth
between procedures vs true per-procedure peak blow-ups.

Within a single large proc, the invariant map can retain dead post graph. The
focused `DES_ede3_cbcm_encrypt` probe found `~28.1M` retained post heap nodes and
`~55.2M` edges before heap/attr GC, with `~25.6M / ~50.3M` of those dead; after
GC, dead post heap/edges dropped to `276 / 276` (`6104123dc4
docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). This is the clearest RAM attack
surface for the Linux profile: retained invariant maps can look semantically
stable to `state_cmp` while still physically storing and cloning disconnected
heap/attr data.

### Disjunct fan-out and `pulse-max-disjuncts`

Whirlpool's retained state decomposed as `8 disjuncts = 2 pre-side variants x 4
post-side tiers`; each post tier originally added `+258` heap nodes, `+129`
attrs, and about `+896` formula items in the global `Cx` subtree (`d83d701542
docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). OCaml retained even more disjuncts at
the inspected node (`10` vs Rust `8`) but with much denser per-disjunct state,
so the old conclusion was: fan-out matters, but per-disjunct cost matters more.

DES confirmed the cap interaction. OCaml and Rust both hit the
`pulse_max_disjuncts = 20` cap on `DES_ede3_cfb_encrypt`; the count per node was
not the differentiator (`5e07ca1a32 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`).
For Linux, the relevant questions are whether the same cap keeps node fan-out
bounded, whether total retained disjuncts across nodes still reach the thousands,
and whether `DisjunctiveDomain::leq` still spends most of its wall time comparing
large but bounded disjunct sets.

### Summary surface size and attribute proliferation

Early whole-program hypotheses suspected the summary store or other long-lived
caches might retain full `AbductiveDomain`-shaped summaries with heavy heap,
attrs, and formula state (`b75dcbfb8a docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`).
Later work added `AbductiveDomain::shrink_for_storage` and retained-state GC, but
the Linux task should still measure whether RSS grows after each proc even when
per-proc peaks are modest.

Formula/surface cleanup remains a mixed area. The current dashboard says
`--pulse-intermediate-formula-gc` is roughly neutral on the clean macOS-derived
corpus: `238.56s` median vs `244.70s` default, with max RSS `16.60 GB` vs
`16.79 GB` and peak footprint `7.42 GB` vs `7.66 GB` (`11fa5b8649
docs/STATUS.md`). Earlier DES-focused numbers showed formula cleanup could
reduce focused RSS (`11m04s` / `~3.99 GB` to `8m50s` / `~3.69 GB`) while
whole-program capped wall could regress (`6104123dc4
docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`). Treat formula GC as a knob, not
an assumed default win.

`BasedOn`-style attribute proliferation should be tracked only as historical
context: commit `43a55e6f1d` removed the unused BasedOn provenance attribute
today. Because that commit touched summary/export/comparison files also relevant
to the structural-sharing plan, Linux summary-size comparisons should not mix
pre- and post-`43a55e6f1d` builds.

### Formula maps and cleanup-pass targets

The formula maps that matter for RAM are the ones repeatedly named in the
historical work: `term_value_index`, `fn_app_eqs`, `atoms`, `const_cache`, plus
classic `linear_eqs`, `intervals`, and `is_int` facts. The current dashboard
records that the formula-GC cleanup pass was expanded in `01a51f99ed` to prune
`term_value_index`, `fn_app_eqs`, `atoms`, and `const_cache` entries that become
unreachable after formula-variable GC (`11fa5b8649 docs/STATUS.md`).

Important nuance: direct `term_value_index` reuse was a win, but stale-key repair
was not. Reverse term lookup collapsed Whirlpool per-tier heap-node growth from
`+258` to `+2` and reduced the focused slice to `3m22s` / `~0.51 GB`
(`d83d701542 docs/plans/CONVERGENCE_8D4V_FINDINGS.md`). The later stale repair
path rebuilt the index many times with no focused repair hits and was removed
from the default path (`edfa22d595 docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`).
Linux profiling should distinguish direct cache hit value from repair/rebuild
cost.

## Section 3: Hypothesis: wall ↔ RAM coupling

Wall and RAM are likely coupled because retained state widening drives both. If
a proc revisits WTO nodes or keeps post tiers alive, the invariant map retains
more snapshots; each snapshot consumes heap/attrs/formula memory and each later
`leq`/join has more state to compare. That was explicit in the bsearch-family
history: `OBJ_bsearch_ex_` had a `max_visit_count=10001` convergence pathology
before B-track fixes, and the fix path went through state equality stability
(widen semantics, timestamp stripping, dynamic types, post-stable convergence)
as much as through raw CPU optimization (`6104123dc4
docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`).

Disjunct fan-out has the same dual effect. More disjuncts mean more retained
memory and more pairwise or subset comparisons; even when the cap holds at
`20`/node, the DES and Whirlpool histories show that per-disjunct size can make
bounded fan-out expensive. This is why `state_cmp::canonicalize` fixes cut wall
without necessarily changing semantic state count: the same retained abstract
states became cheaper to canonicalize, compare, sort, and drop (`856a747291
docs/STATUS.md`; `1608ab4eae` commit message).

Formula GC is the cautionary example. It can lower focused DES memory and was
expanded to cover `term_value_index`, `fn_app_eqs`, `atoms`, and `const_cache`,
but the latest full-corpus dashboard says it is roughly neutral: about `2.5%`
wall win and RSS/footprint within noise (`11fa5b8649 docs/STATUS.md`). That
suggests the remaining Linux profile may not separate cleanly into "CPU-only" or
"RAM-only" tracks; the same retained-state and formula-surface shapes should be
inspected for both wall and RSS.

## Section 4: Open questions for Linux measurement

1. **Does the Rust analyzer scale with `-j N` on Linux?** Historical trustworthy
   dashboard runs are macOS-derived `-j 4`; the Linux host has `96` cores and a
   different corpus shape (`74` `.sil` / `150` procs in the current Linux sample
   vs macOS `74` `.sil` / `446` procs) (`11fa5b8649 docs/STATUS.md`). Scaling
   should not be inferred from the macOS table.
2. **What is the per-procedure wall distribution?** We need to know whether a
   few procs (`DES_ede3_cfb_encrypt`, `DES_ede3_cbcm_encrypt`, `OBJ_bsearch_ex_`,
   OBJ-family, Whirlpool/SHA block loops) still dominate, or whether Linux now
   spreads time evenly across many medium procs.
3. **What is the per-procedure peak RSS distribution?** Distinguish single-proc
   blow-ups from cumulative baseline growth between procs. The old heartbeat
   pattern showed both, and fixes may have changed their relative size.
4. **Does `--pulse-intermediate-formula-gc` shift the picture on the new Linux
   corpus?** It was roughly neutral on the historical clean corpus, but the
   Linux corpus has fewer procs and may contain a different mix of DES/OBJ
   procedures.
5. **Did BasedOn removal change summary/attribute surface size?** Current HEAD
   no longer has BasedOn (`43a55e6f1d`), so Linux measurements should use only
   post-removal builds and should not compare summary sizes against old
   pre-removal artifacts without noting the semantic surface changed.
6. **What do worker-1 and worker-2 change?** Worker-1 is regrowing / deciding
   the corpus shape; worker-2 is auditing perf tooling. The Linux measurement
   plan should wait for worker-1's corpus decision and use worker-2's approved
   profiler/RSS tooling rather than creating another incomparable artifact set.

## Section 5: Recommended Linux experiments (NOT to be run by this task)

Priority order for the next worker:

1. **Freeze corpus and tooling before timing.** Coordinate with worker-1 to pick
   the corpus directory and record `.sil`/proc counts, especially whether DES,
   OBJ, Whirlpool/SHA, and `__infer_globals_initializer_Cx` are present.
   Coordinate with worker-2 on the approved Linux profiler stack. Do not compare
   against the macOS dashboard until the corpus mismatch is resolved.

2. **Full-corpus baseline at historical concurrency.** Run the hardened script
   with the final worker-1 corpus at `JOBS=4`, `RUNS=3`, default caps, formula
   GC off:

   ```sh
   cd infer-rs
   OUT_DIR="$(pwd)/bench-out/linux-openssl-j4-$(date +%Y%m%d-%H%M%S)" \
     RUNS=3 JOBS=4 TEXTUAL_DIR=<worker-1-final-textual-dir> \
     scripts/bench_openssl_partial.sh
   ```

   Collect wall, max RSS, peak footprint, proc counts, heap/wall aborts,
   max-visit count, slow-proc list, and per-proc heartbeat summaries. This is
   the direct comparator for the historical `244.70s` default and `238.56s`
   formula-GC dashboard rows.

3. **Linux parallel-scaling sweep.** On the same corpus and idle host, run
   `RUNS=1` exploratory first, then repeat suspicious points with `RUNS=3`:
   `JOBS=1`, `4`, `16`, `32`, `64`, `96`, default caps, formula GC off. Use
   `/usr/bin/time -v` around each script invocation or the worker-2 approved RSS
   wrapper. The goal is not just speedup: record aborts, max RSS, and whether
   high `-j` amplifies retained summary/corpus-level memory.

4. **Focused per-procedure wall/RSS probes at `-j 1`.** Build release once, then
   run focused filters with default caps and with caps disabled only when safe.
   Suggested first filters:

   ```sh
   --procedures-filter OBJ_bsearch_ex_
   --procedures-filter DES_ede3_cfb_encrypt
   --procedures-filter DES_ede3_cbcm_encrypt
   --procedures-filter whirlpool_block
   --procedures-filter OBJ_obj2txt
   --procedures-filter DES_ofb_encrypt
   --procedures-filter sha256_block_data_order
   --procedures-filter fcrypt_body
   ```

   Command shape:

   ```sh
   /usr/bin/time -v target/release/infer-rs \
     --pulse-only --quiet --trace-ondemand -j 1 \
     --procedures-filter <PROC> \
     --pulse-max-wall-secs 0 --pulse-max-heap-mb 0 \
     <worker-1-final-textual-dir>/*.sil
   ```

   Start with `OBJ_bsearch_ex_` and `DES_ede3_cfb_encrypt`; use uncapped DES
   only if host policy allows it, otherwise keep default caps and record abort
   details.

5. **CPU profile of current `state_cmp` surface.** For the top two focused
   procs from step 4, run Linux `perf` (or worker-2's replacement) at `-j 1`:

   ```sh
   perf record -F 997 -g --call-graph dwarf -- \
     target/release/infer-rs --pulse-only --quiet --trace-ondemand -j 1 \
     --procedures-filter <HOT_PROC> \
     --pulse-max-wall-secs 0 --pulse-max-heap-mb 0 \
     <worker-1-final-textual-dir>/*.sil
   perf report
   ```

   Compare samples under `DisjunctiveDomain::leq`, `state_cmp::canonicalize`,
   `canonicalize_state`, sort/key construction, `CanonTerm` compare/drop, formula
   operations, and summary application against the historical map above.

6. **Per-procedure RSS attribution.** For the top RSS procs and a few small AES
   sentinels, collect `/usr/bin/time -v` plus worker-2-approved heap tooling
   (`heaptrack`, `dhat`, `massif`, or sampled `/proc/<pid>/smaps_rollup`). The
   key comparison is single-proc peak vs post-proc retained RSS. If possible,
   sample the process after each `done:` heartbeat to identify cumulative
   summary/invariant retention.

7. **Formula-GC A/B on Linux corpus.** Repeat the full-corpus `JOBS=4 RUNS=3`
   baseline with:

   ```sh
   EXTRA_ARGS="--pulse-intermediate-formula-gc"
   ```

   or the equivalent script knob. Then run focused `DES_ede3_cfb_encrypt` and
   `DES_ede3_cbcm_encrypt` with formula GC on/off at `-j 1`. Decide whether the
   Linux corpus matches the historical neutral full-corpus result or the focused
   DES memory win.

8. **Report against this map.** The next worker should explicitly classify every
   new hotspot as one of: `state_cmp/leq`, formula solver/map cleanup,
   retained-invariant storage, summary-store/cumulative RSS, disjunct fan-out,
   parser/input, or new/unmapped. Any unmapped Linux hotspot should become the
   first candidate for the post-profile decision gate.
