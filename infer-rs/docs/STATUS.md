# infer-rs status

This file is the canonical current dashboard for infer-rs correctness and
performance. Active work items live in `mu` tasks; historical investigations live
in `docs/plans/`.

```sh
mu state -w infer-rs
mu task list -w infer-rs --status OPEN
```

## Correctness checkpoint

| area | current status |
|---|---|
| Store-textual C Pulse sweep | `52` OK / `0` FAIL / `0` TIMEOUT |
| NPE count | expected `131`, found `140` (`+9` over expected; per-file classification is correctness-aligned with OCaml direct or Rust-strictly-more-precise) |
| Leak count | expected `20`, found `20` (EXACT) |
| UAF count | expected `7`, found `7` (EXACT) |
| `latent.c` issue-set compare | exact at `(procedure, line, issue-type)`: `17` Rust / `17` OCaml |
| specialization summary harness | `20 / 20` (held; C-suite `may_double_free_if_alias` residual deferred to the apply-post deep port) |
| `virt.sil` virtual dispatch | `0` skipped procedures (full coverage) |
| `make check` | current checkpoint passes with `INFER_BIN=../infer/bin/infer` |
| C-suite OCaml↔Rust Pulse summary triage | `113 matching / 24 diffs` (+26/-26 today from the `87/50` session start; +63/-63 vs original `50/87`) |

### NPE issue-count deltas (current Linux)

Current Linux store-textual NPE count is expected `131`, found `140` (`+9`
over expected), measured by the existing store-textual sweep notes; do not
re-run the sweep for doc refreshes. The sweep harness mirrors the upstream C
Pulse test Makefile's `--no-pulse-force-continue` setting before comparing
against `issues.exp`, and the regression guard now pins `52` OK / `0` FAIL /
`0` TIMEOUT, exact LEAK/UAF parity, and `NPE >= expected` without pinning an
exact NPE total.

The current sweep state is held at NPE expected `131`, found `140` (`+9`),
LEAK `20/20` EXACT, and UAF `7/7` EXACT; do not re-run sweeps for doc
refreshes. The dynamic-type specialized abort propagation fix (`a8b8fe7bde`)
contributed a real `+1` NPE catch by surfacing a specialized function-pointer
abort that was previously dropped, not a duplicate callee-local manifest
report. The closed `scout_npe_per_file_full_remeasure` classification covers
the remaining per-file surface: all live NPE deltas are either aligned with
OCaml direct behavior under the test config or are Rust-strictly-more-precise
catches.

Leak per-file detail: LEAK now matches expected exactly per file after
`bug_store_textual_leak_dead_root_parity` (commit `9078f04176`).

LEAK baseline note (historical): the previous `expected 20 / found 16` row was
recovered after the cluster A/B/C/D pass landed. Numbers above are the live
sweep, not aspirational targets.

Recent correctness work that should stay in place mirrors OCaml's dynamic-type
specialization path, direct known-call unknown fallback for resolved
`__call_c_function_ptr` targets without summaries, caller-visible pre-edge
materialization, latent-invalid-access export/import parity, latent.c trace
detail (callee formal anchoring on synthesized actual-argument trace steps),
and comparator normalizations for semantic noise only. The recent SIL virtual
dispatch stack (`902b2deb50`, `cda27f6239`, `70365d047b`) closed the last
`virt.sil` skips: `plus_formal` / `plus_ok`,
`devirtualize_with_final_good`, and `devirtualize_with_static_call_good` all
now analyze.

Today's 52-commit Linux session is summarized by track below (landed SHAs from
`git log --oneline fccf3f0b7d..HEAD`; no new sweeps were run for this doc
refresh):

- **Sweep parity / store-textual (Track 1A).** `502236c5b2` pre-warms the
  sweep harness to avoid first-run TIMEOUT races. `9078f04176` moves LEAK to
  `20/20` exact parity by inspecting dead roots and importing allocator
  returns. `459ec03492`, `f78e622ff1`, and `7e28401d3d` align the recency
  limit and C textual return-slot import/export. `83e63f2cb8`, `dd4f671d96`,
  `a625c0dd55`, `fe0e95be3e`, `11fa5b8649`, `d9da630ae7`, and `48655710c4`
  close array-index, struct-formal, stdio-model, diagnostic-dedup, harness
  config, guard-test, and guard-doc gaps. The held checkpoint is now `52` OK /
  `0` FAIL / `0` TIMEOUT, NPE expected `131` / found `140` (`+9`, classified),
  LEAK `20/20` EXACT, and UAF `7/7` EXACT.
- **Per-procedure summary parity (Track 1B).** `e7dd96291a` routed latent
  witnesses; `bfb235e881` fixed canonical formula-var import; `a8b8fe7bde`
  propagated dynamic-type specialized function-pointer aborts; `3b7b90f1a9`
  avoided branch-only constant invalidation attrs; `e18143e41d` preserved
  benign `Continue` summary duplicates; `bc0dc998c7` deduped latent summaries
  ignoring hidden history; `58c100411b` aligned latent null-exit written-to
  comparison; `a8ba9a20b5` pruned canonical heap alias contradictions;
  `da98dd6b09` coalesced zero direct formals on continue summaries; and
  `fa938dc6dd` aligned imported callback `Closure` attrs. The scoped C-suite
  parity total moves from the `87/50` session start to `113/24`.
- **Perf / OpenSSL Linux wave (Track 2).** `b512df2924` aligns stopped latent
  leq with OCaml and closes the OBJ_bsearch convergence gate. `da9b92c384`
  avoids sorting unmapped canonical roots; `2f2c26a6a9` shares
  `ValueHistory` clones with `Arc<ValueHistory>`; `70ca0c562d` reuses keyed
  canonicalizer sort helpers; `5e09cd82a1` skips unchanged canonical heap-edge
  rebuilds; and `1319de4f19` bounds `ValueHistory::merge` growth. The canonical
  OpenSSL Linux post-wave remeasure (`RUNS=3 JOBS=4`, all exits `0`) lands at
  median `4:47.79`, `5.70 GiB` max RSS, `445/445` procs, `6` aborts, and max
  visit count `4`.
- **Bench infrastructure and cleanup/tooling.** `51b68ec816` makes the OpenSSL
  partial bench pass explicit `--pulse-max-heap-mb 2048` /
  `--pulse-max-wall-secs 60` caps on every `infer-rs` invocation, and
  `9a80eb9be7` detects Linux GNU `/usr/bin/time -v` versus macOS BSD
  `/usr/bin/time -l`. `1295efbfbc` adds Linux profiling quick references;
  `6b6af3ea19` documents the in-process OOM hazard for
  `test_summary_comparison_c_triage`; `266a17dd9d`, `43a55e6f1d`,
  `1037a81a4e`, and `ca805cac19` remove/factor unused summary/formula helpers;
  and the OpenSSL planning/result docs landed in `02a79d2833`, `e4e4bc887e`,
  `006b39cd2b`, `e9d292ad2c`, and `a900e9f603`.

### C-suite OCaml↔Rust Pulse summary parity (`113 matching / 24 diffs`)

A separate parity track compares OCaml and Rust Pulse summaries directly per
procedure on a slice of the C Pulse test suite (`arithmetic.c`, `funptr.c`,
`interprocedural.c`, `latent.c`, `memory_leak.c`, `specialization.c`);
`nullptr.c` remains a harness/OOM and NPE-scout reference rather than part of
these six-file parity totals. Standalone Rust analysis of `nullptr.c` completes
in ~0.02s under the standard 60s/2GB caps. The historical "recursion hang" was
actually an in-process OOM (~7.86 GB) inside
`crates/pulse/tests/end_to_end.rs::test_summary_comparison_c_triage`. The
single-file behavior is now sound. The full narrative is in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Initial cluster pass landed (commits on branch):

- `cluster_a_drop_spurious` — thread access modes through eval, drop spurious
  `MustBeInitialized` on formals.
- `cluster_a_taint_initial_formal_preeval_gap` — pre-evaluate formals at
  procedure entry to mirror OCaml `taint_initial`.
- `cluster_b_propagate_initialized_after_store` — pair `Initialized` with
  `WrittenTo` in `add_one`.
- `cluster_c_pair_allocated_cmalloc_with` — emit `Uninitialized` companion
  on malloc/realloc primitives.
- `cluster_c_tenv_struct_uninitialized_followup` — thread `&Tenv`, port
  Tstruct field walk.
- `cluster_d_align_global_function_pointer` — align global function-pointer
  summary surface (pre_stack + `0 < addr`).
- `cluster_e_stop_over_attaching` — gate `UsedAsBranchCond` like OCaml.
- `cluster_e_residual_state_shape_self_cycle` — canonicalize summary pre.
- `cluster_f_route_witness_atoms_through` — filter summary conditions by
  precondition vocabulary.
- `cluster_g_preserve_closure_call_return` — import callee formula before
  post.
- `cluster_g_residual_funptr_return_export` — preserve return zero facts
  on exported summary surface.
- `cluster_h_keep_linear_equalities_across` — preserve imported affine
  equations.
- `bug_specialization_c_stack_overflow_lin_arith_of_q` — break imported
  linear cycles introduced by Cluster H (regression fix).

Residual track also landed (commits on branch):

- `cluster_d_residual_global_pre_stack_initializer` — dynamic
  CFunction/ObjcBlock pre-stack seeding + `apply_callback` key surface.
- `cluster_specialization_residual_post_overflow_fix` — stop recursive Cfun
  harness fallback exposed by the cycle-break fix.
- `cluster_h_residual_inequality_witness_export` — OCaml-style
  restricted/tableau witness export for inequalities.
- `cluster_e_residual_cycle_eq_repr` — align VarUF representative ordering.
- `cluster_g_residual_funptr_apply_post_canonical_edges` — preserve no-op
  call summaries instead of unknown-call havoc.
- `bug_h_residual_witness_regresses_specialization` — scope H witness export
  to direct summary roots; restored specialization.c parity.

Secondary residual fixes also landed:

- `cluster_d_residual_funptr_atom_repr` — prefer global funptr pointee as the
  atom representative during summary comparison/normalization.
- `cluster_e_residual_apply_post_cycle_edges` — remove eager subst-value
  canonicalization during imported formula application; preserve direct
  pre/post cycle heap edges in latent summary shape.

Latent.c-focused fourth pass also landed:

- `cluster_atom_normalization_argc_lin_signed` — comparator-side normalization
  for cond ↔ phi.atom routing on zero (in)equalities and sign-normalized
  linear-form atoms.
- `cluster_use_after_free_caller_alpha_substitution` — prefer caller-actual
  representative when canonicalizing imported substitution ranges in
  `apply_post`.
- `cluster_latent_classification_extra_aborts` — align latent invalid-access
  classification (no over-broad local manifest twin; latent diagnostics replay
  on specialized apply).
- `cluster_latent_witness_routing_surface` — comparator-side
  `LatentAbortProgram` diagnostic routing plus `UsedAsBranchCond` on aliased
  heap-path values.

Store-textual LEAK fifth pass also landed:

- `bug_store_textual_leak_dead_root_parity` — dead-root leak inspection
  mirroring OCaml `check_memory_leaks` over `astate_before_filter`, plus
  allocator-return import to caller.

Function-pointer abort propagation, interprocedural cleanup, Continue summary dedup,
latent summary dedup, alias-contradiction pruning, zero-direct-formal coalescing,
and callback-attr import also landed:

- `a8b8fe7bde` — propagate dynamic-type specialized `AbortProgram` pre/posts
  through callers while preserving duplicate callee-local manifest-abort
  suppression; `funptr.c` moved `20/8` -> `22/6` on that step.
- `3b7b90f1a9` — avoid branch-only constant invalidation attrs on summary
  surfaces; `interprocedural.c` moved `11/6` -> `15/2`.
- `e18143e41d` — preserve benign `Continue` summary duplicates;
  `memory_leak.c` moved `25/21` -> `37/9`, and `interprocedural.c` moved to
  `16/1`.
- `bc0dc998c7` / `58c100411b` — dedup latent summaries ignoring hidden history
  and align latent null-exit written-to summary comparison.
- `a8ba9a20b5` / `da98dd6b09` — prune canonical heap alias contradictions and
  coalesce zero direct formals on continue summaries; `latent.c` reaches
  `10/4`.
- `fa938dc6dd` — align imported callback `Closure` attrs; `funptr.c` reaches
  `24/4`.

Full six-file triage delta vs original 2026-05-11 baseline
(`50 matching / 87 diffs`) is now `113 matching / 24 diffs`
(`+63 matching / -63 diffs`). Today's Linux session started at `87/50` and
ended at `113/24` (`+26 matching / -26 diffs`). Current scoped per-file totals:

| file | session start | now | best landed commit / residual |
|---|---:|---:|---|
| `arithmetic.c` | `6/5` | `6/5` | residual: OCaml `NonDisjDomain` non-disj sideband mechanism; scout-only |
| `funptr.c` | `20/8` | `24/4` | `fa938dc6dd` callback `Closure` attr surface |
| `interprocedural.c` | `11/6` | `16/1` | `3b7b90f1a9` branch-only attr cleanup + `e18143e41d` benign `Continue` duplicates; residual `trace_correctly_through_wrappers_bad` multiplicity |
| `latent.c` | `5/9` | `10/4` | `da98dd6b09` zero direct-formal coalesce after `bc0dc998c7`, `58c100411b`, `a8ba9a20b5` |
| `memory_leak.c` | `25/21` | `37/9` | `e18143e41d` benign `Continue` duplicate preservation |
| `specialization.c` | `20/1` | `20/1` | held throughout; `may_double_free_if_alias` residual deferred to the apply-post/record-post deep port |
| **total** | **`87/50`** | **`113/24`** | **+26/-26 today; +63/-63 vs original `50/87` baseline** |

Per-file breakdown and per-pass narrative live in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Current residual work is saturated at mechanism boundaries: arithmetic still
needs the OCaml `NonDisjDomain` sideband mechanism, interprocedural has one
summary-multiplicity residual, funptr has the remaining callback-attr/minor
surface after `fa938dc6dd`, latent's remaining `10/4` surface and
specialization's `may_double_free_if_alias` both point at the parked
`cluster_latent_record_post_for_address_porting` apply-post/record-post deep
port, and the sweep-level NPE `131/140` delta is correctness-aligned and
classified. The next layer is multi-day mechanism porting, not another quick
single-commit parity pass.

## OpenSSL benchmark dashboard

Default Rust caps for full-corpus OpenSSL runs:

- `pulse-max-heap-mb = 2048`
- `pulse-max-wall-secs = 60`
- pass `0` to disable either cap

### Historical macOS-derived reference

Historical reference corpus: 74-file partial OpenSSL capture under
`~/infer-rs-bench/openssl-20260501-084151/`, with the fresh patched-exporter
re-export `textual-out-reexport-20260508-102338/` (`74` `.sil` files; DES and
OBJ targets present; `RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh`). These
numbers remain useful as historical reference only; they should not be treated
as the current Linux baseline.

| metric | OCaml old baseline (`-j 1`) | Rust default (`-j 4`) | Rust + formula-gc (`-j 4`) |
|---|---:|---:|---:|
| wall time | `42.9s` | `244.70s` median | `238.56s` median |
| max RSS | `~1.17 GB` | `16.79 GB` median | `16.60 GB` median |
| peak footprint | `~1.10 GB` | `7.66 GB` median | `7.42 GB` median |
| procs analyzed | `570 / 570` | `446 / 446` | `446 / 446` |
| heap+wall aborts | n/a | `21 / 446` median | `20 / 446` median |
| max visit count | n/a | `4` | `4` |
| process exit | clean (`0`) | `2` due reported leaks | `2` |

Default Rust = the clean checkpoint with the textual parser O(N²) fix
(`2a17574854`) on top of the perf cleanup (`01a51f99ed`); the formula-gc
column adds `--pulse-intermediate-formula-gc`. Both columns are 3 runs each on
the same fresh export. Old-export historical numbers (`239.67s` median wall,
`13.17 GB` median max RSS, `18 / 570` aborts, clean exit) are from the original
`textual-out/` and are not directly comparable — the fresh export has fewer
procedure definitions (`446` analyzed instead of `570`) but includes newer
cleanup/nullify/exit-scope metadata.

Historical interpretation:

- At that checkpoint, default Rust vs OCaml old baseline was
  `244.70 / 42.9 ≈ 5.7×` (was `6.7×` before the parser fix and `8.0×` before
  the perf cleanup; `~30%` cumulative wall improvement on the same input).
- `--pulse-intermediate-formula-gc` was roughly neutral on that corpus (`~2.5%`
  wall win; max RSS and peak footprint within noise of the default column).
- Focused `state_cmp` landings before today's Linux work had cut isolated
  hotspots substantially (`OBJ_bsearch_ex_` from `1.91s` to `~0.47s`;
  `DES_ede3_cfb_encrypt` from `~40.2s` after the first structural fixes to
  `~21.8s` after cached propagation sort keys and flat-slab `CanonTerm`).

### Linux baseline (established)

Current Linux corpus: `~/infer-rs-bench/openssl-20260514-121752/`, built by
worker-1 from OpenSSL 1.0.2d with the partial benchmark subset regrown to
`74` `.sil` files / `454` Textual procedures (`445` Pulse-reachable procs in
the full-corpus dynamic run). Hotspots `OBJ_bsearch_ex_` (`obj_dat.sil`) and
`DES_ede3_cfb_encrypt` (`cfb64ede.sil`) are present.

Worker-1's guarded scout results are recorded in
[`docs/plans/OPENSSL_LINUX_PERF_BASELINE_RESULTS_2026_05.md`](plans/OPENSSL_LINUX_PERF_BASELINE_RESULTS_2026_05.md)
(commit `006b39cd2b`). That scout initially found no publishable median: the
final `a8b8fe7bde` full-corpus attempt tripped the 650s outer guard at roughly
`407/445` procs analyzed (`405/445` completed in the log), with
`OBJ_bsearch_sn` / `OBJ_bsearch_ln` active and repeated `OBJ_bsearch_ex_`
live-fixpoint growth.

Commit `b512df2924` (`pulse: align stopped latent leq with OCaml`) closed the
OBJ convergence gate. Specialized `OBJ_bsearch_` analysis now converges in
`24.31s` combined (previously timing out at `90s` with max visit count `>51`
and growing), and the full Linux bench reaches end of corpus. The combined perf
wave through `1319de4f19` then traded a small wall regression for a major RAM
and abort improvement: latent summary/state-shape fixes (`e18143e41d`,
`bc0dc998c7`), canonical-root sorting avoidance (`da9b92c384`), shared
`ValueHistory` clones (`2f2c26a6a9`), keyed sort-helper reuse (`70ca0c562d`),
unchanged-edge rebuild skipping (`5e09cd82a1`), and bounded
`ValueHistory::merge` growth (`1319de4f19`).

| checkpoint | run config | wall | max RSS | procs | aborts | max visit count |
|---|---|---:|---:|---:|---:|---:|
| Linux session-start OBJ reach-end (`b512df2924`) | `JOBS=4 RUNS=1` | `4:17.11` | `26.3 GiB` | `445/445` | `27` | `4` |
| after canonical-root sort skip (`da9b92c384`) | `JOBS=4 RUNS=1` | ~neutral | `25.4 GiB` | `445/445` | `23` | `4` |
| after `Arc<ValueHistory>` (`2f2c26a6a9`) | `JOBS=4 RUNS=1` | `~4:58` | `18.96 GiB` | `445/445` | `19` | `4` |
| canonical Linux post-wave (`1319de4f19`) | `JOBS=4 RUNS=3` median | `4:47.79` (`287.79s`) | `5.70 GiB` (`5,979,620 KiB`) | `445/445` | `6` | `4` |

Current canonical Linux Rust baseline: corpus
`~/infer-rs-bench/openssl-20260514-121752/` (`74` `.sil` / `454` Textual procs),
`JOBS=4 RUNS=3` via `scripts/bench_openssl_partial.sh`, all three runs exit
`0`, median wall `4:47.79` (`287.79s`), median max RSS `5,979,620 KiB`
(`5.70 GiB`), procs `445/445`, median aborts `6`, and max visit count `4`.
The Linux script uses GNU `/usr/bin/time -v`, so `peak_footprint_bytes` is not
reported; do not compare the per-proc progress-log `peak_rss` heartbeat as the
macOS malloc peak-footprint metric.

Cross-baseline dashboard, keeping corpus/OS/accounting differences explicit and
preserving the historical macOS reference separately from the current Linux row:

| metric | macOS-derived original Rust (`-j 4`) | Linux session start Rust (`b512df2924`) | canonical Linux post-wave Rust (`-j 4`) | delta / note |
|---|---:|---:|---:|---|
| wall time | `244.70s` median | `4:17.11` (`257s`) | `4:47.79` (`287.79s`) median | `+17.6%` vs macOS-derived original; `+11.9%` vs Linux session start |
| max RSS | `16.79 GiB` median | `26.3 GiB` | `5.70 GiB` median | `-66%` vs macOS-derived original; `-78%` vs Linux session start |
| procs analyzed | `446 / 446` | `445 / 445` | `445 / 445` | parity on current Linux corpus |
| heap+wall aborts | `21 / 446` median | `27 / 445` | `6 / 445` median | `-71%` vs macOS-derived original; `-78%` vs Linux session start |
| max visit count | `4` | `4` | `4` | bounded after OBJ convergence fix |
| Rust/OCaml old wall ratio | `5.7×` | `5.99×` | `6.71×` | modest wall regression in exchange for the RAM/abort win |
| process exit | `2` due reported leaks | `0` | `0` on all 3 runs | Linux post-wave completed cleanly |

Old OCaml macOS-era reference, retained only for ratio context: `42.9s` wall,
`~1.17 GB` max RSS, `570/570` procs, clean exit. The Linux script now detects
GNU `/usr/bin/time -v` versus macOS BSD `/usr/bin/time -l`, so
`peak_footprint_bytes` exists only on the macOS/BSD-time side.

Interpretation:

- Versus the session-start Linux post-OBJ-fix baseline (`4:17.11`, `26.3 GiB`,
  `27` aborts), canonical post-wave Linux wall is `+11.9%`, but max RSS is
  `-78.3%` (`26.3` -> `5.70 GiB`, a `4.6×` cut) and aborts drop `27` -> `6`
  (`-78%`).
- Versus the historical macOS-derived Rust reference (`244.70s`, `16.79 GB`,
  `21` aborts), canonical Linux wall is `+17.6%`, while max RSS is `-66%` lower
  and aborts drop `21` -> `6` (`-71%`). The current Rust/old-OCaml wall ratio is
  `287.79 / 42.9 = 6.71×`, back near the historical `5.7-6.7×` dashboard range.
- The key dashboard message is RAM and stability: Linux Rust is now below the
  historical macOS-derived Rust max-RSS reference, with far fewer cap aborts,
  at the cost of a modest wall regression from the Linux reach-end checkpoint.

Recommended next perf wave: profile the remaining wall cost only after treating
this RUNS=3 row as the canonical post-wave baseline. Likely targets remain
latent/state comparison fast paths and residual `state_cmp` canonicalization
work, but new changes should preserve the `5.70 GiB` / `6`-abort gains.

Benchmark artifacts from the latest runs are under ignored `bench-out/` or `/tmp`
paths in the worker checkout. Historical OpenSSL archaeology is in
[`docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`](plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md).

## Current active work

`mu` is the source of truth; this section is a coarse map only and may lag the
DAG between gated docs refreshes.

```sh
mu state -w infer-rs            # tracks + ready set + agents
mu task list -w infer-rs --status OPEN --sort roi
mu task list -w infer-rs --status DEFERRED
```

Live themes (track headlines, not exhaustive task lists):

- **DONE today — full Linux perf wave.** OBJ_bsearch convergence and OpenSSL
  reach-end (`b512df2924`), canonical-root sorting avoidance (`da9b92c384`),
  shared `ValueHistory` clones (`2f2c26a6a9`), keyed sort-helper reuse
  (`70ca0c562d`), unchanged-edge rebuild skipping (`5e09cd82a1`), and bounded
  `ValueHistory::merge` growth (`1319de4f19`) establish the canonical
  `RUNS=3 JOBS=4` Linux medians: `4:47.79`, `5.70 GiB`, `445/445`, `6` aborts,
  max visit count `4`. The perf track is at a natural saturation point: the RAM
  and abort wins should be preserved before any multi-day wall-recovery pass.
- **DONE today — Linux correctness parity wave.** Track 1A holds the
  store-textual sweep at `52` OK / `0` FAIL / `0` TIMEOUT, NPE `131/140` (`+9`,
  classified), LEAK `20/20` exact, and UAF `7/7` exact. Track 1B moves the
  scoped C-suite total from `87/50` to `113/24`: arithmetic `6/5`, funptr
  `24/4`, interproc `16/1`, latent `10/4`, memory_leak `37/9`, specialization
  `20/1`. This correctness track is also saturated at the quick-fix layer; the
  next diffs require multi-day mechanism ports.
- **DONE today — benchmark infrastructure hardening.**
  `scripts/bench_openssl_partial.sh` now explicitly passes
  `--pulse-max-heap-mb 2048` and `--pulse-max-wall-secs 60` on every `infer-rs`
  invocation (`51b68ec816`) and auto-detects Linux GNU `/usr/bin/time -v` versus
  macOS BSD `/usr/bin/time -l` (`9a80eb9be7`). `TESTING.md` documents the
  in-process OOM hazard for `test_summary_comparison_c_triage` (`6b6af3ea19`).
- **PARKED correctness backlog.** `cluster_latent_record_post_for_address_porting`
  is deferred as a three-day deep porting track despite high ROI (`11.7`).
  Reopen when someone can reserve multi-day focus to port the OCaml
  apply-post/record-post address mechanism and remeasure the remaining
  latent.c `10/4` surface plus specialization `may_double_free_if_alias`.
- **Next perf wave.** Treat the `RUNS=3` canonical Linux row (`4:47.79`,
  `5.70 GiB`, `6` aborts) as the baseline; any wall recovery should preserve
  the RAM and abort gains. Likely targets remain latent/state comparison fast
  paths and residual `state_cmp` canonicalization work, but not as a quick
  follow-up to today's wave.
- **Deferred backlog.** Micro-cleanups (`code_*`), speculative representation
  work (`perf_component_clone_reduction`), Textual enhancements, and accepted
  parity limits (`parity_sizeof_type_eval`) are parked with explicit
  reopen-when notes. Run `mu task list -w infer-rs --status DEFERRED` for the
  live set.

## Test commands

```sh
cd infer-rs
cargo fmt -p pulse
cargo test -p pulse --lib
cargo test -p pulse --test end_to_end
cargo test -p infer-rs
INFER_BIN=../infer/bin/infer make check
```

See [`docs/TESTING.md`](TESTING.md) for methodology and benchmark reproduction.

## Key references

- [`README.md`](../README.md) — quickstart and doc map.
- [`docs/TESTING.md`](TESTING.md) — test/benchmark commands.
- [`docs/PULSE.md`](PULSE.md) — Pulse architecture.
- [`docs/STORE_TEXTUAL.md`](STORE_TEXTUAL.md) — capture/export notes and accepted fidelity limits.
- [`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md) — C-suite OCaml↔Rust Pulse summary triage and per-cluster status.
- [`docs/plans/`](plans/) — archived investigations.
