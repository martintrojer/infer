// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Boolean atoms for the formula solver.
//!
//! Mirrors OCaml's `PulseFormulaAtom.ml` (simplified).
//!
//! An atom is a boolean constraint between two terms.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::term::Term;

/// A boolean constraint between two terms.
///
/// Mirrors OCaml's `Atom.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Atom {
    /// t1 <= t2
    LessEqual(Term, Term),
    /// t1 < t2
    LessThan(Term, Term),
    /// t1 = t2
    Equal(Term, Term),
    /// t1 ≠ t2
    NotEqual(Term, Term),
}

impl Atom {
    /// Negate this atom.
    pub fn negate(&self) -> Self {
        match self {
            Atom::LessEqual(a, b) => Atom::LessThan(b.clone(), a.clone()),
            Atom::LessThan(a, b) => Atom::LessEqual(b.clone(), a.clone()),
            Atom::Equal(a, b) => Atom::NotEqual(a.clone(), b.clone()),
            Atom::NotEqual(a, b) => Atom::Equal(a.clone(), b.clone()),
        }
    }

    /// Substitute a variable with a term in both sides.
    pub fn subst_var(&self, old: crate::abstract_value::AbstractValue, replacement: &Term) -> Self {
        match self {
            Atom::LessEqual(a, b) => {
                Atom::LessEqual(a.subst_var(old, replacement), b.subst_var(old, replacement))
            }
            Atom::LessThan(a, b) => {
                Atom::LessThan(a.subst_var(old, replacement), b.subst_var(old, replacement))
            }
            Atom::Equal(a, b) => {
                Atom::Equal(a.subst_var(old, replacement), b.subst_var(old, replacement))
            }
            Atom::NotEqual(a, b) => {
                Atom::NotEqual(a.subst_var(old, replacement), b.subst_var(old, replacement))
            }
        }
    }

    /// Check if this atom is trivially true with constant terms.
    pub fn is_trivially_true(&self) -> Option<bool> {
        match self {
            Atom::Equal(a, b) => {
                if a == b {
                    return Some(true);
                }
                match (a.as_const(), b.as_const()) {
                    (Some(x), Some(y)) => Some(x == y),
                    _ => None,
                }
            }
            Atom::NotEqual(a, b) => {
                if a == b {
                    return Some(false);
                }
                match (a.as_const(), b.as_const()) {
                    (Some(x), Some(y)) => Some(x != y),
                    _ => None,
                }
            }
            Atom::LessEqual(a, b) => {
                if a == b {
                    return Some(true);
                }
                match (a.as_const(), b.as_const()) {
                    (Some(x), Some(y)) => Some(x <= y),
                    _ => None,
                }
            }
            Atom::LessThan(a, b) => {
                if a == b {
                    return Some(false);
                }
                match (a.as_const(), b.as_const()) {
                    (Some(x), Some(y)) => Some(x < y),
                    _ => None,
                }
            }
        }
    }
}

impl Atom {
    /// Translate all variables using a mapping function.
    pub fn translate(
        &self,
        f: impl Fn(crate::abstract_value::AbstractValue) -> crate::abstract_value::AbstractValue,
    ) -> Self {
        let translate_term = |t: &Term| t.translate(&f);
        match self {
            Atom::LessEqual(a, b) => Atom::LessEqual(translate_term(a), translate_term(b)),
            Atom::LessThan(a, b) => Atom::LessThan(translate_term(a), translate_term(b)),
            Atom::Equal(a, b) => Atom::Equal(translate_term(a), translate_term(b)),
            Atom::NotEqual(a, b) => Atom::NotEqual(translate_term(a), translate_term(b)),
        }
    }

    /// Collect all variables in both sides of this atom.
    pub fn all_vars(&self) -> Vec<crate::abstract_value::AbstractValue> {
        let (a, b) = match self {
            Atom::Equal(a, b)
            | Atom::NotEqual(a, b)
            | Atom::LessEqual(a, b)
            | Atom::LessThan(a, b) => (a, b),
        };
        let mut vars = a.vars();
        vars.extend(b.vars());
        vars
    }
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Atom::LessEqual(a, b) => write!(f, "{a} ≤ {b}"),
            Atom::LessThan(a, b) => write!(f, "{a} < {b}"),
            Atom::Equal(a, b) => write!(f, "{a} = {b}"),
            Atom::NotEqual(a, b) => write!(f, "{a} ≠ {b}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstract_value::AbstractValue;

    #[test]
    fn test_negate() {
        let v1 = Term::var(AbstractValue::of_raw(1));
        let v2 = Term::var(AbstractValue::of_raw(2));

        let eq = Atom::Equal(v1.clone(), v2.clone());
        assert!(matches!(eq.negate(), Atom::NotEqual(_, _)));

        let lt = Atom::LessThan(v1.clone(), v2.clone());
        // !(a < b) = b <= a
        assert!(matches!(lt.negate(), Atom::LessEqual(_, _)));
    }

    #[test]
    fn test_trivially_true() {
        let c5 = Term::Const(5);
        let c10 = Term::Const(10);

        assert_eq!(
            Atom::Equal(c5.clone(), c5.clone()).is_trivially_true(),
            Some(true)
        );
        assert_eq!(
            Atom::Equal(c5.clone(), c10.clone()).is_trivially_true(),
            Some(false)
        );
        assert_eq!(
            Atom::LessThan(c5.clone(), c10.clone()).is_trivially_true(),
            Some(true)
        );

        // Non-constant atoms are unknown
        let v = Term::var(AbstractValue::of_raw(1));
        assert_eq!(Atom::Equal(v, c5).is_trivially_true(), None);
    }
}
