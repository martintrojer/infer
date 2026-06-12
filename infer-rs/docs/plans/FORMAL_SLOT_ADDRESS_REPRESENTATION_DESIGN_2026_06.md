# Formal Slot-Address Representation — Design (aliasing.c parity)

Status: **DESIGN-ONLY**. No code change landed. High regression risk; see
"Decision" at the end.

## Problem

`aliasing.c` sits at `4/2` in the expanded parity sweep (docs/STATUS.md). The
two remaining diffs are **summary-shape only** — reported NPE issues already
match OCaml (`FP_local_addr_noalias_ok`, `local_addr_noalias_bad`,
`global_addr_alias_bad` all report at parity per `issues.exp`). The divergence
is in the *shape of the exported pre/post*, specifically how the stack slot of a
formal pointer is modelled.

### Root cause

OCaml and Rust model a formal differently at the stack/heap boundary.

OCaml (`PulseAbductiveDomain.mk_initial`):
- Each formal `p` gets a fresh address `vp` bound in BOTH pre.stack and
  post.stack: `&p = vp`. `vp` is the address of the *stack slot* of `p`.
- The slot's *contents* (the pointer value the caller passed) is only
  materialized lazily, on first deref, as a heap edge `vp --*--> v1`.
- So the canonical shape is two levels: `&p = vp`, `vp -*-> v_value`.

Rust (`AbductiveDomain::mk_initial`, abductive.rs:343):
- Each formal `p` gets a fresh address `addr` bound in pre.stack and post.stack
  and registered in both heaps.
- `eval_var` returns that slot address directly; the first deref adds
  `addr -*-> target` via `read_heap`. This matches OCaml's two-level shape for
  the *common* path.

The mismatch shows up for `&x`-style **address-of-local** comparisons. In
`local_addr_noalias`, the frontend compares `&x` (address of a local `int x`)
against formal `p`. OCaml models `&x` as the address of `x`'s stack slot and
attaches an `AddressOfStackVariable(x, loc)` attribute to it; the
disequality/equality `&x == p` then constrains two *slot addresses*. Rust's
`eval_var` for the local `x` produces a value address but never attaches
`AddressOfStackVariable`, so the exported summary's attribute set and the
slot-vs-value identity of the compared term differ from OCaml.

## Where the two implementations diverge (file map)

- `crates/pulse/src/abductive.rs`
  - `mk_initial` (343): binds formal slot addresses; does NOT differentiate the
    formal's slot address from its loaded value beyond the lazy deref edge.
  - `eval_var_with_history` (446): returns the slot address; for globals it also
    seeds a pre.stack `g=g` binding. Locals get no `AddressOfStackVariable`.
  - `is_stack_allocated` (1041): scans post.stack for `ProgramVar` slots whose
    repr equals `addr`. This is the only "slot address" notion currently used,
    and it is *structural* (derived from the stack map) rather than *attribute
    backed*.
  - `remove_vars` (477): drops post-only stack bindings; never marks
    `AddressOfStackVariable` (unlike OCaml's `remove_vars`).
- `crates/pulse/src/attribute.rs`
  - `AddressOfStackVariable(Var, Location)` (95): the variant EXISTS but is never
    constructed anywhere in the Rust tree (confirmed: only decl + rank match).
- `crates/pulse/src/operations.rs`
  - `eval_with_history_mode` (153, `Exp::Lvar`): evaluates a program var to its
    slot address with no stack-address attribute.
- `crates/pulse/src/transfer.rs`
  - `ExitScope` (80): calls `remove_vars`; no `AddressOfStackVariable` /
    `GoneOutOfScope` marking.
- OCaml reference:
  - `PulseOperations.ml:893 mark_address_of_stack_variable` (UNSAT on two vars
    sharing a slot address — this is the formal noalias mechanism).
  - `PulseOperations.ml:931 remove_vars` marks `AddressOfStackVariable` for
    source-level locals on scope exit.
  - `PulseAbductiveDomain.ml:935 restore_formals_for_summary` (already ported in
    summary.rs:495).

## What "formal slot-address representation" must provide

To match OCaml's summary shape we need a first-class notion that an abstract
value IS the address of a named stack slot, carried as an attribute, so that:

1. `mark_address_of_stack_variable`-style UNSAT fires when two distinct
   source-level locals/formals would collapse to the same slot address (this is
   exactly the `local_addr_noalias` disequality reasoning).
2. Scope exit (`ExitScope`/`remove_vars`) attaches `AddressOfStackVariable` to
   the dead local's slot so later use is flagged `GoneOutOfScope`
   (`invalidate_locals` parity — currently unported).
3. Summary export carries the `AddressOfStackVariable` attr on the slot address,
   matching OCaml's exported attribute set for these procedures.

## Proposed representation

Keep the existing `addr` (abstract value) as the slot address — do NOT introduce
a new IR-level "slot" type. Instead make the *slot identity* explicit via the
already-present `AddressOfStackVariable` attribute, mirroring OCaml exactly.
This is the lowest-risk representation because it changes only the attribute
layer, not the heap/stack value model.

Concretely:

- Slot address of a stack variable `v` = the abstract value bound to `v` in
  `post.stack` (unchanged).
- Attribute `AddressOfStackVariable(v, loc)` is attached to that abstract value
  when the address is *taken* (`&v`) or when `v` goes out of scope, exactly as
  OCaml does. The attribute is the canonical marker; `is_stack_allocated`'s
  structural scan becomes a fallback, not the source of truth.

### Helper to add (abductive.rs)

```rust
/// Mirror OCaml PulseOperations.mark_address_of_stack_variable.
/// Returns Unsat if `addr` is already the slot address of a *different*
/// source-level variable (two distinct stack vars cannot share an address).
fn mark_address_of_stack_variable(
    &mut self,
    var: &Var,
    loc: &Location,
    addr: AbstractValue,
) -> SatUnsat<()> {
    let repr = self.path_condition.get_var_repr(addr);
    match self.post.attrs.get_address_of_stack_variable(repr) {
        None => {
            self.post.attrs.add_one(repr,
                Attribute::AddressOfStackVariable(var.clone(), loc.clone()));
            SatUnsat::Sat(())
        }
        Some((existing_var, _)) if existing_var == var => SatUnsat::Sat(()),
        Some(_) => SatUnsat::Unsat, // distinct vars, same slot => infeasible
    }
}
```

### Call sites (parity with OCaml)

1. `remove_vars` (abductive.rs:477) / `ExitScope` (transfer.rs:80): for each
   removed var that `appears_in_source_code && is_local`, call
   `mark_address_of_stack_variable` BEFORE dropping the stack binding. This is
   the port of `PulseOperations.remove_vars` (PulseOperations.ml:931).

2. Address-of-local evaluation: wherever the frontend produces `&x` for a local
   (today this flows through `eval_var`/`Exp::Lvar` in `eval_with_history_mode`),
   the produced slot address should be markable. In C/Rust SIL the address-of a
   local is just its `Lvar` slot value, so no new eval path is needed — the
   marking on scope-exit (#1) plus the UNSAT check is what produces the noalias
   reasoning, matching OCaml which only marks at `remove_vars`.

3. `invalidate_locals` parity: after marking, a follow-up pass turns
   `AddressOfStackVariable` into `Invalid(GoneOutOfScope ...)` for locals leaving
   scope (PulseAbductiveDomain.ml:993 `invalidate_locals`). This is a separate,
   independently-shippable port and is the *real* behavioral payoff.

## Required supporting API (base_attrs)

`base_attrs` must expose a ranked getter, mirroring
`PulseAttribute.get_address_of_stack_variable` (get_by_rank):

```rust
impl BaseAddressAttributes {
    pub fn get_address_of_stack_variable(
        &self, addr: AbstractValue,
    ) -> Option<(&Var, &Location)> { /* find the AddressOfStackVariable attr */ }
}
```

`add_out_of_scope_attribute` parity (for step 3):

```rust
// Attribute::Invalid(Invalidation::GoneOutOfScope(pvar, typ), history)
```

`Invalidation::GoneOutOfScope` must exist (check invalidation.rs before
implementing — if absent this is extra scope and bumps risk).

## Prototype / staging plan (smallest-first, each independently shippable)

P0 (no behavior change, scaffolding):
- Add `get_address_of_stack_variable` getter to base_attrs + unit test.
- Add `mark_address_of_stack_variable` to abductive.rs + unit test for the
  UNSAT-on-collision case. NOT wired into transfer yet.

P1 (summary-shape parity, the aliasing.c target):
- Wire `mark_address_of_stack_variable` into `remove_vars`/`ExitScope` for
  source-level locals.
- Re-export summaries and diff against OCaml for `aliasing.c`. Expect the two
  summary-shape diffs to close (`4/2 -> 6/0`) IF reported issues stay stable.

P2 (behavioral, optional, larger): port `invalidate_locals` +
`GoneOutOfScope` to flag use-after-scope on locals. This is the genuine
new-capability step; gate it behind its own parity sweep across dangling_deref.c
and the broader C suite because it can introduce new reports.

## Verification plan

Each step must be verified with the store-textual C Pulse sweep and the
expanded parity diff (docs/STATUS.md methodology). Specifically:

- `make -C infer-rs check` (fmt + clippy + unit tests).
- Build OCaml infer (`./build-infer.sh`) to get `INFER_BIN`, then run the
  C-suite OCaml↔Rust summary diff for at minimum: `aliasing.c`,
  `dangling_deref.c`, `interprocedural.c`, `memory_leak.c`. P2 additionally
  requires a full-suite re-sweep because `GoneOutOfScope` can add reports.

NOTE: at design time there is **no infer binary** in this workspace
(`INFER_BIN` empty, no `bin/infer`), so a live cross-tool summary diff could not
be run. Any patch MUST be re-verified once a built infer is available.

## Risk assessment

- P0: negligible (dead helpers + getter, unit-tested).
- P1: MODERATE. `remove_vars` is on the hot ExitScope path and runs on every
  scope exit. The UNSAT-on-collision check can prune disjuncts that currently
  survive. Risk: over-pruning if Rust's stack-slot canonicalization ever maps
  two distinct locals to one repr for benign reasons (it should not, but the
  formula layer's `get_var_repr` interactions are subtle). Must diff the full C
  suite, not just aliasing.c.
- P2: HIGH. New `GoneOutOfScope` invalidations are genuinely new reports; high
  chance of new FPs/FNs across the suite. Separate task.

## Decision

**Close this task design-only.** The remaining `aliasing.c` diffs are
summary-shape only with reported-issue parity already achieved, so the value of
P1 is cosmetic-parity, and it cannot be verified here (no infer binary). The
concrete, low-risk path forward is the staged P0→P1→P2 plan above. Recommend:

1. File P0 as a standalone scaffolding task (safe to land now).
2. Gate P1 behind a full-suite OCaml↔Rust summary diff once `INFER_BIN` exists.
3. Treat P2 (`invalidate_locals`/`GoneOutOfScope`) as a separate behavioral
   feature with its own parity budget — it is the only step that adds real
   detection capability and carries the real regression risk.

The downstream task `decide_next_architectural_track_after_rebaseline` should
weigh P2's behavioral payoff against other tracks; pure summary-shape parity
(P1) is low priority relative to capability work.
