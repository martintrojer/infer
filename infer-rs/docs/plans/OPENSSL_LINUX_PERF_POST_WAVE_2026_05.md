# OpenSSL Linux perf post-wave remeasure (2026-05)

This records `scout_openssl_perf_post_wave_remeasure`: a re-profile at current HEAD after the five-commit perf wave that fixed OBJ convergence, canonicalizer sorting, `ValueHistory` sharing, and sort-helper reuse.

- Workspace: `/home/mtrojer/infer-rs/infer-rs`
- Corpus: `/home/mtrojer/infer-rs-bench/openssl-20260514-121752` (`74` `.sil` files; `445` Pulse-reachable procs in the dynamic run)
- Measured SHA: `70ca0c562d6339dccefd003905b27fbce4f26123` (`pulse: reuse canonicalizer keyed sort helpers`)
- Full-corpus entrypoint: `scripts/bench_openssl_partial.sh` with built-in `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`
- Focused runs: `-j 1 --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`, wrapped with `ulimit -v 8388608` and `timeout 180` where applicable
- Tooling: `perf record/report` and `valgrind --tool=massif --pages-as-heap=yes`; no heaptrack/cargo-flamegraph install attempted.
- Baseline for diffs: `infer-rs/docs/plans/OPENSSL_LINUX_PERF_BASELINE_RESULTS_2026_05.md` (`a8b8fe7bde`, pre-wave, full-corpus did not finish under guard).

## Headline

The original blocker moved down decisively: OBJ no longer dominates the full-corpus wall and `max_visit_count` is bounded at `4`. The new dominant wall/RAM surface is retained-state memory pressure in DES/hash/application procs under the 2 GiB per-proc cap. The top focused CPU symbols are still canonicalization/allocator-heavy, but the leading self-time has shifted from one huge `state_cmp::canonicalize` site to a broader mix of:

- `pulse::state_cmp::canonicalize` and its `map_sorted_edges`/`map_value`/`propagate_attrs` helpers,
- allocator churn (`malloc`, `_int_free`, `_int_malloc`, `malloc_consolidate`), and
- formula/history expansion (`pulse::formula::expand_formula_reachable`, `ValueHistory::merge` / `HistoryPath` cloning in Massif).

## Experiment 1: full OpenSSL bench, `RUNS=1 JOBS=4`

Command:

```sh
cd /home/mtrojer/infer-rs/infer-rs
OUT_DIR="/tmp/bench-post-wave-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=1 JOBS=4 REBUILD=1 scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/bench-post-wave.log
```

Result artifact: `/tmp/bench-post-wave-20260515-014524/`.

| run | exit | checker elapsed | `/usr/bin/time` wall | user | sys | max RSS | analyzed | aborts | max visit count |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 0 | 4m33s | 4:35.38 | 822.22s | 7.59s | 19,802,296 KiB = 18.89 GiB | 445/445 | 15 | 4 |

Notes:

- The script summary TSV omitted `real_s` because its GNU-time parser expected the `Elapsed` line to start at column 0; the raw log has `Elapsed (wall clock) time ...: 4:35.38`.
- Compared with the first completed post-OBJ baseline (`4:17.11`, 26,256,148 KiB, aborts 27) and the `Arc<ValueHistory>` note (`~4:58`, 19,886,540 KiB, aborts 19), this HEAD is in the same broad 4-5 minute band with RSS near 19 GiB and fewer aborts than the pre-Arc run. Single-run wall is noisy due to scheduling/tail shape.
- Compared with the pre-wave baseline doc, this is the main result: full corpus now completes. The pre-wave `a8b8fe7bde` run hit the 650s guard at about `405/445`, had no publishable RSS, and had `max_visit_count >= 378`.

### Full-run slow-proc tail

Top slow procs from `/tmp/bench-post-wave-20260515-014524/run-1.slow.tsv`:

| rank | proc | elapsed in full run | abort signal |
|---:|---|---:|---|
| 1 | `DES_cfb_encrypt` | 57.7s | heap cap (`peak_rss_delta=2.22GB`) |
| 2 | `mdc2_body` | 42.9s | heap cap (`2.19GB`) |
| 3 | `ca_main` | 40.3s | none in full-run root; focused all-corpus root hits heap cap |
| 4 | `dgst_main` | 39.2s | focused all-corpus root hits heap cap; appears twice under dynamic specialization |
| 5 | `cms_main` | 35.7s | no focused profile in this pass |
| 6 | `s_client_main` | 34.6s | no focused profile in this pass |
| 7 | `make_ocsp_response` | 33.7s | no focused profile in this pass |
| 8 | `pkcs12_main` | 33.6s | no abort; large retained state, full-run `peak_rss=18.88GB` process-wide |
| 9 | `DES_ede3_cbcm_encrypt` | 33.3s | heap cap (`2.45GB`) |
| 10 | `req_main` / `DES_cbc_cksum` | 32.2s | no focused profile / no focused profile |
| 13 | `sha512_block_data_order` | 26.5s | no focused abort; full-run process RSS high because parallel workers were live |
| 16 | `OBJ_bsearch_sn` | 15.7s | now bounded, no runaway |
| 22 | `DES_ede3_cfb_encrypt` | 11.9s | heap cap (`3.24GB` in full run) |

Full-run heap/wall aborts (`15` total):

`DES_ede3_cfb_encrypt`, `x509_main`, `ripemd160_block_data_order`, `md4_block_data_order`, `DES_ede3_cbcm_encrypt`, `DES_cfb_encrypt`, `mdc2_body`, `md5_block_data_order`, `DES_ofb_encrypt`, `sha256_block_data_order`, `DES_ede3_cfb64_encrypt`, `DES_ede3_cbc_encrypt`, `main`, `fcrypt_body`, `whirlpool_block`.

## Focused wall/RSS reconnaissance before profiling

For focused wall profiles, using the whole corpus with `--procedures-filter` matters because transitive callees and duplicate declarations change retained dependencies. A single-file `mdc2_body` run, for example, is only `0.25s/69MB`, while all-corpus transitive filtering retains DES dependencies and reproduces the full-run 50s shape.

Representative focused all-corpus runs (`target/release/infer-rs`, `RUST_LOG=warn,ondemand=info`):

| proc filter | retained procs/files | focused wall | max RSS | key progress line |
|---|---:|---:|---:|---|
| `DES_cfb_encrypt` | 3 / 2 | 1:23.53 | 2,903,648 KiB | root done 1m21s; wall cap warning at 1m01s; root `peak_rss=2.77GB` |
| `mdc2_body` | 7 / 3 | 1:03.35 | 2,481,236 KiB | root done 50.6s; `peak_rss=2.37GB` |
| `ca_main` | 111 / 6 | 1:02.30 | 4,643,496 KiB | OBJ callees ~16s each; root heap-cap warning and root done 12.1s |
| `dgst_main` | 25 / 6 | 0:49.54 | 3,027,072 KiB | root heap-cap warning; two root summaries 23.7s + 20.3s |
| `sha512_block_data_order` | 2 / 1 | 0:28.34 | 1,250,916 KiB | root done 27.1s, no abort |

## Experiment 2/3: `perf record` on top wall-bound procs

`perf` command shape:

```sh
ulimit -v 8388608
RUST_LOG=warn,ondemand=info timeout 180 perf record -F 999 -g --call-graph fp \
  -o /tmp/post-wave-perf/perf-<proc>.data -- \
  target/profiling/infer-rs --pulse-only --quiet --trace-ondemand \
  --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 -j 1 \
  --procedures-filter '<proc>' \
  /home/mtrojer/infer-rs-bench/openssl-20260514-121752/textual-out/*.sil
perf report --stdio --no-children --percent-limit 1 -i /tmp/post-wave-perf/perf-<proc>.data
```

I profiled the top three reproducible wall-bound focused filters (`DES_cfb_encrypt`, `mdc2_body`, `ca_main`) and also re-ran `sha512_block_data_order` as the canonicalizer sentinel from the baseline.

### `DES_cfb_encrypt` (top full-run wall)

Focused all-corpus perf run: root wall `1m19s`, wall-cap warning at `1m01s`, retained `3/3` procs, `max_visit_count=4`.

Top self-time symbols:

| self % | symbol |
|---:|---|
| 9.33 | `pulse::formula::expand_formula_reachable` |
| 9.32 | `malloc` |
| 8.14 | `pulse::state_cmp::canonicalize` |
| 7.20 | `_int_free` |
| 5.86 | `pulse::state_cmp::Canonicalizer::map_sorted_edges` |
| 5.61 | `Filter<I,P>::next` |
| 4.88 | `pulse::state_cmp::Canonicalizer::map_value` |
| 4.14 | `pulse::state_cmp::Canonicalizer::propagate_attrs::{{closure}}` |
| 3.27 | `_int_malloc` |
| 2.12 | `pulse::checker::DisjunctiveStateStats::from_domain` |
| 1.97 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 1.95 | `__memmove_avx512_unaligned_erms` |
| 1.89 | `pulse::state_cmp::reachable_from_stack` |
| 1.86 | `core::hash::BuildHasher::hash_one` |
| 1.84 | `malloc_consolidate` |
| 1.67 | `pulse::state_cmp::canonical_heap` |
| 1.47 | `pulse::state_cmp::Canonicalizer::flatten_term` |
| 1.32 | `BTreeMap::insert` |
| 1.10 | `pulse::abductive::AbductiveDomain::history_of_value` |
| 1.00 | `pulse::state_cmp::Canonicalizer::assign_remaining_attrs` |

Interpretation: DES CFB is no longer dominated by `malloc` alone. Formula reachability expansion and canonicalizer edge/value mapping are now co-equal with allocator churn.

### `mdc2_body` (second full-run wall)

Focused all-corpus perf run: root wall `53.0s`, retained `7/7` procs, `DES_set_key_unchecked` dependency `11.6s`, `max_visit_count=4`.

Top self-time symbols:

| self % | symbol |
|---:|---|
| 17.94 | `pulse::state_cmp::canonicalize` |
| 10.63 | `malloc` |
| 6.97 | `_int_free` |
| 5.48 | `pulse::state_cmp::Canonicalizer::map_value` |
| 5.42 | `pulse::state_cmp::Canonicalizer::map_sorted_edges` |
| 4.54 | `pulse::state_cmp::Canonicalizer::propagate_attrs::{{closure}}` |
| 3.49 | `_int_malloc` |
| 2.81 | `pulse::formula::expand_formula_reachable` |
| 2.51 | `__memmove_avx512_unaligned_erms` |
| 2.22 | `BTreeMap::insert` |
| 2.10 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 1.83 | `core::hash::BuildHasher::hash_one` |
| 1.82 | `pulse::checker::DisjunctiveStateStats::from_domain` |
| 1.69 | `pulse::state_cmp::reachable_from_stack` |
| 1.59 | `pulse::state_cmp::canonical_heap` |
| 1.47 | `malloc_consolidate` |
| 1.15 | `core::ops::function::FnMut::call_mut` |

Interpretation: `mdc2_body` is the cleanest post-wave wall CPU target: canonicalization is still the largest single Rust self symbol, but it is specifically the edge/value mapping pass after the helper refactor rather than the old broad sort-key construction.

### `ca_main` (third reproducible wall-bound focused filter)

Focused all-corpus perf run: retained `111/111` procs; OBJ dependencies now complete (`OBJ_bsearch_ln` ~16s); root heap-cap warning then root done ~12s; total focused elapsed ~49s under perf.

Top self-time symbols:

| self % | symbol |
|---:|---|
| 13.15 | `malloc` |
| 10.39 | `_int_free` |
| 8.70 | `pulse::state_cmp::canonicalize` |
| 4.58 | `pulse::state_cmp::Canonicalizer::map_sorted_edges` |
| 4.12 | `_int_malloc` |
| 3.45 | `pulse::state_cmp::Canonicalizer::map_value` |
| 2.73 | `core::hash::BuildHasher::hash_one` |
| 2.63 | `pulse::state_cmp::Canonicalizer::propagate_attrs::{{closure}}` |
| 2.25 | `pulse::checker::DisjunctiveStateStats::from_domain` |
| 2.22 | `__memmove_avx512_unaligned_erms` |
| 2.17 | `SipHasher::write` |
| 1.86 | `pulse::state_cmp::reachable_from_stack` |
| 1.82 | `malloc_consolidate` |
| 1.67 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 1.43 | `pulse::state_cmp::canonical_heap` |
| 1.35 | `BTreeMap::clone::clone_subtree` |
| 1.24 | `BTreeMap::insert` |
| 1.22 | `pulse::state_cmp::Canonicalizer::assign_remaining_attrs` |
| 1.03 | `unlink_chunk.constprop.0` |

Interpretation: application roots are allocator/canonicalizer mixtures over many callees, not a single-proc convergence issue.

### `sha512_block_data_order` sentinel (not top-3 wall this run, but baseline cross-reference)

Focused all-corpus perf run: root `28.0s`, retained `2/2`, no abort.

Top self-time symbols:

| self % | symbol |
|---:|---|
| 31.38 | `pulse::state_cmp::canonicalize` |
| 8.32 | `pulse::state_cmp::Canonicalizer::propagate_attrs::{{closure}}` |
| 6.48 | `pulse::state_cmp::Canonicalizer::map_value` |
| 4.83 | `malloc` |
| 3.57 | `_int_free` |
| 3.11 | `BTreeMap::insert` |
| 2.76 | `Map<I,F>::next` |
| 2.63 | `FnMut::call_mut` |
| 2.40 | `pulse::state_cmp::Canonicalizer::partial_fn_app_key` |
| 2.29 | `pulse::state_cmp::Canonicalizer::propagate_memory` |
| 2.00 | `pulse::state_cmp::Canonicalizer::assign_remaining_attrs` |
| 1.97 | `core::slice::sort::stable::quicksort::quicksort` |
| 1.79 | `_int_malloc` |
| 1.65 | `core::slice::sort::stable::quicksort::quicksort` |
| 1.61 | `core::hash::BuildHasher::hash_one` |
| 1.57 | `__memmove_avx512_unaligned_erms` |
| 1.46 | `pulse::state_cmp::Canonicalizer::map_sorted_edges` |

The canonicalizer remains the largest single CPU symbol, but it is down from the pre-wave `41.24%` and the immediate post-sort-key-fix `33.5-31.6%` notes.

## Experiment 4: Massif on top RSS procs

Full-run RSS abort deltas ranked the top procs as `DES_ede3_cfb_encrypt` (`3.24GB`), `x509_main` (`3.13GB`), `ripemd160_block_data_order` (`2.79GB`), followed by `md4_block_data_order` (`2.60GB`). I ran Massif on the top three; because the DES run under Valgrind stayed below its native peak, I also ran `md4_block_data_order` as a hash-family comparator.

Massif command shape:

```sh
ulimit -v 8388608
RUST_LOG=warn,ondemand=info timeout 180 valgrind --tool=massif --pages-as-heap=yes \
  --massif-out-file=/tmp/post-wave-massif/massif-prof-<proc>.out \
  target/profiling/infer-rs --pulse-only --quiet --trace-ondemand \
  --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 -j 1 \
  --procedures-filter '<proc>' \
  /home/mtrojer/infer-rs-bench/openssl-20260514-121752/textual-out/*.sil
```

| proc | full-run RSS signal | Massif peak (`pages-as-heap`) | largest detailed snapshot | dominant stack / mechanism | growth shape |
|---|---:|---:|---:|---|---|
| `DES_ede3_cfb_encrypt` | `peak_rss_delta=3.24GB` | 1.49 GiB | 1.30 GiB | `BTreeMap::insert` into `BaseMemory::Edges::add_with_history_limited` <- `Edges::from_recency_bindings_limited` <- `BaseMemory::map_values` <- `AbductiveDomain::preserve_canonical_heap_targets` <- `preserve_canonical_access_targets` <- `exec_node` | Under Valgrind hit wall cap and completed later with lower native `peak_rss=1.43GB`; still shows edge-map rebuilding under canonical heap preservation. |
| `x509_main` | `peak_rss_delta=3.13GB` | 2.74 GiB | 2.61 GiB | same `BaseMemory::Edges::add_with_history_limited` / `from_recency_bindings_limited` / `map_values` / `preserve_canonical_heap_targets` stack, with ~738MB in BTree leaf allocations at the detailed peak | Application-root retained-state growth; many callees plus heap-cap root summary. |
| `ripemd160_block_data_order` | `peak_rss_delta=2.79GB` | 3.36 GiB | 3.24 GiB | `Vec<HistoryEvent>::clone` -> `HistoryPath::clone` / `ValueHistory::merge` (`value_history.rs:342`) -> `operations::eval_with_history_mode` | Sharp hash-body history merge blow-up with `max_node_disjuncts=1`; not disjunct fan-out. |
| `md4_block_data_order` (comparator) | `peak_rss_delta=2.60GB` | 2.49 GiB | 2.36 GiB | same `Vec<HistoryEvent>::clone` -> `HistoryPath::clone` / `ValueHistory::merge` -> `operations::eval_with_history_mode` stack | Same shape as RIPEMD/old md4, confirming the remaining history issue is merge/path growth, not the old cheap-clone path alone. |

Massif artifacts: `/tmp/post-wave-massif/massif-prof-*.out` and matching logs.

### Important memory interpretation

The `Arc<ValueHistory>` fix removed the prior dominant cost of cloning entire histories during every retained-state clone. It did **not** eliminate deep copies when histories are intentionally transformed or merged. Post-wave Massif splits into two remaining mechanisms:

1. `BaseMemory::map_values` / `preserve_canonical_heap_targets` still rebuilds BTree edge maps, allocating many `LeafNode<Access, ValueWithHistory>` entries.
2. Hash-body transfer (`md4`, `ripemd160`) still clones `HistoryEvent` vectors inside `ValueHistory::merge` / `HistoryPath` set union. This is an explicit semantic merge path, so `Arc::clone` cannot help unless merge becomes structurally shared/capped/deduplicated.

## Cross-reference: what moved down vs up/stayed

### Full-corpus behavior

| metric | pre-wave baseline doc (`a8b8fe7bde`) | post-wave HEAD (`70ca0c562d`) | movement |
|---|---:|---:|---|
| Completion | no; outer 650s guard at ~405/445 | yes; 445/445 | **down/fixed** OBJ gate |
| Wall | `>650s` lower bound | 4:35.38 | **down by >2.36x** vs guard lower bound |
| Max RSS | unavailable (timeout killed time summary) | 19,802,296 KiB | publishable; near Arc-note 19.9 GiB |
| Aborts | partial `15 heap + 2 wall` before guard | 15 total | fewer and bounded; no OBJ runaway |
| Max visit count | `>=378` (pre-fast-forward attempt `1279`) | 4 | **down massively** |
| OBJ tail | active ~10m at guard (`OBJ_bsearch_sn/ln`, repeated `OBJ_bsearch_ex_`) | `OBJ_bsearch_sn` 15.7s; no runaway | **down/fixed** |

### CPU symbols

| symbol / surface | pre-wave baseline | post-wave | movement |
|---|---:|---:|---|
| `pulse::state_cmp::canonicalize` on `sha512_block_data_order` | 41.24% self; focused ~35s | 31.38% self; focused ~28s | **down** ~10 points and ~20% wall |
| `Canonicalizer::propagate_attrs` on `sha512` | 9.93% | 8.32% closure | **down slightly**, but still high |
| sort helpers (`core::slice::sort::*`) on `sha512` | smallsort 3.16% + unstable quicksort 2.91% + stable 1.36% | stable quicksort ~1.97% + 1.65% | **down/redistributed**, not gone |
| `OBJ_bsearch_*` canonicalize/convergence | `OBJ_bsearch_sn` canonicalize 17.56%; full run non-convergent | OBJ completes ~16s; no top-3 wall profile needed | **down/fixed as gate** |
| `DES_cfb_encrypt` allocator | `malloc` 24.59%, `_int_free` 18.13%, `_int_malloc` 5.97%, `malloc_consolidate` 5.07% | `malloc` 9.32%, `_int_free` 7.20%, `_int_malloc` 3.27%, `malloc_consolidate` 1.84% | **down substantially** |
| `DES_cfb_encrypt` canonical/formula | baseline `canonicalize` 4.34%, `flatten_term` 6.55%, `Term::cmp` 5.11% | `expand_formula_reachable` 9.33%, `canonicalize` 8.14%, `map_sorted_edges` 5.86%, `map_value` 4.88% | **up/visible** after allocator clone pressure moved down |
| `BTreeMap::clone::clone_subtree` | OBJ top 2.16%; Massif dominant via `ValueHistory` clone | only 1.35% in `ca_main` perf; Massif still sees clone_subtree under `ValueHistory::merge` for hash bodies | **down for ordinary retained clones; stayed in merge paths** |
| `pulse::formula::expand_formula_reachable` | not a top symbol in baseline tables | top `DES_cfb_encrypt` symbol at 9.33%; 2.81% in `mdc2_body` | **moved up** as next visible CPU surface |

### Memory stacks

| memory stack | baseline Massif | post-wave Massif | movement |
|---|---|---|---|
| `ValueHistory/HistoryEvent clone -> BTreeMap clone -> BaseMemory::map_values/preserve_canonical_heap_targets` | dominant in `md4`, `DES_ede3_cfb_encrypt`, `DES_cfb_encrypt` | no longer the universal retained-clone stack; `ValueHistory` is `Arc` and ordinary clone is cheap | **down/fixed for snapshot cloning** |
| `BaseMemory::map_values -> preserve_canonical_heap_targets -> Edges::from_recency_bindings_limited/add_with_history_limited` | part of old stack beneath history clone | dominant in `DES_ede3_cfb_encrypt` and `x509_main` detailed Massif | **stayed/up as exposed next layer** |
| `ValueHistory::merge -> HistoryPath/HistoryEvent clone -> operations::eval_with_history_mode` | not separated from generic clone pressure | dominant in `ripemd160_block_data_order` and `md4_block_data_order` | **moved up/stayed**; now the main history-specific RAM target |

## Recommendations / next perf-fix tasks

1. **`fix_value_history_merge_path_growth_hash_bodies`**
   - Target procs: `ripemd160_block_data_order` and `md4_block_data_order`.
   - Dominant stack: `ValueHistory::merge` (`value_history.rs:342`) cloning `HistoryPath` / `Vec<HistoryEvent>` from `operations::eval_with_history_mode`.
   - Mechanism: OCaml Pulse histories are trace-like and aggressively bounded/deduplicated for diagnostics. Rust currently preserves and merges path sets structurally; after `Arc<ValueHistory>`, intentional merge is the remaining deep-copy path. Investigate capping equivalent histories, short-circuiting merge when one history subsumes/equals the other by `Arc`/structural identity, or using structurally shared path storage for `HistoryPath` tails.

2. **`fix_base_memory_canonical_preserve_edge_rebuild_pressure`**
   - Target procs: `DES_ede3_cfb_encrypt`, `DES_cfb_encrypt`, `x509_main` / `ca_main`.
   - Dominant stack/symbols: `BaseMemory::map_values` -> `AbductiveDomain::preserve_canonical_heap_targets` -> `Edges::from_recency_bindings_limited` / `add_with_history_limited` / `BTreeMap::insert`; CPU side `Canonicalizer::map_sorted_edges`, `map_value`, and allocator churn.
   - Mechanism: canonical heap preservation rebuilds edge maps even when most `ValueWithHistory` values are unchanged/shared. Compare OCaml `PulseBaseMemory` recency-map sharing and update patterns; try reusing unchanged `Edges`/recency buckets or adding a copy-on-write/identity fast path in `map_values`.

A smaller CPU-only follow-up could target `pulse::formula::expand_formula_reachable` in `DES_cfb_encrypt`, but the Massif data says the next highest ROI remains retained-state/history memory, because those paths drive both RSS aborts and allocator self-time.
