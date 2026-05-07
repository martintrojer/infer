# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- Active parity task:
  the latent publication gate is green again (`make check` passes, the real
  exported `latent.c` issue surface is exact at
  `(procedure, line, issue-type)`, and the reduced real-fixture summary-shape
  mismatch set is green again too); the caller-reified real-`latent.c`
  `USE_AFTER_FREE` subset is now locked by an ignored end-to-end regression, so
  the next active parity work has shifted to the OpenSSL retained-state probe.
- Fresh checkpoint on the `latent.c` summary drift:
  - the remaining false `LatentInvalidAccess` summaries for
    `FN_nonlatent_use_after_free_bad{,2}` are gone again on the real exported
    fixture; Rust now matches OCaml on kind counts there
  - the winning filter tweak was to keep extra direct-formal zero guards only
    when they are locally zero or when the selected path already has a visible
    null invalidation; imported-only cleanup guards from `create_branching`
    no longer rescue a latent-invalid export
  - do not drop the selected direct-formal `x == 0` condition from the
    surviving `latent_use_after_free` latent-invalid summary: that looked
    closer to OCaml locally but reintroduced a manifest `NULLPTR_DEREFERENCE`
    in `main`, so the issue-surface-correct version keeps the extra selected
    zero fact for now
  - a follow-up experiment on the reduced `latent_use_after_free` shape showed
    that blindly coalescing the two locally-zero direct formals in summary
    space is not safe in Rust yet: it flips the latent-invalid path key from
    `*x` to `*b` and unsuppresses the competing `b == 1 && x == 0`
    latent-invalid pre_post that the existing `*x`-keyed tie-break used to
    hide
  - a second experiment moved that coalescing later, after the latent-invalid
    candidate filters, and on the real exported `latent.c` fixture it did
    recover the OCaml-looking latent-invalid summary condition `{b == 0}` with
    reconstructed address `b`; the missing piece was interproc replay of that
    coalesced direct-formal alias relation for value actuals (`b` and `x`)
  - the winning interproc fix is to conjoin equality between caller actuals
    when one callee dereferenced-formal summary value is shared by multiple
    formals during summary import, instead of arbitrarily binding that value
    to the first actual; that keeps the coalesced latent-invalid summary
    export and removes the old `main` / `manifest_use_after_free` manifest NPE
    regressions again
  - report-side trace strings are a little richer now too: default Pulse
    issues keep the old one-line qualifier, the serialized `trace` field now
    appends minimal invalidation/access history signatures, and `report.json`
    now also carries a minimal structured `bug_trace` /
    `bug_trace_{length,max_depth}` payload plus flat `bug_type` / `severity`
    / `category` aliases, stable `key`, `node_key`, `hash`,
    `procedure_start_line`, and empty `extras`
  - access-side bug traces now reorder caller provenance before a synthetic
    `when calling ... here` step into the callee parameter/value, which makes
    `manifest_use_after_free` and `main` look closer to OCaml without changing
    the qualifier text or `issues.exp` surface
  - invalidation histories imported from callee diagnostics are now wrapped in
    the current summary-application call context, so caller reports retain the
    outer callsite in their structured invalidation trace too
  - the invalidation-side structured trace is a bit closer to OCaml now too:
    when translation leaves only the deeper callee formal in the history, the
    report layer synthesizes the outer callee formal before the inner call, and
    modelled allocation call/return pairs such as `malloc` now collapse to one
    `allocated by call to ... (modelled)` access-side step
  - `ValueHistory` now keeps optional source locations on formal/actual events,
    so report-side bug traces can anchor current-procedure parameter entries and
    synthetic call edges more precisely without changing the stable history
    signature strings used elsewhere
  - caller-side UAF qualifiers are a little closer too: when the access
    history still identifies the outer callee call, the top-level report text
    now uses a `The call to ... may trigger ...` shape instead of the generic
    `accessing address that ...` wording
  - invalid-access diagnostics now carry an optional trace-only access
    location, so caller-reified latent UAF traces can start from caller-side
    provenance while still anchoring the final `invalid access occurs here`
    step at the original callee access site (for the real `latent.c` fixture,
    line `18` stays on the terminal access step)
  - an ignored end-to-end regression now locks the current real `latent.c`
    `USE_AFTER_FREE` bug-trace subset for `manifest_use_after_free` and
    `main`, including the caller-side start line and the callee-side final
    access location
  - `transfer::tests::test_store_to_formula_known_zero_detects_error` was
    updated to build the caller-visible heap path first; this matches the
    existing `EqZero`/heap-allocated semantics used elsewhere in the Rust port
- Latest validated checkpoint:
  - compare dir: `/tmp/latent-compare.KcEUWD`
  - exported SIL: `/tmp/latent-compare.KcEUWD/textual/latent.sil`
  - OCaml report: `/tmp/latent-compare.KcEUWD/ocaml-out/report.json`
  - OCaml summaries: `/tmp/latent-compare.KcEUWD/ocaml-out/all_summaries.json`
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
- Reduced `latent.c` summary checkpoint from
  `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`:
  - `FN_nonlatent_use_after_free_bad{,2}`, `latent_use_after_free`,
    `manifest_use_after_free`, and `main` now line up again on kind counts and
    remembered summary conditions in the validated exported fixture
- Cross-reference before changing semantics:
  `infer/src/pulse/PulseSummary.ml`,
  `infer/src/pulse/PulseLatentIssue.ml`,
  `infer/src/pulse/PulseReport.ml`,
  `infer/src/pulse/PulseArithmetic.ml`
- Useful commands:
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`
  - `cargo test -p pulse --lib test_debug_real_abort_recovery_report_keys -- --ignored --nocapture`
  - `./target/debug/infer-rs --pulse-only --quiet --pulse-report-issues-for-tests --results-dir /tmp/latent-compare.Muh07b/ocaml-out --source-override codetoanalyze/c/pulse/latent.c -o /tmp/latent-rs-run.next /tmp/latent-compare.Muh07b/textual/latent.sil`
  - fresh narrow export repro (use `~/infer-rs-bench/...` instead of `/tmp`,
    because `/tmp` is reaped between sessions on this host and reruns lose
    captured corpora):
    `bench=~/infer-rs-bench/wpblock-$(date +%Y%m%d-%H%M%S) && mkdir -p "$bench"`
    `tar -xf ~/infer/benchmarks/openssl/openssl-1.0.2d.tar.gz -C "$bench"`
    `cd "$bench/openssl-1.0.2d"`
    `sdk=$(xcrun --show-sdk-path) && CC=clang CFLAGS="-isysroot $sdk" ./config no-asm`
    `infer capture --store-textual --results-dir infer-out-wp -- clang -I. -Iinclude -c crypto/whrlpool/wp_block.c`
    `infer debug --results-dir infer-out-wp --export-textual textual-out-wp`
  - fresh metadata check:
    `rg -o "__sil_metadata_[a-z_]+" textual-out-wp/wp_block.sil | sort -u`
  - fresh narrowed Rust repro (point at the `~/infer-rs-bench/...` corpus):
    `RUST_LOG=warn,ondemand=info target/release/infer-rs --pulse-only --trace-ondemand -j 1 --procedures-filter whirlpool_block "$bench/openssl-1.0.2d/textual-out-wp/wp_block.sil"`
  - focused selected-node alpha-signature repro:
    `RUST_LOG='pulse::checker::fixpoint=debug,ondemand=info,ondemand::runner=info' target/release/infer-rs --pulse-only --debug-level-analysis 1 --trace-ondemand --debug-fixpoint-nodes 31,35 -j 1 --procedures-filter whirlpool_block "$bench/openssl-1.0.2d/textual-out-wp/wp_block.sil" >~/infer-rs-bench/wpblock-alpha-signatures.log 2>&1`
- Fresh OpenSSL retained-state repro checkpoint:
  - fresh corpus dir: `~/infer-rs-bench/wpblock-20260430-181642` (older
    `/tmp/wpblock-export.*` corpora were reaped by the host)
  - focused alpha-signature log: rerun lives in
    `~/infer-rs-bench/wpblock-alpha-signatures.log` if regenerated
  - completed `whirlpool_block` focused run: `4m53s`, `1222` retained states,
    `max_node_disjuncts=8`, top retained nodes
    `29,31,32,33,35,36,37,38 -> 8d:4v`
  - on this fresh export, `#node_31` / `#node_35` in `textual-out-wp/wp_block.sil`
    currently map to source locations `[598:13]` / `[602:13]`; keep that in
    mind when comparing against older notes that referred to the `540` /
    `752-755` source block
  - the selected-node alpha signatures now show the same shape as the older
    narrowed probe but with a fresh end-to-end repro: `35:POST` matches
    `31:PRE` exactly, and the surviving `8` states still form `4` monotonic
    growth tiers x `2` variants; the tier growth is in retained heap / attrs /
    formula volume rather than in alpha-equivalent duplicates
  - current working interpretation: the remaining `whirlpool_block` blocker is
    primarily an OCaml-parity / loop-convergence gap, while Rust clone /
    storage cost is a secondary RSS amplifier; structural sharing is still
    worth pursuing for merged-run memory, but it is not expected to erase the
    `8d:4v` hotspot by itself
  - the raw `--debug-level-analysis 2` selected-node dump on the fresh export
    finished and confirmed a concrete per-tier growth pattern inside
    `node=31` retained PRE states: across the three larger tiers, each step
    adds about `+129` `Initialized` attrs, `+128..129` `ArrayAccess` edges,
    `+129` `Cx`-subtree nodes, and about `+896` formula items, while
    `MustBeValid` / `WrittenTo` counts stay flat. The extra retained state is
    concentrated in the global `Cx` table subtree rather than in the local
    `K` / `S` / `H` loop-state subgraphs.
  - a focused canonicalized dump regression now exists as an ignored unit test
    (`checker::tests::test_debug_wpblock_retained_canonical_states`) so the
    narrowed `31/35` retained-state shape can be reproduced without scraping a
    giant logger dump by hand
  - probing the global initializer directly is also informative now:
    running `__infer_globals_initializer_Cx` by itself on the fresh export is a
    single-disjunct but very large path (~`16k` live heap nodes after ~`5m22s`
    in the current release probe), so the full-program OpenSSL story likely
    has both (a) the `whirlpool_block` retained loop-head convergence gap and
    (b) a very expensive global-table initializer surface for `Cx`
  - the callgraph / procedures-filter / pre-analysis summary path has now been
    tightened too: loads rooted in globals add an implicit dependency on the
    matching `__infer_globals_initializer_<name>` proc, so
    `--procedures-filter whirlpool_block` now retains `3` procs on the fresh
    export (`whirlpool_block`, `memcpy`, `__infer_globals_initializer_Cx`)
  - Rust now also supports OCaml's default `pulse-max-cfg-size = 15000`, and
    on the fresh filtered release repro that means
    `__infer_globals_initializer_Cx` is retained but skipped as a large
    procedure before `whirlpool_block` runs. With that skip in place the
    filtered checkpoint drops back to the familiar OCaml-comparable single-file
    shape: about `4m42s`, `1222` retained states, and the same
    `29,31,32,33,35,36,37,38 -> 8d:4v` hotspot
  - latest single-file repro under `~/infer-rs-bench/wpblock-20260430-181642`:
    `4m34s` real, peak memory footprint `~16.7 GB`, `~16.9 GB` max RSS for
    just the filtered `whirlpool_block` slice with the skip in place. Rust is
    therefore still way over OCaml on this one-file slice (OCaml peak ~`945
    MB`, ~`32s` wall on the broader 55-file analyze in this same bench), and
    it confirms that with the skip the dominant remaining cost is per-disjunct
    state size in `whirlpool_block` itself, not the global initializer surface.
  - structural-sharing baby steps now in:
    - `BaseMemory.graph` is `Arc<BTreeMap<AbstractValue, Arc<Edges>>>` (two
      layers: outer Arc + per-address Arc<Edges>), with `Arc::make_mut` for
      both layers and cheap no-op pre-checks.
    - `BaseAddressAttributes.map` is similarly
      `Arc<BTreeMap<AbstractValue, Arc<Attributes>>>`.
    - `BaseStack.map` is `Arc<HashMap<Var, ValueWithHistory>>` (whole-map,
      since each stack entry is small).
    - `Formula.phi` is `Arc<Phi>` with a `phi_mut` helper, sharing the heavy
      phi maps (linear_eqs, term_eqs, atoms, intervals, var_eqs, ...).
    - the outer container of each is now itself reference-counted, so
      cloning a Pulse state never deep-copies any of the four big maps
      eagerly; mutations clone-on-write via `Arc::make_mut`, and the public
      `BaseMemory` / `BaseAddressAttributes` / `BaseStack` / `Formula` APIs
      are unchanged.
  - clean reruns on the same bench show progressive memory wins on this
    slice for the same `1222`-state / `8d:4v` filtered `whirlpool_block`
    checkpoint:
    - baseline before any Arc sharing: peak memory footprint ~`16.7 GB`
      (`4m34s` real on a less loaded host)
    - after per-address `Arc<Edges>` only: ~`13.84 GB` peak (`7m17s` real)
    - + per-address `Arc<Attributes>`: ~`9.34 GB` peak (`7m37s` real)
    - + `Arc<BaseStack.map>`: ~`5.97 GB` peak (`4m29s` real)
    - + `Arc<Phi>`: ~`5.73 GB` peak (`4m32s` real)
    - + outer `Arc<BTreeMap>` for `BaseMemory.graph` and
      `BaseAddressAttributes.map`: ~`3.93 GB` peak (`4m33s` real), i.e.
      about a `76%` peak-memory drop vs the pre-Arc baseline, at unchanged
      wall time
    - all reruns observe `0 swaps` and the same `1222` retained-state shape
  - the intermediate `~7m` wall-time numbers correlated with host load on
    those runs. All reruns since the BaseStack increment have matched the
    original `~4m30s` wall time, so on a calm host the structural-sharing
    changes are effectively memory-only wins for this slice.
  - the earlier forced-retention slice remains useful as an upper bound for
    scalability work: if `__infer_globals_initializer_Cx` is actually
    analyzed, it grows to ~`16k` live heap nodes by itself and then pushes the
    following `whirlpool_block` slice into multi-million retained heap / edge
    totals even while `max_node_disjuncts` is still only `4`
  - working interpretation after the `pulse-max-cfg-size` parity fix:
    the default OCaml-like single-file blocker is still the old
    `whirlpool_block` convergence gap, while the forced retained-initializer
    slice exposes a second hidden systems problem around large per-disjunct
    global-table materialization
  - the full raw dump is still unwieldy (`/tmp/wpblock-node-dump.log` reached
    ~`19.9M` lines), so a new lighter debugging aid is now in the working tree:
    selected fixpoint-node dumps can emit canonicalized state lines at
    `--debug-level-analysis 2`, reserving the old raw `{:#?}` dump for level
    `3`
  - a fresh narrower canonical-dump rerun on the same export is now in flight
    and is writing to `/tmp/wpblock-canonical2.log`
- Current validated working-tree checkpoint:
  - `cargo test -p pulse --test end_to_end test_debug_latent_summary -- --ignored --nocapture`
  - `cargo test -p pulse test_coalesce_zero_direct_formals_for_export -- --nocapture`
  - `cargo test -p pulse test_apply_summary_shared_zero_formal_target_prefers_pointer_actual_for_latent_invalid_access -- --nocapture`
  - `cargo test -p pulse test_translate_diagnostic_wraps_invalidation_history_with_callee_call -- --nocapture`
  - `cargo test -p pulse diagnostic::tests:: -- --nocapture`
  - `cargo test -p pulse test_translate_diagnostic_wraps_invalidation_history_with_callee_call -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_latent_real_bug_trace_matches_ocaml_subset -- --ignored --nocapture`
  - `cargo test -p pulse 'test_apply_summary_' -- --nocapture`
  - `cargo test -p pulse --test end_to_end test_e2e_latent_cycle_summary_shapes_match_ocaml_subset -- --nocapture`
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

Most of the older `whirlpool_block` retained-state probes are
resolved by the perf sessions documented in `docs/plans/`. Current
live candidates (full ranked list in `docs/plans/NEXT_STEPS.md`):

- **Re-baseline defaults (next):** use `scripts/bench_openssl_partial.sh` for repeated 74-file OpenSSL runs. The one-shot no-explicit-cap checkpoint is `226.86s` / `14.0GB` max RSS / `20` aborts / `max_visit_count=4`, but host noise is still large.
- **DES-family large-state investigation:** the latest B-track probe
  removes the `OBJ_bsearch_ex_` `max_visit_count=10001` pathology
  (`max_visit_count=4`), but the wall-time long tail is now bounded-
  visit DES-family procedures (`DES_ede3_cbcm_encrypt`,
  `DES_ofb_encrypt`, `DES_cfb_encrypt`, etc.) plus `OBJ_obj2txt`.
  Focus on per-state/per-disjunct heap/attrs/formula cost, not WTO
  convergence.
- **Benchmark plumbing:** add a helper script to run the OpenSSL
  partial benchmark N times and extract wall, max RSS, abort count,
  max_visit_count, and slow-proc tables. Host noise is still large.
- **D/E (open):** per-disjunct value-count residue and OCaml parity
  gaps in `TODO.md` are deeper investments / different tracks.

### Resolved (for reference)

- The old "explain why Rust keeps `4` growth tiers × `2` variants
  at nodes `31/35` ..." probe: investigated in
  `docs/plans/CONVERGENCE_8D4V_FINDINGS.md`. OCaml retains MORE
  disjuncts on the same slice (`10` vs our `8`), so the framing
  was wrong; the real cost was per-disjunct CPU, addressed by the
  drop-dead-logical-vars + term_value_index + ValueSortKey work.
- The old "merged-run abnormal termination / memory growth" work:
  resolved by `pulse-max-heap-mb` / `pulse-max-wall-secs` caps +
  empty-on-abort short-circuit. Whole-program OpenSSL completes
  cleanly with default caps now.
- The old "continue whole-program OpenSSL work on the publishable
  surfaces" line: published, see
  `docs/plans/WHOLE_PROGRAM_OPENSSL_FINDINGS.md` and
  `docs/plans/NEXT_STEPS.md`. Headline now: `~70× / OOM-killed`
  → out-of-box `~5.3× / clean` vs OCaml on the 74-file partial
  corpus, with `max_visit_count=4`
  in the latest convergence probes.
