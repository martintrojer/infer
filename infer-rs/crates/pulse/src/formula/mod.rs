// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Formula solver for Pulse: tracks constraints between abstract values.
//!
//! Mirrors OCaml's `PulseFormula.ml` / `PulseFormulaPhi.ml`.
//!
//! The formula tracks:
//! - **Equality classes** via union-find (which values are known equal)
//! - **Linear equations** (`x = 2y + 3`) for arithmetic reasoning
//! - **Atoms** (inequality/disequality constraints)
//!
//! This enables path-sensitive analysis: after `if (p != NULL)`, the true
//! branch knows p ≠ 0 and the false branch knows p = 0.

pub mod atom;
pub mod citv;
pub mod lin_arith;
pub mod phi;
pub mod term;
pub mod var_uf;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::abstract_value::AbstractValue;
use crate::sat_unsat::SatUnsat;
use atom::Atom;
use lin_arith::{LinArith, Q};
use term::Term;

pub use phi::NewEq;

/// An operand in a formula constraint: either a variable or a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    AbstractValue(AbstractValue),
    ConstOperand(i64),
}

/// The formula: wraps `Phi` with the public API.
///
/// Mirrors OCaml's `Formula.t` which wraps `FormulaPhi.t` plus conditions.
///
/// `phi` is stored behind an `Arc` so cloning the surrounding abductive state
/// shares the (typically large) phi map structure between disjuncts and
/// retained invariant snapshots without deep-copying it. Mutating helpers go
/// through `Arc::make_mut` (clone-on-write) so the public `Formula` API is
/// unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Formula {
    conditions: BTreeMap<Atom, usize>,
    phi: Arc<phi::Phi>,
}

impl Formula {
    /// Create an empty formula (trivially satisfiable).
    pub fn ttrue() -> Self {
        Self::default()
    }

    /// Access the inner Phi state (for formula translation).
    pub fn phi(&self) -> &phi::Phi {
        &self.phi
    }

    fn phi_mut(&mut self) -> &mut phi::Phi {
        Arc::make_mut(&mut self.phi)
    }

    /// Access the recorded prune conditions and the call depth at which they
    /// were added. Depth 0 means local to the current procedure.
    pub fn conditions(&self) -> &BTreeMap<Atom, usize> {
        &self.conditions
    }

    /// Add an atom constraint directly (for formula translation from callee).
    pub fn and_atom_direct(&mut self, atom: Atom) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut().and_atom(atom)
    }

    /// Add a translated callee condition and remember its call depth.
    pub fn and_condition_direct(&mut self, atom: Atom, depth: usize) -> SatUnsat<Vec<NewEq>> {
        let normalized_condition = self.normalize_condition_atom(&atom);
        let result = self.enforce_condition_atom(&atom);
        if result.is_sat() {
            if let Some(atom) = normalized_condition {
                self.record_condition_if_meaningful(atom, depth);
            }
        }
        result
    }

    /// Get the canonical representative of a variable.
    pub fn get_var_repr(&self, v: AbstractValue) -> AbstractValue {
        self.phi.get_repr(v)
    }
}

/// Transitively expand a seed reachability set across the formula's
/// `linear_eqs` (in both directions) and `fn_app_eqs` (ret ↔ actuals)
/// graph. The returned set is a superset of `seed_reachable` containing
/// every variable connected to a seed via at least one canonicalization
/// edge.
///
/// Used to (a) drive the summary-time `simplify_for_summary` keep set
/// (in `summary.rs`), and (b) gate intermediate-state
/// `prune_unreachable_simple_facts` cleanup so that values transitively
/// linked to retained values stay alive for `get_known_const` /
/// `canonical_term_operand` (see `abductive.rs::shrink_post_to_stack_reachable`).
///
/// Cross-ref: OCaml `PulseFormula.DeadVariables.build_var_graph` /
/// `get_reachable_from` (PulseFormula.ml:802-830).
pub fn expand_formula_reachable(
    formula: &Formula,
    seed_reachable: &std::collections::HashSet<AbstractValue>,
) -> std::collections::HashSet<AbstractValue> {
    let phi = formula.phi();
    let mut reachable = seed_reachable.clone();
    let mut worklist: Vec<_> = seed_reachable.iter().copied().collect();

    while let Some(v) = worklist.pop() {
        let repr = phi.get_repr(v);
        if let Some(lin) = phi.linear_eqs.get(&repr) {
            for dep in lin.vars.keys() {
                let dep_repr = phi.get_repr(*dep);
                if reachable.insert(dep_repr) {
                    worklist.push(dep_repr);
                }
            }
        }

        for (&lhs, lin) in &phi.linear_eqs {
            let lhs_repr = phi.get_repr(lhs);
            if lhs_repr != repr
                && lin.vars.keys().any(|dep| phi.get_repr(*dep) == repr)
                && reachable.insert(lhs_repr)
            {
                worklist.push(lhs_repr);
            }
        }

        // Cross-ref: OCaml PulseFormula.DeadVariables.build_var_graph keeps
        // function-application results connected to their actual arguments.
        // Without this, imported conditions on pure-call results can be
        // dropped during summary normalization even when the actuals are
        // caller-visible formals, which makes latent caller-dependent errors
        // look manifest.
        for (key, ret) in phi.iter_fn_app_eqs() {
            let ret_repr = phi.get_repr(*ret);
            let mut connected = ret_repr == repr;
            let mut actual_reprs = Vec::new();
            for actual in &key.actuals {
                let phi::FnAppActual::Var(actual) = actual else {
                    continue;
                };
                let actual_repr = phi.get_repr(*actual);
                connected |= actual_repr == repr;
                actual_reprs.push(actual_repr);
            }
            if !connected {
                continue;
            }
            if reachable.insert(ret_repr) {
                worklist.push(ret_repr);
            }
            for actual_repr in actual_reprs {
                if reachable.insert(actual_repr) {
                    worklist.push(actual_repr);
                }
            }
        }
    }

    reachable
}

impl Formula {
    /// Check if a variable is known to equal zero.
    pub fn is_known_zero(&self, v: AbstractValue) -> bool {
        self.phi.is_known_zero(v)
    }

    /// Check whether summary-space simplification proves a visible value is
    /// equal to zero, even if the proof currently goes through dead alias /
    /// arithmetic temps that `simplify_for_summary` is about to erase.
    ///
    /// Cross-ref: OCaml `PulseFormula.simplify` returns `new_eqs`, and
    /// `PulseAbductiveDomain.incorporate_new_eqs` uses `EqZero` to surface
    /// `PotentialInvalidAccessSummary` on caller-visible values.
    pub fn is_known_zero_for_summary(
        &self,
        v: AbstractValue,
        precondition_vocabulary: &HashSet<AbstractValue>,
        keep: &HashSet<AbstractValue>,
    ) -> bool {
        let atom = self.phi.simplify_condition_atom_for_summary(
            &Atom::Equal(Term::Var(v), Term::Const(0)),
            precondition_vocabulary,
            keep,
        );

        if atom.is_trivially_true() == Some(true) {
            return true;
        }

        matches!(
            atom,
            Atom::Equal(Term::Var(w), Term::Const(0))
                | Atom::Equal(Term::Const(0), Term::Var(w))
                if self.phi.get_repr(w) == self.phi.get_repr(v)
        )
    }

    /// Check if a variable is known to be non-zero (positive or has NotEqual(v, 0)).
    pub fn is_known_nonzero(&self, v: AbstractValue) -> bool {
        let repr = self.phi.get_repr(v);
        // Check if known constant is non-zero
        if let Some(q) = self.phi.get_known_const(v) {
            return q != Q::from_integer(0);
        }
        // Check if the interval excludes zero (lower bound > 0)
        if let Some(citv::CItv::Between(citv::Bound::Int(n), _)) = self.phi.get_interval(v) {
            if *n > 0 {
                return true;
            }
        }
        // Check atoms for LessThan(0, v) which means v > 0.
        // This catches constraints from interproc formula translation
        // (and_positive adds LessThan(Const(0), Var(v)) atoms).
        let zero_term = term::Term::Const(0);
        let var_term = term::Term::Var(repr);
        if self
            .phi
            .atoms
            .contains(&atom::Atom::LessThan(zero_term, var_term))
        {
            return true;
        }
        false
    }

    /// Check if a variable is known to equal a specific constant.
    pub fn is_known_const(&self, v: AbstractValue) -> Option<Q> {
        self.phi.get_known_const(v)
    }

    /// Record that two operands are equal.
    pub fn and_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        match (op1, op2) {
            (Operand::AbstractValue(v1), Operand::AbstractValue(v2)) => {
                self.phi_mut().and_var_equal(*v1, *v2)
            }
            (Operand::AbstractValue(v), Operand::ConstOperand(c))
            | (Operand::ConstOperand(c), Operand::AbstractValue(v)) => {
                self.phi_mut().and_const_eq(*v, *c)
            }
            (Operand::ConstOperand(c1), Operand::ConstOperand(c2)) => {
                if c1 == c2 {
                    SatUnsat::Sat(Vec::new())
                } else {
                    SatUnsat::Unsat
                }
            }
        }
    }

    /// Record that two variables are equal.
    pub fn and_equal_vars(&mut self, v1: AbstractValue, v2: AbstractValue) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut().and_var_equal(v1, v2)
    }

    /// Record that a variable equals a constant.
    pub fn and_equal_const(&mut self, v: AbstractValue, c: i64) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut().and_const_eq(v, c)
    }

    /// Record a pure function application: ret_val = f(actuals).
    /// If the same function with same actuals was seen before, unify return values.
    pub fn and_fn_app(
        &mut self,
        ret_val: AbstractValue,
        callee: &str,
        actuals: &[AbstractValue],
    ) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut().and_fn_app(ret_val, callee, actuals)
    }

    /// Record that a variable equals a linear expression.
    pub fn and_equal_linear(&mut self, v: AbstractValue, lin: LinArith) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut().and_linear_eq(v, lin)
    }

    pub fn and_equal_linear_with_preferred(
        &mut self,
        v: AbstractValue,
        lin: LinArith,
        preferred: AbstractValue,
    ) -> SatUnsat<Vec<NewEq>> {
        self.phi_mut()
            .and_linear_eq_with_preferred(v, lin, Some(preferred))
    }

    /// Record that two operands are NOT equal.
    pub fn and_not_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        let t1 = operand_to_term(op1, &self.phi);
        let t2 = operand_to_term(op2, &self.phi);
        self.phi_mut().and_atom(Atom::NotEqual(t1, t2))
    }

    /// Record that op1 ≤ op2.
    /// Cross-ref: OCaml PulseFormula.ml prune_binop uses CItv.abduce_binop_is_true.
    pub fn and_less_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        // Check CItv satisfiability first
        if self
            .check_citv_binop(false, &sil::binop::Binop::Le, op1, op2)
            .is_unsat()
        {
            return SatUnsat::Unsat;
        }
        let t1 = operand_to_term(op1, &self.phi);
        let t2 = operand_to_term(op2, &self.phi);
        self.phi_mut().and_atom(Atom::LessEqual(t1, t2))
    }

    /// Record that v > 0 (v is positive / non-null).
    /// Cross-ref: OCaml PulseArithmetic.ml and_positive.
    pub fn and_positive(&mut self, v: AbstractValue) -> SatUnsat<Vec<NewEq>> {
        self.and_less_than(&Operand::ConstOperand(0), &Operand::AbstractValue(v))
    }

    /// Record that op1 < op2.
    /// Cross-ref: OCaml PulseFormula.ml prune_binop uses CItv.abduce_binop_is_true.
    pub fn and_less_than(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        if self
            .check_citv_binop(false, &sil::binop::Binop::Lt, op1, op2)
            .is_unsat()
        {
            return SatUnsat::Unsat;
        }
        let t1 = operand_to_term(op1, &self.phi);
        let t2 = operand_to_term(op2, &self.phi);
        self.phi_mut().and_atom(Atom::LessThan(t1, t2))
    }

    /// Mark a variable as integer-typed. When the linear solver later
    /// derives a non-integer rational for this variable, the path is Unsat.
    /// Cross-ref: OCaml PulseFormula.ml and_is_int.
    pub fn and_is_int(&mut self, v: AbstractValue) {
        self.phi_mut().mark_is_int(v);
    }

    /// Add a prune constraint (from a branch condition).
    /// Cross-ref: OCaml PulseFormula.ml prune_binop checks CItv first.
    pub fn prune_eq(
        &mut self,
        v1: AbstractValue,
        v2: AbstractValue,
        negated: bool,
    ) -> SatUnsat<Vec<NewEq>> {
        self.prune_eq_with_depth(v1, v2, negated, 0)
    }

    /// Add a prune equality/disequality at a specific call depth.
    pub fn prune_eq_with_depth(
        &mut self,
        v1: AbstractValue,
        v2: AbstractValue,
        negated: bool,
        depth: usize,
    ) -> SatUnsat<Vec<NewEq>> {
        let bop = if negated {
            sil::binop::Binop::Ne
        } else {
            sil::binop::Binop::Eq
        };
        let op1 = Operand::AbstractValue(v1);
        let op2 = Operand::AbstractValue(v2);
        if self.check_citv_binop(false, &bop, &op1, &op2).is_unsat() {
            return SatUnsat::Unsat;
        }
        let condition_atom = if negated {
            Atom::NotEqual(
                operand_to_term(&op1, &self.phi),
                operand_to_term(&op2, &self.phi),
            )
        } else {
            Atom::Equal(
                operand_to_term(&op1, &self.phi),
                operand_to_term(&op2, &self.phi),
            )
        };
        let normalized_condition = self.normalize_condition_atom(&condition_atom);
        let result = if negated {
            self.and_not_equal(&op1, &op2)
        } else {
            self.phi_mut().and_var_equal(v1, v2)
        };
        if result.is_sat() {
            if let Some(atom) = normalized_condition {
                self.record_condition_if_meaningful(atom, depth);
            }
        }
        result
    }

    /// Record a local `<` prune condition.
    pub fn prune_less_than(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        let atom = Atom::LessThan(
            operand_to_term(op1, &self.phi),
            operand_to_term(op2, &self.phi),
        );
        let normalized_condition = self.normalize_condition_atom(&atom);
        let result = self.and_less_than(op1, op2);
        if result.is_sat() {
            if let Some(atom) = normalized_condition {
                self.record_condition_if_meaningful(atom, 0);
            }
        }
        result
    }

    /// Record a local `<=` prune condition.
    pub fn prune_less_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        let atom = Atom::LessEqual(
            operand_to_term(op1, &self.phi),
            operand_to_term(op2, &self.phi),
        );
        let normalized_condition = self.normalize_condition_atom(&atom);
        let result = self.and_less_equal(op1, op2);
        if result.is_sat() {
            if let Some(atom) = normalized_condition {
                self.record_condition_if_meaningful(atom, 0);
            }
        }
        result
    }

    /// Check a comparison against CItv intervals.
    /// Returns Unsat if the comparison is infeasible according to intervals.
    /// Also refines intervals if possible.
    ///
    /// Cross-ref: OCaml PulseFormula.ml Normalizer.prune_binop calls
    /// CItv.abduce_binop_is_true to check interval satisfiability.
    fn check_citv_binop(
        &mut self,
        negated: bool,
        bop: &sil::binop::Binop,
        op1: &Operand,
        op2: &Operand,
    ) -> SatUnsat<()> {
        let i1 = operand_interval(op1, &self.phi);
        let i2 = operand_interval(op2, &self.phi);
        // Only check if at least one operand has an interval
        if i1.is_none() && i2.is_none() {
            return SatUnsat::Sat(());
        }
        match citv::CItv::abduce_binop_is_true(negated, bop, i1.as_ref(), i2.as_ref()) {
            citv::AbductionResult::Unsatisfiable => SatUnsat::Unsat,
            citv::AbductionResult::Satisfiable(refined1, refined2) => {
                // Refine intervals if we got tighter bounds
                if let (Some(better), Operand::AbstractValue(v)) = (refined1, op1) {
                    if self.phi_mut().add_interval(*v, better).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                if let (Some(better), Operand::AbstractValue(v)) = (refined2, op2) {
                    if self.phi_mut().add_interval(*v, better).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                SatUnsat::Sat(())
            }
        }
    }

    /// Add a prune constraint with a constant.
    pub fn prune_eq_const(
        &mut self,
        v: AbstractValue,
        c: i64,
        negated: bool,
    ) -> SatUnsat<Vec<NewEq>> {
        // If v has a term equality (v = binop(x, y)) and we're pruning v ≠ 0
        // (truthy) or v = 0 (falsy), resolve back to the comparison and add
        // the appropriate atom. This matches OCaml's prune_binop which looks
        // up term_eqs to derive constraints from boolean results.
        //
        // Prefer the direct key, then fall back to the canonical
        // representative. Direct lookup preserves existing term_eqs if a
        // comparison result is later merged into a simpler value. The
        // canonical fallback handles the term_value_index cache-hit path:
        // the freshly minted `v` is unified with the cached representative,
        // but `term_eqs` is only keyed under that representative. We
        // deliberately do not scan `term_value_index` or repair stale keys.
        let mut comparison_condition_atom = None;
        if c == 0 {
            let teq = self.phi.term_eqs.get(&v).or_else(|| {
                let repr = self.phi.get_repr(v);
                (repr != v).then(|| self.phi.term_eqs.get(&repr)).flatten()
            });
            if let Some(teq) = teq.cloned() {
                comparison_condition_atom = if negated {
                    // prune(v ≠ 0) i.e. v is truthy → the comparison is true
                    comparison_to_atom(teq.op, &teq.lhs, &teq.rhs, false, &self.phi)
                } else {
                    // prune(v = 0) i.e. v is falsy → the comparison is false (negate it)
                    comparison_to_atom(teq.op, &teq.lhs, &teq.rhs, true, &self.phi)
                };
            }
        }

        let condition_atom = comparison_condition_atom.clone().unwrap_or_else(|| {
            if negated {
                Atom::NotEqual(
                    operand_to_term(&Operand::AbstractValue(v), &self.phi),
                    Term::Const(c),
                )
            } else {
                Atom::Equal(
                    operand_to_term(&Operand::AbstractValue(v), &self.phi),
                    Term::Const(c),
                )
            }
        });
        let normalized_condition = self.normalize_condition_atom(&condition_atom);
        if let Some(atom) = comparison_condition_atom {
            if self.enforce_condition_atom(&atom).is_unsat() {
                return SatUnsat::Unsat;
            }
        }

        let result = if negated {
            self.and_not_equal(&Operand::AbstractValue(v), &Operand::ConstOperand(c))
        } else {
            self.phi_mut().and_const_eq(v, c)
        };

        if result.is_sat() {
            if let Some(atom) = normalized_condition {
                self.record_condition_if_meaningful(atom, 0);
            }
        }

        result
    }

    /// Record that v = binop(x, y).
    pub fn and_equal_binop(
        &mut self,
        v: AbstractValue,
        op: sil::binop::Binop,
        x: &Operand,
        y: &Operand,
    ) -> SatUnsat<Vec<NewEq>> {
        // Cross-ref: OCaml `PulseFormulaPhi.term_eqs` is itself indexed by
        // the term, so a repeated `xor(v37, v31)` returns the same `v38`.
        // Mirror that with our reverse `term_value_index`: if the same
        // BinOp on the same canonical operands has already been evaluated
        // in this disjunct, equate the freshly minted `v` with the cached
        // representative instead of paying the formula-update cost twice.
        if let Some(existing) = self.phi.find_term_value(&op, x, y) {
            if existing != v {
                return self.phi_mut().and_var_equal(v, existing);
            }
        }

        if result_of_binop_is_integer(&op, x, y, &self.phi) {
            self.phi_mut().mark_is_int(v);
        }

        let result = self.and_equal_binop_inner(v, op.clone(), x, y);
        // After the inner call has populated linear_eqs / intervals /
        // term_eqs, register the canonical key in the reverse index so a
        // subsequent identical evaluation in the same disjunct can reuse
        // `v` instead of minting a fresh value.
        self.phi_mut().register_term_value(&op, x, y, v);
        result
    }

    fn and_equal_binop_inner(
        &mut self,
        v: AbstractValue,
        op: sil::binop::Binop,
        x: &Operand,
        y: &Operand,
    ) -> SatUnsat<Vec<NewEq>> {
        // For supported arithmetic ops, create a linear equation AND
        // propagate intervals through CItv.
        // Cross-ref: OCaml PulseFormula.ml Normalizer.and_var_binop_var
        // calls CItv.binop to compute result intervals.
        match op {
            sil::binop::Binop::PlusA(_) | sil::binop::Binop::PlusPI => {
                let lx = operand_to_lin(x, &self.phi);
                let ly = operand_to_lin(y, &self.phi);
                // Propagate intervals: result_interval = lhs_interval + rhs_interval
                if let Some(result_itv) = operand_interval(x, &self.phi).and_then(|ix| {
                    operand_interval(y, &self.phi).and_then(|iy| citv::CItv::binop(&op, &ix, &iy))
                }) {
                    if self.phi_mut().add_interval(v, result_itv).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi_mut().and_linear_eq(v, lx.add(&ly))
            }
            sil::binop::Binop::MinusA(_)
            | sil::binop::Binop::MinusPI
            | sil::binop::Binop::MinusPP => {
                let lx = operand_to_lin(x, &self.phi);
                let ly = operand_to_lin(y, &self.phi);
                if let Some(result_itv) = operand_interval(x, &self.phi).and_then(|ix| {
                    operand_interval(y, &self.phi).and_then(|iy| citv::CItv::binop(&op, &ix, &iy))
                }) {
                    if self.phi_mut().add_interval(v, result_itv).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi_mut().and_linear_eq(v, lx.sub(&ly))
            }
            // Comparison ops: if both operands are known constants, fold to 0 or 1
            sil::binop::Binop::Eq
            | sil::binop::Binop::Ne
            | sil::binop::Binop::Lt
            | sil::binop::Binop::Gt
            | sil::binop::Binop::Le
            | sil::binop::Binop::Ge => {
                if let (Some(cx), Some(cy)) =
                    (operand_const(x, &self.phi), operand_const(y, &self.phi))
                {
                    let cmp = match op {
                        sil::binop::Binop::Eq => cx == cy,
                        sil::binop::Binop::Ne => cx != cy,
                        sil::binop::Binop::Lt => cx < cy,
                        sil::binop::Binop::Gt => cx > cy,
                        sil::binop::Binop::Le => cx <= cy,
                        sil::binop::Binop::Ge => cx >= cy,
                        _ => unreachable!(),
                    };
                    self.phi_mut().and_const_eq(v, if cmp { 1 } else { 0 })
                } else {
                    // Record term equality: v = op(lhs, rhs).
                    // When pruning on v later, we can resolve back to the comparison.
                    let phi = self.phi_mut();
                    phi.term_eqs.insert(
                        v,
                        phi::TermEq {
                            op,
                            lhs: x.clone(),
                            rhs: y.clone(),
                        },
                    );
                    let _ = phi.var_eqs.find(v);
                    SatUnsat::Sat(Vec::new())
                }
            }
            // DivF: rational division (not integer truncation).
            // Must be handled separately to preserve fractional results.
            sil::binop::Binop::DivF => {
                if let (Some(qx), Some(qy)) = (operand_q(x, &self.phi), operand_q(y, &self.phi)) {
                    if qy != Q::from_integer(0) {
                        let result = qx / qy;
                        let lin = LinArith::of_q(result);
                        return self.phi_mut().and_linear_eq(v, lin);
                    }
                }
                let _ = self.phi_mut().var_eqs.find(v);
                SatUnsat::Sat(Vec::new())
            }
            // Mul, DivI, Mod, Shift: fold if both operands are known constants
            sil::binop::Binop::Mult(_)
            | sil::binop::Binop::DivI
            | sil::binop::Binop::Mod
            | sil::binop::Binop::Shiftlt
            | sil::binop::Binop::Shiftrt => {
                if let (Some(cx), Some(cy)) =
                    (operand_const(x, &self.phi), operand_const(y, &self.phi))
                {
                    let result = match op {
                        sil::binop::Binop::Mult(_) => cx * cy,
                        sil::binop::Binop::DivI if cy != 0 => cx / cy,
                        sil::binop::Binop::Mod if cy != 0 => cx % cy,
                        sil::binop::Binop::Shiftlt if (0..64).contains(&cy) => cx << cy,
                        sil::binop::Binop::Shiftrt if (0..64).contains(&cy) => cx >> cy,
                        _ => {
                            let _ = self.phi_mut().var_eqs.find(v);
                            return SatUnsat::Sat(Vec::new());
                        }
                    };
                    self.phi_mut().and_const_eq(v, result)
                } else {
                    let _ = self.phi_mut().var_eqs.find(v);
                    SatUnsat::Sat(Vec::new())
                }
            }
            // Bitwise ops: fold if both operands are known constants
            sil::binop::Binop::BAnd | sil::binop::Binop::BOr | sil::binop::Binop::BXor => {
                if let (Some(cx), Some(cy)) =
                    (operand_const(x, &self.phi), operand_const(y, &self.phi))
                {
                    let result = match op {
                        sil::binop::Binop::BAnd => cx & cy,
                        sil::binop::Binop::BOr => cx | cy,
                        sil::binop::Binop::BXor => cx ^ cy,
                        _ => unreachable!(),
                    };
                    self.phi_mut().and_const_eq(v, result)
                } else {
                    let _ = self.phi_mut().var_eqs.find(v);
                    SatUnsat::Sat(Vec::new())
                }
            }
            _ => {
                // For remaining unsupported ops, just register v
                let _ = self.phi_mut().var_eqs.find(v);
                SatUnsat::Sat(Vec::new())
            }
        }
    }

    /// Cheap intermediate-state GC: remove high-volume unary facts for
    /// unreachable values without running the full formula simplifier.
    pub fn prune_unreachable_simple_facts(&mut self, reachable: &HashSet<AbstractValue>) {
        self.phi_mut().prune_unreachable_simple_facts(reachable);
    }

    /// Simplify the formula.
    pub fn simplify(&mut self, reachable: &HashSet<AbstractValue>) {
        self.phi_mut().simplify(reachable);
        let mut conditions: BTreeMap<Atom, usize> = BTreeMap::new();
        for (atom, depth) in std::mem::take(&mut self.conditions) {
            if atom
                .all_vars()
                .into_iter()
                .all(|v| reachable.contains(&self.phi.get_repr(v)))
            {
                match conditions.get_mut(&atom) {
                    Some(existing_depth) => *existing_depth = (*existing_depth).min(depth),
                    None => {
                        conditions.insert(atom, depth);
                    }
                }
            }
        }
        self.conditions = conditions;
    }

    /// Summary-specific simplification.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` calls
    /// `PulseFormula.simplify ~precondition_vocabulary ~keep` so exported
    /// summaries keep caller-visible conditions in their original shape but
    /// substitute away callee-local condition variables.
    pub fn simplify_for_summary(
        &mut self,
        precondition_vocabulary: &HashSet<AbstractValue>,
        keep: &HashSet<AbstractValue>,
    ) {
        let _ = self.simplify_for_summary_with_witness_targets(precondition_vocabulary, keep, keep);
    }

    /// Summary-specific simplification with an explicit set of direct summary
    /// roots that may receive synthesized restricted/tableau inequality witnesses.
    ///
    /// OCaml `DeadVariables.eliminate` may keep formula-only affine temps alive
    /// through the var graph, but its exported tableau witnesses are still rooted
    /// in directly visible summary values. Passing only the pre-expansion seeds
    /// here prevents synthesized witnesses from leaking onto recursive
    /// specialization temps that escape alpha-renaming.
    pub fn simplify_for_summary_with_witness_targets(
        &mut self,
        precondition_vocabulary: &HashSet<AbstractValue>,
        keep: &HashSet<AbstractValue>,
        witness_targets: &HashSet<AbstractValue>,
    ) -> Vec<NewEq> {
        self.simplify_for_summary_with_witness_and_eq_zero_targets(
            precondition_vocabulary,
            keep,
            witness_targets,
            keep,
        )
    }

    pub fn simplify_for_summary_with_witness_and_eq_zero_targets(
        &mut self,
        precondition_vocabulary: &HashSet<AbstractValue>,
        keep: &HashSet<AbstractValue>,
        witness_targets: &HashSet<AbstractValue>,
        eq_zero_targets: &HashSet<AbstractValue>,
    ) -> Vec<NewEq> {
        let rewritten_conditions: Vec<_> = std::mem::take(&mut self.conditions)
            .into_iter()
            .filter_map(|(atom, depth)| {
                let atom = self.phi.simplify_condition_atom_for_summary(
                    &atom,
                    precondition_vocabulary,
                    keep,
                );
                (atom.is_trivially_true() != Some(true)).then_some((atom, depth))
            })
            .collect();

        let summary_atoms: Vec<_> = self
            .phi
            .atoms
            .iter()
            .chain(rewritten_conditions.iter().map(|(atom, _)| atom))
            .cloned()
            .collect();
        let witness_vars = self.phi_mut().export_inequality_witnesses_for_summary(
            summary_atoms.iter(),
            keep,
            witness_targets,
        );
        let mut keep_with_witnesses = keep.clone();
        keep_with_witnesses.extend(witness_vars.iter().copied());

        let new_eqs = self
            .phi_mut()
            .simplify_with_new_eqs_for_targets(&keep_with_witnesses, eq_zero_targets);
        self.phi_mut()
            .drop_atoms_involving_or_restricted(&witness_vars);

        // Cross-ref: OCaml `PulseFormula.DeadVariables.eliminate` filters
        // `formula.conditions` against `closed_prunable_vars`, the formula-graph
        // closure of `precondition_vocabulary` — *not* against `keep`. Conditions
        // mentioning post-only values (e.g. a callee-local malloc return that
        // is heap-reachable from a formal but not formula-reachable from the
        // precondition) are dropped by OCaml because the caller has no way to
        // influence them. Filtering by `keep` (the formula-reachable post set)
        // would retain those conditions and surface them as `cond:0 < v` in
        // exported summaries while OCaml only keeps the corresponding
        // `phi.atoms` entry. This was Cluster F in the C-suite triage
        // (memory_leak.c, arithmetic.c).
        let condition_vocabulary_reprs: HashSet<_> = precondition_vocabulary
            .iter()
            .map(|v| self.phi.get_repr(*v))
            .collect();
        let mut conditions: BTreeMap<Atom, usize> = BTreeMap::new();
        for (atom, depth) in rewritten_conditions {
            if atom.is_trivially_true() == Some(true) {
                continue;
            }
            if atom
                .all_vars()
                .into_iter()
                .all(|v| condition_vocabulary_reprs.contains(&self.phi.get_repr(v)))
            {
                match conditions.get_mut(&atom) {
                    Some(existing_depth) => *existing_depth = (*existing_depth).min(depth),
                    None => {
                        conditions.insert(atom, depth);
                    }
                }
            }
        }

        self.conditions = conditions;
        new_eqs
    }

    pub(crate) fn replace_conditions(&mut self, conditions: BTreeMap<Atom, usize>) {
        self.conditions = conditions;
    }

    /// Forget summary-only constraints that mention the given values.
    ///
    /// Cross-ref: OCaml summary export can drop later caller-controlled guards
    /// when publishing an earlier `PotentialInvalidAccessSummary` obligation.
    /// Keep the heap shape, but erase pure constraints on the forgotten roots
    /// so the exported summary only retains the selected access prefix.
    pub fn forget_constraints_involving(&mut self, ignored: &HashSet<AbstractValue>) {
        if ignored.is_empty() {
            return;
        }

        let ignored_reprs: HashSet<_> = ignored
            .iter()
            .map(|addr| self.phi.get_repr(*addr))
            .collect();

        self.conditions.retain(|atom, _depth| {
            atom.all_vars()
                .into_iter()
                .all(|v| !ignored_reprs.contains(&self.phi.get_repr(v)))
        });
        self.phi_mut().forget_constraints_involving(&ignored_reprs);
    }

    /// Forget summary-only constraints that mention the given values while
    /// preserving `is_int` facts on those values.
    pub fn forget_non_type_constraints_involving(&mut self, ignored: &HashSet<AbstractValue>) {
        if ignored.is_empty() {
            return;
        }

        let ignored_reprs: HashSet<_> = ignored
            .iter()
            .map(|addr| self.phi.get_repr(*addr))
            .collect();

        self.conditions.retain(|atom, _depth| {
            atom.all_vars()
                .into_iter()
                .all(|v| !ignored_reprs.contains(&self.phi.get_repr(v)))
        });
        self.phi_mut()
            .forget_non_type_constraints_involving(&ignored_reprs);
    }

    fn record_condition_if_meaningful(&mut self, atom: Atom, depth: usize) {
        if atom.is_trivially_true() == Some(true) {
            return;
        }
        match self.conditions.get_mut(&atom) {
            Some(existing_depth) => *existing_depth = (*existing_depth).min(depth),
            None => {
                self.conditions.insert(atom, depth);
            }
        }
    }

    fn normalize_condition_atom(&self, atom: &Atom) -> Option<Atom> {
        let normalized = self.phi.normalize_condition_atom(atom);
        if normalized.is_trivially_true() == Some(true) {
            None
        } else {
            Some(normalized)
        }
    }

    fn enforce_condition_atom(&mut self, atom: &Atom) -> SatUnsat<Vec<NewEq>> {
        match atom {
            Atom::Equal(Term::Var(v1), Term::Var(v2)) => self.phi_mut().and_var_equal(*v1, *v2),
            Atom::Equal(Term::Var(v), Term::Const(c))
            | Atom::Equal(Term::Const(c), Term::Var(v)) => self.phi_mut().and_const_eq(*v, *c),
            Atom::LessEqual(lhs, rhs) => {
                if let (Some(op1), Some(op2)) =
                    (simple_operand_of_term(lhs), simple_operand_of_term(rhs))
                {
                    if self
                        .check_citv_binop(false, &sil::binop::Binop::Le, &op1, &op2)
                        .is_unsat()
                    {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi_mut().and_atom(atom.clone())
            }
            Atom::LessThan(lhs, rhs) => {
                if let (Some(op1), Some(op2)) =
                    (simple_operand_of_term(lhs), simple_operand_of_term(rhs))
                {
                    if self
                        .check_citv_binop(false, &sil::binop::Binop::Lt, &op1, &op2)
                        .is_unsat()
                    {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi_mut().and_atom(atom.clone())
            }
            _ => self.phi_mut().and_atom(atom.clone()),
        }
    }
}

/// Get the known constant value of an operand, if any.
/// Convert a comparison binop to an atom constraint.
/// If `negated`, produce the negation of the comparison.
fn comparison_to_atom(
    op: sil::binop::Binop,
    lhs: &Operand,
    rhs: &Operand,
    negated: bool,
    phi: &phi::Phi,
) -> Option<Atom> {
    use sil::binop::Binop;
    let effective_op = if negated {
        // Negate the comparison
        match op {
            Binop::Eq => Binop::Ne,
            Binop::Ne => Binop::Eq,
            Binop::Lt => Binop::Ge,
            Binop::Ge => Binop::Lt,
            Binop::Gt => Binop::Le,
            Binop::Le => Binop::Gt,
            _ => return None,
        }
    } else {
        op
    };
    let lt = operand_to_term(lhs, phi);
    let rt = operand_to_term(rhs, phi);
    match effective_op {
        Binop::Eq => Some(Atom::Equal(lt, rt)),
        Binop::Ne => Some(Atom::NotEqual(lt, rt)),
        Binop::Lt => Some(Atom::LessThan(lt, rt)),
        Binop::Le => Some(Atom::LessEqual(lt, rt)),
        Binop::Gt => Some(Atom::LessThan(rt, lt)),
        Binop::Ge => Some(Atom::LessEqual(rt, lt)),
        _ => None,
    }
}

fn result_of_binop_is_integer(
    op: &sil::binop::Binop,
    _lhs: &Operand,
    _rhs: &Operand,
    _phi: &phi::Phi,
) -> bool {
    use sil::binop::Binop;

    matches!(
        op,
        Binop::Eq | Binop::Ne | Binop::Lt | Binop::Gt | Binop::Le | Binop::Ge
    )
}

fn simple_operand_of_term(term: &Term) -> Option<Operand> {
    match term {
        Term::Var(v) => Some(Operand::AbstractValue(*v)),
        Term::Const(c) => Some(Operand::ConstOperand(*c)),
        _ => None,
    }
}

fn operand_const(op: &Operand, phi: &phi::Phi) -> Option<i64> {
    match op {
        Operand::ConstOperand(c) => Some(*c),
        Operand::AbstractValue(v) => phi.get_known_const(*v).map(|q| *q.numer() / *q.denom()),
    }
}

/// Get the rational (Q) constant value of an operand, if known.
fn operand_q(op: &Operand, phi: &phi::Phi) -> Option<Q> {
    match op {
        Operand::ConstOperand(c) => Some(Q::from_integer(*c)),
        Operand::AbstractValue(v) => phi.get_known_const(*v),
    }
}

/// Get the CItv interval for an operand.
/// Cross-ref: OCaml PulseFormula.ml interval_of_operand.
fn operand_interval(op: &Operand, phi: &phi::Phi) -> Option<citv::CItv> {
    match op {
        Operand::ConstOperand(c) => Some(citv::CItv::equal_to(*c)),
        Operand::AbstractValue(v) => phi.get_interval(*v).cloned(),
    }
}

fn operand_to_term(op: &Operand, phi: &phi::Phi) -> Term {
    match op {
        Operand::AbstractValue(v) => Term::Var(phi.get_repr(*v)),
        Operand::ConstOperand(c) => Term::Const(*c),
    }
}

fn operand_to_lin(op: &Operand, phi: &phi::Phi) -> LinArith {
    match op {
        Operand::AbstractValue(v) => LinArith::of_var(phi.get_repr(*v)),
        Operand::ConstOperand(c) => LinArith::of_int(*c),
    }
}

impl std::fmt::Display for Formula {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        for (&v, lin) in &self.phi.linear_eqs {
            parts.push(format!("{v} = {lin}"));
        }
        for atom in &self.phi.atoms {
            parts.push(format!("{atom}"));
        }
        if parts.is_empty() {
            write!(f, "true")
        } else {
            write!(f, "{}", parts.join(" ∧ "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ttrue() {
        let f = Formula::ttrue();
        let v = AbstractValue::of_raw(1);
        assert!(!f.is_known_zero(v));
    }

    #[test]
    fn test_equal_const_zero() {
        let mut f = Formula::ttrue();
        let v = AbstractValue::of_raw(1);
        let result = f.and_equal_const(v, 0);
        assert!(result.is_sat());
        assert!(f.is_known_zero(v));
    }

    #[test]
    fn test_equal_vars_propagates_const() {
        let mut f = Formula::ttrue();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        f.and_equal_const(v1, 0);
        f.and_equal_vars(v1, v2);
        assert!(f.is_known_zero(v2));
    }

    #[test]
    fn test_contradiction_different_constants() {
        let mut f = Formula::ttrue();
        let v = AbstractValue::of_raw(1);

        f.and_equal_const(v, 0);
        let result = f.and_equal_const(v, 42);
        assert!(result.is_unsat());
    }

    #[test]
    fn test_contradiction_via_vars() {
        let mut f = Formula::ttrue();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        f.and_equal_const(v1, 0);
        f.and_equal_const(v2, 42);
        let result = f.and_equal_vars(v1, v2);
        assert!(result.is_unsat());
    }

    #[test]
    fn test_not_equal_contradiction() {
        let mut f = Formula::ttrue();
        let v = AbstractValue::of_raw(1);

        f.and_equal_const(v, 0);
        let result = f.and_not_equal(&Operand::AbstractValue(v), &Operand::ConstOperand(0));
        assert!(result.is_unsat());
    }

    #[test]
    fn test_prune_null_check() {
        let mut f = Formula::ttrue();
        let p = AbstractValue::of_raw(1);
        let null_val = AbstractValue::of_raw(2);

        f.and_equal_const(null_val, 0);

        // True branch: p ≠ null → p is not null
        let mut f_true = f.clone();
        let result = f_true.prune_eq(p, null_val, true);
        assert!(result.is_sat());
        assert!(!f_true.is_known_zero(p));

        // False branch: p = null → p IS null
        let mut f_false = f.clone();
        let result = f_false.prune_eq(p, null_val, false);
        assert!(result.is_sat());
        assert!(f_false.is_known_zero(p));
    }

    #[test]
    fn test_prune_records_depth_zero_condition() {
        let mut f = Formula::ttrue();
        let p = AbstractValue::of_raw(1);

        let result = f.prune_eq_const(p, 0, true);
        assert!(result.is_sat());
        assert_eq!(
            f.conditions()
                .get(&Atom::NotEqual(Term::Var(p), Term::Const(0))),
            Some(&0)
        );
    }

    #[test]
    fn test_simplify_drops_unreachable_conditions() {
        let mut f = Formula::ttrue();
        let p = AbstractValue::of_raw(1);

        let result = f.prune_eq_const(p, 0, true);
        assert!(result.is_sat());
        f.simplify(&HashSet::new());

        assert!(
            f.conditions().is_empty(),
            "conditions on dead values should not survive summary simplification"
        );
    }

    #[test]
    fn test_simplify_keeps_reachable_conditions_even_if_implied_by_phi() {
        let mut f = Formula::ttrue();
        let p = AbstractValue::of_raw(1);

        let result = f.prune_eq_const(p, 0, false);
        assert!(result.is_sat());
        f.simplify(&HashSet::from([p]));

        assert_eq!(
            f.conditions().len(),
            1,
            "reachable prune conditions must survive simplification for manifestness checks"
        );
    }

    #[test]
    fn test_simplify_keeps_original_nontrivial_local_condition_shape() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        let result = f.prune_eq(x, y, false);
        assert!(result.is_sat());
        f.simplify(&HashSet::from([x]));

        assert!(
            f.conditions()
                .contains_key(&Atom::Equal(Term::Var(x), Term::Var(y))),
            "conditions should keep their original variables instead of collapsing to x = x"
        );
    }

    #[test]
    fn test_simplify_for_summary_drops_redundant_non_precondition_equality() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        assert!(f.prune_eq(x, y, false).is_sat());
        f.simplify_for_summary(&HashSet::from([x]), &HashSet::from([x]));

        assert!(
            f.conditions().is_empty(),
            "summary simplification should drop equality conditions that only mention dead callee-local aliases"
        );
    }

    #[test]
    fn test_simplify_for_summary_rewrites_dead_const_guard_to_visible_alias() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        assert!(f.prune_eq_const(y, 0, false).is_sat());
        assert!(f.and_equal_vars(x, y).is_sat());
        f.simplify_for_summary(&HashSet::from([x]), &HashSet::from([x]));

        assert_eq!(
            f.conditions().get(&Atom::Equal(Term::Var(x), Term::Const(0))),
            Some(&0),
            "summary simplification should rewrite dead condition vars onto the visible precondition alias instead of erasing the caller-controlled guard"
        );
    }

    #[test]
    fn test_simplify_for_summary_rewrites_dead_linear_guard_to_visible_operands() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let neg_x = AbstractValue::of_raw(2);

        assert!(f
            .and_equal_linear(neg_x, LinArith::of_var(x).neg())
            .is_sat());
        assert!(f
            .and_condition_direct(Atom::Equal(Term::Var(neg_x), Term::Const(0)), 1)
            .is_sat());
        f.simplify_for_summary(&HashSet::from([x, neg_x]), &HashSet::from([x]));

        let only_condition = f
            .conditions()
            .keys()
            .next()
            .expect("expected the imported linear guard to survive summary simplification");
        let condition_vars: HashSet<_> = only_condition.all_vars().into_iter().collect();
        assert_eq!(
            condition_vars,
            HashSet::from([x]),
            "summary simplification should rewrite dead arithmetic temps back to visible operands"
        );
        assert_ne!(
            only_condition.is_trivially_true(),
            Some(true),
            "summary simplification should not erase caller-controlled linear guards"
        );
    }

    #[test]
    fn test_simplify_for_summary_drops_conditions_outside_precondition_vocabulary() {
        // Cluster F repro: caller-invisible callee-local witness conditions
        // (e.g. the `0 < ret` recorded by `free`'s prune_positive on a value
        // that escaped via a heap edge but whose only formula-graph link is
        // through dead vars) must be dropped from the exported summary's
        // `conditions`.
        let mut f = Formula::ttrue();
        let formal = AbstractValue::of_raw(1);
        let post_only = AbstractValue::of_raw(2);

        // Witness atom recorded in BOTH phi and conditions, mirroring how
        // OCaml's `prune_binop` populates phi.atoms via `prune_atoms` and
        // conditions via `add_condition`.
        assert!(f
            .prune_less_than(
                &Operand::ConstOperand(0),
                &Operand::AbstractValue(post_only)
            )
            .is_sat());

        // `formula_reachable` (keep) sees `post_only` because some heap edge
        // pulls it in; `precondition_vocabulary` does not because the formula
        // graph never connects it to the formal.
        let precondition_vocabulary = HashSet::from([formal]);
        let keep = HashSet::from([formal, post_only]);
        f.simplify_for_summary(&precondition_vocabulary, &keep);

        assert!(
            !f.conditions()
                .contains_key(&Atom::LessThan(Term::Const(0), Term::Var(post_only))),
            "conditions on values outside the precondition-vocabulary closure should be dropped, \
             matching OCaml PulseFormula.DeadVariables.eliminate's `closed_prunable_vars` filter"
        );
        assert!(
            f.phi()
                .atoms
                .contains(&Atom::LessThan(Term::Const(0), Term::Var(post_only))),
            "pure positive atoms without a restricted/tableau witness should survive summary export"
        );
    }

    #[test]
    fn test_simplify_for_summary_exports_restricted_witness_for_le_guard() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);

        assert!(f
            .prune_less_equal(&Operand::AbstractValue(x), &Operand::ConstOperand(5))
            .is_sat());
        f.simplify_for_summary(&HashSet::from([x]), &HashSet::from([x]));

        let lin = f
            .phi()
            .linear_eqs
            .get(&x)
            .expect("visible inequality should export an affine witness equality");
        assert_eq!(lin.constant, Q::from_integer(5));
        assert_eq!(lin.vars.len(), 1);
        let (witness, coeff) = lin.vars.iter().next().unwrap();
        assert!(witness.is_restricted());
        assert_eq!(*coeff, Q::from_integer(-1));
    }

    #[test]
    fn test_simplify_for_summary_exports_restricted_witness_for_strict_gt_guard() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);

        assert!(f
            .prune_less_than(&Operand::ConstOperand(5), &Operand::AbstractValue(x))
            .is_sat());
        f.simplify_for_summary(&HashSet::from([x]), &HashSet::from([x]));

        let lin = f
            .phi()
            .linear_eqs
            .get(&x)
            .expect("strict visible inequality should export an affine witness equality");
        assert_eq!(lin.constant, Q::from_integer(6));
        assert_eq!(lin.vars.len(), 1);
        let (witness, coeff) = lin.vars.iter().next().unwrap();
        assert!(witness.is_restricted());
        assert_eq!(*coeff, Q::from_integer(1));
    }

    #[test]
    fn test_simplify_for_summary_does_not_export_witness_on_formula_only_temp() {
        let mut f = Formula::ttrue();
        let i = AbstractValue::of_raw(1);
        let recursive_temp = AbstractValue::of_raw(2);

        assert!(f
            .and_equal_linear(
                i,
                LinArith::of_var(recursive_temp).add(&LinArith::of_int(2))
            )
            .is_sat());
        assert!(f
            .prune_less_than(
                &Operand::ConstOperand(0),
                &Operand::AbstractValue(recursive_temp),
            )
            .is_sat());
        f.simplify_for_summary_with_witness_targets(
            &HashSet::from([i]),
            &HashSet::from([i, recursive_temp]),
            &HashSet::from([i]),
        );

        assert!(
            !f.phi().linear_eqs.contains_key(&recursive_temp),
            "formula-only temps kept alive through an exported affine equality must not receive a \
             synthesized restricted witness"
        );
        let lin = f
            .phi()
            .linear_eqs
            .get(&i)
            .expect("visible affine equality should remain exported");
        assert_eq!(
            lin.get_coefficient(recursive_temp),
            Some(&Q::from_integer(1)),
            "the existing visible equality should still keep the temp available for canonicalization"
        );
    }

    #[test]
    fn test_forget_constraints_involving_drops_conditions_and_phi_facts() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        assert!(f.prune_eq_const(x, 0, false).is_sat());
        assert!(f
            .prune_less_than(&Operand::ConstOperand(0), &Operand::AbstractValue(y))
            .is_sat());

        f.forget_constraints_involving(&HashSet::from([y]));

        assert_eq!(
            f.conditions()
                .get(&Atom::Equal(Term::Var(x), Term::Const(0))),
            Some(&0)
        );
        assert!(
            !f.conditions()
                .contains_key(&Atom::LessThan(Term::Const(0), Term::Var(y))),
            "forgotten roots should not keep remembered summary conditions"
        );
        assert!(
            !f.phi()
                .atoms
                .contains(&Atom::LessThan(Term::Const(0), Term::Var(y))),
            "forgotten roots should not keep pure phi atoms either"
        );
    }

    #[test]
    fn test_and_condition_direct_drops_imported_tautology_after_normalization() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        assert!(f.and_equal_vars(x, y).is_sat());
        assert!(f
            .and_condition_direct(Atom::Equal(Term::Var(x), Term::Var(y)), 1)
            .is_sat());

        assert!(
            f.conditions().is_empty(),
            "imported conditions already implied by phi should not stay as tautological remembered conditions"
        );
    }

    #[test]
    fn test_and_condition_direct_normalizes_imported_condition_to_known_constant() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        assert!(f.and_equal_const(y, 0).is_sat());
        assert!(f
            .and_condition_direct(Atom::Equal(Term::Var(x), Term::Var(y)), 2)
            .is_sat());

        assert_eq!(
            f.conditions()
                .get(&Atom::Equal(Term::Var(x), Term::Const(0))),
            Some(&2),
            "imported conditions should be remembered in normalized caller-space form"
        );
    }

    #[test]
    fn test_linear_arithmetic() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        // x = y + 1
        let lin = LinArith::of_var(y).add(&LinArith::of_int(1));
        f.and_equal_linear(x, lin);

        // y = 0 → x = 1
        f.and_equal_const(y, 0);
        assert_eq!(f.is_known_const(x), Some(Q::from_integer(1)));
    }

    #[test]
    fn test_integer_var_rejects_non_integer_constant_solution() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);

        f.and_is_int(x);
        let result = f.and_equal_linear(x, LinArith::of_q(Q::new(1, 2)));

        assert!(
            result.is_unsat(),
            "integer-typed variables should reject non-integer constant solutions"
        );
    }

    #[test]
    fn test_binop_plus() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let z = AbstractValue::of_raw(3);

        // z = x + y
        f.and_equal_binop(
            z,
            sil::binop::Binop::PlusA(None),
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );

        // x = 3, y = 4 → z = 7
        f.and_equal_const(x, 3);
        f.and_equal_const(y, 4);
        assert_eq!(f.is_known_const(z), Some(Q::from_integer(7)));
    }

    #[test]
    fn test_and_equal_binop_reuses_direct_term_value_index_hit() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let first_result = AbstractValue::of_raw(3);
        let second_result = AbstractValue::of_raw(4);
        let op = sil::binop::Binop::PlusA(None);

        assert!(f
            .and_equal_binop(
                first_result,
                op.clone(),
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert!(f
            .and_equal_binop(
                second_result,
                op,
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());

        assert_eq!(
            f.get_var_repr(second_result),
            f.get_var_repr(first_result),
            "repeated BinOps on the same canonical operands should reuse the first result"
        );
    }

    #[test]
    fn test_stale_term_value_index_after_alias_mints_fresh_consistent_value() {
        let mut f = Formula::ttrue();
        let alias = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let x = AbstractValue::of_raw(10);
        let first_result = AbstractValue::of_raw(20);
        let second_result = AbstractValue::of_raw(21);
        let op = sil::binop::Binop::Lt;

        assert!(f
            .and_equal_binop(
                first_result,
                op.clone(),
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert!(f.and_equal_vars(x, alias).is_sat());
        assert_eq!(
            f.get_var_repr(x),
            alias,
            "the alias must become x's representative to make the old term_value_index key stale"
        );

        assert!(f
            .and_equal_binop(
                second_result,
                op,
                &Operand::AbstractValue(alias),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert_ne!(
            f.get_var_repr(second_result),
            f.get_var_repr(first_result),
            "stale alias misses should not repair/reuse the old term_value_index entry"
        );

        assert!(f.prune_eq_const(first_result, 0, true).is_sat());
        assert!(f.prune_eq_const(second_result, 0, true).is_sat());
        assert!(
            f.conditions()
                .contains_key(&Atom::LessThan(Term::Var(alias), Term::Var(y))),
            "both stale and fresh comparison values should prune to the same consistent alias < y fact"
        );
    }

    #[test]
    fn test_stale_term_value_index_after_constant_folding_mints_fresh_consistent_value() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let first_result = AbstractValue::of_raw(3);
        let second_result = AbstractValue::of_raw(4);
        let op = sil::binop::Binop::Lt;

        assert!(f
            .and_equal_binop(
                first_result,
                op.clone(),
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert!(f.and_equal_const(x, 3).is_sat());

        assert!(f
            .and_equal_binop(
                second_result,
                op,
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert_ne!(
            f.get_var_repr(second_result),
            f.get_var_repr(first_result),
            "stale constant-folding misses should not repair/reuse the old term_value_index entry"
        );

        assert!(f.prune_eq_const(first_result, 0, true).is_sat());
        assert!(f.prune_eq_const(second_result, 0, true).is_sat());
        assert!(
            f.conditions()
                .contains_key(&Atom::LessThan(Term::Const(3), Term::Var(y))),
            "both stale and fresh comparison values should prune to the same consistent 3 < y fact"
        );
    }

    #[test]
    fn test_prune_cached_comparison_refines_operands_via_canonical_term_eq() {
        // Build two `Lt` comparisons on the same canonical operands.
        // The second `and_equal_binop` should hit the `term_value_index`
        // cache and unify its result with the first comparison's result.
        // Pruning the cached result must still resolve back to the
        // original comparison's `term_eqs` entry (via the canonical repr)
        // so the operands get refined just like the first prune would.
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let first_cmp = AbstractValue::of_raw(3);
        let second_cmp = AbstractValue::of_raw(4);
        let op = sil::binop::Binop::Lt;

        assert!(f
            .and_equal_binop(
                first_cmp,
                op.clone(),
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert!(f
            .and_equal_binop(
                second_cmp,
                op,
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());

        // Sanity: cache hit unified the two comparison results.
        assert_eq!(
            f.get_var_repr(second_cmp),
            f.get_var_repr(first_cmp),
            "repeated comparison should reuse the cached representative"
        );
        // Sanity: term_eqs is only keyed under the canonical repr (the
        // first comparison's value), not under the freshly minted second
        // result. Union-find keeps the lower raw id as representative, so
        // first_cmp is the stable representative here.
        let cmp_repr = f.get_var_repr(first_cmp);
        assert_eq!(
            cmp_repr, first_cmp,
            "the first comparison should be the representative that owns the term_eq"
        );
        assert!(
            f.phi().term_eqs.contains_key(&cmp_repr),
            "term_eqs should still be keyed under the canonical comparison value"
        );
        assert_ne!(
            cmp_repr, second_cmp,
            "the test only exercises the canonical lookup if the second result is not itself the repr"
        );

        // Prune the cached (second) result truthy: x < y should follow.
        let truthy = f.prune_eq_const(second_cmp, 0, true);
        assert!(
            truthy.is_sat(),
            "pruning the cached comparison truthy must remain Sat"
        );
        assert!(
            f.conditions()
                .contains_key(&Atom::LessThan(Term::Var(x), Term::Var(y))),
            "pruning the cached comparison result truthy must record the original `x < y` refinement, \
             not collapse to a generic `v != 0` atom"
        );
    }

    #[test]
    fn test_prune_comparison_keeps_direct_term_eq_after_result_merge() {
        // Direct lookup must still win when a comparison value keeps its
        // term_eq entry under the original key but is later merged into a
        // simpler representative.
        let mut f = Formula::ttrue();
        let alias = AbstractValue::of_raw(1);
        let cmp = AbstractValue::of_raw(10);
        let x = AbstractValue::of_raw(20);
        let y = AbstractValue::of_raw(21);

        assert!(f
            .and_equal_binop(
                cmp,
                sil::binop::Binop::Lt,
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());
        assert!(
            f.phi().term_eqs.contains_key(&cmp),
            "the comparison's term_eq starts under the raw comparison value"
        );

        assert!(f.and_equal_vars(cmp, alias).is_sat());
        assert_eq!(
            f.get_var_repr(cmp),
            alias,
            "union-find keeps the lower raw id as the representative, so pruning cmp exercises the direct term_eq lookup"
        );

        let truthy = f.prune_eq_const(cmp, 0, true);
        assert!(
            truthy.is_sat(),
            "pruning the merged comparison truthy must remain Sat"
        );
        assert!(
            f.conditions()
                .contains_key(&Atom::LessThan(Term::Var(x), Term::Var(y))),
            "direct term_eq lookup should preserve the original comparison refinement"
        );
    }

    #[test]
    fn test_prune_eq_const_nonzero_skips_comparison_term_eq() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let cmp = AbstractValue::of_raw(3);

        assert!(f
            .and_equal_binop(
                cmp,
                sil::binop::Binop::Lt,
                &Operand::AbstractValue(x),
                &Operand::AbstractValue(y),
            )
            .is_sat());

        let result = f.prune_eq_const(cmp, 1, false);
        assert!(
            result.is_sat(),
            "pruning comparison result to a non-zero const should stay Sat"
        );
        assert!(
            f.conditions()
                .contains_key(&Atom::Equal(Term::Var(cmp), Term::Const(1))),
            "non-zero constants should use the generic equality condition rather than the comparison term_eq path"
        );
        assert!(
            !f.conditions()
                .contains_key(&Atom::LessThan(Term::Var(x), Term::Var(y))),
            "the comparison term_eq path should only run for zero pruning"
        );
    }

    #[test]
    fn test_pure_int_fn_app_collision_makes_odd_doubled_sum_unsat() {
        let mut f = Formula::ttrue();
        let x1 = AbstractValue::of_raw(1);
        let x2 = AbstractValue::of_raw(2);
        let sum = AbstractValue::of_raw(3);

        f.and_is_int(x1);
        let first = f.and_fn_app(x1, "pure_offset", &[]);
        assert!(first.is_sat());

        f.and_is_int(x2);
        let second = f.and_fn_app(x2, "pure_offset", &[]);
        assert!(
            second.is_sat(),
            "repeated pure calls should unify, not fail immediately"
        );
        assert_eq!(
            f.get_var_repr(x2),
            f.get_var_repr(x1),
            "repeated pure calls with identical actuals should unify return values"
        );
        assert!(
            f.phi().is_marked_int(x1),
            "the canonical pure-call result should remain marked integer-typed"
        );

        let sum_eq = f.and_equal_binop(
            sum,
            sil::binop::Binop::PlusA(None),
            &Operand::AbstractValue(x1),
            &Operand::AbstractValue(x2),
        );
        assert!(sum_eq.is_sat());

        let odd = f.prune_eq_const(sum, 1, false);
        assert!(
            odd.is_unsat(),
            "if pure_offset() is int and pure, pure_offset()+pure_offset()==1 is impossible"
        );
    }

    #[test]
    fn test_binop_minus() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let z = AbstractValue::of_raw(3);

        // z = x - y
        f.and_equal_binop(
            z,
            sil::binop::Binop::MinusA(None),
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );

        // x = 10, y = 3 → z = 7
        f.and_equal_const(x, 10);
        f.and_equal_const(y, 3);
        assert_eq!(f.is_known_const(z), Some(Q::from_integer(7)));
    }

    #[test]
    fn test_singleton_interval_propagates_back_through_minus_linear_eq() {
        let mut f = Formula::ttrue();
        let i = AbstractValue::of_raw(1);
        let i_minus_one = AbstractValue::of_raw(2);

        f.and_is_int(i);
        f.and_is_int(i_minus_one);
        assert!(f.and_positive(i).is_sat());
        assert!(f
            .and_equal_binop(
                i_minus_one,
                sil::binop::Binop::MinusA(None),
                &Operand::AbstractValue(i),
                &Operand::ConstOperand(1),
            )
            .is_sat());
        assert!(f
            .and_less_equal(
                &Operand::AbstractValue(i_minus_one),
                &Operand::ConstOperand(0)
            )
            .is_sat());

        assert_eq!(
            f.is_known_const(i_minus_one),
            Some(Q::from_integer(0)),
            "the recursive actual should collapse to zero on the base branch"
        );
        assert_eq!(
            f.is_known_const(i),
            Some(Q::from_integer(1)),
            "singleton intervals should feed back through x = (i - 1) so the caller-visible i becomes 1"
        );
    }

    #[test]
    fn test_binop_comparison_with_constants() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let r = AbstractValue::of_raw(3);

        // r = (x == y) where x=5, y=5 → r=1
        f.and_equal_const(x, 5);
        f.and_equal_const(y, 5);
        f.and_equal_binop(
            r,
            sil::binop::Binop::Eq,
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );
        assert_eq!(f.is_known_const(r), Some(Q::from_integer(1)));
    }

    #[test]
    fn test_binop_comparison_non_constant() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let r = AbstractValue::of_raw(3);

        // r = (x == y) where x and y are unknown → r is unconstrained
        let result = f.and_equal_binop(
            r,
            sil::binop::Binop::Eq,
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );
        assert!(result.is_sat(), "non-constant comparison should be Sat");
        assert_eq!(f.is_known_const(r), None, "result should be unconstrained");
        assert!(
            f.phi().is_marked_int(r),
            "comparison results should stay integer-typed even before pruning"
        );
    }

    #[test]
    fn test_binop_mod_constant() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let r = AbstractValue::of_raw(3);

        // r = 4 % 4 → r = 0
        f.and_equal_const(x, 4);
        f.and_equal_const(y, 4);
        f.and_equal_binop(
            r,
            sil::binop::Binop::Mod,
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );
        assert_eq!(f.is_known_const(r), Some(Q::from_integer(0)));
    }

    #[test]
    fn test_binop_div_constant() {
        let mut f = Formula::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let r = AbstractValue::of_raw(3);

        // r = 10 / 3 → r = 3 (integer division)
        f.and_equal_const(x, 10);
        f.and_equal_const(y, 3);
        f.and_equal_binop(
            r,
            sil::binop::Binop::DivI,
            &Operand::AbstractValue(x),
            &Operand::AbstractValue(y),
        );
        assert_eq!(f.is_known_const(r), Some(Q::from_integer(3)));
    }
}
