# Next steps after Phase 1 structural sharing

Phase 1 structural sharing landed (commits `47670022bf`, `c35f1a60f6`,
`8c94a4f6d6`, `52a340de85`, `4458f9e7f3`). On the filtered single-file
`whirlpool_block` slice, peak memory is now `~3.93 GB` (down from `~16.7 GB`,
`~76%` reduction) at unchanged wall time and unchanged analysis behavior
(same `1222`-state / `8d:4v` retained shape, `0` swaps).

We've also hit clear diminishing returns on more sharing: the last increment
(`Arc<Phi>`) only saved `~0.24 GB`. The remaining per-state cost is spread
across smaller maps (`const_cache`, `must_be_valid`, `dynamic_types`,
`need_dynamic_type_specialization`, `Formula.conditions`).

Three live tracks from here, ordered by where the leverage now sits:

## A. Diagnose the `8d:4v` convergence gap (recommended)

This is the actual remaining blocker. OCaml runs `whirlpool_block` to a much
tighter fixpoint than we do; that's why we retain `1222` snapshots where it
retains far fewer. The Arc work made each snapshot `~4×` cheaper, but did not
reduce the count.

We have a starting datum from the per-tier debug dump at retained node
`31 PRE`: each tier added `~129` `Cx`-subtree nodes/array-edges/`Initialized`
attrs and `~896` formula items, all concentrated in the global `Cx` table
subtree, not in local `K` / `S` / `H`. That points at how we treat reads from
a large global table versus how OCaml does.

### Concrete first step

1. Reproduce the 8-disjunct retained state at `31 PRE` in a controlled way
   (we already have `--debug-fixpoint-nodes` + the lighter canonical dump
   helper in `state_cmp::debug_canonical_dump` for this).
2. Pick two adjacent disjuncts from that 8-set, run them through both
   OCaml's and Rust's `Formula.join` / abductive `join`, and diff the result.
3. If OCaml collapses where we split, the cause is one of:
   - (a) an atom we keep that they discard,
   - (b) a disjunct distinction we preserve in attrs/heap that they widen
     away,
   - (c) a renaming we fail to perform,
   - (d) a difference in how reads from large globals get summarized at the
     join point.

### Pros / cons

- **Pros**: real OCaml-parity win; reduces retained count; compounds with
  Arc savings; success here lifts the absolute memory floor on this slice
  below what cheap structural sharing alone can reach.
- **Cons**: invasive; potentially multi-session; requires reading OCaml join
  code carefully and reproducing pieces of it.

## B. Validate Arc savings on whole-program OpenSSL

The `~76%` drop is on a single-procedure slice. The original
`24-33 GB` OOMs were on the merged `-j > 1` whole-program run. Confirming
the same shape of savings at scale is what would actually let us claim
"OpenSSL works now."

The capture pipeline keeps stalling on this host (the latest attempt died
silently mid-build with only `74` `.o` files instead of `753`, exit file
never written). Two options:

- Add an idle-timeout watchdog around the capture pipeline so a stuck
  build pipeline either succeeds or fails loudly within a bounded time.
- Skip rebuilding capture and reuse whatever historical corpus still exists
  on the host, even if partial.

### Pros / cons

- **Pros**: turns "looks like it should help" into a measured number on the
  actual original problem; would let us close the OpenSSL OOM ticket if it
  works.
- **Cons**: spends session time on build infra rather than analysis;
  doesn't move OCaml parity at all.

## C. Mop up remaining smaller Arc candidates

Wrap the remaining per-state collections: `const_cache`, `must_be_valid`,
`dynamic_types`, `need_dynamic_type_specialization`,
`Formula.conditions`.

### Pros / cons

- **Pros**: mechanical, low risk, very symmetric to the existing Arc work.
- **Cons**: low value. The last `Arc<Phi>` step only saved `~0.24 GB`; these
  candidates are individually smaller. Likely buys `~0.3-0.5 GB` collectively
  at best, doesn't address the real blocker, and adds boilerplate.

## Recommendation

**A**, starting narrow with the join-diff harness above. If A turns out to be
a deep rabbit hole, B is the natural fallback so we at least measure the Arc
work at scale before moving on. C is only worth doing as filler if both A and
B stall.

### Status

First investigative pass on (A) is documented in
[`CONVERGENCE_8D4V_FINDINGS.md`](./CONVERGENCE_8D4V_FINDINGS.md). Summary:

- The `8` disjuncts decompose cleanly as `2 (pre-side) × 4 (post-side
  tier)`. Both splits look like behaviors OCaml's strict-isograph `leq` and
  shared `pulse_widen_threshold = 3` would also produce on the same
  `wp_block.sil` capture.
- A real but secondary canonicalization gap exists in Rust: `PRE#0` and
  `PRE#2` differ only by abstract-value renaming on the pre subgraph but
  fail to canonicalize identically. That is a missed dedup opportunity, not
  the explanation for the `4`-tier post-side growth.
- Concrete next step before deciding whether to keep digging on (A): re-run
  OCaml with `--debug` on the same `wp_block.sil` capture and count the
  retained pre/post disjuncts at `whirlpool_block` node `31`. If OCaml also
  retains `~8` comparable disjuncts, the `8d:4v` framing was wrong and the
  next track to pursue is (B) instead.
