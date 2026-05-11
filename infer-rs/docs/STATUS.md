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

Latest clean repeated checkpoint on the fresh patched-exporter re-export
(`textual-out-reexport-20260508-102338/`, `74` `.sil` files; DES and OBJ targets
present; `RUNS=3 JOBS=4 scripts/bench_openssl_partial.sh` with
`TEXTUAL_DIR=.../textual-out-reexport-20260508-102338`). This table is the
latest trustworthy full-corpus measurement, not necessarily current HEAD:

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

- **OpenSSL perf / benchmark hygiene** — the benchmark script now has stricter
  preflight/failure behavior and focused `state_cmp` fixes have landed. The
  remaining ready item is the clean quiescent-host full OpenSSL remeasure; do
  not run it while load/security daemons are high.
- **Decision gate** — after the clean full-corpus remeasure closes,
  `perf_decide_next_track_after_profile_and_remeasure` should prune obsolete
  placeholders and choose the next concrete track.
- **Correctness parity** — current C store-textual deltas are accepted and
  documented above. Reopen parity work only for new sweep regressions or a real
  Textual/export-fidelity project.
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
- [`docs/plans/`](plans/) — archived investigations.
