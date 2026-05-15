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
| C-suite OCaml↔Rust Pulse summary triage | `108 matching / 29 diffs` (+21/-21 today from the `87/50` session start; +58/-58 vs original `50/87`) |

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
now analyze. Today's Linux session additions:

- `502236c5b2` — sweep harness pre-warm avoids first-run TIMEOUT races on cold
  caches.
- `9078f04176` — dead-root leak inspection plus allocator-return import moves
  LEAK to `20/20` exact parity.
- `459ec03492` — Rust's default `pulse_recency_limit` now matches OCaml
  (`Some(32)`).
- `e7dd96291a` — latent witness routing: comparator-side
  `LatentAbortProgram` routing plus `UsedAsBranchCond` on aliased heap-path
  values.
- `f78e622ff1` / `7e28401d3d` — preserve/import the C textual `return` slot,
  accept `return` and `__return`, and restore C return facts.
- `dd4f671d96` / `83e63f2cb8` — restore struct-formal empty pre leaves and
  retain `ArrayAccess` index attrs during summary normalization.
- `11fa5b8649` — the store-textual harness passes
  `--pulse-force-continue=false`, matching upstream C Pulse Makefiles' semantic
  test config.
- `d9da630ae7` / `48655710c4` — sweep regression guard and docs pin `52` OK /
  `0` FAIL / `0` TIMEOUT, exact LEAK/UAF parity, and `NPE >= expected`.
- `43a55e6f1d` — removes unused `BasedOn` summary provenance.
- `3368d70702` / `bfb235e881` — map canonical formula vars on summary import,
  fixing the apps.sil `set_multi_opts` -> `set_name_ex` formula-substitution
  panic while preserving the held sweep checkpoint.
- `a8b8fe7bde` — propagate dynamic-type specialized function-pointer aborts
  through callers while still suppressing duplicate callee-local manifest aborts;
  `funptr.c` summary parity moves `20/8` -> `22/6`, and the held sweep state
  remains NPE `131/140`, LEAK `20/20` EXACT, UAF `7/7` EXACT; the `+1` NPE from
  this specialized abort propagation is a real catch.
- `3b7b90f1a9` — avoid branch-only constant invalidation attrs;
  `interprocedural.c` summary parity moves `11/6` -> `15/2`.
- `e18143e41d` — preserve benign `Continue` summary duplicates;
  `memory_leak.c` summary parity moves `27/19` -> `37/9`, and
  `interprocedural.c` moves `15/2` -> `16/1`.
- `a625c0dd55` / `fe0e95be3e` — stdio FILE-argument modeling and
  report-location dedup keep NPE deltas aligned with the per-file scout
  classification.
- OpenSSL bench hygiene and planning: `51b68ec816` caps bench invocations,
  `9a80eb9be7` detects GNU/BSD `time`, `1295efbfbc` adds Linux profiling tool
  docs, `02a79d2833` maps the attack surface, `e4e4bc887e` records the
  experiment plan, worker-1 regrew the Linux corpus to `74` `.sil` / `454`
  procs at `~/infer-rs-bench/openssl-20260514-121752/`, and `006b39cd2b`
  records the guarded Linux perf scout results.
- `b512df2924` — align stopped latent leq with OCaml; the specialized
  `OBJ_bsearch_` analysis converges in `24.31s` combined, and the OpenSSL
  Linux bench reaches the end of corpus at `445/445` procs (`4:17.11` wall,
  `26.3 GiB` max RSS, `27` aborts, max visit count `4`).
- `bc0dc998c7` — dedup latent summaries ignoring hidden history; the
  `latent.c` `FN_nonlatent_use_after_free_bad{,2}` pre/post mismatch is
  eliminated.
- `da9b92c384` — avoid sorting unmapped canonical roots; SHA512 focused
  `canonicalize` self time moves `41.13%` -> `33.53%`, with full-bench wall
  roughly neutral, max RSS `26.3` -> `25.4 GiB`, and aborts `27` -> `23`.
- `2f2c26a6a9` — share `ValueHistory` clones with `Arc<ValueHistory>`; the
  full bench max RSS drops `25.4` -> `18.96 GiB` (`-28%` vs the original
  `26.3 GiB` Linux baseline), aborts move `23` -> `19`, and wall is `~4:58`.
- `58c100411b` — align latent null-exit written-to summary comparison;
  `latent.c` summary parity moves `6/8` -> `7/7` and scoped C-suite parity
  reaches `108/29`.
- `5e09cd82a1` / `1319de4f19` — skip unchanged canonical heap edge rebuilds
  and bound `ValueHistory::merge` growth. The canonical post-wave Linux OpenSSL
  remeasure (`RUNS=3 JOBS=4`, all exits `0`) lands at median `4:47.79`,
  `5.70 GiB` max RSS, `445/445` procs, `6` aborts, max visit count `4`:
  max RSS is `-78.3%` vs the session-start Linux post-OBJ baseline
  (`26.3` -> `5.70 GiB`) while wall is `+11.9%`.

### C-suite OCaml↔Rust Pulse summary parity (`108 matching / 29 diffs`)

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

Function-pointer abort propagation also landed:

- `cluster_funptr_abort_propagation_specialized` (`a8b8fe7bde`) — propagate
  dynamic-type specialized `AbortProgram` pre/posts through callers while
  preserving duplicate callee-local manifest-abort suppression. `funptr.c` moves
  `20/8` -> `22/6`; specialization remains `20/1`.

Interprocedural branch-only attr cleanup, Continue summary dedup, and latent
summary dedup also landed:

- `3b7b90f1a9` — avoid branch-only constant invalidation attrs on summary
  surfaces. `interprocedural.c` moves `11/6` -> `15/2`.
- `e18143e41d` — preserve benign `Continue` summary duplicates.
  `memory_leak.c` moves `27/19` -> `37/9`, and `interprocedural.c` moves
  `15/2` -> `16/1`.
- `bc0dc998c7` — dedup latent summaries ignoring hidden history, eliminating
  the `latent.c` `FN_nonlatent_use_after_free_bad{,2}` pre/post mismatch while
  preserving the scoped `107/30` total.
- `58c100411b` — align latent null-exit written-to summary comparison;
  `latent.c` moves `6/8` -> `7/7`, and the scoped C-suite total moves
  `107/30` -> `108/29`.

Full six-file triage delta vs original 2026-05-11 baseline
(`50 matching / 87 diffs`) is now `108 matching / 29 diffs`
(`+58 matching / -58 diffs`). Today's Linux session started at `87/50`, moved
through `96/41` and `107/30`, and is now `108/29` (`+21 matching / -21 diffs`).
Current scoped per-file totals:

| file | matching / diffs | residual |
|---|---:|---|
| `arithmetic.c` | `6/5` | OCaml `NonDisjDomain` non-disj sideband mechanism; follow-up `arithmetic_ocaml_non_disj_summary_fallback` filed by worker-leak |
| `funptr.c` | `22/6` | `cluster_funptr_abort_propagation_specialized` residuals are closed; remaining surface is callback `Closure` plus minor issues |
| `interprocedural.c` | `16/1` | remaining residual: `trace_correctly_through_wrappers_bad` summary multiplicity |
| `latent.c` | `7/7` | producer-side alias/free-null split follow-ups remain in flight; `bc0dc998c7` closed the `FN_nonlatent_use_after_free_bad{,2}` pre/post dedup mismatch, and `58c100411b` closed the null-exit written-to summary compare |
| `memory_leak.c` | `37/9` | remaining surface after benign `Continue` duplicate preservation: array/index loop value-shape, realloc fail/success branch counts, `alias_ptr_free` flag, mutual-recursion shape, `alloc_ref_counted_arith` pointer arithmetic |
| `specialization.c` | `20/1` | `may_double_free_if_alias` summary-surface; deferred to apply-post deep port |
| **total** | **`108/29`** | **+21 matching / -21 diffs from session start `87/50`; +58/-58 vs original `50/87`** |

Per-file breakdown and per-pass narrative live in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Current residual work is split between the closed funptr abort-propagation
surface after `a8b8fe7bde`, the remaining callback `Closure`/minor funptr
surface, `interprocedural.c`'s remaining summary-multiplicity residual after
`e18143e41d`, the in-flight `cluster_latent_summary_alias_contradiction` and
`cluster_latent_free_null_split_constant_invalid` follow-ups after `58c100411b`
moved `latent.c` to `7/7`, the parked
`cluster_latent_record_post_for_address_porting` deep porting track for
remaining producer/record-post shape and `may_double_free_if_alias`, and the
sweep-level NPE `131/140` correctness-aligned delta documented above.

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
`ValueHistory` clones (`2f2c26a6a9`), unchanged-edge rebuild skipping
(`5e09cd82a1`), and bounded `ValueHistory::merge` growth (`1319de4f19`).

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

Cross-baseline dashboard, keeping corpus/OS/accounting differences explicit:

| metric | old OCaml reference (`-j 1`, macOS-era corpus) | historical macOS-derived Rust default (`-j 4`) | canonical Linux post-wave Rust (`-j 4`) |
|---|---:|---:|---:|
| wall time | `42.9s` | `244.70s` median | `287.79s` median (`4:47.79`) |
| max RSS | `~1.17 GB` | `16.79 GB` median | `5.70 GiB` median |
| peak footprint | `~1.10 GB` | `7.66 GB` median | n/a on GNU time/current script |
| procs analyzed | `570 / 570` | `446 / 446` | `445 / 445` |
| heap+wall aborts | n/a | `21 / 446` median | `6 / 445` median |
| max visit count | n/a | `4` | `4` |
| process exit | clean (`0`) | `2` due reported leaks | `0` on all 3 runs |

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

- **Done today** — full Linux perf wave is complete: OBJ_bsearch convergence and
  OpenSSL reach-end (`b512df2924`), benign-continue/state-shape and latent
  summary dedup (`e18143e41d`, `bc0dc998c7`), canonical-root sorting avoidance
  (`da9b92c384`), shared `ValueHistory` clones (`2f2c26a6a9`), unchanged-edge
  rebuild skipping (`5e09cd82a1`), and bounded `ValueHistory::merge` growth
  (`1319de4f19`). Canonical `RUNS=3 JOBS=4` Linux medians are `4:47.79`,
  `5.70 GiB`, `445/445`, `6` aborts, max visit count `4`: versus the
  session-start Linux post-OBJ baseline, max RSS is `-78.3%` (`26.3` ->
  `5.70 GiB`) and aborts are `-78%` (`27` -> `6`) for a `+11.9%` wall trade.
  Correctness tracks also closed Track 1A LEAK `20/20` plus NPE classification,
  and Track 1B improved the C-suite total to `108/29` after `58c100411b`.
- **C-suite OCaml↔Rust Pulse summary parity** — current totals are
  `108 matching / 29 diffs`, up `+58/-58` from the original `50/87` baseline.
  Per-file: arithmetic `6/5`, funptr `22/6`, interproc `16/1`, latent `7/7`,
  memory_leak `37/9`, specialization `20/1`.
- **Sweep correctness checkpoint** — store-textual sweep is `52` OK / `0` FAIL /
  `0` TIMEOUT; NPE expected `131` / found `140` (`+9`, classified as
  correctness-aligned with OCaml direct or Rust-strictly-more-precise by
  `scout_npe_per_file_full_remeasure`); LEAK `20/20` exact; UAF `7/7` exact.
- **In-flight latent parity** — `cluster_latent_summary_alias_contradiction` is
  in flight with worker-2, and `cluster_latent_free_null_split_constant_invalid`
  is in flight with worker-1.
- **Next perf wave** — treat the `RUNS=3` canonical Linux row (`4:47.79`,
  `5.70 GiB`, `6` aborts) as the baseline; any wall recovery should preserve
  the RAM and abort gains.
- **Parked correctness backlog** — `cluster_latent_record_post_for_address_porting`
  is deferred as a three-day deep porting track despite high ROI (`11.7`), and
  covers the remaining latent plus `may_double_free_if_alias` residuals.
- **OpenSSL perf / benchmark hygiene** — Linux now has the canonical
  corpus-comparable `RUNS=3 JOBS=4` row on the `74` `.sil` / `454` proc
  artifact: `445/445` procs, `4:47.79` median wall, `5.70 GiB` median max RSS,
  `6` median aborts, max visit count `4`; Linux max RSS is now below the
  historical macOS-derived Rust reference, while wall ratio is `6.71×` vs the
  old OCaml reference.
- **Deferred backlog** — micro-cleanups (`code_*`), speculative representation
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
