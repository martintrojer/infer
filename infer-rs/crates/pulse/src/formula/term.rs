// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Term expressions for the formula solver.
//!
//! Mirrors OCaml's `PulseFormulaTerm.ml` (simplified).
//!
//! Terms represent symbolic expressions over abstract values and constants.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::lin_arith::Q;
use crate::abstract_value::AbstractValue;

/// A term in the formula language.
///
/// Simplified from OCaml's full `Term.t` which has ~30 variants.
/// We include what's needed for null-check path sensitivity and basic arithmetic.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Term {
    /// An abstract value (variable).
    Var(AbstractValue),
    /// An integer constant.
    Const(i64),
    /// A non-integer rational constant (e.g. `5/2` or the exact IEEE-754
    /// value of a float literal). Mirrors OCaml's `Term.Const` which carries a
    /// `Q.t`. Integer-valued constants must use `Const` instead so the bulk of
    /// the integer-only folding/canonicalization paths stay unchanged; this
    /// variant only ever holds a non-integer `Q`.
    Rational(Q),
    /// Addition: t1 + t2.
    Add(Box<Term>, Box<Term>),
    /// Subtraction: t1 - t2.
    Sub(Box<Term>, Box<Term>),
    /// Multiplication: t1 * t2.
    Mult(Box<Term>, Box<Term>),
    /// Negation: -t.
    Neg(Box<Term>),
    /// Logical not: !t.
    Not(Box<Term>),
    /// Is-zero test: t == 0.
    IsZero(Box<Term>),
}

impl Term {
    pub fn var(v: AbstractValue) -> Self {
        Term::Var(v)
    }

    /// Build a constant term from a rational, choosing `Const` for
    /// integer-valued rationals and `Rational` otherwise. This is the single
    /// place that decides the representation, so the `Rational`-only-holds-
    /// non-integers invariant is maintained.
    pub fn of_q(q: Q) -> Self {
        use num_traits::ToPrimitive;
        if q.is_integer() {
            if let Some(i) = q.to_integer().to_i64() {
                return Term::Const(i);
            }
        }
        Term::Rational(q)
    }

    /// Try to extract the integer constant value of a term. Non-integer
    /// rationals deliberately return `None` so existing `i64`-only folding
    /// paths do not silently truncate.
    pub fn as_const(&self) -> Option<i64> {
        match self {
            Term::Const(c) => Some(*c),
            Term::Rational(_) => None,
            Term::Add(a, b) => Some(a.as_const()? + b.as_const()?),
            Term::Sub(a, b) => Some(a.as_const()? - b.as_const()?),
            Term::Mult(a, b) => Some(a.as_const()? * b.as_const()?),
            Term::Neg(t) => Some(-t.as_const()?),
            Term::Not(t) => Some(if t.as_const()? == 0 { 1 } else { 0 }),
            Term::IsZero(t) => Some(if t.as_const()? == 0 { 1 } else { 0 }),
            Term::Var(_) => None,
        }
    }

    /// Try to extract the exact rational value of a term, preserving
    /// non-integer constants (unlike [`Term::as_const`]).
    pub fn as_q(&self) -> Option<Q> {
        use num_traits::Zero;
        match self {
            Term::Const(c) => Some(Q::from_integer(*c)),
            Term::Rational(q) => Some(*q),
            Term::Add(a, b) => Some(a.as_q()? + b.as_q()?),
            Term::Sub(a, b) => Some(a.as_q()? - b.as_q()?),
            Term::Mult(a, b) => Some(a.as_q()? * b.as_q()?),
            Term::Neg(t) => Some(-t.as_q()?),
            Term::Not(t) => Some(Q::from_integer(if t.as_q()?.is_zero() { 1 } else { 0 })),
            Term::IsZero(t) => Some(Q::from_integer(if t.as_q()?.is_zero() { 1 } else { 0 })),
            Term::Var(_) => None,
        }
    }

    /// Collect all variables mentioned in this term.
    pub fn vars(&self) -> Vec<AbstractValue> {
        let mut result = Vec::new();
        self.collect_vars(&mut result);
        result
    }

    fn collect_vars(&self, out: &mut Vec<AbstractValue>) {
        match self {
            Term::Var(v) => out.push(*v),
            Term::Const(_) | Term::Rational(_) => {}
            Term::Add(a, b) | Term::Sub(a, b) | Term::Mult(a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
            Term::Neg(t) | Term::Not(t) | Term::IsZero(t) => t.collect_vars(out),
        }
    }

    /// Translate all variables using a mapping function.
    pub fn translate(&self, f: &impl Fn(AbstractValue) -> AbstractValue) -> Term {
        match self {
            Term::Var(v) => Term::Var(f(*v)),
            Term::Const(_) | Term::Rational(_) => self.clone(),
            Term::Add(a, b) => Term::Add(Box::new(a.translate(f)), Box::new(b.translate(f))),
            Term::Sub(a, b) => Term::Sub(Box::new(a.translate(f)), Box::new(b.translate(f))),
            Term::Mult(a, b) => Term::Mult(Box::new(a.translate(f)), Box::new(b.translate(f))),
            Term::Neg(t) => Term::Neg(Box::new(t.translate(f))),
            Term::Not(t) => Term::Not(Box::new(t.translate(f))),
            Term::IsZero(t) => Term::IsZero(Box::new(t.translate(f))),
        }
    }

    /// Substitute a variable with a term.
    pub fn subst_var(&self, old: AbstractValue, replacement: &Term) -> Term {
        match self {
            Term::Var(v) if *v == old => replacement.clone(),
            Term::Var(_) | Term::Const(_) | Term::Rational(_) => self.clone(),
            Term::Add(a, b) => Term::Add(
                Box::new(a.subst_var(old, replacement)),
                Box::new(b.subst_var(old, replacement)),
            ),
            Term::Sub(a, b) => Term::Sub(
                Box::new(a.subst_var(old, replacement)),
                Box::new(b.subst_var(old, replacement)),
            ),
            Term::Mult(a, b) => Term::Mult(
                Box::new(a.subst_var(old, replacement)),
                Box::new(b.subst_var(old, replacement)),
            ),
            Term::Neg(t) => Term::Neg(Box::new(t.subst_var(old, replacement))),
            Term::Not(t) => Term::Not(Box::new(t.subst_var(old, replacement))),
            Term::IsZero(t) => Term::IsZero(Box::new(t.subst_var(old, replacement))),
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Term::Var(v) => write!(f, "{v}"),
            Term::Const(c) => write!(f, "{c}"),
            Term::Rational(q) => write!(f, "{q}"),
            Term::Add(a, b) => write!(f, "({a} + {b})"),
            Term::Sub(a, b) => write!(f, "({a} - {b})"),
            Term::Mult(a, b) => write!(f, "({a} * {b})"),
            Term::Neg(t) => write!(f, "-{t}"),
            Term::Not(t) => write!(f, "!{t}"),
            Term::IsZero(t) => write!(f, "({t} == 0)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Term;

    #[test]
    fn test_as_const_folds_nested_constant_terms() {
        assert_eq!(
            Term::Add(Box::new(Term::Const(0)), Box::new(Term::Const(1))).as_const(),
            Some(1)
        );
        assert_eq!(
            Term::Sub(
                Box::new(Term::Const(5)),
                Box::new(Term::Add(
                    Box::new(Term::Const(2)),
                    Box::new(Term::Const(1))
                )),
            )
            .as_const(),
            Some(2)
        );
        assert_eq!(
            Term::Not(Box::new(Term::Sub(
                Box::new(Term::Const(1)),
                Box::new(Term::Const(1)),
            )))
            .as_const(),
            Some(1)
        );
    }
}
