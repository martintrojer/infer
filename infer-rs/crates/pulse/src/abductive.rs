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

use sil::location::Location;
use sil::procdesc::Procdesc;
use sil::pvar::Pvar;
use sil::var::Var;

use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::{Allocator, Attribute};
use crate::base_domain::BaseDomain;
use crate::formula::{Formula, NewEq, Operand};
use crate::invalidation::Invalidation;
use crate::sat_unsat::SatUnsat;

/// The abductive domain: pre-state + post-state + path condition.
///
/// Mirrors OCaml's `PulseAbductiveDomain.t`:
/// - `pre`: the procedure's precondition — what it assumes about inputs.
///   Populated during analysis whenever memory is read (Load).
/// - `post`: the procedure's postcondition — the current abstract state.
/// - `path_condition`: constraints on abstract values.
#[derive(Clone, Debug)]
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
            let var = Var::ProgramVar(Box::new(pvar));
            state.post.stack.add(var.clone(), addr);
            state.post.heap.register_address(addr);
            state.pre.stack.add(var, addr);
            state.pre.heap.register_address(addr);
        }

        state
    }

    /// Look up a variable's abstract address in the stack.
    /// If not found, allocates a fresh address and binds it.
    pub fn eval_var(&mut self, var: &Var) -> AbstractValue {
        if let Some(addr) = self.post.stack.find(var) {
            addr
        } else {
            let addr = AbstractValue::mk_fresh();
            self.post.stack.add(var.clone(), addr);
            addr
        }
    }

    /// Read through a heap edge: follow `addr --access--> target`.
    /// If no edge exists, creates a fresh target and adds the edge.
    /// Also records the read in the pre-state (biabduction).
    pub fn read_heap(&mut self, addr: AbstractValue, access: Access) -> AbstractValue {
        let target = if let Some(target) = self.post.heap.find_edge(addr, &access) {
            target
        } else {
            let target = AbstractValue::mk_fresh();
            self.post.heap.add_edge(addr, access.clone(), target);
            target
        };
        // Abduce: record this read in the pre-state so the summary captures
        // what the procedure assumes about its inputs.
        self.pre.heap.add_edge(addr, access, target);
        target
    }

    /// Write through a heap edge: set `addr --access--> value`.
    pub fn write_heap(&mut self, addr: AbstractValue, access: Access, value: AbstractValue) {
        self.post.heap.add_edge(addr, access, value);
    }

    /// Check if dereferencing an address is valid.
    ///
    /// Returns `Ok(())` if valid, or `Err` with the invalidation reason
    /// if the address is known to be invalid (null, freed, etc.).
    ///
    /// This is THE null-dereference / use-after-free check.
    pub fn check_valid(&self, addr: AbstractValue) -> Result<(), Box<(Invalidation, Location)>> {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.check_valid(repr)
    }

    /// Mark an address as invalid (freed, null, etc.).
    pub fn invalidate(&mut self, addr: AbstractValue, inv: Invalidation, loc: Location) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.invalidate(repr, inv, loc);
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

    /// Add a generic attribute to an address.
    pub fn add_attr(&mut self, addr: AbstractValue, attr: Attribute) {
        let repr = self.path_condition.get_var_repr(addr);
        self.post.attrs.add_one(repr, attr);
    }

    /// Record that two abstract values are equal.
    pub fn and_equal(&mut self, v1: AbstractValue, v2: AbstractValue) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.and_equal_vars(v1, v2)
    }

    /// Record that an abstract value equals a constant.
    pub fn and_equal_const(&mut self, v: AbstractValue, c: i64) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.and_equal_const(v, c)
    }

    /// Record that an abstract value is positive (> 0, i.e., non-null for pointers).
    /// Cross-ref: OCaml PulseArithmetic.ml and_positive.
    pub fn and_positive(&mut self, v: AbstractValue) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.and_positive(v)
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
    pub fn and_not_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.and_not_equal(op1, op2)
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
            if let Some(&existing) = self.const_cache.get(&c) {
                if existing != v {
                    let _ = self.path_condition.and_equal_vars(v, existing);
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
    ) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.prune_eq(v1, v2, negated)
    }

    /// Add a prune constraint with a constant.
    pub fn prune_eq_const(
        &mut self,
        v: AbstractValue,
        c: i64,
        negated: bool,
    ) -> SatUnsat<Vec<NewEq>> {
        self.path_condition.prune_eq_const(v, c, negated)
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
            Location::dummy(),
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
        state.invalidate(addr, Invalidation::CFree, Location::dummy());
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
            Location::dummy(),
        );

        // After p = null_val, p should also be known-zero
        state.and_equal(p, null_val);
        assert!(state.is_known_zero(p));
    }
}
