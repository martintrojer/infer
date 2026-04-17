# Debug Log

This file is for short-lived but important debugging context that should
survive chat compaction. Keep it current when the active line of investigation
changes, and move finished results to durable docs/tests/commits.

## Current Focus

- 2026-04-17 OpenSSL merged-parallel stability after the rebase:
  - benchmark dir:
    `/tmp/infer-rs-openssl-20260417-095315-rebase-j`
  - shared-capture recipe validated on this host:
    - repo clang on `PATH`
    - `CC=clang`
    - explicit SDK `-isysroot $(xcrun --show-sdk-path)`
    - `./Configure darwin64-x86_64-cc no-asm`
  - measured setup:
    - capture: `371.32s`
    - export: `0.35s`
    - exported corpus: `753` `.sil`
  - measured OCaml baseline on the same capture:
    - `infer analyze --pulse-only --results-dir infer-out -j 1`
    - `589.76s`
    - about `2.55 GB` max RSS
  - measured Rust direct `.sil` runs on the exported corpus:
    - `-j 8 --trace-ondemand`
      - parse done: `43.1s`
      - merged input: `8395` procedures / `683` types
      - runner reached round 1 with `active=8`
      - terminated abnormally at `190.81s`
      - about `24.5 GB` max RSS
    - `-j 4`
      - terminated abnormally at `690.77s`
      - about `33.2 GB` max RSS
  - current diagnosis:
    - the old immediate macOS `-j > 1` startup failure is no longer the main
      blocker
    - the current blocker is merged parallel-memory growth / abnormal
      termination on this benchmark
    - do not publish a whole-program Rust timing claim from OpenSSL yet
    - duplicate-proc exported-Textual proc-UID loss is still real, but it is a
      separate fidelity limitation rather than the main reason these runs die
  - still-valid hotspot result:
    - `ssl_set_client_disabled` dropped from about `1m09s` to about `5.2s`
      after restoring the OCaml-style `equal_fast` / semantic-`leq` split
    - execution shape stayed the same: `173` transfer steps, `20` disjuncts,
      hottest node `33:24`
  - new state-shape instrumentation checkpoint:
    - landed cheap `pulse-progress` counters for per-disjunct state size and
      end-of-fixpoint invariant-map shape
    - extended those counters with post-state reachable-vs-dead heap / attr
      counts
    - validation:
      - `cargo test -p pulse test_size_stats_counts_key_state_surfaces`
      - `cargo test -p ondemand test_get_arc_shares_cached_summary`
      - `cargo build -p infer-rs --release`
      - `cargo fmt`
      - `cargo clippy -- -D warnings`
      - note: one local `make check` attempt got through fmt/clippy/unit tests,
        then the remaining `pulse` `tests/end_to_end.rs` process stayed asleep
        at `0%` CPU with no children; not treated as a clean full pass
    - focused OpenSSL probes with the new release binary:
      - `wp_block.sil` / `whirlpool_block` (`-j 1 --procedures-filter`)
        - at `10.0s`: `20` live disjuncts already held about
          `7839` post heap nodes, `10542` post heap edges, `20039` post attr
          entries, `5520` linear eqs, `4718` intervals, `7543` `must_be_valid`
        - at `1m16s`: those grew to about
          `11557` post heap nodes, `14698` post heap edges, `26141` post attr
          entries, `8580` linear eqs, `7998` intervals, `11061` `must_be_valid`
        - dynamic-type / specialization sets stayed `0`
      - `camellia.sil` / `Camellia_Ekeygen` (`-j 1 --procedures-filter`)
        - at `10.0s`: `14` live disjuncts held about
          `2142` post heap nodes, `2982` post heap edges, `4634` post attr
          entries, `672` linear eqs, `434` intervals, `1806` `must_be_valid`
        - at `2m14s`: those grew to about
          `2716` post heap nodes, `4130` post heap edges, `7966` post attr
          entries, `1911` linear eqs, `1673` intervals, `2380` `must_be_valid`
        - live RSS sample during this probe: about `12.7 GB`
      - whole exported corpus at `-j 1` with the same instrumentation:
        - parse completed in `3m01s`; merged input stayed `8395` procedures /
          `683` types
        - analysis reached `4 / 8395` completed procedures at `10s` and was
          already monopolized by `whirlpool_block`
        - at `~30s` analysis it was still `4 / 8395`, `active=1`, with the
          same `whirlpool_block` state-shape growth as the isolated probe
        - live RSS sample during this merged `-j 1` run: about `29.8 GB`
      - reachable-vs-dead split on isolated `whirlpool_block`:
        - at `10.0s`: about `3364` live post heap nodes vs `4456` dead,
          `6048` live post heap edges vs `4456` dead,
          `6810` live post attr entries vs `13153` dead
        - at `24.7s`: about `3801` live post heap nodes vs `5376` dead,
          `6922` live post heap edges vs `5376` dead,
          `7875` live post attr entries vs `16046` dead
        - at `34.7s`: about `3821` live post heap nodes vs `6056` dead,
          `6962` live post heap edges vs `6056` dead,
          `8999` live post attr entries vs `15562` dead
    - conclusion from the probes:
      - the dominant growth is in live post-state heap / attrs /
        path-condition breadth inside hot procedures
      - a large fraction of that post-state surface is already dead retained
        graph, not just genuinely live reachable state
      - this blow-up does not require parallel scheduling; a single hot
        procedure can push RSS into the tens of GB on the merged corpus
      - dynamic type specialization is not the driver here
      - summary sharing helped throughput, but it is not the main memory fix
  - recency-limit checkpoint:
    - Rust now has an opt-in `pulse-recency-limit` knob in both `.inferconfig`
      and CLI, plus an OCaml-style batched recency structure in
      `BaseMemory::Edges`
    - validation:
      - `cargo test -p config`
      - `cargo test -p pulse base_memory`
      - `cargo test -p infer-rs test_default_rust_log_filter_adds_ondemand_info_for_trace_flag -- --nocapture`
      - `cargo test -p pulse --test end_to_end test_e2e_nullptr_old_vector_element_is_still_tracked -- --ignored --nocapture`
    - important policy result:
      - when the recency cap was temporarily enabled by default during local
        validation, it reintroduced the known `nullptr.c`
        `FN_nullptr_deref_old_bad` false negative
      - because that is a real bug Rust currently reports, the Rust default now
        stays unset / unbounded; the recency cap is for explicit experiments,
        not the default analyzer behavior
    - focused OpenSSL probe with the new release binary:
      - `wp_block.sil` / `whirlpool_block`
        `--pulse-recency-limit 32 -j 1 --procedures-filter whirlpool_block`
        was effectively identical to the unbounded baseline
      - at `10.0s`: about
        `7761` post heap nodes, `10426` post heap edges, `19829` post attr
        entries, `3345` live heap nodes vs `4416` dead,
        `6010` live heap edges vs `4416` dead,
        `6753` live attr entries vs `13076` dead
      - at `26.0s`: about
        `9177` post heap nodes, `12298` post heap edges, `23921` post attr
        entries, `3801` live heap nodes vs `5376` dead,
        `6922` live heap edges vs `5376` dead,
        `7875` live attr entries vs `16046` dead
      - at `36.0s`: about
        `9837` post heap nodes, `12978` post heap edges, `24541` post attr
        entries, `3821` live heap nodes vs `6016` dead,
        `6962` live heap edges vs `6016` dead,
        `8999` live attr entries vs `15542` dead
      - conclusion: OCaml-style edge recency alone is not the dominant fix for
        the `whirlpool_block` live-state explosion
  - live fixpoint-map checkpoint:
    - added a low-frequency `live-fixpoint` heartbeat by wiring a default
      `TransferFunctions::observe_fixpoint(...)` hook through `absint`
    - validation:
      - `cargo test -p pulse test_size_stats_counts_key_state_surfaces -- --nocapture`
      - `cargo build -p infer-rs --release`
    - focused default-behavior OpenSSL probe with the new release binary:
      - `wp_block.sil` / `whirlpool_block`
        `-j 1 --procedures-filter whirlpool_block --trace-ondemand`
      - at `10.3s`:
        - current frontier heartbeat still showed only `20` live disjuncts and
          about `7820` summed post heap nodes
        - but `live-fixpoint` already showed `164` CFG nodes retained,
          `2180` disjunct snapshots total, and about `545860` summed post heap
          nodes / `740169` summed post heap edges / `1357857` summed post attr
          entries across the invariant map
      - at `26.1s`:
        - current frontier still only about `9177` summed post heap nodes
        - `live-fixpoint` had grown to `2806` retained disjunct snapshots and
          about `819767` summed post heap nodes / `1107340` summed post heap
          edges / `2061622` summed post attr entries
      - at `36.1s`:
        - current frontier still only about `9837` summed post heap nodes
        - `live-fixpoint` had grown to `2995` retained disjunct snapshots and
          about `975641` summed post heap nodes / `1313138` summed post heap
          edges / `2464294` summed post attr entries
      - conclusion:
        - the single active disjunct set is large but not the whole memory
          story
        - the dominant multiplier is that the fixpoint invariant map is
          retaining near-maximum bounded disjunct sets at many CFG nodes
        - next investigation should focus on invariant-map storage / sharing
          cost and on whether OCaml keeps the same logical shape with much
          cheaper persistent sharing

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

- Measure the same shared exported corpus at `-j 1` and `-j 2` before doing
  more whole-program `-j 4` / `-j 8` experiments.
- Use the new state-shape counters on a merged corpus run to identify which
  hot procedures dominate memory and whether growth is mostly:
  post heap, attr sets, or formula facts.
- Cross-reference the hottest growth surfaces against OCaml Pulse state
  lifecycle points:
  overwrite / havoc semantics, invalidation / overwrite forgetting, formula
  retention, and any intermediate dead-state cleanup before summary export.
- Do not assume `pulse-recency-limit` is the main answer:
  the new opt-in probe on `whirlpool_block` showed near-identical growth to the
  unbounded baseline while the default-on version reintroduced the known
  `nullptr.c` false negative.
- Use the new `live-fixpoint` heartbeat to separate:
  current frontier size vs retained invariant-map size. The latest
  `whirlpool_block` probe shows the invariant map is the dominant multiplier.
- Cross-reference OCaml's logical per-node state retention against Rust's
  physical storage cost:
  if the logical shape is similar, the next real work is data-structure /
  sharing strategy rather than another semantic cap.
- Add one more layer of cheap counters only if needed:
  invariant-map totals per active procedure are now visible at procedure end,
  but live merged-run retention may still need scheduler-time snapshots.
