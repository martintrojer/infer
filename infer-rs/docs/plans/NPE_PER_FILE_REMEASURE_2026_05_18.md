# NPE per-file remeasure after today's changes (2026-05-18)

Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak`  
HEAD: `ad5731ac39384656a287a00b427c082e97a81e66` (`docs: record post apply-post OpenSSL perf`)  
Task: `scout_npe_per_file_remeasure_after_today_changes`  
Mode: read-only scout; no source edits; sweep harness unmodified.

## Commands / raw evidence

### Required sweep

Ran from `infer-rs` with the requested ignored release test and scoped process cap:

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak/infer-rs
ulimit -v 8388608
cargo test -p pulse --release --test end_to_end \
  test_store_textual_sweep -- --ignored --nocapture \
  > /tmp/npe_sweep_head_2026_05_18.raw 2>&1
```

Important reproducibility note: the first cold run used the workspace-local sibling lookup for OCaml (`../infer/bin/infer`, version `v1.2.0-fccf3f0b7d`) because this freshly recreated mu workspace has no built `infer/bin/infer`; that stale OCaml capture produced noisy per-file buckets (`angelism.c +5`, `nullptr.c +1`). I therefore re-ran the already-built test binary with the current OCaml binary explicitly selected:

```sh
INFER_BIN=/home/mtrojer/infer/infer/bin/infer \
/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak/infer-rs/target/release/deps/end_to_end-b8796b1be6526438 \
  test_store_textual_sweep --ignored --nocapture \
  > /tmp/npe_sweep_head_2026_05_18_rerun.raw 2>&1
```

This is the run used for classification below. It is the same unmodified harness/test; only `INFER_BIN` changed to match the current OCaml checkout on PATH.

Sweep result:

```text
=== NPE: expected 131, found 137 ===
  Differences:
    funptr.c: expected 11, found 13
    latent.c: expected 5, found 4
    nullptr_more.c: expected 7, found 8
    sizeof.c: expected 0, found 2
    var_arg.c: expected 4, found 6

=== LEAK: expected 20, found 20 ===

=== UAF: expected 7, found 7 ===

=== Store-textual sweep ===
  OK: 52, FAIL_ANALYZE: 0, TIMEOUT: 0
  509 procs analyzed, 183 issues found
```

Raw logs:

- `/tmp/npe_sweep_head_2026_05_18.raw` (cold stale-OCaml run; not used for final per-file classification)
- `/tmp/npe_sweep_head_2026_05_18_rerun.raw` (current-OCaml run used here)

### Single-file cross-checks for shifted/current delta files

For all files that were in the old classification or appeared in the current sweep diff, I ran single-file capped capture/export + Rust CLI + OCaml direct test-config cross-checks. Artifacts live under:

- `/tmp/npe_single_requested_cmd_2026_05_18/<file>/`

Recipe shape:

```sh
# capture/export with current OCaml
/home/mtrojer/infer/infer/bin/infer capture --pulse-only --store-textual -j 1 \
  -o infer-out -- clang -c infer/tests/codetoanalyze/c/pulse/<file>.c
/home/mtrojer/infer/infer/bin/infer debug --results-dir infer-out --export-textual textual-out

# Rust CLI, capped, test reporting, requested force-continue flag spelling
ulimit -v 8388608
timeout 180 target/release/infer-rs --pulse-only --quiet \
  --pulse-force-continue=false --pulse-report-issues-for-tests \
  -j 1 --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -o rust-out --results-dir infer-out \
  --source-override infer/tests/codetoanalyze/c/pulse/<file>.c \
  textual-out/<file>.sil

# OCaml direct test config used for cross-reference
ulimit -v 8388608
timeout 180 /home/mtrojer/infer/infer/bin/infer --pulse-only \
  --debug-exceptions --project-root infer/tests \
  --pulse-report-issues-for-tests --no-pulse-force-continue --pulse-eternal \
  --pulse-report-issues-reachable-from entry_point \
  --pulse-taint-config infer/tests/codetoanalyze/c/pulse/.infertaintconfig \
  --pulse-transitive-access-config infer/tests/codetoanalyze/c/pulse/transitive-access.conf \
  --jobs 1 -o ocaml-out -- clang -c infer/tests/codetoanalyze/c/pulse/<file>.c
```

## Prior classification reference

Prior closed scout: `scout_npe_per_file_full_remeasure` (2026-05-14). Its close note classified the then-current nonzero surface as:

| file | prior issues.exp / OCaml direct / Rust | prior delta | prior procedure surface |
|---|---:|---:|---|
| `struct_values.c` | 0 / 0 / 1 | +1 | `struct_value_in_callee_ok` |
| `var_arg.c` | 4 / 4 / 6 | +2 | `FN_sum_four_then_npe_bad`, `FN_sum_then_reachable_npe_bad` |
| `nullptr.c` | 13 / 13 / 15 | +2 | mixed Rust-only publications/duplicate balanced by two OCaml-only FNs |
| `fopen.c` | 17 / 17 / 14 | -3 | missing `no_fopen_check_{fputc,putc,ungetc}_bad` |
| `latent.c` | 5 / 5 / 4 | -1 | missing manifest `traverse_and_crash_if_equal_to_root` |

Subsequent same-day tasks before today's 68 commits moved that baseline: `fopen.c`, `struct_values.c`, and `var_arg.c` were fixed/aligned; `funptr.c` gained two real NPE catches via dynamic-type specialized abort propagation; `sizeof.c +2` remained accepted Textual fidelity. STATUS.md before this scout said the held total was `131/140` (+9).

## HEAD per-file classification

Single-file current-OCaml cross-check table for changed/current delta files:

| file | OCaml direct NPE | Rust store-textual NPE | delta vs OCaml direct | procedures accounting for delta | classification |
|---|---:|---:|---:|---|---|
| `angelism.c` | 7 | 12 | +5 | Rust-only: `skip_external_function_ok`, `call_by_ref_actual_already_in_footprint_ok`, `returnPassByRef2Ok`, `struct_value_by_ref_ptr_write_before_ok`, `struct_value_by_ref_write_then_skip_ok` | **NEW regression / reopened old surface**. This surface had been confirmed closed at rebuilt HEAD `21296ab33e` (7/7 exact) by `cluster_npe_pre_eval_surface_audit`, but is back on current HEAD when captured with current OCaml. Not part of the current full-sweep diff only because the harness run used a different capture path/config/binary mix; the single-file repro is concrete. |
| `fopen.c` | 17 | 17 | 0 | none | Closed/aligned by the stdio model fix. |
| `funptr.c` | 11 | 13 | +2 | Rust-only: `funptr_apply_funptr_with_intptrptr_and_after_specialized_bad`, `funptr_apply_funptr_with_intptrptr_and_after_respecialized_bad` | **Rust-strictly-more-precise real catches**. These are the expected follow-on from `a8b8fe7bde` / `cluster_funptr_abort_propagation_specialized`: OCaml direct still does not report these two specialized callers, but Rust now propagates the specialized abort instead of dropping it. |
| `latent.c` | 5 | 4 | -1 | OCaml-only: `traverse_and_crash_if_equal_to_root` | **Still aligned-with-known deferred latent producer/classification gap**, not part of the +N over-count. Existing worker-2 EqZero/latent sideband work is adjacent but not a new NPE over-report. |
| `nullptr.c` | 13 | 14 | +1 | Rust-only: `unknown_is_functional_ok`, `unknown_from_parameters_latent`, `no_invalidation_compare_to_NULL_bad`; OCaml-only: `create_null_path2_bad_FN`, `malloc_then_call_create_null_path_then_deref_unconditionally_bad_FN` | **Still historical mixed publication/suppression surface, reduced from prior +2 to +1.** No new follow-up from this scout; existing nullptr publication/suppression notes already capture the class. |
| `nullptr_more.c` | 7 | 7 (single-file) / 8 (full sweep) | 0 single-file / +1 sweep | Full-sweep extra: `unreachable_null_no_return_ok` from the old issue.exp delta; single-file current-OCaml capture does not reproduce it. | **No confirmed single-file regression.** Treat the sweep-only +1 as harness/capture sensitivity unless it reproduces under the single-file recipe. |
| `sizeof.c` | 0 | 2 | +2 | two reports in `sizeof_eval_ok` (lines 25, 29) | **Accepted Textual fidelity limitation** (`docs/STORE_TEXTUAL.md`): exported Textual loses rich `Sizeof`/array-extent data, so Rust cannot fold the ok branches. |
| `struct_values.c` | 0 | 0 | 0 | none | Closed/aligned by restore-formals fix. |
| `var_arg.c` | 4 | 4 (single-file requested config) / 6 (full sweep) | 0 single-file / +2 sweep | Sweep-only historical procedures: `FN_sum_four_then_npe_bad`, `FN_sum_then_reachable_npe_bad`; single-file requested command with `--pulse-force-continue=false` reports 4. | **No confirmed single-file regression.** Prior semantic config alignment still holds in the requested single-file repro. The full-sweep +2 should be treated as harness/config sensitivity, not a source-level Rust regression from today's summary changes. |

## Net interpretation

- The full sweep at current HEAD with current OCaml is `131/137` (+6), not the stale STATUS `131/140` (+9).
- The current sweep's visible +6 over-count decomposes as:
  - `funptr.c +2`: Rust-strictly-more-precise real catches from specialized abort propagation.
  - `sizeof.c +2`: accepted exported-Textual fidelity limitation.
  - `var_arg.c +2`: sweep-only; single-file requested config reproduces 4/4, so do not classify as a new source-level regression here.
  - plus offsetting `latent.c -1` and `nullptr_more.c +1` in the sweep buckets.
- Single-file remeasure also shows a **reopened `angelism.c +5` Rust-only surface** against current OCaml direct. Because this is concrete under the capped single-file recipe and had previously been confirmed closed, I filed a follow-up bug for it.

## Follow-up filed

Filed `bug_reopened_angelism_byref_skip_npe_surface_after_apply_post` with concrete repro:

- workspace HEAD `ad5731ac39384656a287a00b427c082e97a81e66`
- artifacts `/tmp/npe_single_requested_cmd_2026_05_18/angelism/`
- current OCaml direct `7` NPE vs Rust store-textual `12` NPE
- Rust-only procedures: `skip_external_function_ok`, `call_by_ref_actual_already_in_footprint_ok`, `returnPassByRef2Ok`, `struct_value_by_ref_ptr_write_before_ok`, `struct_value_by_ref_write_then_skip_ok`

## Source edits

None. This scout did not edit Rust sources or the sweep harness.
