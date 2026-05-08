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
| Store-textual C Pulse sweep | `52 / 55` files analyzed; 3 skipped for fixpoint exhaustion |
| NPE count | expected `131`, found `134` |
| Leak count | expected `20`, found `20` |
| UAF count | expected `7`, found `7` |
| `latent.c` issue-set compare | exact at `(procedure, line, issue-type)`: `17` Rust / `17` OCaml |
| specialization summary harness | `21 / 21` procedures match; specialized-summary checkpoint has no diffs |
| `make check` | current checkpoint passes with `INFER_BIN=../infer/bin/infer` |

Accepted current count deltas:

- `nullptr.c`: `+1` real-bug divergence (`FN_nullptr_deref_old_bad`).
- `sizeof.c`: `+2` exported-Textual fidelity limit, not a Pulse workaround target.

Recent correctness work that should stay in place mirrors OCaml's dynamic-type
specialization path, direct known-call unknown fallback for resolved
`__call_c_function_ptr` targets without summaries, caller-visible pre-edge
materialization, latent-invalid-access export/import parity, and comparator
normalizations for semantic noise only.

## OpenSSL benchmark dashboard

Corpus: 74-file partial OpenSSL capture under
`~/infer-rs-bench/openssl-20260501-084151/`.

Default Rust caps:

- `pulse-max-heap-mb = 2048`
- `pulse-max-wall-secs = 60`
- pass `0` to disable either cap

Latest repeated current-HEAD checkpoint (`5f82b3f88b`, cached-comparison
pruning; `RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh`) used the original
`textual-out/` export. A fresh patched-exporter re-export also exists at
`textual-out-reexport-20260508-102338/` (`74` `.sil` files; DES and OBJ targets
present) but has not yet been re-baselined for whole-program wall time:

| metric | OCaml (`-j 1`) | Rust current HEAD (`-j 4`) |
|---|---:|---:|
| wall time | `42.9s` | `239.67s` median |
| max RSS | `~1.17 GB` | `13.17 GB` median (`13.79 GB` max run) |
| peak footprint | `~1.10 GB` | `8.33 GB` median (`12.25 GB` max run) |
| procs analyzed | `570 / 570` | `570 / 570` |
| heap+wall aborts | n/a | `18 / 570` |
| max visit count | n/a | `4` |
| exit | clean (`0`) | clean (`0`) |

Interpretation:

- Current slowdown vs OCaml: `239.67 / 42.9 ~= 5.6×`.
- Best pre-cache default repeated median remains `226.63s`; cached-comparison
  pruning is a correctness/abort-count improvement, not a wall-time win on this
  host.
- Stale `term_value_index` repair was rejected for the default path: it improved
  selected DES target wall time but made whole-program median slower and produced
  no repair hits in the focused counter run.
- `--pulse-intermediate-formula-gc` remains opt-in: useful for memory headroom,
  not a default wall-time win on capped whole-program OpenSSL.

Benchmark artifacts from the latest run are under ignored `bench-out/` in the
main checkout. Historical OpenSSL archaeology is in
[`docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`](plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md).

## Current active work

Use `mu`, not this document, as the source of truth:

```sh
mu task list -w infer-rs --status OPEN
```

Important task clusters migrated from the old ad-hoc backlog docs:

- OpenSSL/DES performance:
  - `perf_profile_des_formula_volume`
  - `perf_investigate_obj_obj2txt_state_explosion`
  - `perf_incremental_formula_cleanup`
  - `perf_component_clone_reduction`
  - `openssl_reexport_shared_corpus`
- Correctness/parity:
  - `parity_valuehistory_trace_detail`
  - `parity_latent_reporting_detail`
  - `parity_sizeof_type_eval`
- SIL/Textual gaps:
  - `sil_virtual_dispatch_loads`
  - `sil_devirtualization_return_values`
  - `sil_cross_file_resolution`
  - `textual_declenv_enhancements`
- Code cleanup:
  - `code_find_return_value_fallback`
  - `code_prune_branch_metadata`
  - `code_declenv_typed_keys`
  - `code_annotation_default_cleanup`
  - `code_procdesc_vec_edges`
  - `code_tenv_get_supers_borrow`
  - `code_dummy_location_const`

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
- [`docs/plans/`](plans/) — archived investigations.
