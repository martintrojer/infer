# Whole-program OpenSSL findings archive

This is an archive of the OpenSSL scaling/performance investigation. Current
benchmark numbers live in [`../STATUS.md`](../STATUS.md). Active follow-up work
lives in `mu` tasks.

```sh
mu task list -w infer-rs --status OPEN
```

## Follow-up task IDs

Historical tasks from this investigation have mostly closed or been deferred.
The current live OpenSSL follow-ups are in `mu`; as of 2026-05-11 the important
ones are:

- `bench_harden_openssl_benchmark_script`
- `perf_remeasure_quiescent_host`
- `docs_refresh_status_after_quiescent_remeasure`
- `perf_profile_current_state_cmp_after_structural`
- `perf_fix_next_profiled_state_cmp_hotspot`
- `perf_validate_next_perf_fix_focused`
- `perf_decide_next_track_after_profile_and_remeasure`

Older task names in this archive remain useful search terms for `mu task notes`,
not active backlog.

## Headline conclusions preserved from the investigation

- The 74-file partial OpenSSL corpus now completes out of the box under default
  per-procedure heap/wall caps.
- The original `OBJ_bsearch_ex_ max_visit_count=10001` convergence pathology is
  no longer the dominant blocker after the B-track convergence fixes.
- The remaining long tail is mostly bounded-visit large-state cost, especially
  DES-family procedures and OBJ-family procedures.
- Heap/attribute retained-state pruning and later structural canonicalization
  work were clear focused wins.
- Formula GC improves memory headroom in focused/uncapped runs but was not a
  capped whole-program wall-time win on this host, so it remains opt-in.
- Stale `term_value_index` repair was rejected for the default path: it helped a
  selected DES slow proc but worsened whole-program median wall time and focused
  counters showed no repair hits.
- Direct term-value cache reuse and cached-comparison pruning remain valuable;
  cheap TermKey shape normalization was measured on focused DES and rejected as
  inert for that target.
- Later state-comparison fixes (`sort_by_cached_key`, structural canonical
  formula keys, structural canonical heap/attrs/stack/dynamic-types keys,
  cached propagation sort keys, and flat-slab `CanonTerm`) cut isolated
  `OBJ_bsearch_ex_` wall from `1.91s` to `~0.47s`; DES-family focused probes
  also improved materially. The clean whole-program remeasure is still pending
  a quiescent host.

## Dated benchmark checkpoints

See [`../STATUS.md`](../STATUS.md) for the current dashboard. Historical
checkpoints from the investigation are intentionally not repeated here as active
status tables to avoid drift; recover exact raw numbers from:

- commit messages in the performance branch,
- `mu` task notes in workstream `infer-rs`, and
- ignored benchmark artifacts under `infer-rs/bench-out/` when preserved.

Known preserved artifact paths from the latest sessions include:

- `infer-rs/bench-out/current-head-openssl-20260507-170249/`
- prior `openssl-partial-*` benchmark directories mentioned in `mu` notes.

## How to reproduce the benchmark

```sh
cd infer-rs
OUT_DIR="$(pwd)/bench-out/current-head-openssl-$(date +%Y%m%d-%H%M%S)" \
  RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh
```

For focused procedure probes, use the shared OpenSSL Textual export and a
procedure filter, for example:

```sh
RUST_LOG=warn,ondemand=info target/release/infer-rs \
  --pulse-only --quiet --trace-ondemand -j 1 \
  --procedures-filter DES_ede3_cbcm_encrypt \
  --pulse-max-wall-secs 0 --pulse-max-heap-mb 0 \
  ~/infer-rs-bench/openssl-20260501-084151/textual-out/*.sil
```

## Related archive docs

- [`CONVERGENCE_8D4V_FINDINGS.md`](CONVERGENCE_8D4V_FINDINGS.md) — detailed
  retained-state decomposition for the earlier `whirlpool_block` probe.
- [`STRUCTURAL_SHARING_PROTOTYPE.md`](STRUCTURAL_SHARING_PROTOTYPE.md) — early
  structural-sharing plan.
