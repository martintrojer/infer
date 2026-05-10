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
| Store-textual C Pulse sweep | `501` procs analyzed; `185` issues; `51` OK / `0` FAIL_ANALYZE / `1` TIMEOUT (known recursion hang) |
| NPE count | expected `127`, found `130` |
| Leak count | expected `20`, found `16` (see baseline note below) |
| UAF count | expected `7`, found `7` |
| `latent.c` issue-set compare | exact at `(procedure, line, issue-type)`: `17` Rust / `17` OCaml |
| specialization summary harness | `21 / 21` procedures match; specialized-summary checkpoint has no diffs |
| `virt.sil` virtual dispatch | `0` skipped procedures (full coverage) |
| `make check` | current checkpoint passes with `INFER_BIN=../infer/bin/infer` |

Accepted current count deltas (NPE, +3 over expected):

- `fopen.c`: `-3`
- `nullptr.c`: `+1` real-bug divergence (`FN_nullptr_deref_old_bad`).
- `sizeof.c`: `+2` exported-Textual fidelity limit, not a Pulse workaround target.
- `struct_values.c`: `+1`
- `var_arg.c`: `+2`

Leak count deltas (-4 vs expected):

- `memory_leak.c`: `-3`
- `nullptr.c`: `-1`

LEAK baseline note: the previous `expected 20` figure was stale/aspirational —
`worker-leak-1`'s bisect across 50+ commits (5 sample points) showed the actual
sweep result has been ~`12-16` for the entire window. The dashboard now uses
the actual sweep number as baseline.

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

## OpenSSL benchmark dashboard

Corpus: 74-file partial OpenSSL capture under
`~/infer-rs-bench/openssl-20260501-084151/`.

Default Rust caps:

- `pulse-max-heap-mb = 2048`
- `pulse-max-wall-secs = 60`
- pass `0` to disable either cap

Latest repeated current-HEAD checkpoint on the fresh patched-exporter re-export
(`textual-out-reexport-20260508-102338/`, `74` `.sil` files; DES and OBJ targets
present; `RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh` with
`TEXTUAL_DIR=.../textual-out-reexport-20260508-102338`):

| metric | OCaml old baseline (`-j 1`) | Rust fresh export (`-j 4`) |
|---|---:|---:|
| wall time | `42.9s` | `344.03s` median |
| max RSS | `~1.17 GB` | `11.44 GB` median (`13.69 GB` max run) |
| peak footprint | `~1.10 GB` | `9.86 GB` median (`10.33 GB` max run) |
| procs analyzed | old export: `570 / 570` | `446 / 446` |
| heap+wall aborts | n/a | `20 / 446` |
| max visit count | n/a | `4` |
| process exit | clean (`0`) | `2` due reported leaks despite full analysis |

Previous repeated checkpoint on the original `textual-out/` export was
`239.67s` median wall, `13.17 GB` median max RSS, `18 / 570` aborts, and clean
exit. Do not compare old-export and fresh-export wall times without noting the
input changed: the fresh export has fewer procedure definitions (`446` analyzed
instead of `570`) but includes newer cleanup/nullify/exit-scope metadata.

Interpretation:

- Fresh-export slowdown vs the old OCaml baseline: `344.03 / 42.9 ~= 8.0×`.
- The fresh export changes the benchmark input enough that the old `239.67s`
  dashboard is now historical, not the current fresh-export baseline.
- Stale `term_value_index` repair was rejected for the default path: it improved
  selected DES target wall time but made whole-program median slower and produced
  no repair hits in the focused counter run.
- `--pulse-intermediate-formula-gc` remains opt-in (default OFF). The cleanup
  pass was expanded (commit `01a51f99ed`) to also prune `term_value_index`,
  `fn_app_eqs`, `atoms`, and `const_cache` entries that become unreachable
  after the formula-variable GC. Useful for memory headroom; whole-program
  wall-time impact on capped OpenSSL has not been remeasured yet — tracked
  under `perf_track_walltime_gap_vs_ocaml` and the in-flight
  `perf_remeasure_fresh_openssl_after_formula_cleanup` run.

Benchmark artifacts from the latest run are under ignored `bench-out/` in the
main checkout. Historical OpenSSL archaeology is in
[`docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md`](plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md).

## Current active work

`mu` is the source of truth; this section is a coarse map only and may lag the
DAG between gated docs refreshes (`docs_refresh_status_after_parallel_stack`).

```sh
mu state -w infer-rs            # tracks + ready set + agents
mu task list -w infer-rs --status OPEN --sort roi
mu task list -w infer-rs --status DEFERRED
```

Live themes (track headlines, not exhaustive task lists):

- **SIL virtual dispatch** — all `virt.sil` cases pass after the recent
  dispatch stack landed (`902b2deb50`, `cda27f6239`, `70365d047b`); the
  `test_full_check_current_stack` gate closed green. New regressions in this
  area should reopen the relevant follow-ups in `mu`.
- **OpenSSL perf** — formula-cleanup expansion has landed behind the opt-in
  flag; a one-shot remeasurement
  (`perf_remeasure_fresh_openssl_after_formula_cleanup`) is in flight. The
  ~`8×` wall-time gap vs OCaml on whole-program OpenSSL is tracked under
  `perf_track_walltime_gap_vs_ocaml`.
- **Test infrastructure** — opt-in trace-step assertions
  (`test_trace_step_assertions`).
- **Deferred backlog** — micro-cleanups (`code_*`), speculative perf
  (`perf_component_clone_reduction`), and accepted parity limits
  (`parity_sizeof_type_eval`) are parked with explicit reopen-when-... notes.
  Run `mu task list -w infer-rs --status DEFERRED` for the live set.

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
