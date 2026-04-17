# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- Active benchmark / repro dir:
  `/tmp/infer-rs-openssl-20260417-095315-rebase-j`
- Key result from the latest OpenSSL work:
  - the old immediate macOS `-j > 1` startup failure is no longer the main
    blocker
  - whole-program direct-`.sil` runs still die from memory growth /
    abnormal termination before we have a publishable Rust timing
  - `ssl_set_client_disabled` still proves the `equal_fast` split was real:
    about `1m09s` -> about `5.2s`, same `173` transfer steps / `20`
    disjuncts / hottest node `33:24`
- Current strongest hotspot evidence on `whirlpool_block`:
  - Rust frontier at `36.1s`: `20` live disjuncts, about `9837` summed post
    heap nodes
  - Rust retained invariant map at `36.1s`: `2995` post snapshots,
    about `975641` post heap nodes, `1313138` post heap edges,
    `2464294` post attr entries
  - OCaml final retained state on the same narrowed proc:
    `152` post snapshots across `178` CFG nodes, about `98727` post heap
    nodes, `53889` post heap edges, `13698` attr addrs, `39663` attr entries,
    and no final node with more than `1` disjunct
- Active conclusion:
  - this is not just a storage-sharing problem; Rust is retaining many more
    logical post states than OCaml on `whirlpool_block`
  - recency is not the dominant answer: `--pulse-recency-limit 32` stayed
    essentially unchanged on `whirlpool_block`, and default-enabling it
    reintroduced the real `nullptr.c` `FN_nullptr_deref_old_bad`
  - current suspect is semantic convergence at loop heads:
    `ExecutionDomain::leq` / `state_cmp::alpha_equivalent` and attribute /
    history normalization are likely still stricter than OCaml
- Useful paths / commands:
  - Rust hotspot file:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j/textual-out/wp_block.sil`
  - OCaml HTML dir:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j/infer-out/captured/wp_block.c.b43ab3043ea2edad`
  - narrowed OCaml repro:
    `printf 'openssl-1.0.2d/crypto/whrlpool/wp_block.c\n' > /tmp/infer-rs-openssl-wp_block.changed`
    `infer analyze --pulse-only --debug --results-dir infer-out -j 1 --changed-files-index /tmp/infer-rs-openssl-wp_block.changed --procedures-filter whirlpool_block`
- Last validated code checkpoint:
  - commit `7536cda6e8` (`perf(pulse): trace fixpoint retention`)
  - validated with:
    `make check` (earlier clean checkpoint),
    `cargo test -p pulse test_size_stats_counts_key_state_surfaces`,
    `cargo test -p ondemand test_get_arc_shares_cached_summary`,
    `cargo build -p infer-rs --release`

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

- Compare Rust-retained states at loop heads `3` and `17` against the OCaml
  HTML and identify the first canonicalized difference that prevents collapse.
- Audit `crates/pulse/src/state_cmp.rs` against OCaml
  `PulseAbductiveDomain.leq` / graph isomorphism, with special attention to:
  `WrittenTo`, `MustBeValid`, `MustBeInitialized`, invalidation history, and
  any other iteration-sensitive attrs.
- Only after semantic retention is closer to OCaml should we spend more time
  on storage/persistence work or new whole-program `-j 4` / `-j 8` benchmark
  runs.
