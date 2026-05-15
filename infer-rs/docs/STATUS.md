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
| C-suite OCaml↔Rust Pulse summary triage | `107 matching / 30 diffs` (+20/-20 today from the `87/50` session start; +57/-57 vs original `50/87`) |

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

### C-suite OCaml↔Rust Pulse summary parity (`107 matching / 30 diffs`)

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

Full six-file triage delta vs original 2026-05-11 baseline
(`50 matching / 87 diffs`) is now `107 matching / 30 diffs`
(`+57 matching / -57 diffs`). Today's Linux session started at `87/50`, moved
through `96/41`, and is now `107/30` (`+20 matching / -20 diffs`). Current
scoped per-file totals:

| file | matching / diffs | residual |
|---|---:|---|
| `arithmetic.c` | `6/5` | OCaml `NonDisjDomain` non-disj sideband mechanism; follow-up `arithmetic_ocaml_non_disj_summary_fallback` filed by worker-leak |
| `funptr.c` | `22/6` | `cluster_funptr_abort_propagation_specialized` residuals are closed; remaining surface is callback `Closure` plus minor issues |
| `interprocedural.c` | `16/1` | remaining residual: `trace_correctly_through_wrappers_bad` summary multiplicity |
| `latent.c` | `6/8` | producer-side cycle-cursor and record-post shape remain deferred; `bc0dc998c7` closed the `FN_nonlatent_use_after_free_bad{,2}` pre/post dedup mismatch |
| `memory_leak.c` | `37/9` | remaining surface after benign `Continue` duplicate preservation: array/index loop value-shape, realloc fail/success branch counts, `alias_ptr_free` flag, mutual-recursion shape, `alloc_ref_counted_arith` pointer arithmetic |
| `specialization.c` | `20/1` | `may_double_free_if_alias` summary-surface; deferred to apply-post deep port |
| **total** | **`107/30`** | **+20 matching / -20 diffs from session start `87/50`; +57/-57 vs original `50/87`** |

Per-file breakdown and per-pass narrative live in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Current residual work is split between the closed funptr abort-propagation
surface after `a8b8fe7bde`, the remaining callback `Closure`/minor funptr
surface, `interprocedural.c`'s remaining summary-multiplicity residual after
`e18143e41d`, the parked `cluster_latent_record_post_for_address_porting` deep
porting track for the remaining `latent.c` producer/record-post shape and
`may_double_free_if_alias` after `bc0dc998c7` closed the latent dedup mismatch,
and the sweep-level NPE `131/140` correctness-aligned delta documented above.

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
and growing), and the full Linux bench reaches end of corpus. Today's perf wave
then added latent summary dedup (`bc0dc998c7`), canonical-root sorting avoidance
(`da9b92c384`), and shared `ValueHistory` clones (`2f2c26a6a9`):

| checkpoint | run config | wall | max RSS | procs | aborts | max visit count |
|---|---|---:|---:|---:|---:|---:|
| OBJ reach-end (`b512df2924`) | `JOBS=4 RUNS=1` | `4:17.11` | `26.3 GiB` | `445/445` | `27` | `4` |
| after canonical-root sort skip (`da9b92c384`) | `JOBS=4 RUNS=1` | ~neutral | `25.4 GiB` | `445/445` | `23` | `4` |
| current (`2f2c26a6a9`) | `JOBS=4 RUNS=1` | `~4:58` | `18.96 GiB` | `445/445` | `19` | `4` |

Current Linux Rust baseline: corpus
`~/infer-rs-bench/openssl-20260514-121752/` (`74` `.sil` / `454` Textual procs),
`JOBS=4 RUNS=1`, wall `~4:58`, max RSS `18.96 GiB`, procs `445/445`, aborts
`19`, max visit count `4`. The `da9b92c384` focused SHA512 measurement moved
`state_cmp::canonicalize` self time from `41.13%` to `33.53%`. The
`2f2c26a6a9` `Arc<ValueHistory>` change cuts full-bench max RSS by `-28%` vs the
original `26.3 GiB` Linux reach-end baseline (`25.4` -> `18.96 GiB` over the
last step). The updated bench dashboard marks Linux as now lower on max RSS
than the historical macOS-derived Rust reference (`244.70s` wall, `16.79 GiB`,
`446` procs, `21` aborts), with the usual cross-machine/accounting caveat. Wall
did not improve: `~4:58` is slightly higher than the `4:17.11` reach-end
baseline, so the Rust/OCaml wall ratio is now roughly
`4:58 / 0:42.9 ≈ 6.9×`, worse than the historical `5.7×` macOS-reference ratio
because the latent dedup work added per-comparison cost and the canonicalize fix
only partially offset it.

Recommended next perf wave: profile the new wall cost. Likely target is the
`bc0dc998c7` latent dedup comparison path; a cheap fast path for exact-equal
heap shapes before hidden-history-insensitive comparison may recover wall while
preserving the RSS and abort gains.

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
  OpenSSL reach-end (`b512df2924`), latent summary dedup
  (`bc0dc998c7`), canonical-root sorting avoidance (`da9b92c384`), and shared
  `ValueHistory` clones (`2f2c26a6a9`). Correctness tracks also closed Track 1A
  LEAK `20/20` plus NPE classification, and Track 1B improved four C-suite
  files (`funptr.c`, `interprocedural.c`, `latent.c`, `memory_leak.c`) after
  `a8b8fe7bde`, `3b7b90f1a9`, `e18143e41d`, and `bc0dc998c7`.
- **C-suite OCaml↔Rust Pulse summary parity** — current totals are
  `107 matching / 30 diffs`, up `+57/-57` from the original `50/87` baseline.
  Per-file: arithmetic `6/5`, funptr `22/6`, interproc `16/1`, latent `6/8`,
  memory_leak `37/9`, specialization `20/1`.
- **Sweep correctness checkpoint** — store-textual sweep is `52` OK / `0` FAIL /
  `0` TIMEOUT; NPE expected `131` / found `140` (`+9`, classified as
  correctness-aligned with OCaml direct or Rust-strictly-more-precise by
  `scout_npe_per_file_full_remeasure`); LEAK `20/20` exact; UAF `7/7` exact.
- **In-flight refactors** — `state_cmp` paired-pass refactor is in flight with
  worker-1, and formula term helpers refactor is in flight with worker-2.
- **Next perf wave** — profile the new `~4:58` wall after the perf/RSS wave;
  likely target is latent dedup per-comparison cost, with an exact-equal heap
  shape fast path before hidden-history-insensitive comparison.
- **Parked correctness backlog** — `cluster_latent_record_post_for_address_porting`
  is deferred as a three-day deep porting track despite high ROI (`11.7`), and
  covers the remaining latent plus `may_double_free_if_alias` residuals.
- **OpenSSL perf / benchmark hygiene** — Linux now has a corpus-comparable
  current run on the `74` `.sil` / `454` proc artifact: `445/445` procs,
  `~4:58` wall, `18.96 GiB` max RSS, `19` aborts, max visit count `4`; max RSS
  is down from `26.3 GiB`, while wall is worse than the `4:17.11` reach-end
  baseline.
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
