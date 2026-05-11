# `whirlpool_block` `8d:4v` retained-state findings

First pass at option A from `CONVERGENCE_NEXT_STEPS.md`: reproducing the
8-disjunct retained state at `whirlpool_block` node `31 PRE` and inspecting
what makes those 8 disjuncts distinct, plus first guesses about which OCaml
mechanism might collapse them where Rust does not.

Bench source: `~/infer-rs-bench/wpblock-20260430-181642/openssl-1.0.2d/textual-out-wp/wp_block.sil`,
filtered single-procedure run with the post-Arc<Phi> + outer-Arc baselines in
place.

## Decomposition: `8 disjuncts = 2 (pre) × 4 (post tier)`

The retained PRE alpha summary at node `31`:

```
#0:continue pre[s=3 h=21 a=30] post[s=450 h=584  a=338] formula=1061
#1:continue pre[s=3 h=37 a=46] post[s=458 h=601  a=349] formula=1058
#2:continue pre[s=3 h=21 a=30] post[s=451 h=842  a=468] formula=1956
#3:continue pre[s=3 h=37 a=46] post[s=459 h=859  a=479] formula=1953
#4:continue pre[s=3 h=21 a=30] post[s=451 h=1100 a=597] formula=2852
#5:continue pre[s=3 h=37 a=46] post[s=459 h=1117 a=608] formula=2849
#6:continue pre[s=3 h=21 a=30] post[s=451 h=1358 a=726] formula=3748
#7:continue pre[s=3 h=37 a=46] post[s=459 h=1375 a=737] formula=3745
```

Two independent splits combine multiplicatively:

- **Pre-side split (2-way)**: even-indexed disjuncts have `pre[h=21 a=30]`,
  odd-indexed have `pre[h=37 a=46]`. The delta is exactly `+16` heap nodes
  (`8` array cells + `8` derefs) and `+16` precondition attrs
  (`8 × {MustBeInitialized, MustBeValid}`).
- **Post-side tier split (4-way)**: each tier adds exactly `+258` heap
  nodes (`+129` array edges + `+129` deref edges), `+129` post attrs, and
  `~+896` formula items, all concentrated in the global `Cx` table subtree.

`2 × 4 = 8`.

## Pre-side split traces back to two distinct loop-body paths

Inspecting the per-disjunct precondition `MustBeInitialized` /
`MustBeValid` attrs:

- `PRE#0`-style (smaller pre): preconditions are anchored at source lines
  `[478, 517-525]`. These correspond to wp_block.sil basic blocks
  (`#node_22`-`#node_15`, etc.) that read only `H[i]` and write
  `K[i]` / `S[i] ^= K[i]`.
- `PRE#1`-style (larger pre): preconditions are anchored at source lines
  `[478, 530-537]`. These correspond to a second cluster of basic blocks
  (`#node_12`, `#node_10`, `#node_7`, `#node_6`, ...) that read `H[i]`,
  write `K[i]`, **and** additionally load `pa[i]` (an extra `*int` deref),
  then xor `S[i] ^= K[i] ^ pa[i]`.

The `+16` nodes / `+16` attrs in `PRE#1` are exactly that extra `pa[i]`
chain: `8` `array:void` reads from `pa` plus the `8` derefs they imply.

These are genuinely two different abductive inferences (one path didn't
deref `pa`, the other did), so OCaml's `PulseAbductiveDomain.leq`
(graph-isomorphism + `Formula.equal`) would also keep them separate.

## Post-side tier split is per-iteration global-table growth

For a fixed pre, the four tiers are clean stride-`+258` heap-node /
`+129`-attr / `~+896`-formula increments, all in the global `Cx` table
subtree, not in local `K` / `S` / `H`. With `pulse_widen_threshold=3`
(the default we share with OCaml), four tiers is exactly the
post-widen-cap shape: tier `0` from before any loop body, tier `1`-`3`
from the three widening iterations OCaml allows before stopping.

## Smoking gun in our canonicalizer

`PRE#0` and `PRE#2` share the same alpha summary
(`pre[s=3, h=21, a=30]`, same value-count shape) and traverse the same
`H -> .anonymous_union -> array[i] -> deref` subgraph. But our canonical
PRE dumps for them are **not** byte-identical — they differ purely in
which abstract-value labels were assigned to which structural roles:

```
PRE#0 pre_heap (only-in-#0):
  u354:field:...anonymous_union_..._478_5.q -> u417
  u372:field:WHIRLPOOL_CTX.H            -> u418
  u417:array:void:u342                  -> u548
  ...

PRE#2 pre_heap (only-in-#2):
  u354:field:...anonymous_union_..._478_5.q -> u418
  u372:field:WHIRLPOOL_CTX.H            -> u419
  u418:array:void:u342                  -> u548
  ...
```

That is, `u417 ↔ u418` and `u418 ↔ u419` between the two states for the
same structural roles. `state_cmp::canonicalize` is supposed to alpha-rename
both states into a common form so that this kind of pure renaming
disappears, but here it does not. Existing alpha-equivalence unit tests
(`test_alpha_equivalent_states_*`) cover simple renamings only; whatever
PRE#0 vs PRE#2 hits in this larger graph is escaping that coverage.

Note also: the same `u548` carries different `MustBeInitialized` /
`MustBeValid` traces in `PRE#0` (line `518`) vs `PRE#2` (line `519`).
That is structural, not just a label issue: the two preconditions arrived
via different source lines, and Pulse's `Trace.equal` is structural so
those two `MustBeInitialized` attrs are genuinely unequal even before any
canonicalization.

## What this rules in / out

- **`leq` is not too strict in some Rust-only sense.** OCaml's
  `PulseAbductiveDomain.leq` is `Formula.equal` on the path condition plus
  `GraphComparison.isograph` on both pre and post, which is also a strict
  "exact subgraph isomorphism" up to abstract-value renaming. Both rule out
  collapsing the 4 tiers (different node counts) and rule out collapsing
  block-A pre vs block-B pre (different precondition graphs).
- **Loop-head widening is not bypassed.** With
  `pulse_widen_threshold = 3`, four tiers is exactly the post-cap shape we
  expect from one initial state plus three widening iterations.
- **Recency limit is not the culprit.** Forcing `pulse-recency-limit = 32`
  to match OCaml's default left this slice essentially identical (the active
  follow-up is tracked in `mu`, not in a checked-in TODO file).
- **There is a real partial canonicalization gap** in Rust:
  `PRE#0`/`PRE#2` should canonicalize to the same form on the pre subgraph
  modulo the `u417 ↔ u418` rename, and currently they do not. That is at
  least a missed dedup opportunity, but it is not by itself the explanation
  for the `4`-tier post-side growth.

## Open question this points at

The `2`-way pre split and the `4`-way post-tier split both look like
behaviors OCaml would reproduce on the same `wp_block.sil` capture. If
that's true, the remaining "OCaml is fast on this slice, we are slow" gap
isn't really `8d:4v` retained at this node — it would be per-state
representation cost (largely already addressed by Phase 1 Arc work) and/or
some other procedure / code path entirely.

Concrete next step before deciding whether to keep digging in this
direction:

- Re-run **OCaml** with `--debug` on the same `wp_block.sil` capture and
  inspect the corresponding HTML for `whirlpool_block` node `31`. Count
  the retained pre/post disjuncts and their per-state `h=` / `a=` /
  `formula=` shape.
- If OCaml shows `~8` disjuncts of comparable size, "convergence" was the
  wrong frame and we should pivot to (B) "validate Arc savings on
  whole-program OpenSSL."
- If OCaml shows `<= 2` disjuncts or much smaller per-state shape, then
  there is a real OCaml-only convergence mechanism (likely in how it joins
  / abstracts global-table reads) we need to identify.

## Update: OCaml comparison data

Ran `infer analyze --pulse-only --debug --procedures-filter whirlpool_block`
on the same shared capture
(`infer-out-wp/captured/wp_block.c.45acfcec405ead7e`):

| metric                        | OCaml `--debug`         | Rust post-Arc filtered probe |
|-------------------------------|--------------------------|------------------------------|
| wall time                     | `~120s`                  | `~273s` (`~2.3×` slower)     |
| max resident set size         | `~10.05 GB`              | `~3.93 GB` (`~2.6×` smaller) |
| peak memory footprint         | `~14.21 GB`              | `~3.87 GB`                   |
| retained disjuncts at node 31 | up to **`10`** per visit | `8` per visit                |
| per-disjunct unique values    | `~487` (final 10-disj)   | `~1500-3000` per disjunct    |

Key observations from the OCaml HTML:

- OCaml retains **more** disjuncts at node `31` than we do (`10` vs `8`),
  growing in the same `+2`-per-visit pattern. So our `8d:4v` shape is in
  fact *better* converged than OCaml on this slice.
- OCaml's per-disjunct representation is `3-6×` denser in unique abstract
  values than ours (`~487` vs `~1500-3000`). It reuses the same value
  across chained field accesses where we tend to mint a fresh value at
  each step.
- After the Phase 1 Arc work, our peak memory is now `~2.6×` *smaller*
  than OCaml's on this slice, even though OCaml has fewer per-state
  values.

## Reframe

The original "`8d:4v` convergence gap" framing was wrong on the data.
OCaml does not converge tighter than we do on this slice; if anything we
have fewer disjuncts. Memory after Phase 1 Arc work is well below OCaml
here. The remaining `~2.3×` wall-time gap is therefore a per-disjunct
*CPU* cost (each of our disjuncts has more unique values to manipulate),
not a retained-state count problem.

### Update: per-disjunct value count is *bounded* in OCaml, *unbounded* in Rust

Follow-up extraction from OCaml's `--debug` HTML for `whirlpool_block`
node `31` (the converged `10`-disjunct block):

| disjunct | OCaml unique values | Rust unique values |
|----------|---------------------|--------------------|
| #0 / #1 (initial)  | `67`, `81`        | `1228` (PRE#0)    |
| #2-#9 (per tier)   | `478-502` (stable) | `1500-3914` (linear growth) |

Key observation: **OCaml's per-disjunct unique-value count is bounded
at ~`500`** even as the loop iterates and disjunct count grows from
`#2` to `#9`. **Rust's per-disjunct count grows linearly** with tier
(`1228` at PRE#0, climbing to `3914` at PRE#7). The +`~258` heap nodes
+ `~129` attrs + `~896` formula items per tier we previously
documented map directly to fresh abstract values that OCaml apparently
shares across iterations and we don't.

This is the actual mechanism behind the per-disjunct cost gap. The
gap is not constant 3-6×; it grows with iteration count, capped on
OCaml's side by some sharing/abstraction we have yet to identify.

### Investigative attempts so far

- Added OCaml-style `find_edge_opt` canonical fallback for
  `ArrayAccess` (commit `a7a3bd61ef`). The on-this-slice array indices
  are constants (`q[0]`..`q[7]`) already canonicalized at the
  `operations.rs` call site via `canonicalize_for_access` /
  `const_cache`, so the new fallback rarely fires. No perf change. Kept
  as a fidelity improvement for non-constant-index workloads.
- **Found the dominant cause and landed a fix (commit `1e2bf5cb9d`):**
  Drop dead `Var::LogicalVar(_)` post-stack bindings at Pulse node
  exits, mirroring the effect of OCaml's `Metadata (ExitScope ids)`
  cleanup that the textual exporter strips (`grep` shows the OpenSSL
  textual SIL has zero `__sil_metadata_exit_scope` markers and zero
  `__return` pvar stores). Implemented via backward liveness +
  per-node cleanup, preserving the candidate return-value Ident so
  `summary::find_return_value`'s fallback heuristic still works.
  Measured impact on the filtered single-procedure `whirlpool_block`
  slice:

  | step                                       | wall  | max RSS    |
  |--------------------------------------------|-------|------------|
  | baseline (no Arc)                          | 4m34s | ~16.7 GB   |
  | + Phase 1 Arc (5 increments)               | 4m33s | ~3.93 GB   |
  | + drop dead logical-vars (`1e2bf5cb9d`)    | 4m18s | **~0.77 GB** |

  That is `~80%` peak-memory reduction beyond Phase 1 Arc and `~95%`
  vs the pre-Arc baseline, with a small wall-time win. We now sit
  `~13×` below OCaml's `~10 GB` peak on the same slice (OCaml runs
  faster: `~120s`).

### What the drop pass does NOT change

Follow-up dump on `whirlpool_block` node `31` after the drop pass
landed:

| disjunct | before drop                          | after drop                       |
|----------|--------------------------------------|----------------------------------|
| #0       | `pre[s=3 h=21 a=30] post[s=450 h=584 a=338]` 1228 vals | `pre[s=3 h=21 a=30] post[s=17 h=584 a=338]` 1228 vals |
| #7       | `pre[s=3 h=37 a=46] post[s=458 h=1375 a=737]` 3914 vals | `pre[s=3 h=37 a=46] post[s=18 h=1375 a=736]` 3914 vals |

Key points:

- Post stack went from `450` to `17` entries per disjunct (`~26×`
  reduction). That is where the `5×` peak-memory win came from.
- Heap node count, attrs count, and per-disjunct unique-value count
  are **unchanged**. The values previously bound to logical-temp
  stack slots are still referenced by heap edges and the formula, so
  dropping the stack binding doesn't lose them.
- OCaml's per-disjunct unique-value cap of `~500` is therefore *not*
  reached by the drop pass. Whatever OCaml does to bound per-disjunct
  value count must additionally GC values that have lost all
  stack-rooted references and become formula-only / heap-only
  garbage.

### What the drop pass changes at scale (74-file partial OpenSSL)

Mixed:

- We now reach procedures that previously OOM'd before completion
  (`fcrypt_body` finishes at `1m25s` / `830 MB`).
- Per-procedure peak savings on small procs are marginal
  (`private_AES_set_encrypt_key 252MB → 250MB`, `AES_encrypt 308MB →
  285MB`, `AES_decrypt 378MB → 362MB`).
- `DES_ede3_cfb_encrypt` is *worse* on wall time after the drop
  pass (`>32 min` and counting vs `~17 min` before, where it was
  killed by the OOM rather than completing). Its retained shape after
  drop has fewer disjuncts (`disj=2175` vs `4405`) but more total
  retained heap (`hn=4.7M` vs `168k`) and more total formula
  (`lin=1.16M` vs `214k`), suggesting the dropped temp bindings
  freed up something that lets each per-disjunct heap grow
  unboundedly across iterations.

Net: the drop pass is a real per-procedure peak win on big procs
like `whirlpool_block`, but it is not a wall-time win on the worst
procs and may even be a wall-time regression there. The remaining
gap is firmly per-disjunct CPU cost on encryption-style outliers.

### Phi work landed: reverse `term_value_index`

Follow-on commits address per-disjunct CPU cost directly:

- `c5782b297e` canonicalize the `BinOp` result through `const_cache`
  after `and_equal_binop` (mirrors OCaml's
  `incorporate_new_eqs_on_val`). Pure constant arithmetic in array
  indices like `__sil_plusa_int(__sil_mult_int(3, 8), 1)` collapses
  through `const_cache` to a shared value per constant. Wall-time
  `~13%` win, no per-disjunct value-count change.
- `7302d1a0de` add a reverse `term_value_index: BTreeMap<TermKey,
  AbstractValue>` to `Phi`, mirroring OCaml `PulseFormulaPhi.term_eqs`'s
  term-to-value direction. A repeated `xor(v37, v31)` in the same
  disjunct now equates the freshly minted `v` with the cached
  representative instead of running the full per-op formula update.
  Per-tier deltas at node 31:

  | metric           | before this commit | after this commit |
  |------------------|--------------------|-------------------|
  | post heap nodes  | `+258` / tier      | **`+2`** / tier   |
  | formula entries  | `+896` / tier      | `+638` / tier     |
  | unique values    | `+893` / tier      | `+637` / tier     |

The `+258 → +2` collapse on per-tier heap-node growth is direct
evidence the index is firing.

Final cumulative single-procedure measurement on `whirlpool_block`:

  | step                                          | wall  | max RSS    |
  |-----------------------------------------------|-------|------------|
  | baseline (no Arc)                             | 4m34s | ~16.7 GB   |
  | + Phase 1 Arc (5 increments)                  | 4m33s | ~3.93 GB   |
  | + drop dead logical-vars                      | 4m18s | ~0.77 GB   |
  | + canonicalize BinOp via `const_cache`        | 3m45s | ~0.77 GB   |
  | + reverse `term_value_index`                  | **3m22s** | **~0.51 GB** |

That is `~26%` wall-time reduction and `~97%` peak-memory reduction
vs the pre-Arc baseline. We are now `~20×` below OCaml's `~10 GB`
peak on this slice (OCaml runs `~120s`, we are `~3m22s`).

Multi-procedure (74-file) impact is smaller and more uneven: per-proc
baseline peak drops `~10-12%` on small procs
(`private_AES_set_encrypt_key 252MB → 222MB`,
`AES_encrypt 308MB → 273MB`, `AES_decrypt 378MB → 343MB`),
`fcrypt_body` peak rises slightly (`830MB → 918MB`), and
`DES_ede3_cfb_encrypt` is still the wall-time blocker (still in the
`>17 min` regime, retained-state shape comparable to before).

### Open: per-tier `+637` value-count residue

With the `term_value_index` in, per-tier value growth is `+637` per
loop iteration instead of `+893`. OCaml's per-disjunct count is
stable at `~487`, so `+0` per tier. Remaining gap candidates worth
investigating later:

- Stale-key misses in the index: every `subst_var` makes some keys
  stale. We currently mint a fresh value on a miss instead of
  re-keying the index.
- Per-iteration formula GC: a value whose only reference was a heap
  edge that just got overwritten loses all roots. Its `linear_eqs` /
  `intervals` / `is_int_vars` entries persist until summary-time
  `Formula.simplify`. OCaml seems to keep similar entries during
  analysis (the `--debug` HTML shows a tiny `Raw state` formula
  block that is hard to reconcile with this hypothesis), but verifying
  this requires a closer mid-analysis OCaml dump.
- Integer-typed-mark redundancy: `is_int=1152` per disjunct is one of
  the dominant counts. We mark every BinOp result `mark_is_int(v)`
  even when the lookup-cache already has the same `v`. Could be
  hoisted into the `find_term_value` short-circuit.

### Open: where does OCaml's per-iteration value sharing come from

Not yet identified. Candidates worth investigating:

- OCaml may discard / GC unreachable values from the heap+attrs+formula
  on every loop iteration (we only do this at summary time via
  `discard_unreachable_`).
- OCaml's `MakeDisjunctiveTransferFunctions` may run a `canonicalize`
  pass on each loop-head visit (Rust does not).
- OCaml may collapse compiler-temp `n0..n9` stack bindings between
  instructions, while Rust retains every temp `n$N` slot in the post
  stack for the lifetime of the procedure.
- OCaml's `Formula.simplify` may run more eagerly in mid-analysis
  (we run it only in `simplify_for_summary`).

Any one of these alone, applied between loop iterations, would cap the
per-disjunct value count the way OCaml's `~500` cap suggests.

Consequences for the (A) / (B) / (C) plan in
`CONVERGENCE_NEXT_STEPS.md`:

- (A) as originally framed ("diagnose the `8d:4v` convergence gap") is
  closed by this data. We can revisit it as "reduce per-disjunct unique-
  value count to match OCaml's denser representation," but that is a
  semantic-fidelity change in how Pulse mints abstract values during
  field/array access, not a retained-state convergence change.
- (B) ("validate Arc savings on whole-program OpenSSL") became the right next
  step and produced the later whole-program/profile work summarized in
  `STATUS.md` and `WHOLE_PROGRAM_OPENSSL_FINDINGS.md`.

2026-05 update: this archive is not the active perf plan. Subsequent work moved
the live bottleneck to `state_cmp::canonicalize` and landed `sort_by_cached_key`,
structural canonical keys for formula/heap/attrs/stack/dynamic types, cached
propagation sort keys, and flat-slab `CanonTerm`. Current live tasks are in
`mu` (not this archive), with the clean full-corpus remeasure parked on host
quiescence. Future findings should prefer mu task notes plus `STATUS.md`
dashboard updates over appending more live backlog to this archive.
