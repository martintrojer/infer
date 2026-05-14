# OpenSSL Linux perf baseline/profile results (2026-05)

This records the Linux measurement pass for `scout_openssl_profile_wall_ram_hotspots` on the worker-1 OpenSSL corpus.

- Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1/infer-rs`
- Corpus: `/home/mtrojer/infer-rs-bench/openssl-20260514-121752` (`74` `.sil` files, `454` Textual procs; `445` Pulse-reachable procs in the full-corpus dynamic run)
- Final measured SHA: `a8b8fe7bde` (`pulse: propagate dynamic-type specialized aborts`), after fast-forwarding from the panic-fix SHA `bfb235e881`
- Full-corpus entrypoint: `scripts/bench_openssl_partial.sh` with built-in caps `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`
- Focused profile entrypoint: one `.sil`, `-j 1`, explicit `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`
- Tool availability: `perf` and `valgrind` are present; `cargo-flamegraph`, `flamegraph`, `heaptrack`, `heaptrack_print`, and `ms_print` are not on `PATH`, so Experiment 4 used `perf record/report` and Experiment 5 used Massif (`valgrind --tool=massif --pages-as-heap=yes`).
- Release profile strips symbols; profiling runs used `target/profiling/infer-rs` built with `RUSTFLAGS="-C force-frame-pointers=yes" cargo build --profile profiling -p infer-rs`.

## Resource-cap note

The planned full-corpus baseline did not complete within the hard wall guard. Both attempted full-corpus runs reached an OBJ-family long tail rather than finishing:

- Initial `bfb235e881` run: wrapper timed out at ~36m40s while at `407/445`, with `OBJ_bsearch_sn` active for ~35m and `max_visit_count=1279` still increasing.
- Final `a8b8fe7bde` run: wrapper timed out at 650s while at `405/445`, with two active OBJ-family workers (`OBJ_bsearch_sn` and `OBJ_bsearch_ln`), `max_visit_count=378` still increasing, and projected ETA still optimistic.

Therefore the tables below are **guarded/partial** where explicitly marked. They are still useful as hotspot attribution because the run had already analyzed/aborted all DES/hash/application hotspots and was dominated by OBJ at the guard.

## Experiment 1: Linux baseline at `-j 4`, `RUNS=3`

Command attempted:

```sh
OUT_DIR="/tmp/bench-baseline-j4-a8-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 timeout --preserve-status 650s scripts/bench_openssl_partial.sh
```

Result: no median over three completed runs is publishable because run 1 exceeded the wall guard and the remaining runs were not started.

| run | jobs | status | wall at guard | procs analyzed at guard | full-corpus exit | heap aborts seen | wall aborts seen | max RSS / footprint | max visit count | notes |
|---:|---:|---|---:|---:|---:|---:|---:|---|---:|---|
| 1 | 4 | aborted by outer 650s guard | ~650s | 407/445 known, 405/445 completed | 143 from `timeout` | 15 | 2 | unavailable because timeout killed `/usr/bin/time` before summary | 378 | Active tail: `OBJ_bsearch_sn` and `OBJ_bsearch_ln`; `OBJ_bsearch_ex_` heartbeat had `elapsed=10m13s`, `states=disj=311`, post heap nodes `134589`, post edges `260634`, formula lin `21353`, eq `11040`. |
| 2 | 4 | not run | n/a | n/a | n/a | n/a | n/a | n/a | n/a | Stopped after run 1 exceeded guard. |
| 3 | 4 | not run | n/a | n/a | n/a | n/a | n/a | n/a | n/a | Stopped after run 1 exceeded guard. |
| **median** | 4 | **unavailable** | **>650s lower bound** | **partial** | **n/a** | **n/a** | **n/a** | **n/a** | **>=378** | STATUS.md should not be updated from this as a completed baseline. |

Partial slow-proc signal before the `a8b8fe7bde` guard:

| proc | elapsed in full-corpus log | abort? | note |
|---|---:|---|---|
| `OBJ_bsearch_sn` | active ~9m57s at guard | not yet complete | top active wall tail; depends on `OBJ_bsearch_ex_` summary/import path. |
| `OBJ_bsearch_ln` | active ~7m34s at guard | not yet complete | second active OBJ tail. |
| `DES_cfb_encrypt` | 1m19s | wall cap | focused run: 27.4s / 2.03 GiB RSS; Massif under Valgrind hit the 60s cap. |
| `DES_ede3_cbcm_encrypt` | 1m08s | heap cap | focused run: 4.0s / 2.05 GiB RSS. |
| `mdc2_body` | 54.9s | no focused RSS issue | focused run: 0.79s / 0.55 GiB RSS; full-corpus time likely scheduling/contention. |
| `dgst_main` | 48.1s | not in this log excerpt as focused heap cap | focused run: 16.6s / 2.13 GiB RSS. |
| `enc_main` | 27.4s | heap cap | focused run: 15.2s / 2.14 GiB RSS. |
| `sha512_block_data_order` | 24.0s | heap cap in full corpus | focused run: 34.4s / 1.81 GiB RSS, no focused abort. |
| `DES_ofb_encrypt` | 19.4s | heap cap | DES-family memory pressure. |
| `DES_ede3_cbc_encrypt` | 15.7s | heap cap | DES-family memory pressure. |

The completed-run Rust/OCaml ratio cannot be computed. The only honest lower bound is `>650s / 42.9s = >15.2x`, already much worse than the historical macOS dashboard ratio (`244.70s / 42.9s = 5.70x`).

## Experiment 2: `-j` scaling curve

Command attempted with `timeout --preserve-status 650s` and the required `free -h` preflight.

| jobs | wall_secs | max_rss | parallelism_factor | status |
|---:|---:|---:|---:|---|
| 1 | >650 | n/a | 1.00 lower-bound reference | aborted by outer guard before completion; no `/usr/bin/time` summary because the guard killed the wrapper. |
| 4 | >650 | n/a | unknown; also >650 | aborted by outer guard in Experiment 1. |
| 16 | not run | n/a | n/a | skipped after `-j1` and `-j4` both exceeded the 600s/650s guard. |
| 64 | not run | n/a | n/a | skipped; no high-concurrency OOM risk taken. |

Conclusion: the planned scaling curve is not measurable on this SHA under the hard wall cap. The curve is dominated by the OBJ-family non-convergence/long-tail rather than ordinary parallel scheduling.

## Experiment 4: perf profiles for top wall hotspots

Because `cargo-flamegraph`/`flamegraph` are unavailable, CPU attribution used:

```sh
perf record -F 999 -g --call-graph fp -o /tmp/openssl-perf-a8/perf-<proc>.data -- \
  target/profiling/infer-rs --pulse-only --quiet --trace-ondemand \
  --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -j 1 --procedures-filter <proc> <single .sil>
perf report --stdio --no-children --percent-limit 1
```

Artifacts were copied to `/tmp/openssl-profile-data/`.

### `OBJ_bsearch_sn` (`obj_dat.sil`)

Focused `OBJ_bsearch_sn` completed in `19.4s` under perf and analyzed `6/6` dependent OBJ procs. This did **not** reproduce the full-corpus runaway, but it profiles the same OBJ state-comparison surface.

Top self-time symbols:

| self % | symbol |
|---:|---|
| 17.56 | `pulse::state_cmp::canonicalize` |
| 9.09 | `malloc` |
| 7.88 | `_int_free` |
| 7.67 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 4.46 | `_int_malloc` |
| 4.27 | `pulse::state_cmp::Canonicalizer::propagate_attrs` |
| 3.10 | `pulse::state_cmp::Canonicalizer::assign_remaining_memory` |
| 2.33 | `pulse::state_cmp::reachable_from_stack` |
| 2.16 | `BTreeMap::clone::clone_subtree` |
| 1.81 | `pulse::state_cmp::canonical_heap` |
| 1.65 | `core::slice::sort::unstable::quicksort::quicksort` |
| 1.63 | `pulse::state_cmp::Canonicalizer::assign_remaining_attrs` |

Interpretation: OBJ wall is still a `state_cmp::canonicalize`/canonical heap-memory propagation problem with allocator churn from map cloning/sorting.

### `DES_cfb_encrypt` (`cfb_enc.sil`)

Focused run completed in `28.8s` under perf (`peak_rss=2.03GB`, heap cap warning in the non-perf run; Massif under Valgrind hit the wall cap).

Top self-time symbols:

| self % | symbol |
|---:|---|
| 24.59 | `malloc` |
| 18.13 | `_int_free` |
| 6.55 | `pulse::state_cmp::Canonicalizer::flatten_term` |
| 5.97 | `_int_malloc` |
| 5.11 | `pulse::formula::term::Term::cmp` |
| 5.07 | `malloc_consolidate` |
| 4.34 | `pulse::state_cmp::canonicalize` |
| 3.28 | `unlink_chunk.constprop.0` |
| 3.25 | `CanonTerm::partial_cmp` |
| 2.11 | `pulse::formula::term::Term::collect_vars` |
| 1.19 | `pulse::formula::term::Term::subst_var` |

Interpretation: DES CFB is allocator/temporary dominated, with formula-term flattening/comparison inside canonicalization. This is a better focused CPU target than `DES_ede3_cfb_encrypt` on this SHA because it is wall-heavy and hits the wall cap in the full corpus.

### `sha512_block_data_order` (`sha512.sil`)

Focused run completed in `35.3s` under perf (`analyzed=2/2`, `peak_rss=1.80GB`, no focused abort).

Top self-time symbols:

| self % | symbol |
|---:|---|
| 41.24 | `pulse::state_cmp::canonicalize` |
| 9.93 | `pulse::state_cmp::Canonicalizer::propagate_attrs` |
| 4.27 | `malloc` |
| 3.41 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 3.16 | `core::slice::sort::shared::smallsort::small_sort_general` |
| 3.12 | `_int_free` |
| 2.91 | `core::slice::sort::unstable::quicksort::quicksort` |
| 2.62 | `BTreeMap::insert` |
| 2.16 | `pulse::state_cmp::Canonicalizer::partial_fn_app_key` |
| 1.73 | `pulse::state_cmp::Canonicalizer::assign_remaining_attrs` |
| 1.36 | `core::slice::sort::stable::quicksort::quicksort` |

Interpretation: SHA512 is the cleanest single-proc `state_cmp::canonicalize` hotspot: over 40% self-time in canonicalization plus attr propagation/sort-key construction.

## Experiment 5: Massif profiles for top RSS contributors

Because `heaptrack` is unavailable, memory attribution used:

```sh
valgrind --tool=massif --pages-as-heap=yes --massif-out-file=/tmp/openssl-massif-a8/massif-<proc>.out -- \
  target/profiling/infer-rs --pulse-only --quiet --trace-ondemand \
  --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -j 1 --procedures-filter <proc> <single .sil>
```

`--pages-as-heap=yes` makes Massif totals comparable to RSS/arena pressure but coarse (page/syscall oriented); `ms_print` is not installed, so peak stacks were extracted directly from the Massif files.

| proc | `.sil` | focused wall / log peak | Massif peak | dominant allocation stack | growth shape |
|---|---|---:|---:|---|---|
| `md4_block_data_order` | `md4_dgst.sil` | 20.1s / `peak_rss=2.48GB`; heap-cap warning | 2,702,180,352 B = 2.52 GiB | `Vec<HistoryEvent>::clone` -> `ValueHistory::clone` -> `BTreeMap::clone_subtree`; leaf via `exec_load` / `exec_instr_with_summaries` / `PulseTransferFunctions::exec_node` | sharp rise during instruction transfer/load history cloning despite only `max_node_disjuncts=1`; memory is history/clone dominated, not disjunct fan-out. |
| `DES_ede3_cfb_encrypt` | `cfb64ede.sil` | 22.1s under Massif / `peak_rss=2.10GB`; heap-cap warning | 2,234,601,472 B = 2.08 GiB | `Vec<HistoryEvent>::clone` -> `ValueHistory::clone` -> `BTreeMap::clone_subtree` -> `BaseMemory::clone` / `BaseMemory::map_values` -> `AbductiveDomain::preserve_canonical_heap_targets` -> `preserve_canonical_access_targets` | large DES retained-state clone plateau near the 2 GiB cap; max disjuncts 20 with 1,899 retained disjunct states. |
| `DES_cfb_encrypt` | `cfb_enc.sil` | Massif run hit wall cap (`1m08s`, `peak_rss=1.20GB` under Valgrind); non-Valgrind focused run 27.4s / 2.03 GiB | 1,292,382,208 B = 1.20 GiB before wall cap | `Vec<HistoryEvent>::clone` -> `ValueHistory::clone` -> `Edges::recency_bindings_cloned` -> `BaseMemory::map_values` -> `AbductiveDomain::preserve_canonical_heap_targets` | monotonic growth until the 60s wall cap under Valgrind; likely would approach/exceed the non-Valgrind 2 GiB cap if allowed. |

Additional focused RSS sentinels:

| proc | `.sil` | focused wall | max RSS | aborts |
|---|---|---:|---:|---|
| `dgst_main` | `dgst.sil` | 16.6s | 2,236,364 KiB = 2.13 GiB | heap cap |
| `enc_main` | `enc.sil` | 15.2s | 2,239,308 KiB = 2.14 GiB | heap cap |
| `DES_ede3_cbcm_encrypt` | `ede_cbcm_enc.sil` | 4.0s | 2,145,916 KiB = 2.05 GiB | heap cap |
| `sha512_block_data_order` | `sha512.sil` | 34.5s | 1,894,492 KiB = 1.81 GiB | none focused |
| `OBJ_bsearch_ex_` alone | `obj_dat.sil` | 0.67s | 241,132 KiB = 0.23 GiB | none when isolated |
| `OBJ_bsearch_sn`/`OBJ_bsearch_ln` filters | `obj_dat.sil` | >95s timeout in focused filtered run | n/a | wall cap in dependency `OBJ_bsearch_ex_` at 61s |

## Cross-reference to STATUS.md and attack-surface hypotheses

- The historical macOS STATUS row (`244.70s`, `16.79 GiB`, `446 procs`) is **not** matched on Linux. Current Linux full-corpus default is a non-completing `>650s` lower bound with the same safety caps.
- The old OCaml baseline is `42.9s`. A completed Rust/OCaml ratio is unavailable; the lower bound is `>15.2x`, refuting the experiment-plan expectation of a `4-7x` ratio on this SHA/corpus.
- The wall/RAM coupling hypothesis is partly validated:
  - DES/hash/application procs that are slow in focused runs are also near the 2 GiB heap cap (`DES_cfb_encrypt`, `DES_ede3_cfb_encrypt`, `md4_block_data_order`, `dgst_main`, `enc_main`). Their Massif stacks point to retained-state/value-history cloning.
  - OBJ is the exception and the highest-priority wall blocker: isolated `OBJ_bsearch_ex_` is tiny, but full-corpus `OBJ_bsearch_sn`/`OBJ_bsearch_ln` trigger repeated/long `OBJ_bsearch_ex_` live-fixpoint heartbeats with high `max_visit_count`. That suggests an interproc/specialization or summary-demand shape rather than simple single-proc RSS blow-up.
- The attack-surface map's `state_cmp/leq` and retained-state storage categories are still correct. `state_cmp::canonicalize` dominates OBJ and SHA512 CPU, while DES-family memory is dominated by `ValueHistory`/`BaseMemory` clone paths.

## Recommendations / next fix tasks

1. **`fix_openssl_obj_bsearch_interproc_convergence`** — highest priority. Full-corpus completion is blocked by OBJ-family (`OBJ_bsearch_sn`/`OBJ_bsearch_ln`) long-tail behavior and repeated `OBJ_bsearch_ex_` live-fixpoint growth (`max_visit_count` reached at least 378 before the guard on `a8b8fe7bde`, and 1279 on the pre-dynamic-abort-propagation SHA). This should inspect why isolated OBJ filters complete quickly while the full-corpus/specialized environment does not.
2. **`fix_state_cmp_canonicalizer_attr_memory_sortkeys`** — profile-driven CPU fix. `sha512_block_data_order` spends ~41% self-time in `state_cmp::canonicalize`, and OBJ spends ~18% plus propagation/heap/attrs helpers. Target duplicate canonicalizer sorting/key construction and attr/memory propagation allocation.
3. **`fix_value_history_base_memory_clone_pressure`** — RAM fix after the OBJ wall gate. Massif consistently points at `Vec<HistoryEvent>::clone` / `ValueHistory::clone` / `BTreeMap::clone_subtree` under `BaseMemory::map_values` and `preserve_canonical_heap_targets` for DES/hash RSS-cap procs.

Do not update `docs/STATUS.md` from this run; it should get a separate status note saying Linux default currently exceeds the guard / is blocked by OBJ convergence, not a completed median row.
