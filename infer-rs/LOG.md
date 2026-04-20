# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- Active benchmark / repro dir:
  `/tmp/infer-rs-openssl-20260417-095315-rebase-j`
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
  - current `wp_block.sil` export does contain
    `__sil_metadata_variable_lifetime_begins`, but it still contains no
    `__sil_metadata_abstract`, `__sil_metadata_nullify`,
    `__sil_metadata_exit_scope`, or `__sil_metadata_loop_*` calls on the hot
    loop path around line `540`
  - after landing metadata import support, the narrowed traced rerun stayed on
    the same final shape: `1m46s`, `611` retained disjunct states,
    `max_node_disjuncts=4`, and top retained nodes
    `18:4d:4v, 20:4d:4v, 21:4d:4v, 22:4d:4v, 24:4d:4v, 25:4d:4v,
    26:4d:4v, 27:4d:4v`
- Active conclusion:
  - the dominant OpenSSL gap was not just storage sharing and not just
    `state_cmp`; Rust was re-executing already-known pre disjuncts on hot WTO
    revisits
  - recency is still not the dominant answer: `--pulse-recency-limit 32`
    stayed essentially unchanged on `whirlpool_block`, and default-enabling it
    reintroduced the real `nullptr.c` `FN_nullptr_deref_old_bad`
  - Rust is now much closer to OCaml on `whirlpool_block`, but the remaining
    semantic gap is the smaller set of loop-head states that still retain up
    to `4` disjuncts instead of OCaml's final `0/1`
  - the remaining `whirlpool_block` gap is no longer a Rust-side Textual
    metadata import miss: importer support is in place and tested, but the
    current export surface still drops the cleanup/abstraction metadata that
    OCaml carries on the hot loop path
- Useful paths / commands:
  - Rust hotspot file:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j/textual-out/wp_block.sil`
  - OCaml HTML dir:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j/infer-out/captured/wp_block.c.b43ab3043ea2edad`
  - narrowed OCaml repro:
    `printf 'openssl-1.0.2d/crypto/whrlpool/wp_block.c\n' > /tmp/infer-rs-openssl-wp_block.changed`
    `infer analyze --pulse-only --debug --results-dir infer-out -j 1 --changed-files-index /tmp/infer-rs-openssl-wp_block.changed --procedures-filter whirlpool_block`
  - current narrowed Rust repro:
    `RUST_LOG=warn,ondemand=info target/release/infer-rs --pulse-only --trace-ondemand -j 1 --procedures-filter whirlpool_block /tmp/infer-rs-openssl-20260417-095315-rebase-j/textual-out/wp_block.sil`
  - narrowed post-importer metadata check:
    `rg -n "__sil_metadata_(abstract|nullify|exit_scope|loop_back_edge|loop_entry|loop_exit|try_entry|try_exit|catch_entry|skip|variable_lifetime_begins)" /tmp/infer-rs-openssl-20260417-095315-rebase-j/textual-out/wp_block.sil`
- Current validated working-tree checkpoint:
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

- Keep the missing exported cleanup metadata documented as an upstream/export
  fidelity limit. If `--export-textual` starts emitting the corresponding
  `__sil_metadata_{abstract,nullify,exit_scope,loop_*}` calls on this proc,
  rerun `whirlpool_block` immediately.
- Use the current importer support as the floor: do not add a Pulse workaround
  for this specific `whirlpool_block` gap just to move the retained-state
  numbers.
- Continue whole-program OpenSSL work on the publishable surfaces that remain:
  merged-run abnormal termination / memory growth and then a clean `-j 1`
  rerun on the shared exported corpus.
