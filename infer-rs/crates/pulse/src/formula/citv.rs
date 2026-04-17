// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Concrete integer interval domain (CItv).
//!
//! Mirrors OCaml's `PulseCItv.ml`.
//!
//! Tracks integer ranges for abstract values. An interval is either:
//! - `Between(lower, upper)`: the value is in [lower, upper]
//! - `Outside(l, u)`: the value is NOT in [l, u] (i.e., < l or > u)
//!
//! This enables detecting infeasibility for integer constraints that
//! the rational formula solver can't handle (e.g., 2x = 5 has no
//! integer solution).

/// A bound of an interval: an integer, -∞, or +∞.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bound {
    Int(i64),
    MinusInfinity,
    PlusInfinity,
}

impl Bound {
    fn le(&self, other: &Bound) -> bool {
        match (self, other) {
            (Bound::MinusInfinity, _) | (_, Bound::PlusInfinity) => true,
            (Bound::PlusInfinity, _) | (_, Bound::MinusInfinity) => false,
            (Bound::Int(a), Bound::Int(b)) => a <= b,
        }
    }

    fn lt(&self, other: &Bound) -> bool {
        match (self, other) {
            (Bound::MinusInfinity, Bound::MinusInfinity)
            | (Bound::PlusInfinity, Bound::PlusInfinity) => false,
            (Bound::MinusInfinity, _) | (_, Bound::PlusInfinity) => true,
            (Bound::PlusInfinity, _) | (_, Bound::MinusInfinity) => false,
            (Bound::Int(a), Bound::Int(b)) => a < b,
        }
    }

    fn ge(&self, other: &Bound) -> bool {
        other.le(self)
    }

    fn gt(&self, other: &Bound) -> bool {
        other.lt(self)
    }

    fn min(a: &Bound, b: &Bound) -> Bound {
        if a.le(b) {
            a.clone()
        } else {
            b.clone()
        }
    }

    fn max(a: &Bound, b: &Bound) -> Bound {
        if a.le(b) {
            b.clone()
        } else {
            a.clone()
        }
    }

    fn add_int(&self, i: i64) -> Bound {
        match self {
            Bound::Int(v) => Bound::Int(v + i),
            other => other.clone(),
        }
    }

    fn minus(&self) -> Bound {
        match self {
            Bound::MinusInfinity => Bound::PlusInfinity,
            Bound::Int(i) => Bound::Int(-i),
            Bound::PlusInfinity => Bound::MinusInfinity,
        }
    }
}

/// A concrete integer interval.
///
/// Mirrors OCaml's `CItv.t`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CItv {
    /// Value is in [lower, upper].
    Between(Bound, Bound),
    /// Value is NOT in [l, u] (i.e., value < l or value > u).
    Outside(i64, i64),
}

/// Result of abducing a comparison constraint.
#[derive(Clone, Debug)]
pub enum AbductionResult {
    /// The assertion can never be true.
    Unsatisfiable,
    /// The assertion can be true; optionally refine lhs and/or rhs.
    Satisfiable(Option<CItv>, Option<CItv>),
}

impl CItv {
    /// Create an interval equal to a single integer.
    pub fn equal_to(i: i64) -> Self {
        CItv::Between(Bound::Int(i), Bound::Int(i))
    }

    /// Create the "not equal to" interval for a single integer.
    pub fn not_equal_to(i: i64) -> Self {
        CItv::Outside(i, i)
    }

    /// The universal interval: all integers.
    pub fn top() -> Self {
        CItv::Between(Bound::MinusInfinity, Bound::PlusInfinity)
    }

    /// Check if this is a singleton and return its value.
    pub fn to_singleton(&self) -> Option<i64> {
        match self {
            CItv::Between(Bound::Int(l), Bound::Int(u)) if l == u => Some(*l),
            _ => None,
        }
    }

    /// Check if this represents exactly zero.
    pub fn is_equal_to_zero(&self) -> bool {
        self.to_singleton() == Some(0)
    }

    /// Add an integer offset to the interval.
    fn add_int(&self, i: i64) -> Self {
        match self {
            CItv::Between(l, u) => CItv::Between(l.add_int(i), u.add_int(i)),
            CItv::Outside(l, u) => CItv::Outside(l + i, u + i),
        }
    }

    /// Negate the interval.
    fn negate(&self) -> Self {
        match self {
            CItv::Between(l, u) => CItv::Between(u.minus(), l.minus()),
            CItv::Outside(l, u) => CItv::Outside(-u, -l),
        }
    }

    /// Intersect two intervals. Returns None if empty.
    ///
    /// Cross-ref: OCaml `CItv.intersection`.
    pub fn intersection(&self, other: &CItv) -> Option<CItv> {
        match self.abduce_eq(other) {
            AbductionResult::Unsatisfiable => None,
            AbductionResult::Satisfiable(r1, r2) => Some(r1.or(r2).unwrap_or_else(|| self.clone())),
        }
    }

    /// Compute the result interval of a binary operation.
    ///
    /// Cross-ref: OCaml `CItv.binop`.
    pub fn binop(bop: &sil::binop::Binop, lhs: &CItv, rhs: &CItv) -> Option<CItv> {
        match bop {
            sil::binop::Binop::PlusA(_) | sil::binop::Binop::PlusPI => Self::add(lhs, rhs),
            sil::binop::Binop::MinusA(_)
            | sil::binop::Binop::MinusPI
            | sil::binop::Binop::MinusPP => {
                let neg = rhs.negate();
                Self::add(lhs, &neg)
            }
            _ => None,
        }
    }

    /// Add two intervals.
    fn add(a: &CItv, b: &CItv) -> Option<CItv> {
        match (a, b) {
            (CItv::Between(l1, u1), CItv::Between(l2, u2)) => {
                let lower = bound_add(l1, l2)?;
                let upper = bound_add(u1, u2)?;
                Some(CItv::Between(lower, upper))
            }
            _ => None,
        }
    }

    /// Abduce equality: can a1 == a2?
    ///
    /// Cross-ref: OCaml `CItv.abduce_eq`.
    fn abduce_eq(&self, other: &CItv) -> AbductionResult {
        match (self, other) {
            (CItv::Between(l1, u1), CItv::Between(l2, u2)) => {
                let lower = Bound::max(l1, l2);
                let upper = Bound::min(u1, u2);
                if upper.lt(&lower) {
                    AbductionResult::Unsatisfiable
                } else {
                    let tighter = CItv::Between(lower.clone(), upper.clone());
                    AbductionResult::Satisfiable(Some(tighter.clone()), Some(tighter))
                }
            }
            (CItv::Outside(l1, u1), CItv::Outside(l2, u2)) => {
                if *l1 <= *u2 && *l2 <= *u1 {
                    let l = (*l1).min(*l2);
                    let u = (*u1).max(*u2);
                    let tighter = CItv::Outside(l, u);
                    AbductionResult::Satisfiable(Some(tighter.clone()), Some(tighter))
                } else {
                    AbductionResult::Satisfiable(None, None)
                }
            }
            (CItv::Outside(..), CItv::Between(..)) => {
                let r = other.abduce_eq(self);
                flip_abduced(r)
            }
            (CItv::Between(l1, u1), CItv::Outside(l2, u2)) => {
                if l1.lt(&Bound::Int(*l2)) && u1.gt(&Bound::Int(*u2)) {
                    // case 1: Between spans Outside
                    if matches!((l1, u1), (Bound::MinusInfinity, Bound::PlusInfinity)) {
                        AbductionResult::Satisfiable(Some(other.clone()), None)
                    } else {
                        AbductionResult::Satisfiable(None, None)
                    }
                } else if l1.ge(&Bound::Int(*l2)) && u1.le(&Bound::Int(*u2)) {
                    // case 2: Between inside Outside gap → empty
                    AbductionResult::Unsatisfiable
                } else if l1.lt(&Bound::Int(*l2)) {
                    // case 3/4: left side
                    let upper = Bound::min(u1, &Bound::Int(*l2 - 1));
                    let tighter = CItv::Between(l1.clone(), upper);
                    AbductionResult::Satisfiable(Some(tighter.clone()), Some(tighter))
                } else {
                    // case 3/4: right side
                    let lower = Bound::max(l1, &Bound::Int(*u2 + 1));
                    let tighter = CItv::Between(lower, u1.clone());
                    AbductionResult::Satisfiable(Some(tighter.clone()), Some(tighter))
                }
            }
        }
    }

    /// Abduce disequality: can a1 != a2?
    fn abduce_ne(&self, other: &CItv) -> AbductionResult {
        if self.intersection(other).is_none() {
            // Already disjoint — trivially satisfiable
            AbductionResult::Satisfiable(None, None)
        } else {
            match (self.to_singleton(), other.to_singleton()) {
                (Some(_), Some(_)) => {
                    // Both singletons with non-empty intersection → same value → can't be ≠
                    AbductionResult::Unsatisfiable
                }
                (Some(e), None) => {
                    let refined = remove_element(e, other);
                    AbductionResult::Satisfiable(None, refined)
                }
                (None, Some(e)) => {
                    let refined = remove_element(e, self);
                    AbductionResult::Satisfiable(refined, None)
                }
                (None, None) => AbductionResult::Satisfiable(None, None),
            }
        }
    }

    /// Abduce less-than-or-equal: can a1 <= a2?
    fn abduce_le(&self, other: &CItv) -> AbductionResult {
        match (self, other) {
            (CItv::Between(l1, u1), CItv::Between(l2, u2)) => {
                let min_u = Bound::min(u1, u2);
                let max_l = Bound::max(l1, l2);
                if min_u.lt(l1) || u2.lt(&max_l) {
                    AbductionResult::Unsatisfiable
                } else {
                    AbductionResult::Satisfiable(
                        Some(CItv::Between(l1.clone(), min_u)),
                        Some(CItv::Between(max_l, u2.clone())),
                    )
                }
            }
            _ => AbductionResult::Satisfiable(None, None),
        }
    }

    /// Abduce strict less-than: can a1 < a2?
    fn abduce_lt(&self, other: &CItv) -> AbductionResult {
        let a1_plus_1 = self.add_int(1);
        match a1_plus_1.abduce_le(other) {
            AbductionResult::Satisfiable(Some(ref abduced1), abduced2) => {
                AbductionResult::Satisfiable(Some(abduced1.add_int(-1)), abduced2)
            }
            r => r,
        }
    }

    /// Abduce a binary comparison constraint.
    ///
    /// Given intervals for lhs and rhs and a comparison operator,
    /// returns whether the comparison is satisfiable and optionally
    /// refined intervals.
    ///
    /// Cross-ref: OCaml `CItv.abduce_binop_is_true`.
    pub fn abduce_binop_is_true(
        negated: bool,
        bop: &sil::binop::Binop,
        v1: Option<&CItv>,
        v2: Option<&CItv>,
    ) -> AbductionResult {
        let unknown = CItv::top();
        let a1 = v1.unwrap_or(&unknown);
        let a2 = v2.unwrap_or(&unknown);

        use sil::binop::Binop;
        match (bop, negated) {
            (Binop::Eq, false) | (Binop::Ne, true) => a1.abduce_eq(a2),
            (Binop::Eq, true) | (Binop::Ne, false) => a1.abduce_ne(a2),
            (Binop::Le, false) | (Binop::Gt, true) => a1.abduce_le(a2),
            (Binop::Ge, false) | (Binop::Lt, true) => flip_abduced(a2.abduce_le(a1)),
            (Binop::Lt, false) | (Binop::Ge, true) => a1.abduce_lt(a2),
            (Binop::Gt, false) | (Binop::Le, true) => flip_abduced(a2.abduce_lt(a1)),
            _ => AbductionResult::Satisfiable(None, None),
        }
    }
}

fn flip_abduced(r: AbductionResult) -> AbductionResult {
    match r {
        AbductionResult::Unsatisfiable => AbductionResult::Unsatisfiable,
        AbductionResult::Satisfiable(l, r) => AbductionResult::Satisfiable(r, l),
    }
}

fn bound_add(a: &Bound, b: &Bound) -> Option<Bound> {
    match (a, b) {
        (Bound::Int(x), Bound::Int(y)) => Some(Bound::Int(x + y)),
        (Bound::MinusInfinity, Bound::PlusInfinity)
        | (Bound::PlusInfinity, Bound::MinusInfinity) => None,
        (Bound::MinusInfinity, _) | (_, Bound::MinusInfinity) => Some(Bound::MinusInfinity),
        (Bound::PlusInfinity, _) | (_, Bound::PlusInfinity) => Some(Bound::PlusInfinity),
    }
}

fn remove_element(e: i64, from: &CItv) -> Option<CItv> {
    match from {
        CItv::Between(l, u) if l == u => None, // singleton → empty
        CItv::Between(Bound::Int(l), u) if *l == e => {
            Some(CItv::Between(Bound::Int(l + 1), u.clone()))
        }
        CItv::Between(l, Bound::Int(u)) if *u == e => {
            Some(CItv::Between(l.clone(), Bound::Int(u - 1)))
        }
        CItv::Between(Bound::MinusInfinity, Bound::PlusInfinity) => Some(CItv::not_equal_to(e)),
        CItv::Between(..) => None,
        CItv::Outside(l, u) => {
            if e == *l - 1 {
                Some(CItv::Outside(e, *u))
            } else if e == *u + 1 {
                Some(CItv::Outside(*l, e))
            } else {
                None
            }
        }
    }
}

impl std::fmt::Display for CItv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CItv::Between(Bound::MinusInfinity, Bound::PlusInfinity) => write!(f, "∈ℤ"),
            CItv::Between(l, Bound::PlusInfinity) => write!(f, "≥{l:?}"),
            CItv::Between(Bound::MinusInfinity, u) => write!(f, "≤{u:?}"),
            CItv::Between(l, u) if l == u => write!(f, "={l:?}"),
            CItv::Between(l, u) => write!(f, "∈[{l:?},{u:?}]"),
            CItv::Outside(l, u) if l == u => write!(f, "≠{l}"),
            CItv::Outside(l, u) => write!(f, "∉[{l},{u}]"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equal_to() {
        let i = CItv::equal_to(5);
        assert_eq!(i.to_singleton(), Some(5));
        assert!(!i.is_equal_to_zero());
        assert!(CItv::equal_to(0).is_equal_to_zero());
    }

    #[test]
    fn test_intersection_disjoint() {
        let a = CItv::Between(Bound::Int(0), Bound::Int(5));
        let b = CItv::Between(Bound::Int(10), Bound::Int(20));
        assert!(a.intersection(&b).is_none());
    }

    #[test]
    fn test_intersection_overlap() {
        let a = CItv::Between(Bound::Int(0), Bound::Int(10));
        let b = CItv::Between(Bound::Int(5), Bound::Int(20));
        let r = a.intersection(&b).unwrap();
        assert_eq!(r, CItv::Between(Bound::Int(5), Bound::Int(10)));
    }

    #[test]
    fn test_between_outside_unsat() {
        // x ∈ [3, 5] and x ∉ [2, 6] → UNSAT (x must be in [3,5] but [2,6] excludes it)
        let a = CItv::Between(Bound::Int(3), Bound::Int(5));
        let b = CItv::Outside(2, 6);
        assert!(a.intersection(&b).is_none());
    }

    #[test]
    fn test_abduce_eq_singleton_unsat() {
        // x = 5 and x = 3 → UNSAT
        let a = CItv::equal_to(5);
        let b = CItv::equal_to(3);
        assert!(matches!(a.abduce_eq(&b), AbductionResult::Unsatisfiable));
    }

    #[test]
    fn test_abduce_ne_singleton_unsat() {
        // x = 5 and x ≠ 5 → UNSAT
        let a = CItv::equal_to(5);
        let b = CItv::equal_to(5);
        assert!(matches!(a.abduce_ne(&b), AbductionResult::Unsatisfiable));
    }

    #[test]
    fn test_abduce_le_unsat() {
        // x ∈ [10, 20] ≤ y ∈ [0, 5] → UNSAT
        let a = CItv::Between(Bound::Int(10), Bound::Int(20));
        let b = CItv::Between(Bound::Int(0), Bound::Int(5));
        assert!(matches!(a.abduce_le(&b), AbductionResult::Unsatisfiable));
    }

    #[test]
    fn test_binop_add() {
        let a = CItv::Between(Bound::Int(1), Bound::Int(3));
        let b = CItv::Between(Bound::Int(10), Bound::Int(20));
        let r = CItv::binop(&sil::binop::Binop::PlusA(None), &a, &b).unwrap();
        assert_eq!(r, CItv::Between(Bound::Int(11), Bound::Int(23)));
    }
}
