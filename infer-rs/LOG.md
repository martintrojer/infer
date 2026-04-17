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
  - the new node scrape sharpens that further: OCaml reaches the same hot
    block with up to `7` incoming disjuncts, but collapses those nodes back to
    `1` or `0`; Rust still keeps `4`-way retained posts there, so the next
    probe is post-node collapse / dedup on that block rather than frontier
    breadth or recency
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
- Current validated working-tree checkpoint:
  - `cargo test -p pulse exec_node_skips_reexecuting_old_pre_disjuncts -- --nocapture`
  - `cargo test -p pulse state_cmp -- --nocapture`
  - `cargo test -p absint --lib -- --nocapture`
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

- Compare the retained Rust post states on nodes
  `18, 20, 21, 22, 24, 25, 26, 27` against the OCaml HTML / Rust debug trace
  and identify the first post-node semantic difference that prevents collapse
  from `4` to OCaml's `1/0`.
- Use the new debug-only `fixpoint-top-nodes` logger as the entry point before
  adding any broader instrumentation or revisiting storage work.
- After that narrower comparison is understood, rerun the shared OpenSSL
  corpus at `-j 1` before spending more time on whole-program `-j 4` / `-j 8`
  runs or storage/persistence work.
