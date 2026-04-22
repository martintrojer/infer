# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- Active benchmark / repro dirs:
  - shared exported corpus:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j`
  - richer single-file hotspot repro:
    `/tmp/wpblock-export.CAfXIo/openssl-1.0.2d`
- Latest validated OpenSSL finding:
  - `state_cmp` now mirrors OCaml `PulseAbductiveDomain.leq` more closely:
    compare only the stack-reachable heap / attr graph, ignore disconnected
    retained garbage, and ignore Rust-only helper caches such as
    `must_be_valid` and `need_dynamic_type_specialization`
  - that comparator cleanup was semantically correct but did not materially
    move the `whirlpool_block` hotspot by itself
  - the large reduction came from a new `TransferFunctions::exec_node(...)`
    hook plus a Pulse override mirroring OCaml
    `AbstractInterpreter.MakeDisjunctiveTransferFunctions.exec_node_instrs`:
    on WTO revisits, re-execute only pre disjuncts that are new w.r.t. the
    retained node pre-state and join those results into the retained post
  - the old immediate macOS `-j > 1` startup failure is no longer the main
    blocker; whole-program direct-`.sil` runs still die from memory growth /
    abnormal termination before we have a publishable Rust timing
  - `ssl_set_client_disabled` still proves the earlier `equal_fast` split was
    real: about `1m09s` -> about `5.2s`, same `173` transfer steps / `20`
    disjuncts / hottest node `33:24`
  - latest selected-node alpha-signature rerun on the richer
    `whirlpool_block` export finished in `4m30s` with the same
    `1222` retained states and `29/31/32/33/35/36/37/38 -> 8d:4v`, but it
    proves nodes `31` and `35` contain eight distinct semantic states under
    the current Rust canonicalizer, not duplicate alpha-equivalent states
- Current strongest hotspot evidence on `whirlpool_block`:
  - new correctness fix in Rust attr storage:
    `Attribute::add` now mirrors OCaml
    `PulseAttribute.Attributes.Set.add` rank semantics instead of keeping
    multiple same-kind payloads in a raw `BTreeSet`
  - precise parity implemented:
    ordinary same-rank attrs keep the first payload,
    `WrittenTo` replaces the previous payload by rank,
    and `Invalid(OptionalEmpty, ...)` also replaces by rank
  - focused Rust regressions for that behavior:
    `attribute::tests::test_invalid_keeps_first_same_rank_attribute_like_ocaml`,
    `attribute::tests::test_written_to_replaces_previous_same_rank_attribute`,
    and `attribute::tests::test_invalid_optional_empty_replaces_previous_invalid`
  - targeted validation after the attr-rank fix:
    `cargo test -p pulse attribute::tests -- --nocapture` and
    `cargo test -p pulse checker::tests::test_fixpoint_loop_does_not_keep_exit_scope_temps_rooted -- --nocapture`
    both pass
  - fresh rebuilt release rerun on the richer single-file export still ends at
    the same top-level hotspot shape:
    `4m58s`, `1222` retained states, `max_visit_count=4`,
    `max_node_disjuncts=8`, `exit_disjuncts=2`, `pre_posts=2`,
    top retained nodes
    `29:8d:4v, 31:8d:4v, 32:8d:4v, 33:8d:4v, 35:8d:4v, 36:8d:4v,
    37:8d:4v, 38:8d:4v`
  - however the retained selected blocks did change materially internally:
    `compare_fixpoint_blocks.py` on the rebuilt release log shows large
    duplicate-invalid cleanup without changing the disjunct counts
  - representative examples from that retained-block compare:
    `29:PRE invalid 2566 -> 80`,
    `31:PRE 3338 -> 84`,
    `35:PRE 3336 -> 84`,
    `38:POST 3332 -> 84`,
    while `must_be_valid` stays `8` and the visible var-set stays `18`
  - real Textual correctness fix landed too:
    `remove_effects_in_subexprs` / `let_propagation` now mirror OCaml
    `Textual.ProcDecl.is_side_effect_free_sil_expr` instead of treating all
    top-level `__sil_*` calls as removable; metadata builtins such as
    `__sil_metadata_exit_scope` now survive the real transform path
  - focused regressions for that metadata fix:
    `textual::transform::test_let_propagation_keeps_metadata_calls` and
    `textual::to_sil::test_transform_preserves_metadata_after_prune`
  - retained-block compare against the pre-metadata-fix log shows the hotspot
    blocks got materially smaller and more faithful even though the final
    `1222 / 8d` shape did not change:
    `29:PRE lines 1020591 -> 544317 vars 18 -> 9 uninitialized 0 -> 20`,
    `31:PRE 1390025 -> 812463 vars 18 -> 9`,
    `35:PRE 1383567 -> 809423 vars 18 -> 10`,
    `38:POST 1370651 -> 803343 vars 18 -> 12`
  - focused count on node `35:PRE` confirms the remaining problem is not gone:
    occurrences of `col: 33` drop `388 -> 266`, but logical temp stamps
    `56/57/58` stay present with the same counts and the final hotspot shape
    is unchanged
  - new debug-only alpha signatures in Rust `state_cmp` plus selected-node
    fixpoint logging give a much better readout than raw pretty-printed
    retained states:
    `/tmp/wpblock-alpha-signatures.log`
  - those alpha signatures show a clear ladder, not accidental duplicates:
    nodes `31` and `35` each have `8` unique hashes arranged as
    `4` growth tiers x `2` structural variants;
    `35:POST` is exactly the same signature set as `31:PRE`;
    successive tiers add roughly `+258` post heap entries,
    `+129` post attr entries, and `+896` formula items
  - OCaml final retained state on the narrowed proc:
    `152` post snapshots across `178` CFG nodes, about `98727` post heap
    nodes, `53889` post heap edges, `13698` attr addrs, `39663` attr entries,
    and no final node with more than `1` disjunct
  - old Rust baseline before the WTO `exec_node` fix:
    at `36.1s`, frontier `20` disjuncts and retained invariant map
    `2995` post snapshots, about `975641` post heap nodes,
    `1313138` post heap edges, `2464294` post attr entries
  - current Rust after the WTO `exec_node` fix:
    at `10.1s`, frontier `1` disjunct, retained `366` snapshots,
    `max_node_disjuncts=3`;
    at `30.3s`, retained `466` snapshots, `max_visit_count=4`,
    `max_node_disjuncts=4`;
    at `40.6s`, retained `495` snapshots;
    at `50.8s`, retained `520` snapshots;
    at `61.0s`, retained `544` snapshots;
    at `71.1s`, retained `566` snapshots;
    final current run completes in `1m52s` with `611` retained snapshots
    across `180` nodes, `max_visit_count=4`, `max_node_disjuncts=4`,
    `exit_disjuncts=2`, and `pre_posts=2`
  - new debug-only fixpoint tail logger points at the retained hot block:
    `18:4d:4v, 20:4d:4v, 21:4d:4v, 22:4d:4v, 24:4d:4v, 25:4d:4v,
    26:4d:4v, 27:4d:4v`
  - OCaml HTML on the same node IDs shows the remaining gap is not incoming
    frontier width alone:
    node `18` last `PRE STATE=1`, last `Got 1`;
    nodes `20/21/24/25/26/27` last `PRE STATE=7`, last `Got 1`;
    node `22` last `PRE STATE=7`, last `Got 0`
  - Rust Textual→SIL now lowers OCaml-exported `__sil_metadata_*` helper
    calls back to `Instr::Metadata` (cross-ref:
    `infer/src/textual/TextualOfSil.ml` `InstrBridge.of_sil_metadata` and
    `infer-rs/crates/textual/src/to_sil.rs`), with focused unit tests
  - the sibling OCaml `infer` repo now locally fixes `infer debug
    --export-textual` for C/Java by regenerating textual from freshly loaded
    procdescs after preanalysis/WTO setup instead of dumping the raw stored
    `source_files.textual`
  - Rust Pulse now mirrors OCaml `Pulse.ml` / `PulseAbductiveDomain.Stack`
    for `Metadata::ExitScope`: remove dead post-stack vars while preserving
    pre-rooted formals that must survive into summaries
  - focused Rust regressions:
    `transfer::tests::test_exit_scope_removes_dead_post_stack_vars` and
    `transfer::tests::test_exit_scope_keeps_pre_rooted_formals`
  - focused OCaml regression:
    `infer/tests/codetoanalyze/c/export-textual/metadata.c` now verifies that
    exported textual contains `__sil_metadata_abstract`,
    `__sil_metadata_nullify`, `__sil_metadata_exit_scope`, and
    `__sil_metadata_variable_lifetime_begins`
  - fresh single-file OpenSSL `wp_block.c` export now carries those cleanup
    helpers too, so the old export-boundary explanation is gone for this
    hotspot
  - correctness first result: the fresh filtered Rust `whirlpool_block` run on
    that richer export now finishes in `4m52s` with `1222` retained states,
    `max_visit_count=4`, `max_node_disjuncts=8`, and top retained nodes
    `29:8d:4v, 31:8d:4v, 32:8d:4v, 33:8d:4v, 35:8d:4v, 36:8d:4v,
    37:8d:4v, 38:8d:4v`; export fidelity improved, but the hotspot did not
    magically collapse
  - after wiring `ExitScope` semantics into Rust transfer, a fresh rerun on
    the same richer export showed materially lower transient retained-state
    growth:
    `10.3s -> 668 states / max_node_disjuncts=6`,
    `31.0s -> 768 / 6`,
    `1m12s -> 922 / 6`,
    `1m23s -> 942 / 8`,
    `2m05s -> 1022 / 8`
  - completed selected-node rerun after that fix:
    `4m58s`, `204` CFG nodes, `173` revisited nodes,
    `1222` retained states, `max_visit_count=4`, `max_node_disjuncts=8`,
    `pre_posts=2`, same top retained nodes
    `29:8d:4v, 31:8d:4v, 32:8d:4v, 33:8d:4v, 35:8d:4v, 36:8d:4v,
    37:8d:4v, 38:8d:4v`
  - Rust Pulse now also mirrors OCaml `Pulse.ml` /
    `PulseOperations.realloc_pvar` for
    `Metadata::VariableLifetimeBegins`: non-global locals are rebound to a
    fresh stack slot, and ordinary scalar/pointer locals are marked
    uninitialized unless `is_cpp_structured_binding=true`
  - focused Rust regressions:
    `transfer::tests::test_variable_lifetime_begins_rebinds_local_and_marks_scalar_uninitialized`
    and
    `transfer::tests::test_variable_lifetime_begins_structured_binding_skips_uninitialized_mark`
  - completed selected-node rerun after that fix:
    `4m56s`, `204` CFG nodes, `173` revisited nodes,
    `1222` retained states, `max_visit_count=4`, `max_node_disjuncts=8`,
    `pre_posts=2`, same top retained nodes
    `29:8d:4v, 31:8d:4v, 32:8d:4v, 33:8d:4v, 35:8d:4v, 36:8d:4v,
    37:8d:4v, 38:8d:4v`
  - representative retained blocks are structurally unchanged before vs after
    that fix:
    `29:PRE`, `29:POST`, `31:PRE`, `31:POST`, `38:PRE`, and `38:POST`
    keep identical line counts plus identical counts of abstract values,
    invalid attrs, initialized attrs, `must_be_valid`, and distinct var names;
    raw normalized hashes still differ and the first visible line diff is
    local ordering (`ctx` vs `n`), so do not treat old/new Rust block hashes
    as semantic evidence by themselves
  - selected-node mapping from the Rust dump:
    node `29` is the empty join block at line `540`,
    nodes `31/32/33` are the `r++` / load / prune chain at line `540`,
    nodes `35/36/37/38` are the `S.q[...] = L*` stores at lines `752-755`
  - richer-export OCaml cross-map is now tighter too:
    OCaml node `30` ~= Rust node `31` (`r++`),
    OCaml node `31` ~= Rust node `32` (load),
    OCaml node `32` ~= Rust node `33` (prune / exit-scope),
    and OCaml nodes `34/35/36/37` ~= Rust nodes `35/36/37/38`
    for the `S.q[7/6/5/4]` stores; Rust node `29`'s empty join block does
    not have a clean same-number OCaml partner
  - latest OCaml HTML clue on that block:
    the store-chain PRE states at OCaml nodes `34-37` visibly keep
    `r -> { line 540, column 21 -> { 80, 90 }, line 602, column 22 -> { 115, 125 } }`
    with no visible `line 540, column 33` increment history, while nearby
    OCaml nodes such as `28/30` still show the `line 540, column 33`
    history component; likely next Rust check is whether the retained
    `r` / temp histories are being collapsed later than OCaml
  - matching OCaml HTML on those source lines ends with `Got 1 disjunct back`
    and smaller last visible PRE widths:
    node `29` -> `2`, nodes `31/32` -> `4`, nodes `35/36/37/38` -> `2`
  - OCaml cross-check: `Pulse.ml` keeps `Nullify` and `Abstract` as no-op
    metadata too, so the remaining gap is not "implement more metadata"
  - conclusion from the completed reruns: missing `ExitScope` and
    `VariableLifetimeBegins` handling were real Rust correctness gaps, but
    neither changes the final fixpoint shape on this hotspot; the remaining
    problem is later retained-state convergence on the line `540` /
    `752-755` block
- Active conclusion:
  - the dominant OpenSSL gap was not just storage sharing and not just
    `state_cmp`; Rust was re-executing already-known pre disjuncts on hot WTO
    revisits
  - the new attr-rank fix was also a real OCaml parity bug and removes a lot
    of duplicate retained invalidation payload, but it is not the main
    `whirlpool_block` convergence lever because the final `1222 / 8d` shape
    remains unchanged
  - the remaining `31/35` hotspot is not "missed alpha dedup inside a node":
    the retained disjunct hashes are all distinct under the current
    stack-reachable canonicalizer, and the block now looks like loop-cycle
    growth across `4` revisit tiers rather than duplicate copies of one state
  - recency is still not the dominant answer: `--pulse-recency-limit 32`
    stayed essentially unchanged on `whirlpool_block`, and default-enabling it
    reintroduced the real `nullptr.c` `FN_nullptr_deref_old_bad`
  - Rust is now much closer to OCaml on the old shared export, and on the
    corrected richer export we have now fixed both the export boundary and the
    Rust-side `ExitScope` semantics; the remaining `whirlpool_block` problem
    is explicitly later fixpoint / retained-state convergence work on the
    line `540` / `752-755` block
- Useful paths / commands:
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
  - `cargo test -p pulse test_debug_signature -- --nocapture`
  - `cargo test -p pulse test_exit_scope_ -- --nocapture`
  - `cargo test -p pulse test_variable_lifetime_begins_ -- --nocapture`
  - `cargo test -p pulse exec_node_skips_reexecuting_old_pre_disjuncts -- --nocapture`
  - `cargo test -p pulse state_cmp -- --nocapture`
  - `cargo test -p absint --lib -- --nocapture`
  - `cargo test -p textual -- --nocapture`
  - `cargo test -p analyses test_liveness_on_c_fixture -- --nocapture`
  - `cargo build -p infer-rs --release`
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
