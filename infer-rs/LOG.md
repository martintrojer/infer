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
- Current strongest hotspot evidence on `whirlpool_block`:
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
- Current validated working-tree checkpoint:
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

## Next Probes

- Compare the current richer single-file `wp_block.c` retained PRE/POST states
  against the OCaml line `540` / `752-755` block. Do not spend more time
  diffing pre-VLB vs post-VLB Rust dumps unless a normalized retained-block
  compare points to a real structural change.
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
