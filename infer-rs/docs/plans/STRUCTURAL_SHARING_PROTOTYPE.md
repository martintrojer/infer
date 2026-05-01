# Structural Sharing Prototype

## Goal

Reduce physical copy cost in Pulse state snapshots without changing analysis
semantics.

This is a performance / memory track, not the current primary correctness fix
for the remaining `whirlpool_block` convergence gap.

## Current Evidence

- Pulse state is still deeply owned today:
  - `BaseStack` owns `HashMap<Var, ValueWithHistory>`
  - `BaseMemory` owns `BTreeMap<AbstractValue, Edges>`
  - `BaseAddressAttributes` owns `BTreeMap<AbstractValue, Attributes>`
  - `Formula` / `Phi` own multiple maps plus union-find tables
- Hot clone sites still exist in the fixpoint engine:
  - retained post snapshot cloning in `PulseTransferFunctions::exec_node(...)`
  - disjunctive `join` / `widen` cloning in `absint::DisjunctiveDomain`
- Latest `whirlpool_block` alpha-signature runs show the remaining hot
  disjuncts are semantically distinct (`4` growth tiers x `2` variants), so
  clone reduction is not expected to collapse the current `8d` node shape by
  itself.
- The biggest hotspot reductions so far came from OCaml-parity fixes
  (`equal_fast` split and WTO revisit `exec_node(...)` parity), not from a
  representation change. Treat structural sharing as a physical-RSS fix for
  the bytes occupied by retained states, not as a substitute for the remaining
  `whirlpool_block` convergence work.
- Whole-program merged direct-`.sil` runs still show memory growth /
  abnormal termination, so reducing physical duplication is still a realistic
  systems-level lever.
- The newer filtered OpenSSL repro now retains the implicit
  `__infer_globals_initializer_Cx` dependency too. In the default
  OCaml-compatible configuration Rust now skips that proc via
  `pulse-max-cfg-size = 15000`, which keeps the standard single-file slice on
  the familiar `1222`-state `whirlpool_block` checkpoint.
- The forced retained-initializer repro is still useful as an upper bound:
  if `__infer_globals_initializer_Cx` is actually analyzed, it materializes a
  very large single-disjunct heap and pushes `whirlpool_block` into
  multi-million retained heap/edge totals while `max_node_disjuncts` is still
  only `4`. That makes structural sharing very relevant for the bytes occupied
  by per-state global-table subgraphs once the semantic dependency is modeled
  correctly.

## Non-Goals

- Do not rewrite the fixpoint engine around long-lived borrows / lifetimes.
  The interpreter, invariant map, and summaries all store owned snapshots.
- Do not wrap the whole `AbductiveDomain` in `Arc<_>` and call it done.
  That only makes full-state clones cheaper on paper; it does not reduce copy
  cost once transfer mutates large owned substructures.
- Do not change semantic dedup / `leq` behavior in this prototype.

## Recommended Shape

Use **component-level persistent data structures**, not borrow-heavy APIs.

### Phase 0: Measurement First

Before changing representations, measure where the copy pressure actually is.

- Add focused counters around:
  - `PulseTransferFunctions::exec_node(...)`
  - `DisjunctiveDomain::{join,widen}`
  - summary application / returned-state cloning
- Record per-state size stats already available in Pulse together with:
  - clone call counts
  - retained post heap / attr / formula sizes
  - whole-proc elapsed time on narrowed `whirlpool_block`
- Success criterion for moving ahead:
  clone-heavy sites must line up with the big retained-state components rather
  than just shallow wrapper copies.

### Phase 1: Persistent Heap / Attrs Prototype

Prototype structural sharing in the components with the clearest value/risk
ratio first.

Recommended first targets:

1. `BaseMemory.graph`
2. `BaseAddressAttributes.map`

Suggested shape:

- Replace outer `BTreeMap` storage with a persistent ordered map, e.g.
  `im::OrdMap`.
- Keep the public Pulse APIs mostly unchanged (`&mut self` mutation surface is
  fine); rely on path-copying under the hood.
- Start with only the outer maps persistent.
- If outer-map sharing is not enough, make `Edges.values` persistent too.

Why this first:

- `BaseMemory` and attrs dominate retained-state bulk on the OpenSSL hotspot.
- Their APIs are simpler than formula's union-find / normalization machinery.
- This keeps the prototype scoped to snapshot/storage cost without entangling
  solver semantics.

### Initial baby steps landed

As low-risk first moves:

- `BaseMemory.graph` is now
  `Arc<BTreeMap<AbstractValue, Arc<Edges>>>` (the outer map is itself
  reference-counted with `Arc::make_mut`, and each address still points to
  its own per-address `Arc<Edges>`).
- `BaseAddressAttributes.map` is similarly
  `Arc<BTreeMap<AbstractValue, Arc<Attributes>>>`.
- `BaseStack.map` is `Arc<HashMap<Var, ValueWithHistory>>`, sharing the
  whole stack map (whole-map rather than per-entry, since each stack entry is
  small).
- `Formula.phi` is `Arc<Phi>` with a `phi_mut` helper, sharing the heavy
  phi maps (linear_eqs, term_eqs, atoms, intervals, var_eqs, fn_app_eqs).

Nothing on the read side changed: each container exposes the same public
API as before. Mutating helpers clone-on-write through `Arc::make_mut`, with
cheap no-op pre-checks (`contains_key`, `iter().all(...)`, `values().any(...)`)
on the common paths so reads that detect a no-op mutation never force a
clone-on-write.

Measured impact on the filtered single-file `whirlpool_block` slice for the
same `1222`-state / `8d:4v` checkpoint:

- baseline before any Arc sharing: ~`16.7 GB` peak, `4m34s` real
- after per-address `Arc<Edges>`: ~`13.84 GB` peak
- + per-address `Arc<Attributes>`: ~`9.34 GB` peak
- + `Arc<BaseStack.map>`: ~`5.97 GB` peak, `4m29s` real
- + `Arc<Phi>`: ~`5.73 GB` peak, `4m32s` real
- + outer `Arc<BTreeMap>` for `BaseMemory.graph` and
  `BaseAddressAttributes.map`: ~`3.93 GB` peak, `4m33s` real (~`76%`
  peak-memory drop vs the pre-Arc baseline, no wall-time regression on a
  calm host)

All reruns observe `0 swaps` and the same `1222` retained-state shape, so
these are pure storage / clone-cost wins, not analysis behavior changes.

These are Phase 1 increments, not the full persistent-map switch the rest of
this plan still recommends as a longer-term option.

### Phase 2: Formula Sharing, If Needed

Only after the heap/attrs prototype is measured.

Formula is a plausible cost center too, but it is structurally riskier:

- `Formula.conditions` is easy enough to make persistent
- `Phi` is not:
  - union-find parent/rank tables
  - several correlated maps (`linear_eqs`, `term_eqs`, `intervals`,
    function-app eqs, atoms)
  - aggressive normalization / substitution paths

If Phase 1 helps whole-program RSS but formula still dominates the retained
tiers, the next step should be a **separate** formula-specific design, not a
casual extension of the heap prototype.

### Not Recommended As The First Prototype

- Borrowing `&AbductiveDomain` through transfer and retaining references inside
  invariant maps
- Whole-state `Arc<AbductiveDomain>`
- Arena handles without first proving that state mutation patterns fit
  generational invalidation / compaction constraints

## Initial Prototype Plan

1. Add measurement counters and capture one narrowed `whirlpool_block` run.
2. Swap `BaseMemory.graph` to a persistent ordered map.
3. Re-run:
   - `make check`
   - narrowed `whirlpool_block`
   - shared OpenSSL corpus at `-j 1`
4. If RSS / elapsed improve without semantic regressions, extend the same idea
   to `BaseAddressAttributes`.
5. Re-evaluate before touching formula.

## Success Criteria

- No issue-count or summary-parity regressions on the existing fast checks
- Narrowed `whirlpool_block`:
  - same semantic shape unless a separate correctness fix is found
  - lower wall time and/or lower retained size
- Shared OpenSSL corpus:
  - lower RSS on merged direct-`.sil` runs
  - ideally fewer abnormal terminations at `-j 1` / `-j 4`

## Failure Criteria

Stop if any of the following happen:

- The prototype mostly shifts cost around while leaving RSS unchanged
- Complexity spills into solver / interproc semantics
- The persistent map overhead hurts the small-proc common case more than it
  helps the big-state benchmark cases
- The code becomes harder to OCaml-cross-reference or reason about than the
  performance win justifies
