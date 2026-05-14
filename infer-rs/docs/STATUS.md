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
| NPE count | expected `131`, found `~139` (`+8` over expected; per-file triage in `bug_npe_per_file_triage_after_return_slot_fix`) |
| Leak count | expected `20`, found `20` (EXACT) |
| UAF count | expected `7`, found `7` (EXACT) |
| `latent.c` issue-set compare | exact at `(procedure, line, issue-type)`: `17` Rust / `17` OCaml |
| specialization summary harness | `20 / 20` procedures match (only `may_double_free_if_alias` residual) |
| `virt.sil` virtual dispatch | `0` skipped procedures (full coverage) |
| `make check` | current checkpoint passes with `INFER_BIN=../infer/bin/infer` |
| C-suite OCaml↔Rust Pulse summary triage | `90 matching / 47 diffs` (+40/-40 vs original `50/87` baseline) |

### NPE issue-count deltas (current Linux)

Current Linux store-textual NPE count is expected `131`, found `~139` (`+8`
net over expected). Per-file triage is in flight under
`bug_npe_per_file_triage_after_return_slot_fix`; do not treat this as a
classified parity limit yet. The `+8` includes false negatives restored by
`bug_recency_shift_new_false_negatives` (commit `7e28401d3d`) plus the original
macOS-style baseline drift that remains after recency-limit alignment
(`459ec03492`) and return-slot preservation (`f78e622ff1`). Await the per-file
classification before updating accepted/bug attribution.

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

- `502236c5b2` — sweep harness pre-warm to avoid first-run TIMEOUT races on
  cold caches (worker-1, `scout_sweep_cold_start_timeout_audit`).
- `e7dd96291a` — `cluster_latent_witness_routing_surface`: comparator-side
  `LatentAbortProgram` diagnostic routing plus `UsedAsBranchCond` on aliased
  heap-path values (worker-2).
- `9078f04176` — `bug_store_textual_leak_dead_root_parity`: dead-root leak
  inspection (mirrors OCaml `check_memory_leaks` over
  `astate_before_filter`), allocator-return import to caller, and `BasedOn`
  pointer-arithmetic provenance for `reaches_into`-style suppression. LEAK
  moved from the first Linux `20/15` measurement to `20/20` exact with three
  concept additions (worker-leak).
- `76f3cf9380` — docs refresh for the Linux correctness checkpoint and the
  `nullptr.c` harness-OOM framing.
- `459ec03492` — `bug_align_pulse_recency_limit_default`: set Rust's default
  `pulse_recency_limit` to `Some(32)`, matching OCaml (worker-1).
- `83e63f2cb8` — `cluster_memory_leak_array_index_residual`: retain
  `ArrayAccess` index attrs during summary normalization (worker-leak; the
  same array-index fix tracked as `6d2ce94de3` in earlier mu notes).
- `f78e622ff1` — `bug_recency_shift_overreports`: preserve the C textual
  `return` slot and remove the allocation-only gate in summary return-value
  discovery (worker-leak).
- `7e28401d3d` — `bug_recency_shift_new_false_negatives`: restore C return
  facts, accept both `return` and `__return`, and add the missing Rust C
  `memset` model (worker-1).
- `5524f43c71` — docs refresh for C-suite triage after recency findings.

### C-suite OCaml↔Rust Pulse summary parity (`90 matching / 47 diffs`)

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
(`50 matching / 87 diffs`) is now `90 matching / 47 diffs`
(`+40 matching / -40 diffs`). Current scoped per-file notes: `latent.c` is
`6/8`, `memory_leak.c` moved from `25/21` to `27/19` after the array-index
commit (`83e63f2cb8`; `6d2ce94de3` in earlier mu notes), and
`specialization.c` remains `20/1`. Per-file breakdown and per-pass narrative
live in
[`docs/triage/c_pulse_summary_mismatches_2026_05_11.md`](triage/c_pulse_summary_mismatches_2026_05_11.md).

Current residual work is the parked `cluster_latent_record_post_for_address_porting`
deep porting track for the remaining `latent.c` producer/record-post shape,
`bug_npe_per_file_triage_after_return_slot_fix` for the current Linux NPE `+8`,
and the `specialization.c` `may_double_free_if_alias` force-continue track.

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

- **C-suite OCaml↔Rust Pulse summary parity** — done today: LEAK parity
  (`9078f04176`), recency-limit alignment (`459ec03492`), latent
  comparator-side surface (`e7dd96291a`), return-slot import/preservation
  (`f78e622ff1`, `7e28401d3d`), array-index attr retention (`83e63f2cb8`;
  `6d2ce94de3` in earlier mu notes), and the C `memset` model (`7e28401d3d`).
  Current full-suite total is `90 matching / 47 diffs` (`+40/-40` vs original
  `50/87` baseline), with `latent.c` `6/8`, `memory_leak.c` `27/19`, and
  `specialization.c` `20/1`.
- **In-flight correctness follow-ups** — `bug_npe_per_file_triage_after_return_slot_fix`
  (worker-1) is classifying the current Linux NPE `+8`; worker-2 is pursuing
  the `specialization.c` `may_double_free_if_alias` force-continue residual.
- **Parked correctness backlog** — `cluster_latent_record_post_for_address_porting`
  is deferred as multi-day deep porting despite high ROI (`11.7`).
- **OpenSSL perf / benchmark hygiene** — the benchmark script has stricter
  preflight/failure behavior and focused `state_cmp` fixes have landed. The
  remaining host-gated item is `perf_remeasure_quiescent_host`, pending a
  corpus parity decision because the Linux sample (`74` `.sil` / `150` procs)
  is not directly comparable to the macOS dashboard sample (`74` `.sil` /
  `446` procs). Do not run it while load/security daemons are high.
- **Decision gate** — after the clean, corpus-comparable full-corpus remeasure
  closes, `perf_decide_next_track_after_profile_and_remeasure` should prune
  obsolete placeholders and choose the next concrete track.
- **Correctness parity** — store-textual sweep-level Linux totals are documented
  above (NPE expected `131`, found `~139`; LEAK `20/20` exact; UAF `7/7`
  exact). Reopen parity work only for new sweep regressions or a real
  Textual/export-fidelity project. Procedure-level summary parity is tracked
  separately by the C-suite triage track above.
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
