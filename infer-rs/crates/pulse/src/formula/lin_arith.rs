// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Linear arithmetic over rationals.
//!
//! Mirrors OCaml's `PulseFormulaLinArit.ml`.
//!
//! A `LinArith` is a linear combination `c + a₁·v₁ + a₂·v₂ + ...` where
//! `c` is a rational constant and `aᵢ` are rational coefficients.
//! Invariant: no coefficient is zero.

use std::collections::BTreeMap;
use std::fmt;

use num_rational::Ratio;
use num_traits::{One, Zero};

use crate::abstract_value::AbstractValue;
use crate::sat_unsat::SatUnsat;

/// Rational number type (arbitrary precision).
pub type Q = Ratio<i64>;

/// A linear arithmetic expression: `constant + Σ(coeff_i * var_i)`.
///
/// Invariant: no coefficient in `vars` is zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinArith {
    pub constant: Q,
    /// Variable → coefficient map. Ordered by AbstractValue for determinism.
    /// Invariant: no zero coefficients.
    pub vars: BTreeMap<AbstractValue, Q>,
}

impl LinArith {
    /// The zero linear expression.
    pub fn zero() -> Self {
        Self {
            constant: Q::zero(),
            vars: BTreeMap::new(),
        }
    }

    /// A constant expression.
    pub fn of_q(q: Q) -> Self {
        Self {
            constant: q,
            vars: BTreeMap::new(),
        }
    }

    /// A constant integer expression.
    pub fn of_int(n: i64) -> Self {
        Self::of_q(Q::from_integer(n))
    }

    /// A single variable with coefficient 1.
    pub fn of_var(v: AbstractValue) -> Self {
        let mut vars = BTreeMap::new();
        vars.insert(v, Q::one());
        Self {
            constant: Q::zero(),
            vars,
        }
    }

    /// Check if this is the zero expression.
    pub fn is_zero(&self) -> bool {
        self.constant.is_zero() && self.vars.is_empty()
    }

    /// Get as a constant, if there are no variables.
    pub fn get_as_const(&self) -> Option<Q> {
        if self.vars.is_empty() {
            Some(self.constant)
        } else {
            None
        }
    }

    /// Get as a single variable (coefficient 1, constant 0).
    pub fn get_as_var(&self) -> Option<AbstractValue> {
        if !self.constant.is_zero() {
            return None;
        }
        if self.vars.len() != 1 {
            return None;
        }
        let (&v, coeff) = self.vars.iter().next().unwrap();
        if coeff.is_one() {
            Some(v)
        } else {
            None
        }
    }

    /// Get the constant part.
    pub fn get_constant_part(&self) -> &Q {
        &self.constant
    }

    /// Get the coefficient of a variable.
    pub fn get_coefficient(&self, v: AbstractValue) -> Option<&Q> {
        self.vars.get(&v)
    }

    /// Get the simplest variable (smallest by AbstractValue ordering).
    pub fn get_simplest(&self) -> Option<AbstractValue> {
        self.vars.keys().next().copied()
    }

    /// Addition: l₁ + l₂.
    pub fn add(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.constant += other.constant;
        for (&v, coeff) in &other.vars {
            let entry = result.vars.entry(v).or_insert_with(Q::zero);
            *entry += coeff;
            if entry.is_zero() {
                result.vars.remove(&v);
            }
        }
        result
    }

    /// Negation: -l.
    pub fn neg(&self) -> Self {
        Self {
            constant: -&self.constant,
            vars: self.vars.iter().map(|(&v, c)| (v, -c)).collect(),
        }
    }

    /// Subtraction: l₁ - l₂.
    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.neg())
    }

    /// Scalar multiplication: q · l.
    pub fn mult_scalar(&self, q: &Q) -> Self {
        if q.is_zero() {
            return Self::zero();
        }
        if q.is_one() {
            return self.clone();
        }
        Self {
            constant: self.constant * q,
            vars: self.vars.iter().map(|(&v, c)| (v, c * q)).collect(),
        }
    }

    /// Solve `self = 0` for the simplest variable.
    ///
    /// Returns `Some((x, l))` where `x = l` (the simplest variable solved
    /// in terms of the others), or `None` if the expression has no variables
    /// (in which case it's either trivially true or a contradiction).
    pub fn solve_eq_zero(&self) -> SatUnsat<Option<(AbstractValue, LinArith)>> {
        match self.vars.keys().next() {
            None => {
                if self.constant.is_zero() {
                    SatUnsat::Sat(None) // 0 = 0, trivially true
                } else {
                    SatUnsat::Unsat // c = 0 where c ≠ 0
                }
            }
            Some(&x) => {
                let coeff = self.vars[&x];
                let pivoted = self.pivot(x, &coeff);
                SatUnsat::Sat(Some((x, pivoted)))
            }
        }
    }

    /// Solve `self = other` by solving `self - other = 0`.
    pub fn solve_eq(&self, other: &Self) -> SatUnsat<Option<(AbstractValue, LinArith)>> {
        self.sub(other).solve_eq_zero()
    }

    /// Pivot: given `self` contains `x` with coefficient `coeff`,
    /// solve for `x` in terms of the other variables.
    ///
    /// If `self = ... + coeff·x + ...`, returns `x = -1/coeff · (self - coeff·x)`.
    fn pivot(&self, x: AbstractValue, coeff: &Q) -> Self {
        let neg_coeff = -coeff;
        let mut vars = BTreeMap::new();
        for (&v, c) in &self.vars {
            if v != x {
                vars.insert(v, c / neg_coeff);
            }
        }
        // Remove zero coefficients (maintain invariant)
        vars.retain(|_, c| !c.is_zero());
        Self {
            constant: self.constant / neg_coeff,
            vars,
        }
    }

    /// Substitute a variable with a linear expression.
    pub fn subst_var(&self, x: AbstractValue, replacement: &LinArith) -> Self {
        match self.vars.get(&x) {
            None => self.clone(),
            Some(q) => {
                let q = *q;
                let mut without_x = self.clone();
                without_x.vars.remove(&x);
                without_x.add(&replacement.mult_scalar(&q))
            }
        }
    }

    /// Get all variables in this expression.
    pub fn get_variables(&self) -> impl Iterator<Item = AbstractValue> + '_ {
        self.vars.keys().copied()
    }

    /// Translate all variables using a mapping function.
    pub fn translate(&self, f: impl Fn(AbstractValue) -> AbstractValue) -> Self {
        let mut result = Self::of_q(self.constant);
        for (&v, coeff) in &self.vars {
            let mapped = f(v);
            let entry = result.vars.entry(mapped).or_insert(Q::zero());
            *entry += coeff;
            if entry.is_zero() {
                result.vars.remove(&mapped);
            }
        }
        result
    }
}

impl fmt::Display for LinArith {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.vars.is_empty() {
            return write!(f, "{}", self.constant);
        }
        let mut first = true;
        for (&v, coeff) in &self.vars {
            if !first && *coeff >= Q::zero() {
                write!(f, "+")?;
            }
            if *coeff == Q::one() {
                write!(f, "{v}")?;
            } else if *coeff == -Q::one() {
                write!(f, "-{v}")?;
            } else {
                write!(f, "{coeff}·{v}")?;
            }
            first = false;
        }
        if !self.constant.is_zero() {
            if self.constant >= Q::zero() {
                write!(f, "+{}", self.constant)?;
            } else {
                write!(f, "{}", self.constant)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero() {
        let z = LinArith::zero();
        assert!(z.is_zero());
        assert_eq!(z.get_as_const(), Some(Q::zero()));
    }

    #[test]
    fn test_of_var() {
        let v = AbstractValue::of_raw(1);
        let l = LinArith::of_var(v);
        assert_eq!(l.get_as_var(), Some(v));
        assert!(!l.is_zero());
    }

    #[test]
    fn test_add() {
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let l1 = LinArith::of_var(v1); // v1
        let l2 = LinArith::of_var(v2); // v2
        let sum = l1.add(&l2); // v1 + v2
        assert_eq!(sum.vars.len(), 2);
    }

    #[test]
    fn test_add_cancellation() {
        let v = AbstractValue::of_raw(1);
        let l = LinArith::of_var(v); // v
        let neg = l.neg(); // -v
        let sum = l.add(&neg); // v + (-v) = 0
        assert!(sum.is_zero());
    }

    #[test]
    fn test_solve_eq_zero_trivial() {
        let z = LinArith::zero();
        match z.solve_eq_zero() {
            SatUnsat::Sat(None) => {} // 0 = 0, ok
            other => panic!("expected Sat(None), got {other:?}"),
        }
    }

    #[test]
    fn test_solve_eq_zero_contradiction() {
        let c = LinArith::of_int(5);
        match c.solve_eq_zero() {
            SatUnsat::Unsat => {} // 5 = 0, contradiction
            other => panic!("expected Unsat, got {other:?}"),
        }
    }

    #[test]
    fn test_solve_eq_zero_single_var() {
        // 2x + 6 = 0 → x = -3
        let v = AbstractValue::of_raw(1);
        let l = LinArith {
            constant: Q::from_integer(6),
            vars: [(v, Q::from_integer(2))].into_iter().collect(),
        };
        match l.solve_eq_zero() {
            SatUnsat::Sat(Some((x, solution))) => {
                assert_eq!(x, v);
                assert_eq!(solution.get_as_const(), Some(Q::from_integer(-3)));
            }
            other => panic!("expected Sat(Some), got {other:?}"),
        }
    }

    #[test]
    fn test_solve_eq() {
        // x = y + 1 → solve x - y - 1 = 0
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        let lx = LinArith::of_var(x);
        let ly_plus_1 = LinArith::of_var(y).add(&LinArith::of_int(1));

        match lx.solve_eq(&ly_plus_1) {
            SatUnsat::Sat(Some((solved_var, solution))) => {
                assert_eq!(solved_var, x); // x is simpler (lower id)
                                           // solution should be y + 1
                assert_eq!(solution.get_coefficient(y), Some(&Q::one()));
                assert_eq!(*solution.get_constant_part(), Q::from_integer(1));
            }
            other => panic!("expected Sat(Some), got {other:?}"),
        }
    }

    #[test]
    fn test_subst_var() {
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);
        // 2x + 3
        let l = LinArith {
            constant: Q::from_integer(3),
            vars: [(x, Q::from_integer(2))].into_iter().collect(),
        };
        // substitute x → y + 1
        let replacement = LinArith::of_var(y).add(&LinArith::of_int(1));
        let result = l.subst_var(x, &replacement);
        // should be 2(y + 1) + 3 = 2y + 5
        assert_eq!(result.get_coefficient(y), Some(&Q::from_integer(2)));
        assert_eq!(*result.get_constant_part(), Q::from_integer(5));
    }
}
