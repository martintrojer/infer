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

    pub fn zero() -> Self {
        Term::Const(0)
    }

    pub fn one() -> Self {
        Term::Const(1)
    }

    /// Try to extract the constant value of a term.
    pub fn as_const(&self) -> Option<i64> {
        match self {
            Term::Const(c) => Some(*c),
            Term::Add(a, b) => Some(a.as_const()? + b.as_const()?),
            Term::Sub(a, b) => Some(a.as_const()? - b.as_const()?),
            Term::Mult(a, b) => Some(a.as_const()? * b.as_const()?),
            Term::Neg(t) => Some(-t.as_const()?),
            Term::Not(t) => Some(if t.as_const()? == 0 { 1 } else { 0 }),
            Term::IsZero(t) => Some(if t.as_const()? == 0 { 1 } else { 0 }),
            Term::Var(_) => None,
        }
    }

    /// Check if this term is a constant zero.
    pub fn is_zero(&self) -> bool {
        matches!(self, Term::Const(0))
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
            Term::Const(_) => {}
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
            Term::Const(_) => self.clone(),
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
            Term::Var(_) | Term::Const(_) => self.clone(),
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
