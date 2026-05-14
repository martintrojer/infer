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
| NPE count | expected `131`, found `131` (Linux baseline exact; macOS historical `140`/`+9`, see note below) |
| Leak count | expected `20`, found `20` (matches expected) |
| UAF count | expected `7`, found `7` (matches expected) |
| `latent.c` issue-set compare | exact at `(procedure, line, issue-type)`: `17` Rust / `17` OCaml |
| specialization summary harness | `20 / 20` procedures match (only `may_double_free_if_alias` residual) |
| `virt.sil` virtual dispatch | `0` skipped procedures (full coverage) |
| `make check` | current checkpoint passes with `INFER_BIN=../infer/bin/infer` |
| C-suite OCaml↔Rust Pulse summary triage | `88 matching / 49 diffs` (+38/-38 vs original `50/87` baseline) |

Linux store-textual NPE baseline is exact: expected `131`, found `131`. The
previously documented `+9` over expected was macOS-derived and does not
reproduce on this Linux checkout. Historical macOS per-host deltas were:

- `angelism.c`: `+5` (Pulse pre-evaluation surface from
  `cluster_a_taint_initial_formal_preeval_gap`).
- `fopen.c`: `-3`
- `latent.c`: `-1`
- `nullptr.c`: `+3` (real-bug divergence + pre-eval surface)
- `sizeof.c`: `+2` exported-Textual fidelity limit.
- `struct_values.c`: `+1`
- `var_arg.c`: `+2`

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
now analyze. Today's Linux tyre-kick additions:

- `502236c5b2` — sweep harness pre-warm to avoid first-run TIMEOUT races on
  cold caches.
- `e7dd96291a` — `cluster_latent_witness_routing_surface`: comparator-side
  `LatentAbortProgram` diagnostic routing plus `UsedAsBranchCond` on aliased
  heap-path values.
- `9078f04176` — `bug_store_textual_leak_dead_root_parity`: dead-root leak
  inspection (mirrors OCaml `check_memory_leaks` over
  `astate_before_filter`), allocator-return import to caller, and `BasedOn`
  pointer-arithmetic provenance for `reaches_into`-style suppression. LEAK
  moved from the first Linux `20/15` measurement to `20/20` exact; the macOS
  dashboard had already shown `20/20`.

### C-suite OCaml↔Rust Pulse summary parity (`88 matching / 49 diffs`)

A separate parity track compares OCaml and Rust Pulse summaries directly per
procedure on a slice of the C Pulse test suite (`arithmetic.c`, `funptr.c`,
`interprocedural.c`, `latent.c`, `memory_leak.c`, `specialization.c`,
`nullptr.c`). Standalone Rust analysis of `nullptr.c` completes in ~0.02s under
the standard 60s/2GB caps. The historical "recursion hang" was actually an
in-process OOM (~7.86 GB) inside
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
  mirroring OCaml `check_memory_leaks` over `astate_before_filter`,
  allocator-return import to caller, and `BasedOn` pointer-arithmetic
  provenance for `reaches_into`-style suppression.

Full six-file triage delta vs original 2026-05-11 baseline
(`50 matching / 87 diffs`) is now `88 matching / 49 diffs`
(`+38 matching / -38 diffs`). Per-file breakdown and per-pass narrative live
in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Current residual work is `cluster_latent_producer_heap_shape` for the remaining
`latent.c` heap-shape rows (after the fourth pass, latent.c is `6/8`) and
`cluster_memory_leak_array_index_residual` for the `memory_leak.c` summary
surface. `may_double_free_if_alias` remains the lone accepted
specialization.c residual.

## OpenSSL benchmark dashboard

Corpus: 74-file partial OpenSSL capture under
`~/infer-rs-bench/openssl-20260501-084151/`.

Default Rust caps:

- `pulse-max-heap-mb = 2048`
- `pulse-max-wall-secs = 60`
- pass `0` to disable either cap

Latest clean repeated checkpoint on the fresh patched-exporter re-export
(`textual-out-reexport-20260508-102338/`, `74` `.sil` files; DES and OBJ targets
present; `RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh` with
`TEXTUAL_DIR=.../textual-out-reexport-20260508-102338`). This table is the
latest trustworthy full-corpus measurement, not necessarily current HEAD:

Note: these dashboard numbers are macOS-derived historical reference. The
current Linux corpus has `74` `.sil` / `150` procs (vs macOS `74` `.sil` /
`446` procs), so it is not directly comparable; see
`mu task notes perf_remeasure_quiescent_host -w infer-rs` for context.

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
column adds `--pulse-intermediate-formula-gc`. Both columns are 3 runs each
on the same fresh export. Old-export historical numbers (`239.67s` median
wall, `13.17 GB` median max RSS, `18 / 570` aborts, clean exit) are from the
original `textual-out/` and are not directly comparable — the fresh export
has fewer procedure definitions (`446` analyzed instead of `570`) but
includes newer cleanup/nullify/exit-scope metadata.

Interpretation:

- At this clean checkpoint, default Rust vs OCaml old baseline is
  `244.70 / 42.9 ≈ 5.7×` (was `6.7×` before the parser fix and `8.0×`
  before the perf cleanup; `~30%` cumulative wall improvement on the same
  input).
- `--pulse-intermediate-formula-gc` is now roughly neutral on this corpus
  (`~2.5%` wall win; max RSS and peak footprint within noise of the default
  column). Worth keeping opt-in until we see another win condition. The
  cleanup pass was expanded (commit `01a51f99ed`) to also prune
  `term_value_index`, `fn_app_eqs`, `atoms`, and `const_cache` entries that
  become unreachable after the formula-variable GC.
- Stale `term_value_index` repair was rejected for the default path: it improved
  selected DES target wall time but made whole-program median slower and produced
  no repair hits in the focused counter run.
- Subsequent focused `state_cmp` landings cumulatively cut the main isolated
  hotspots substantially: `OBJ_bsearch_ex_` from `1.91s` to `~0.47s` and
  `DES_ede3_cfb_encrypt` from `~40.2s` after the first structural fixes to
  `~21.8s` after cached propagation sort keys and flat-slab `CanonTerm`.
  The latest full-corpus OpenSSL remeasure has not landed in this dashboard:
  earlier attempts were load-contaminated, and the current host sample is still
  not quiescent. The clean rerun is tracked by `perf_remeasure_quiescent_host`
  and should use the newly hardened benchmark script.
- The remaining wall-time gap to OCaml is tracked by the current perf DAG:
  `perf_remeasure_quiescent_host` should refresh whole-corpus numbers when the
  host is idle, and `perf_decide_next_track_after_profile_and_remeasure` gates
  any further perf work.

Benchmark artifacts from the latest run are under ignored `bench-out/` in the
main checkout. Historical OpenSSL archaeology is in
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

- **C-suite OCaml↔Rust Pulse summary parity** — five passes landed with
  measurable diff reduction; current full-suite total is `88 matching / 49
  diffs` (`+38/-38` vs original `50/87` baseline). Active residual work is
  `cluster_latent_producer_heap_shape` for latent producer heap-shape rows and
  `cluster_memory_leak_array_index_residual` for the `memory_leak.c` summary
  surface. `may_double_free_if_alias` remains the single accepted
  specialization.c residual.
- **OpenSSL perf / benchmark hygiene** — the benchmark script has stricter
  preflight/failure behavior and focused `state_cmp` fixes have landed. The
  remaining item is the quiescent-host full OpenSSL remeasure, pending a corpus
  parity decision because the Linux sample (`74` `.sil` / `150` procs) is not
  directly comparable to the macOS dashboard sample (`74` `.sil` / `446`
  procs). Do not run it while load/security daemons are high.
- **Decision gate** — after the clean, corpus-comparable full-corpus remeasure
  closes, `perf_decide_next_track_after_profile_and_remeasure` should prune
  obsolete placeholders and choose the next concrete track.
- **Correctness parity** — store-textual sweep-level Linux totals are exact and
  documented above (NPE `131/131`, LEAK `20/20`, UAF `7/7`; the macOS NPE `+9`
  remains only a historical per-host caveat). Reopen parity work only for new
  sweep regressions or a real Textual/export-fidelity project. Procedure-level
  summary parity is tracked separately by the C-suite triage track above.
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
