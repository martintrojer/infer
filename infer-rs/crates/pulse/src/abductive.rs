// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Abductive domain: the main Pulse analysis state.
//!
//! Mirrors OCaml's `PulseAbductiveDomain.ml` (simplified).
//!
//! Wraps `BaseDomain` + `Formula` to provide the safe API for reading/writing
//! the abstract heap, checking validity, and tracking path conditions.
//!
//! The full OCaml version maintains separate pre/post states for biabduction.
//! We simplify to post-state only for now (forward analysis without
//! precondition inference).

use sil::int_lit::IntLit;
use sil::location::Location;
use sil::procdesc::Procdesc;
use sil::pvar::Pvar;
use sil::var::Var;

use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::{Allocator, Attribute};
use crate::base_domain::BaseDomain;
use crate::formula::atom::Atom;
use crate::formula::lin_arith::LinArith;
use crate::formula::{Formula, NewEq, Operand};
use crate::invalidation::Invalidation;
use crate::sat_unsat::SatUnsat;
use crate::value_history::{ValueHistory, ValueWithHistory};

/// The abductive domain: pre-state + post-state + path condition.
///
/// Mirrors OCaml's `PulseAbductiveDomain.t`:
/// - `pre`: the procedure's precondition — what it assumes about inputs.
///   Populated during analysis whenever memory is read (Load).
/// - `post`: the procedure's postcondition — the current abstract state.
/// - `path_condition`: constraints on abstract values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbductiveDomain {
    /// The pre-condition: what the procedure reads from its inputs.
    /// Populated by `abduce_read` when the analysis dereferences memory.
    pub pre: BaseDomain,
    /// The current (post-condition) abstract state.
    pub post: BaseDomain,
    /// Path condition: constraints on abstract values.
    pub path_condition: Formula,
    /// Cache: constant value → canonical abstract value.
    /// Used to canonicalize array indices so that `store &a[0]` and
    /// `load &a[0]` use the same heap edge.
    const_cache: std::collections::BTreeMap<i64, AbstractValue>,
    /// Abstract values that need dynamic type information for specialization.
    /// When `__call_c_function_ptr` can't resolve a function pointer, it
    /// records the pointer's abstract value here. During summary creation,
    /// these are converted to HeapPaths for the specialization request.
    /// Cross-ref: OCaml `PulseAbductiveDomain.need_dynamic_type_specialization`.
    pub need_dynamic_type_specialization: std::collections::HashSet<AbstractValue>,
    /// Addresses the analysis checked validity for (via eval_deref).
    /// Used by interproc to only report pre-condition violations for
    /// addresses the callee actually dereferences, not all pre-edges.
    /// Cross-ref: OCaml MustBeValid attribute in PulseAttribute.ml.
    pub must_be_valid: std::collections::HashSet<AbstractValue>,
}

/// Outcome of applying callee-imported equalities to an abductive state.
///
/// Cross-ref: OCaml `PulseInterproc.conjoin_callee_arith` calls
/// `PulseAbductiveDomain.incorporate_new_eqs`, which distinguishes a plain
/// contradiction from a potential invalid access on imported `EqZero`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportedFormulaEffect {
    Sat,
    PotentialInvalidAccess(AbstractValue),
}

impl AbductiveDomain {
    /// Create the initial state for analyzing a procedure.
    ///
    /// Allocates fresh abstract values for each formal parameter and
    /// binds them in the stack.
    pub fn mk_initial(pdesc: &Procdesc) -> Self {
        let mut state = Self {
            pre: BaseDomain::empty(),
            post: BaseDomain::empty(),
            path_condition: Formula::ttrue(),
            const_cache: std::collections::BTreeMap::new(),
            need_dynamic_type_specialization: std::collections::HashSet::new(),
            must_be_valid: std::collections::HashSet::new(),
        };

        // Bind each formal parameter to a fresh abstract value.
        // Add to both pre and post — the pre records what the procedure
        // assumes about its inputs.
        for (mangled, _typ, _annot) in &pdesc.formals {
            let addr = AbstractValue::mk_fresh();
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar.clone()));
            let value = ValueWithHistory::new(addr, ValueHistory::formal_argument(pvar));
            state
                .post
                .stack
                .add_with_history(var.clone(), value.clone());
            state.post.heap.register_address(addr);
            state.pre.stack.add_with_history(var, value);
            state.pre.heap.register_address(addr);
        }

        state
    }

    /// Look up a variable's abstract address in the stack.
    /// If not found, allocates a fresh address and binds it.
    pub fn eval_var(&mut self, var: &Var) -> AbstractValue {
        self.eval_var_with_history(var).addr
    }

    /// Look up a variable together with its provenance.
    pub fn eval_var_with_history(&mut self, var: &Var) -> ValueWithHistory {
        if let Some(value) = self.post.stack.find_with_history(var) {
            value.clone()
        } else {
            let addr = AbstractValue::mk_fresh();
            let value = ValueWithHistory::new(addr, ValueHistory::epoch());
            self.post.stack.add_with_history(var.clone(), value.clone());
            value
        }
    }

    /// Read through a heap edge: follow `addr --access--> target`.
    /// If no edge exists, creates a fresh target and adds the edge.
    /// Also records the read in the pre-state (biabduction).
    pub fn read_heap(&mut self, addr: AbstractValue, access: Access) -> AbstractValue {
        self.read_heap_with_history(ValueWithHistory::new(addr, ValueHistory::epoch()), access)
            .addr
    }

    /// Read through a heap edge and preserve the target provenance.
    pub fn read_heap_with_history(
        &mut self,
        src: ValueWithHistory,
        access: Access,
    ) -> ValueWithHistory {
        if let Some(target) = self.post.heap.find_edge_with_history(src.addr, &access) {
            return target.clone();
        }

        let target = AbstractValue::mk_fresh();
        let value = ValueWithHistory::new(target, src.history.clone());
        self.post
            .heap
            .add_edge_with_history(src.addr, access.clone(), value.clone());

        // Mirror OCaml's SafeMemory.eval_edge: only abduce reads rooted in the
        // existing pre-state, and never overwrite the original pre-edge on
        // subsequent reads after the post-state has diverged.
        if self.pre.heap.get_edges(src.addr).is_some() {
            self.pre
                .heap
                .add_edge_with_history(src.addr, access, value.clone());
            self.pre.heap.register_address(target);
        }

        value
    }

    /// Write through a heap edge: set `addr --access--> value`.
    pub fn write_heap(&mut self, addr: AbstractValue, access: Access, value: AbstractValue) {
        self.write_heap_with_history(
            addr,
            access,
            ValueWithHistory::new(value, ValueHistory::epoch()),
        );
    }

    /// Write through a heap edge, preserving the target provenance.
    pub fn write_heap_with_history(
        &mut self,
        addr: AbstractValue,
        access: Access,
        value: ValueWithHistory,
    ) {
        self.post.heap.add_edge_with_history(addr, access, value);
    }

    /// Best-effort provenance lookup for a value in the current post-state.
    pub fn history_of_value(&self, addr: AbstractValue) -> Option<ValueHistory> {
        let repr = self.path_condition.get_var_repr(addr);
        let mut history: Option<ValueHistory> = None;

        for (_var, value) in self.post.stack.iter_with_history() {
            if self.path_condition.get_var_repr(value.addr) == repr {
                history = Some(match history {
                    Some(existing) => existing.merge(&value.history),
                    None => value.history.clone(),
                });
            }
        }

        for (_src, edges) in self.post.heap.iter() {
            for (_access, value) in edges.iter_with_history() {
                if self.path_condition.get_var_repr(value.addr) == repr {
                    history = Some(match history {
                        Some(existing) => existing.merge(&value.history),
                        None => value.history.clone(),
                    });
                }
            }
        }

        history
    }

    /// Check if dereferencing an address is valid.
    ///
    /// Returns `Ok(())` if valid, or `Err` with the invalidation reason
    /// if the address is known to be invalid (null, freed, etc.).
    ///
    /// This is THE null-dereference / use-after-free check.
    pub fn check_valid(
        &self,
        addr: AbstractValue,
    ) -> Result<(), Box<(Invalidation, ValueHistory)>> {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.check_valid(repr)
    }

    /// Mark an address as invalid (freed, null, etc.).
    pub fn invalidate(&mut self, addr: AbstractValue, inv: Invalidation, history: ValueHistory) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.invalidate(repr, inv, history);
    }

    /// Mark an address as allocated.
    pub fn allocate(&mut self, addr: AbstractValue, allocator: Allocator, loc: Location) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.allocate(repr, allocator, loc);
    }

    /// Mark an address as initialized.
    pub fn initialize(&mut self, addr: AbstractValue) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.initialize(repr);
    }

    /// Keep an address reachable across summary normalization.
    pub fn always_reachable(&mut self, addr: AbstractValue) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.always_reachable(repr);
    }

    /// Add a generic attribute to an address.
    pub fn add_attr(&mut self, addr: AbstractValue, attr: Attribute) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.add_one(repr, attr);
    }

    fn apply_formula_result(&mut self, result: SatUnsat<Vec<NewEq>>) -> SatUnsat<()> {
        result.and_then(|new_eqs| self.incorporate_new_eqs(new_eqs))
    }

    /// Apply callee-imported formula equalities using OCaml's interproc
    /// `EqZero` behavior instead of persisting a synthetic null invalidation.
    pub fn apply_formula_result_for_summary_import(
        &mut self,
        result: SatUnsat<Vec<NewEq>>,
        imported_must_be_valid: &mut std::collections::HashSet<AbstractValue>,
        stack_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
        heap_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
    ) -> SatUnsat<ImportedFormulaEffect> {
        result.and_then(|new_eqs| {
            self.incorporate_new_eqs_for_summary_import(
                new_eqs,
                imported_must_be_valid,
                stack_allocated_before_call,
                heap_allocated_before_call,
            )
        })
    }

    fn incorporate_new_eqs(&mut self, new_eqs: Vec<NewEq>) -> SatUnsat<()> {
        for new_eq in new_eqs {
            match new_eq {
                NewEq::Equal(old, new) if old == new => {}
                NewEq::Equal(old, new) => self.subst_var(old, new),
                NewEq::EqZero(v) => {
                    let repr = self.path_condition.get_var_repr(v);
                    if self.is_stack_allocated(repr) {
                        return SatUnsat::Unsat;
                    }
                    if self.is_heap_allocated(repr) {
                        self.post.attrs.invalidate(
                            repr,
                            Invalidation::ConstantDereference(IntLit::zero()),
                            ValueHistory::invalidated(
                                Invalidation::ConstantDereference(IntLit::zero()),
                                Location::dummy(),
                            ),
                        );
                    }
                }
            }
        }
        SatUnsat::Sat(())
    }

    fn incorporate_new_eqs_for_summary_import(
        &mut self,
        new_eqs: Vec<NewEq>,
        imported_must_be_valid: &mut std::collections::HashSet<AbstractValue>,
        stack_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
        heap_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
    ) -> SatUnsat<ImportedFormulaEffect> {
        for new_eq in new_eqs {
            match new_eq {
                NewEq::Equal(old, new) if old == new => {}
                NewEq::Equal(old, new) => {
                    self.subst_var(old, new);
                    let updated = std::mem::take(imported_must_be_valid);
                    *imported_must_be_valid = self.subst_value_set(updated, old, new);
                    let updated = std::mem::take(stack_allocated_before_call);
                    *stack_allocated_before_call = self.subst_value_set(updated, old, new);
                    let updated = std::mem::take(heap_allocated_before_call);
                    *heap_allocated_before_call = self.subst_value_set(updated, old, new);
                }
                NewEq::EqZero(v) => {
                    let repr = self.path_condition.get_var_repr(v);
                    if stack_allocated_before_call.contains(&repr) {
                        return SatUnsat::Unsat;
                    }
                    if heap_allocated_before_call.contains(&repr) {
                        if imported_must_be_valid.contains(&repr) {
                            return SatUnsat::Sat(ImportedFormulaEffect::PotentialInvalidAccess(
                                repr,
                            ));
                        }
                        return SatUnsat::Unsat;
                    }
                }
            }
        }
        SatUnsat::Sat(ImportedFormulaEffect::Sat)
    }

    /// Snapshot caller-owned allocated roots before applying a callee post.
    ///
    /// Cross-ref: OCaml imports callee arithmetic before `apply_post`, so
    /// `EqZero` only treats addresses that were already allocated in the
    /// caller as contradictions / latent invalid accesses. Rust currently
    /// imports after heap writes, so preserve the same distinction explicitly.
    pub fn snapshot_allocated_before_call(
        &self,
    ) -> (
        std::collections::HashSet<AbstractValue>,
        std::collections::HashSet<AbstractValue>,
    ) {
        let stack_allocated = self
            .post
            .stack
            .iter()
            .filter_map(|(var, &stack_addr)| {
                matches!(var, Var::ProgramVar(_))
                    .then_some(self.path_condition.get_var_repr(stack_addr))
            })
            .collect();

        let mut heap_allocated = std::collections::HashSet::new();
        for heap in [&self.pre.heap, &self.post.heap] {
            for (src, edges) in heap.iter() {
                if !edges.is_empty() {
                    heap_allocated.insert(self.path_condition.get_var_repr(*src));
                }
            }
        }

        (stack_allocated, heap_allocated)
    }

    fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        let new = self.path_condition.get_var_repr(new);
        self.pre.subst_var(old, new);
        self.post.subst_var(old, new);
        let must_be_valid = std::mem::take(&mut self.must_be_valid);
        self.must_be_valid = self.subst_value_set(must_be_valid, old, new);
        let need_dynamic_type_specialization =
            std::mem::take(&mut self.need_dynamic_type_specialization);
        self.need_dynamic_type_specialization =
            self.subst_value_set(need_dynamic_type_specialization, old, new);
        for value in self.const_cache.values_mut() {
            let value0 = if *value == old { new } else { *value };
            *value = self.path_condition.get_var_repr(value0);
        }
    }

    fn subst_value_set(
        &self,
        values: std::collections::HashSet<AbstractValue>,
        old: AbstractValue,
        new: AbstractValue,
    ) -> std::collections::HashSet<AbstractValue> {
        values
            .into_iter()
            .map(|value| {
                let value = if value == old { new } else { value };
                self.path_condition.get_var_repr(value)
            })
            .collect()
    }

    fn is_heap_allocated(&self, addr: AbstractValue) -> bool {
        self.post
            .heap
            .iter()
            .any(|(src, edges)| !edges.is_empty() && self.path_condition.get_var_repr(*src) == addr)
            || self.pre.heap.iter().any(|(src, edges)| {
                !edges.is_empty() && self.path_condition.get_var_repr(*src) == addr
            })
    }

    fn is_stack_allocated(&self, addr: AbstractValue) -> bool {
        self.post.stack.iter().any(|(var, &stack_addr)| {
            matches!(var, Var::ProgramVar(_))
                && self.path_condition.get_var_repr(stack_addr) == addr
        })
    }

    /// Record that two abstract values are equal.
    pub fn and_equal(&mut self, v1: AbstractValue, v2: AbstractValue) -> SatUnsat<()> {
        let result = self.path_condition.and_equal_vars(v1, v2);
        self.apply_formula_result(result)
    }

    /// Record that an abstract value equals a constant.
    pub fn and_equal_const(&mut self, v: AbstractValue, c: i64) -> SatUnsat<()> {
        let result = self.path_condition.and_equal_const(v, c);
        self.apply_formula_result(result)
    }

    /// Record that an abstract value is positive (> 0, i.e., non-null for pointers).
    /// Cross-ref: OCaml PulseArithmetic.ml and_positive.
    pub fn and_positive(&mut self, v: AbstractValue) -> SatUnsat<()> {
        let result = self.path_condition.and_positive(v);
        self.apply_formula_result(result)
    }

    /// Record that a variable equals a linear expression.
    pub fn and_equal_linear(&mut self, v: AbstractValue, lin: LinArith) -> SatUnsat<()> {
        let result = self.path_condition.and_equal_linear(v, lin);
        self.apply_formula_result(result)
    }

    /// Record that a variable equals a binary operation.
    pub fn and_equal_binop(
        &mut self,
        v: AbstractValue,
        op: sil::binop::Binop,
        x: &Operand,
        y: &Operand,
    ) -> SatUnsat<()> {
        let result = self.path_condition.and_equal_binop(v, op, x, y);
        self.apply_formula_result(result)
    }

    /// Add a translated callee atom directly to the path condition.
    pub fn and_atom_direct(&mut self, atom: Atom) -> SatUnsat<()> {
        let result = self.path_condition.and_atom_direct(atom);
        self.apply_formula_result(result)
    }

    /// Add a translated callee prune condition and remember its depth.
    pub fn and_condition_direct(&mut self, atom: Atom, depth: usize) -> SatUnsat<()> {
        let result = self.path_condition.and_condition_direct(atom, depth);
        self.apply_formula_result(result)
    }

    /// Get the known constant value of a variable, if any.
    pub fn get_const(&self, v: AbstractValue) -> Option<i64> {
        self.path_condition.phi().get_known_const(v).and_then(|q| {
            if q.is_integer() {
                Some(*q.numer() / *q.denom())
            } else {
                None
            }
        })
    }

    /// Get the closure/function-pointer procedure name for an abstract value.
    /// Cross-ref: OCaml `AddressAttributes.get_closure_proc_name`.
    pub fn get_closure_proc_name(&self, addr: AbstractValue) -> Option<&sil::procname::Procname> {
        self.post.attrs.get_closure_proc_name(addr)
    }

    /// Mark an address as requiring validity (the callee dereferences it).
    pub fn mark_must_be_valid(&mut self, addr: AbstractValue) {
        let repr = self.path_condition.get_var_repr(addr);
        self.must_be_valid.insert(repr);
    }

    /// Record a must-be-valid access at a concrete program location.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.check_valid` abduces
    /// `MustBeValid` into the pre-state for addresses already materialized
    /// there. The summary layer later uses that location to publish latent
    /// invalid-access obligations on caller-controlled values.
    pub fn mark_must_be_valid_at(&mut self, addr: AbstractValue, loc: &Location) {
        let repr = self.path_condition.get_var_repr(addr);
        self.must_be_valid.insert(repr);
        if self.pre.heap.get_edges(repr).is_some() {
            self.pre
                .attrs
                .add_one(repr, Attribute::MustBeValid(0, loc.clone(), None));
        }
    }

    /// Check if an address was marked as must-be-valid.
    pub fn is_must_be_valid(&self, addr: AbstractValue) -> bool {
        let repr = self.path_condition.get_var_repr(addr);
        self.must_be_valid.contains(&repr)
    }

    /// Record that an abstract value needs dynamic type information for
    /// specialization (e.g., an unresolved function pointer).
    /// Cross-ref: OCaml `PulseAbductiveDomain.add_need_dynamic_type_specialization`.
    pub fn add_need_dynamic_type_specialization(&mut self, addr: AbstractValue) {
        self.need_dynamic_type_specialization.insert(addr);
    }

    /// Record a disequality.
    pub fn and_not_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<()> {
        let result = self.path_condition.and_not_equal(op1, op2);
        self.apply_formula_result(result)
    }

    /// Check if an abstract value is known to be zero (null).
    pub fn is_known_zero(&self, v: AbstractValue) -> bool {
        self.path_condition.is_known_zero(v)
    }

    /// Get the canonical representative of an abstract value.
    pub fn get_var_repr(&self, v: AbstractValue) -> AbstractValue {
        self.path_condition.get_var_repr(v)
    }

    /// Canonicalize an abstract value for use as an array index.
    ///
    /// If the value is known to equal a constant, unify it with any
    /// previously seen value for the same constant. This ensures
    /// `store &a[0]` and `load &a[0]` use the same heap edge.
    pub fn canonicalize_for_access(&mut self, v: AbstractValue) -> AbstractValue {
        if let Some(q) = self.path_condition.is_known_const(v) {
            let c = *q.numer() / *q.denom();
            // Check if we've seen this constant before
            if let Some(existing) = self.const_cache.get(&c).copied() {
                if existing != v {
                    let _ = self.and_equal(v, existing);
                }
                return self.path_condition.get_var_repr(existing);
            }
            self.const_cache.insert(c, v);
        }
        self.path_condition.get_var_repr(v)
    }

    /// Apply the effect of an unknown/external call on a value: havoc all
    /// memory reachable from `addr` by replacing edge targets with fresh values.
    ///
    /// This is the key mechanism for suppressing false positives when pointer
    /// arguments are passed to unknown functions — the function might modify
    /// the pointed-to memory, so we conservatively make it unknown.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.ml apply_unknown_effect`.
    pub fn apply_unknown_effect(&mut self, addr: AbstractValue) {
        let reachable = self.reachable_from(addr);
        for reachable_addr in &reachable {
            if let Some(edges) = self.post.heap.get_edges(*reachable_addr) {
                let accesses: Vec<Access> = edges.iter().map(|(a, _)| a.clone()).collect();
                for access in accesses {
                    let fresh = AbstractValue::mk_fresh();
                    self.post.heap.add_edge(*reachable_addr, access, fresh);
                }
            }
            // Remove Allocated attribute from havoced addresses to prevent
            // false leak reports. The unknown function may take ownership.
            // Cross-ref: OCaml removes allocation attrs in apply_unknown_effect.
            self.post.attrs.remove_allocated(*reachable_addr);
        }
    }

    /// For unknown calls on `&slot`, ensure future loads from the slot see a
    /// fresh post-state value even if the slot had not been materialized yet.
    pub fn ensure_deref_edge_if_missing(&mut self, addr: AbstractValue) {
        let addr = self.path_condition.get_var_repr(addr);
        if self
            .post
            .heap
            .find_edge(addr, &Access::Dereference)
            .is_none()
        {
            let fresh = AbstractValue::mk_fresh();
            self.post.heap.add_edge(addr, Access::Dereference, fresh);
        }
    }

    /// Collect all abstract addresses reachable from `root` via post-heap edges.
    fn reachable_from(&self, root: AbstractValue) -> std::collections::HashSet<AbstractValue> {
        let mut visited = std::collections::HashSet::new();
        let mut worklist = vec![root];
        while let Some(addr) = worklist.pop() {
            if !visited.insert(addr) {
                continue;
            }
            if let Some(edges) = self.post.heap.get_edges(addr) {
                for (_, &target) in edges.iter() {
                    worklist.push(target);
                }
            }
        }
        visited
    }

    /// Add a prune constraint (from a branch condition).
    pub fn prune_eq(
        &mut self,
        v1: AbstractValue,
        v2: AbstractValue,
        negated: bool,
    ) -> SatUnsat<()> {
        let result = self.path_condition.prune_eq(v1, v2, negated);
        self.apply_formula_result(result)
    }

    /// Add a prune constraint with a constant.
    pub fn prune_eq_const(&mut self, v: AbstractValue, c: i64, negated: bool) -> SatUnsat<()> {
        let result = self.path_condition.prune_eq_const(v, c, negated);
        self.apply_formula_result(result)
    }

    /// Record a local `<` prune condition.
    pub fn prune_less_than(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<()> {
        let result = self.path_condition.prune_less_than(op1, op2);
        self.apply_formula_result(result)
    }

    /// Record a local `<=` prune condition.
    pub fn prune_less_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<()> {
        let result = self.path_condition.prune_less_equal(op1, op2);
        self.apply_formula_result(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::int_lit::IntLit;
    use sil::mangled::Mangled;
    use sil::procname::Procname;
    use sil::typ::Typ;

    fn make_simple_pdesc() -> Procdesc {
        let pname = Procname::c_from_string("test");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        pdesc
    }

    #[test]
    fn test_mk_initial_binds_formals() {
        let pdesc = make_simple_pdesc();
        let state = AbductiveDomain::mk_initial(&pdesc);

        // Formal parameter 'p' should be bound in the stack
        assert!(!state.post.stack.is_empty());
    }

    #[test]
    fn test_null_deref_detection() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        // Create a null value
        let null_addr = AbstractValue::mk_fresh();
        state.invalidate(
            null_addr,
            Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        // Checking validity of null should fail
        assert!(state.check_valid(null_addr).is_err());

        // A different address should be valid
        let other = AbstractValue::mk_fresh();
        assert!(state.check_valid(other).is_ok());
    }

    #[test]
    fn test_use_after_free_detection() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let addr = AbstractValue::mk_fresh();

        // Allocate
        state.allocate(addr, Allocator::CMalloc, Location::dummy());
        assert!(state.check_valid(addr).is_ok());

        // Free
        state.invalidate(
            addr,
            Invalidation::CFree,
            ValueHistory::invalidated(Invalidation::CFree, Location::dummy()),
        );
        assert!(state.check_valid(addr).is_err());
    }

    #[test]
    fn test_heap_read_write() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let addr = AbstractValue::mk_fresh();
        let val = AbstractValue::mk_fresh();

        // Write: addr --*--> val
        state.write_heap(addr, Access::Dereference, val);

        // Read: should find val
        let found = state.read_heap(addr, Access::Dereference);
        assert_eq!(found, val);
    }

    #[test]
    fn test_heap_read_creates_fresh() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let addr = AbstractValue::mk_fresh();

        // Read from unknown: should create fresh
        let v1 = state.read_heap(addr, Access::Dereference);
        // Reading again should return the same value (edge now exists)
        let v2 = state.read_heap(addr, Access::Dereference);
        assert_eq!(v1, v2);
    }

    #[test]
    fn test_heap_read_does_not_abduce_local_roots() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let local_addr = AbstractValue::mk_fresh();
        let _ = state.read_heap(local_addr, Access::Dereference);

        assert_eq!(
            state.pre.heap.find_edge(local_addr, &Access::Dereference),
            None
        );
    }

    #[test]
    fn test_heap_read_preserves_original_pre_edge_after_post_write() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_var = Var::ProgramVar(Box::new(Pvar::mk(
            Mangled::from_string("p"),
            Procname::c_from_string("test"),
        )));
        let formal_addr = state.post.stack.find(&formal_var).unwrap();

        let before = state.read_heap(formal_addr, Access::Dereference);
        let after = AbstractValue::mk_fresh();
        state.write_heap(formal_addr, Access::Dereference, after);

        let found = state.read_heap(formal_addr, Access::Dereference);

        assert_eq!(found, after, "post-state should see the latest write");
        assert_eq!(
            state.pre.heap.find_edge(formal_addr, &Access::Dereference),
            Some(before),
            "pre-state should keep the original value seen through the formal"
        );
    }

    #[test]
    fn test_heap_read_registers_pre_targets_for_deep_reads() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_var = Var::ProgramVar(Box::new(Pvar::mk(
            Mangled::from_string("p"),
            Procname::c_from_string("test"),
        )));
        let formal_addr = state.post.stack.find(&formal_var).unwrap();

        let inner = state.read_heap(formal_addr, Access::Dereference);
        let leaf = state.read_heap(inner, Access::Dereference);

        assert_eq!(
            state.pre.heap.find_edge(inner, &Access::Dereference),
            Some(leaf),
            "new pre targets should be registered so deeper reads are abduced too"
        );
    }

    #[test]
    fn test_and_equal_substitutes_heap_attrs_and_sets() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let src = AbstractValue::of_raw(1);
        let new = AbstractValue::of_raw(10);
        let old = AbstractValue::of_raw(20);

        state.pre.heap.add_edge(src, Access::Dereference, old);
        state.post.heap.add_edge(src, Access::Dereference, old);
        state.add_attr(old, Attribute::Initialized);
        state.must_be_valid.insert(old);
        state.need_dynamic_type_specialization.insert(old);

        assert!(state.and_equal(old, new).is_sat());
        assert_eq!(
            state.pre.heap.find_edge(src, &Access::Dereference),
            Some(new)
        );
        assert_eq!(
            state.post.heap.find_edge(src, &Access::Dereference),
            Some(new)
        );
        assert!(state
            .post
            .attrs
            .get(&new)
            .is_some_and(|attrs| attrs.contains(&Attribute::Initialized)));
        assert!(state.must_be_valid.contains(&new));
        assert!(state.need_dynamic_type_specialization.contains(&new));
    }

    #[test]
    fn test_eq_zero_marks_heap_allocated_value_invalid() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_var = Var::ProgramVar(Box::new(Pvar::mk(
            Mangled::from_string("p"),
            Procname::c_from_string("test"),
        )));
        let formal_addr = state.post.stack.find(&formal_var).unwrap();
        let formal_val = state.read_heap(formal_addr, Access::Dereference);
        let _heap_target = state.read_heap(formal_val, Access::Dereference);

        state.mark_must_be_valid(formal_val);
        assert!(state.and_equal_const(formal_val, 0).is_sat());
        assert!(state.check_valid(formal_val).is_err());
    }

    #[test]
    fn test_imported_eq_zero_reports_potential_invalid_access_without_invalid_attr() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_var = Var::ProgramVar(Box::new(Pvar::mk(
            Mangled::from_string("p"),
            Procname::c_from_string("test"),
        )));
        let formal_addr = state.post.stack.find(&formal_var).unwrap();
        let formal_val = state.read_heap(formal_addr, Access::Dereference);
        let _heap_target = state.read_heap(formal_val, Access::Dereference);

        state.mark_must_be_valid(formal_val);
        let (mut stack_allocated_before_call, mut heap_allocated_before_call) =
            state.snapshot_allocated_before_call();
        let result = state.path_condition.and_equal_const(formal_val, 0);

        let mut imported_must_be_valid =
            std::collections::HashSet::from([state.get_var_repr(formal_val)]);
        assert!(matches!(
            state.apply_formula_result_for_summary_import(
                result,
                &mut imported_must_be_valid,
                &mut stack_allocated_before_call,
                &mut heap_allocated_before_call,
            ),
            SatUnsat::Sat(ImportedFormulaEffect::PotentialInvalidAccess(addr))
                if addr == state.get_var_repr(formal_val)
        ));
        assert!(
            state.check_valid(formal_val).is_ok(),
            "summary import should report the potential invalid access without persisting a synthetic invalid attr"
        );
    }

    #[test]
    fn test_path_condition_null_check() {
        let pdesc = make_simple_pdesc();
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let p = AbstractValue::mk_fresh();
        let null_val = AbstractValue::mk_fresh();

        // null_val = 0
        state.and_equal_const(null_val, 0);

        // Invalidate null_val
        state.invalidate(
            null_val,
            Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        // After p = null_val, p should also be known-zero
        state.and_equal(p, null_val);
        assert!(state.is_known_zero(p));
    }
}
