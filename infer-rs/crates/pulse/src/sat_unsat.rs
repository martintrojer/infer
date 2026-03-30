// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Sat/Unsat result type for constraint operations.
//!
//! Mirrors OCaml's `PulseSatUnsat.ml`.
//!
//! Operations that add constraints to the formula may discover the path is
//! infeasible (Unsat). This type threads that through the analysis.

use std::fmt;

/// Result of a constraint operation: satisfiable or unsatisfiable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SatUnsat<T> {
    Sat(T),
    Unsat,
}

impl<T> SatUnsat<T> {
    /// Extract the Sat value, or None if Unsat.
    pub fn sat(self) -> Option<T> {
        match self {
            SatUnsat::Sat(x) => Some(x),
            SatUnsat::Unsat => None,
        }
    }

    /// Map over the Sat value.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> SatUnsat<U> {
        match self {
            SatUnsat::Sat(x) => SatUnsat::Sat(f(x)),
            SatUnsat::Unsat => SatUnsat::Unsat,
        }
    }

    /// Flat-map (bind) over the Sat value.
    pub fn and_then<U>(self, f: impl FnOnce(T) -> SatUnsat<U>) -> SatUnsat<U> {
        match self {
            SatUnsat::Sat(x) => f(x),
            SatUnsat::Unsat => SatUnsat::Unsat,
        }
    }

    /// Is this Sat?
    pub fn is_sat(&self) -> bool {
        matches!(self, SatUnsat::Sat(_))
    }

    /// Is this Unsat?
    pub fn is_unsat(&self) -> bool {
        matches!(self, SatUnsat::Unsat)
    }
}

impl<T: fmt::Display> fmt::Display for SatUnsat<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SatUnsat::Sat(x) => write!(f, "Sat({x})"),
            SatUnsat::Unsat => write!(f, "Unsat"),
        }
    }
}

/// Fold over a list, threading SatUnsat through. Returns Unsat on first Unsat.
pub fn list_fold<T, A>(
    items: impl IntoIterator<Item = T>,
    init: A,
    f: impl Fn(A, T) -> SatUnsat<A>,
) -> SatUnsat<A> {
    let mut acc = init;
    for item in items {
        match f(acc, item) {
            SatUnsat::Sat(new_acc) => acc = new_acc,
            SatUnsat::Unsat => return SatUnsat::Unsat,
        }
    }
    SatUnsat::Sat(acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sat_unsat_basic() {
        let s: SatUnsat<i32> = SatUnsat::Sat(42);
        assert!(s.is_sat());
        assert_eq!(s.sat(), Some(42));

        let u: SatUnsat<i32> = SatUnsat::Unsat;
        assert!(u.is_unsat());
        assert_eq!(u.sat(), None);
    }

    #[test]
    fn test_map() {
        let s = SatUnsat::Sat(10);
        let mapped = s.map(|x| x * 2);
        assert_eq!(mapped, SatUnsat::Sat(20));

        let u: SatUnsat<i32> = SatUnsat::Unsat;
        let mapped = u.map(|x| x * 2);
        assert_eq!(mapped, SatUnsat::Unsat);
    }

    #[test]
    fn test_and_then() {
        let s = SatUnsat::Sat(10);
        let result = s.and_then(|x| {
            if x > 5 {
                SatUnsat::Sat(x + 1)
            } else {
                SatUnsat::Unsat
            }
        });
        assert_eq!(result, SatUnsat::Sat(11));
    }

    #[test]
    fn test_list_fold() {
        let result = list_fold(vec![1, 2, 3], 0, |acc, x| SatUnsat::Sat(acc + x));
        assert_eq!(result, SatUnsat::Sat(6));

        // Unsat on negative
        let result = list_fold(vec![1, -1, 3], 0, |acc, x| {
            if x < 0 {
                SatUnsat::Unsat
            } else {
                SatUnsat::Sat(acc + x)
            }
        });
        assert_eq!(result, SatUnsat::Unsat);
    }
}
