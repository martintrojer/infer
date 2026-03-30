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

use std::collections::HashSet;

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Formula {
    phi: phi::Phi,
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

    /// Add an atom constraint directly (for formula translation from callee).
    pub fn and_atom_direct(&mut self, atom: Atom) -> SatUnsat<Vec<NewEq>> {
        self.phi.and_atom(atom)
    }

    /// Get the canonical representative of a variable.
    pub fn get_var_repr(&self, v: AbstractValue) -> AbstractValue {
        self.phi.get_repr(v)
    }

    /// Check if a variable is known to equal zero.
    pub fn is_known_zero(&self, v: AbstractValue) -> bool {
        self.phi.is_known_zero(v)
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
                self.phi.and_var_equal(*v1, *v2)
            }
            (Operand::AbstractValue(v), Operand::ConstOperand(c))
            | (Operand::ConstOperand(c), Operand::AbstractValue(v)) => {
                self.phi.and_const_eq(*v, *c)
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
        self.phi.and_var_equal(v1, v2)
    }

    /// Record that a variable equals a constant.
    pub fn and_equal_const(&mut self, v: AbstractValue, c: i64) -> SatUnsat<Vec<NewEq>> {
        self.phi.and_const_eq(v, c)
    }

    /// Record a pure function application: ret_val = f(actuals).
    /// If the same function with same actuals was seen before, unify return values.
    pub fn and_fn_app(
        &mut self,
        ret_val: AbstractValue,
        callee: &str,
        actuals: &[AbstractValue],
    ) -> SatUnsat<Vec<NewEq>> {
        self.phi.and_fn_app(ret_val, callee, actuals)
    }

    /// Record that a variable equals a linear expression.
    pub fn and_equal_linear(&mut self, v: AbstractValue, lin: LinArith) -> SatUnsat<Vec<NewEq>> {
        self.phi.and_linear_eq(v, lin)
    }

    /// Record that two operands are NOT equal.
    pub fn and_not_equal(&mut self, op1: &Operand, op2: &Operand) -> SatUnsat<Vec<NewEq>> {
        let t1 = operand_to_term(op1, &self.phi);
        let t2 = operand_to_term(op2, &self.phi);
        self.phi.and_atom(Atom::NotEqual(t1, t2))
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
        self.phi.and_atom(Atom::LessEqual(t1, t2))
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
        self.phi.and_atom(Atom::LessThan(t1, t2))
    }

    /// Mark a variable as integer-typed. When the linear solver later
    /// derives a non-integer rational for this variable, the path is Unsat.
    /// Cross-ref: OCaml PulseFormula.ml and_is_int.
    pub fn and_is_int(&mut self, v: AbstractValue) {
        self.phi.mark_is_int(v);
    }

    /// Add a prune constraint (from a branch condition).
    /// Cross-ref: OCaml PulseFormula.ml prune_binop checks CItv first.
    pub fn prune_eq(
        &mut self,
        v1: AbstractValue,
        v2: AbstractValue,
        negated: bool,
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
        if negated {
            self.and_not_equal(&op1, &op2)
        } else {
            self.phi.and_var_equal(v1, v2)
        }
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
                    if self.phi.add_interval(*v, better).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                if let (Some(better), Operand::AbstractValue(v)) = (refined2, op2) {
                    if self.phi.add_interval(*v, better).is_unsat() {
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
        if c == 0 {
            if let Some(teq) = self.phi.term_eqs.get(&v).cloned() {
                let atom = if negated {
                    // prune(v ≠ 0) i.e. v is truthy → the comparison is true
                    comparison_to_atom(teq.op, &teq.lhs, &teq.rhs, false)
                } else {
                    // prune(v = 0) i.e. v is falsy → the comparison is false (negate it)
                    comparison_to_atom(teq.op, &teq.lhs, &teq.rhs, true)
                };
                if let Some(atom) = atom {
                    if self.phi.and_atom(atom).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
            }
        }

        if negated {
            self.and_not_equal(&Operand::AbstractValue(v), &Operand::ConstOperand(c))
        } else {
            self.phi.and_const_eq(v, c)
        }
    }

    /// Record that v = binop(x, y).
    pub fn and_equal_binop(
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
                    if self.phi.add_interval(v, result_itv).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi.and_linear_eq(v, lx.add(&ly))
            }
            sil::binop::Binop::MinusA(_)
            | sil::binop::Binop::MinusPI
            | sil::binop::Binop::MinusPP => {
                let lx = operand_to_lin(x, &self.phi);
                let ly = operand_to_lin(y, &self.phi);
                if let Some(result_itv) = operand_interval(x, &self.phi).and_then(|ix| {
                    operand_interval(y, &self.phi).and_then(|iy| citv::CItv::binop(&op, &ix, &iy))
                }) {
                    if self.phi.add_interval(v, result_itv).is_unsat() {
                        return SatUnsat::Unsat;
                    }
                }
                self.phi.and_linear_eq(v, lx.sub(&ly))
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
                    self.phi.and_const_eq(v, if cmp { 1 } else { 0 })
                } else {
                    // Record term equality: v = op(lhs, rhs).
                    // When pruning on v later, we can resolve back to the comparison.
                    self.phi.term_eqs.insert(
                        v,
                        phi::TermEq {
                            op,
                            lhs: x.clone(),
                            rhs: y.clone(),
                        },
                    );
                    let _ = self.phi.var_eqs.find(v);
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
                        return self.phi.and_linear_eq(v, lin);
                    }
                }
                let _ = self.phi.var_eqs.find(v);
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
                            let _ = self.phi.var_eqs.find(v);
                            return SatUnsat::Sat(Vec::new());
                        }
                    };
                    self.phi.and_const_eq(v, result)
                } else {
                    let _ = self.phi.var_eqs.find(v);
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
                    self.phi.and_const_eq(v, result)
                } else {
                    let _ = self.phi.var_eqs.find(v);
                    SatUnsat::Sat(Vec::new())
                }
            }
            _ => {
                // For remaining unsupported ops, just register v
                let _ = self.phi.var_eqs.find(v);
                SatUnsat::Sat(Vec::new())
            }
        }
    }

    /// Simplify the formula.
    pub fn simplify(&mut self, reachable: &HashSet<AbstractValue>) {
        self.phi.simplify(reachable);
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
) -> Option<Atom> {
    use sil::binop::Binop;
    let (effective_op, effective_negated) = if negated {
        // Negate the comparison
        match op {
            Binop::Eq => (Binop::Ne, false),
            Binop::Ne => (Binop::Eq, false),
            Binop::Lt => (Binop::Ge, false),
            Binop::Ge => (Binop::Lt, false),
            Binop::Gt => (Binop::Le, false),
            Binop::Le => (Binop::Gt, false),
            _ => return None,
        }
    } else {
        (op, false)
    };
    let _ = effective_negated; // negation already applied above
    let lt = match lhs {
        Operand::AbstractValue(v) => Term::Var(*v),
        Operand::ConstOperand(c) => Term::Const(*c),
    };
    let rt = match rhs {
        Operand::AbstractValue(v) => Term::Var(*v),
        Operand::ConstOperand(c) => Term::Const(*c),
    };
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
