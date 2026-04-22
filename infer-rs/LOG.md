# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- Active parity task:
  the latent publication gate is green again (`make check` passes and the real
  exported `latent.c` issue surface is still exact at
  `(procedure, line, issue-type)`), so the remaining latent drift is now the
  smaller reduced-summary mismatch set plus the next OpenSSL retained-state
  probe.
- Latest validated checkpoint:
  - compare dir: `/tmp/latent-compare.Muh07b`
  - exported SIL: `/tmp/latent-compare.Muh07b/textual/latent.sil`
  - OCaml report: `/tmp/latent-compare.Muh07b/ocaml-out/report.json`
  - OCaml summaries: `/tmp/latent-compare.Muh07b/ocaml-out/all_summaries.json`
  - exact issue compare result:
    `RUST_COUNT 17`, `OCAML_COUNT 17`, no `RUST_ONLY`, no `OCAML_ONLY`
- Rust correctness fixes now in place:
  - keep `AbortProgram` pre_posts even when recovered latent-invalid-access
    siblings exist; suppress only duplicate manifest publication
  - restore OCaml's local `AbortProgram` + `LatentAbortProgram` twin for
    `traverse_and_crash_if_equal_to_root`
  - keep branch-control-only one-step cycle callees latent-only when no
    caller-side conditions survive, while still letting callers reify the
    manifest abort
  - let purely local trailing field-write aborts keep all recovered latent
    null paths plus the trailing manifest diagnostic without reviving the
    `FN_crash_after_six_nodes_bad` duplicate-manifest bug
  - recover at most one best continue-derived invalid-access candidate instead
    of spraying all candidates
  - dedup latent invalid accesses by caller-visible heap path using exported
    diagnostics, with earlier access locations preferred only as a tiebreaker
  - keep mixed local+imported direct-formal latent-invalid shapes out of the
    report surface
- Remaining summary-surface drift from
  `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`:
  - `FN_nonlatent_use_after_free_bad` / `bad2`: Rust still keeps extra
    latent-invalid / latent-abort shapes where OCaml keeps
    `ContinueProgram` + `LatentAbortProgram`
  - `latent_use_after_free`: Rust still keeps an extra
    `LatentAbortProgram` where OCaml keeps `ContinueProgram`
- Cross-reference before changing semantics:
  `infer/src/pulse/PulseSummary.ml`,
  `infer/src/pulse/PulseLatentIssue.ml`,
  `infer/src/pulse/PulseReport.ml`,
  `infer/src/pulse/PulseArithmetic.ml`
- Useful commands:
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`
  - `cargo test -p pulse --lib test_debug_real_abort_recovery_report_keys -- --ignored --nocapture`
  - `./target/debug/infer-rs --pulse-only --quiet --pulse-report-issues-for-tests --results-dir /tmp/latent-compare.Muh07b/ocaml-out --source-override codetoanalyze/c/pulse/latent.c -o /tmp/latent-rs-run.next /tmp/latent-compare.Muh07b/textual/latent.sil`
  - fresh narrow export repro:
    `tmpdir=$(mktemp -d /tmp/wpblock-export.XXXXXX)`
    `tar -xf ~/infer/benchmarks/openssl/openssl-1.0.2d.tar.gz -C "$tmpdir"`
    `cd "$tmpdir/openssl-1.0.2d"`
    `sdk=$(xcrun --show-sdk-path) && CC=clang CFLAGS="-isysroot $sdk" ./config no-asm`
    `infer capture --store-textual --results-dir infer-out-wp -- clang -I. -Iinclude -c crypto/whrlpool/wp_block.c`
    `infer debug --results-dir infer-out-wp --export-textual textual-out-wp`
  - fresh metadata check:
    `rg -o "__sil_metadata_[a-z_]+" textual-out-wp/wp_block.sil | sort -u`
  - fresh narrowed Rust repro:
    `RUST_LOG=warn,ondemand=info target/release/infer-rs --pulse-only --trace-ondemand -j 1 --procedures-filter whirlpool_block "$tmpdir/openssl-1.0.2d/textual-out-wp/wp_block.sil"`
  - focused selected-node alpha-signature repro:
    `RUST_LOG='pulse::checker::fixpoint=debug,ondemand=info,ondemand::runner=info' target/release/infer-rs --pulse-only --debug-level-analysis 1 --trace-ondemand --debug-fixpoint-nodes 31,35 -j 1 --procedures-filter whirlpool_block "$tmpdir/openssl-1.0.2d/textual-out-wp/wp_block.sil" >/tmp/wpblock-alpha-signatures.log 2>&1`
- Current validated working-tree checkpoint:
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`
  - `cargo test -p pulse test_debug_signature -- --nocapture`
  - `cargo test -p pulse test_exit_scope_ -- --nocapture`
  - `cargo test -p pulse test_variable_lifetime_begins_ -- --nocapture`
  - `cargo test -p pulse exec_node_skips_reexecuting_old_pre_disjuncts -- --nocapture`
  - `cargo test -p pulse state_cmp -- --nocapture`
  - `cargo test -p absint --lib -- --nocapture`
  - `cargo test -p textual -- --nocapture`
  - `cargo test -p analyses test_liveness_on_c_fixture -- --nocapture`
  - `cargo build -p infer-rs --release`
  - real exported `latent.c` compare: `17` Rust / `17` OCaml with no side-only
    `(procedure, line, issue-type)` entries
  - `make check`
  - `cargo fmt --check`

## Current Correctness Checkpoint

- Authoritative store-textual sweep:
  - `52 / 55` C Pulse files analyzed
  - NPE: expected `131`, found `134`
  - Leaks: expected `20`, found `20`
  - UAF: expected `7`, found `7`
- Accepted remaining count deltas:
  - `nullptr.c` `+1`: keep the real `FN_nullptr_deref_old_bad` report
  - `sizeof.c` `+2`: accepted exported-Textual fidelity limit
- Semantic summary parity checkpoint:
  - `specialization.c` main summaries: `21 / 21`
  - combined main + specialized harness: `Matching: 21`
- Wrapper/cycle publication checkpoint:
  - the simplified one-node caller repro without explicit
    `__sil_metadata_variable_lifetime_begins` now keeps the reified caller
    `AbortProgram` instead of suppressing it behind a recovered latent
    invalid-access twin during summary export
  - focused validation now includes
    `checker::tests::test_apply_summary_reifies_one_node_cycle_latent_abort_before_summary_export`,
    `test_e2e_one_node_cycle_keeps_callee_latent_and_reifies_in_caller`,
    `test_e2e_two_hop_field_write_keeps_null_derefs_latent`,
    the latent-cycle subset, full `cargo test -p pulse`, and `make check`

## Next Probes

- Compare the current richer single-file `wp_block.c` retained PRE/POST states
  against the OCaml line `540` / `752-755` block using the new alpha-signature
  readout: explain why Rust keeps `4` growth tiers x `2` variants at nodes
  `31/35` while OCaml ends those source lines at `2-4` visible PRE states and
  `Got 1` on the last transfer.
- After the attr-rank cleanup, focus the next probe on the surviving
  `540:33` / logical-temp provenance inside nodes `31-38`; duplicate
  `Invalid(...)` accumulation is no longer the main noise source there.
- Focus specifically on what grows monotonically between the four Rust
  signature tiers, especially the formula and stack-reachable post graph.
  The next likely correctness gap is later loop-cycle collapse, not import of
  metadata or rank handling.
- Do not chase `Nullify` / `Abstract` metadata work here: OCaml Pulse keeps
  them as no-op too.
- Re-export the shared OpenSSL corpus with the patched OCaml exporter and use
  that as the next apples-to-apples Rust checkpoint.
- Use the current importer support as the floor: do not add a Pulse workaround
  for this specific `whirlpool_block` gap just to move the retained-state
  numbers back toward the old smaller but less faithful export.
- Continue whole-program OpenSSL work on the publishable surfaces that remain:
  merged-run abnormal termination / memory growth and then a clean `-j 1`
  rerun on the shared exported corpus.
