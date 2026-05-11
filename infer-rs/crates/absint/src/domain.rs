// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Abstract domains and domain combinators.
//!
//! Mirrors OCaml's `AbstractDomain.mli`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

// ===========================================================================
// Core traits
// ===========================================================================

/// Partial order with pretty printing.
///
/// Mirrors OCaml's `AbstractDomain.Comparable`.
pub trait Comparable: Clone + fmt::Debug + PartialEq {
    /// The implication/ordering relation: `lhs <= rhs` means `lhs |- rhs`.
    fn leq(&self, rhs: &Self) -> bool;

    /// Cheap equality hook for disjunctive dedup/join.
    ///
    /// Cross-ref: OCaml's disjunctive interpreter uses `equal_fast` when
    /// collapsing instruction-level disjuncts, and keeps semantic `leq` for
    /// widening and convergence checks.
    fn equal_fast(&self, rhs: &Self) -> bool {
        self == rhs
    }

    /// Cross-product subset check used by `DisjunctiveDomain::leq` after
    /// the cheap `equal_fast` / `is_trivial_subset` short-circuits fail.
    ///
    /// Returns `true` iff every disjunct in `lhs_disjuncts` is `<=` some
    /// disjunct in `rhs_disjuncts` under the inner-domain ordering.
    /// Default impl drives the obvious O(N·M) `leq` cross-product.
    /// Implementations may override to amortise per-disjunct work that
    /// the inner `leq` would otherwise repeat across the cross-product
    /// (e.g. Pulse's `state_cmp::canonicalize`, which is the dominant
    /// cost on `DES_ede3_cfb_encrypt`).
    ///
    /// Semantics MUST exactly match the default implementation. This is
    /// a structural rewrite hook, not a heuristic short-circuit.
    fn disjunctive_leq_subset(lhs_disjuncts: &[Self], rhs_disjuncts: &[Self]) -> bool {
        lhs_disjuncts
            .iter()
            .all(|d| rhs_disjuncts.iter().any(|r| d.leq(r)))
    }
}

/// Abstract domain with join and widening.
///
/// Mirrors OCaml's `AbstractDomain.S`.
pub trait AbstractDomain: Comparable {
    /// Least upper bound.
    fn join(&self, other: &Self) -> Self;

    /// Widening operator to ensure convergence on infinite ascending chains.
    /// `prev` is the previous iterate, `next` is the new value,
    /// `num_iters` is the iteration count at this program point.
    fn widen(&self, next: &Self, num_iters: usize) -> Self;
}

/// Domain with an explicit bottom element.
///
/// Mirrors OCaml's `AbstractDomain.WithBottom`.
pub trait WithBottom: AbstractDomain {
    fn bottom() -> Self;
    fn is_bottom(&self) -> bool;
}

/// Domain with an explicit top element.
///
/// Mirrors OCaml's `AbstractDomain.WithTop`.
pub trait WithTop: AbstractDomain {
    fn top() -> Self;
    fn is_top(&self) -> bool;
}

/// Domain with both bottom and top.
///
/// Mirrors OCaml's `AbstractDomain.WithBottomTop`.
pub trait WithBottomTop: WithBottom + WithTop {}

impl<T: WithBottom + WithTop> WithBottomTop for T {}

// ===========================================================================
// Lifted types
// ===========================================================================

/// A domain with an added bottom element.
///
/// Mirrors OCaml's `AbstractDomain.BottomLifted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BottomLifted<D> {
    Bottom,
    NonBottom(D),
}

impl<D: AbstractDomain> Comparable for BottomLifted<D> {
    fn leq(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (BottomLifted::Bottom, _) => true,
            (_, BottomLifted::Bottom) => false,
            (BottomLifted::NonBottom(a), BottomLifted::NonBottom(b)) => a.leq(b),
        }
    }

    fn equal_fast(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (BottomLifted::Bottom, BottomLifted::Bottom) => true,
            (BottomLifted::NonBottom(a), BottomLifted::NonBottom(b)) => a.equal_fast(b),
            _ => false,
        }
    }
}

impl<D: AbstractDomain> AbstractDomain for BottomLifted<D> {
    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (BottomLifted::Bottom, x) | (x, BottomLifted::Bottom) => x.clone(),
            (BottomLifted::NonBottom(a), BottomLifted::NonBottom(b)) => {
                BottomLifted::NonBottom(a.join(b))
            }
        }
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        match (self, next) {
            (BottomLifted::Bottom, x) | (x, BottomLifted::Bottom) => x.clone(),
            (BottomLifted::NonBottom(a), BottomLifted::NonBottom(b)) => {
                BottomLifted::NonBottom(a.widen(b, num_iters))
            }
        }
    }
}

impl<D: AbstractDomain> WithBottom for BottomLifted<D> {
    fn bottom() -> Self {
        BottomLifted::Bottom
    }

    fn is_bottom(&self) -> bool {
        matches!(self, BottomLifted::Bottom)
    }
}

/// A domain with an added top element.
///
/// Mirrors OCaml's `AbstractDomain.TopLifted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopLifted<D> {
    Top,
    NonTop(D),
}

impl<D: AbstractDomain> Comparable for TopLifted<D> {
    fn leq(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (_, TopLifted::Top) => true,
            (TopLifted::Top, _) => false,
            (TopLifted::NonTop(a), TopLifted::NonTop(b)) => a.leq(b),
        }
    }

    fn equal_fast(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (TopLifted::Top, TopLifted::Top) => true,
            (TopLifted::NonTop(a), TopLifted::NonTop(b)) => a.equal_fast(b),
            _ => false,
        }
    }
}

impl<D: AbstractDomain> AbstractDomain for TopLifted<D> {
    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (TopLifted::Top, _) | (_, TopLifted::Top) => TopLifted::Top,
            (TopLifted::NonTop(a), TopLifted::NonTop(b)) => TopLifted::NonTop(a.join(b)),
        }
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        match (self, next) {
            (TopLifted::Top, _) | (_, TopLifted::Top) => TopLifted::Top,
            (TopLifted::NonTop(a), TopLifted::NonTop(b)) => {
                TopLifted::NonTop(a.widen(b, num_iters))
            }
        }
    }
}

impl<D: AbstractDomain> WithTop for TopLifted<D> {
    fn top() -> Self {
        TopLifted::Top
    }

    fn is_top(&self) -> bool {
        matches!(self, TopLifted::Top)
    }
}

// ===========================================================================
// Domain combinators
// ===========================================================================

/// Cartesian product of two domains.
///
/// Mirrors OCaml's `AbstractDomain.Pair`.
impl<A: AbstractDomain, B: AbstractDomain> Comparable for (A, B) {
    fn leq(&self, rhs: &Self) -> bool {
        self.0.leq(&rhs.0) && self.1.leq(&rhs.1)
    }

    fn equal_fast(&self, rhs: &Self) -> bool {
        self.0.equal_fast(&rhs.0) && self.1.equal_fast(&rhs.1)
    }
}

impl<A: AbstractDomain, B: AbstractDomain> AbstractDomain for (A, B) {
    fn join(&self, other: &Self) -> Self {
        (self.0.join(&other.0), self.1.join(&other.1))
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        (
            self.0.widen(&next.0, num_iters),
            self.1.widen(&next.1, num_iters),
        )
    }
}

impl<A: WithBottom, B: WithBottom> WithBottom for (A, B) {
    fn bottom() -> Self {
        (A::bottom(), B::bottom())
    }

    fn is_bottom(&self) -> bool {
        self.0.is_bottom() && self.1.is_bottom()
    }
}

// ===========================================================================
// Concrete domain implementations
// ===========================================================================

/// Boolean "and" domain: bottom=true, join=&&, top=false.
///
/// Mirrors OCaml's `AbstractDomain.BooleanAnd`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BooleanAnd(pub bool);

impl Comparable for BooleanAnd {
    fn leq(&self, rhs: &Self) -> bool {
        // true <= true, true <= false, false <= false; !(false <= true)
        // i.e. self implies rhs
        !self.0 || rhs.0
    }
}

impl AbstractDomain for BooleanAnd {
    fn join(&self, other: &Self) -> Self {
        BooleanAnd(self.0 && other.0)
    }

    fn widen(&self, next: &Self, _num_iters: usize) -> Self {
        self.join(next)
    }
}

/// Boolean "or" domain: bottom=false, join=||, top=true.
///
/// Mirrors OCaml's `AbstractDomain.BooleanOr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BooleanOr(pub bool);

impl Comparable for BooleanOr {
    fn leq(&self, rhs: &Self) -> bool {
        !self.0 || rhs.0
    }
}

impl AbstractDomain for BooleanOr {
    fn join(&self, other: &Self) -> Self {
        BooleanOr(self.0 || other.0)
    }

    fn widen(&self, next: &Self, _num_iters: usize) -> Self {
        self.join(next)
    }
}

/// Finite powerset domain (set of elements, ordered by subset).
///
/// Mirrors OCaml's `AbstractDomain.FiniteSet`.
impl<T: Clone + fmt::Debug + Eq + Ord> Comparable for BTreeSet<T> {
    fn leq(&self, rhs: &Self) -> bool {
        self.is_subset(rhs)
    }
}

impl<T: Clone + fmt::Debug + Eq + Ord> AbstractDomain for BTreeSet<T> {
    fn join(&self, other: &Self) -> Self {
        self.union(other).cloned().collect()
    }

    fn widen(&self, next: &Self, _num_iters: usize) -> Self {
        self.join(next)
    }
}

impl<T: Clone + fmt::Debug + Eq + Ord> WithBottom for BTreeSet<T> {
    fn bottom() -> Self {
        BTreeSet::new()
    }

    fn is_bottom(&self) -> bool {
        self.is_empty()
    }
}

/// Map domain: pointwise join/widen.
///
/// Mirrors OCaml's `AbstractDomain.Map`.
impl<K: Clone + fmt::Debug + Eq + Ord, V: AbstractDomain> Comparable for BTreeMap<K, V> {
    fn leq(&self, rhs: &Self) -> bool {
        self.iter()
            .all(|(k, v)| rhs.get(k).is_some_and(|rv| v.leq(rv)))
    }
}

impl<K: Clone + fmt::Debug + Eq + Ord, V: AbstractDomain> AbstractDomain for BTreeMap<K, V> {
    fn join(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for (k, v) in other {
            result
                .entry(k.clone())
                .and_modify(|existing| *existing = existing.join(v))
                .or_insert_with(|| v.clone());
        }
        result
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        let mut result = self.clone();
        for (k, v) in next {
            result
                .entry(k.clone())
                .and_modify(|existing| *existing = existing.widen(v, num_iters))
                .or_insert_with(|| v.clone());
        }
        result
    }
}

impl<K: Clone + fmt::Debug + Eq + Ord, V: AbstractDomain> WithBottom for BTreeMap<K, V> {
    fn bottom() -> Self {
        BTreeMap::new()
    }

    fn is_bottom(&self) -> bool {
        self.is_empty()
    }
}

// ===========================================================================
// Unit domain (trivial)
// ===========================================================================

impl Comparable for () {
    fn leq(&self, _rhs: &Self) -> bool {
        true
    }
}

impl AbstractDomain for () {
    fn join(&self, _other: &Self) -> Self {}
    fn widen(&self, _next: &Self, _num_iters: usize) -> Self {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bottom_lifted() {
        let bot: BottomLifted<BooleanAnd> = BottomLifted::Bottom;
        let val = BottomLifted::NonBottom(BooleanAnd(true));
        assert!(bot.leq(&val));
        assert!(!val.leq(&bot));
        assert_eq!(bot.join(&val), val);
    }

    #[test]
    fn test_top_lifted() {
        let top: TopLifted<BooleanOr> = TopLifted::Top;
        let val = TopLifted::NonTop(BooleanOr(false));
        assert!(val.leq(&top));
        assert!(!top.leq(&val));
        assert_eq!(val.join(&top), top);
    }

    #[test]
    fn test_pair_domain() {
        let a = (BooleanAnd(true), BooleanOr(false));
        let b = (BooleanAnd(false), BooleanOr(true));
        let joined = a.join(&b);
        assert_eq!(joined, (BooleanAnd(false), BooleanOr(true)));
    }

    #[test]
    fn test_set_domain() {
        let a: BTreeSet<i32> = [1, 2, 3].into_iter().collect();
        let b: BTreeSet<i32> = [2, 3, 4].into_iter().collect();
        assert!(!a.leq(&b));
        let joined = a.join(&b);
        assert_eq!(joined, [1, 2, 3, 4].into_iter().collect());
        assert!(a.leq(&joined));
        assert!(b.leq(&joined));
    }

    #[test]
    fn test_map_domain() {
        let mut a: BTreeMap<String, BooleanOr> = BTreeMap::new();
        a.insert("x".into(), BooleanOr(true));
        a.insert("y".into(), BooleanOr(false));

        let mut b: BTreeMap<String, BooleanOr> = BTreeMap::new();
        b.insert("y".into(), BooleanOr(true));
        b.insert("z".into(), BooleanOr(true));

        let joined = a.join(&b);
        assert_eq!(joined.get("x"), Some(&BooleanOr(true)));
        assert_eq!(joined.get("y"), Some(&BooleanOr(true)));
        assert_eq!(joined.get("z"), Some(&BooleanOr(true)));
    }

    #[test]
    fn test_boolean_and_leq() {
        // In BooleanAnd: false is bottom, true is top.
        // leq means "implies": false implies everything, true only implies true.
        assert!(BooleanAnd(true).leq(&BooleanAnd(true)));
        assert!(BooleanAnd(false).leq(&BooleanAnd(false)));
        assert!(BooleanAnd(false).leq(&BooleanAnd(true)));
        assert!(!BooleanAnd(true).leq(&BooleanAnd(false)));
    }

    // --- Lattice property tests ---

    #[test]
    fn test_join_idempotent() {
        // a.join(a) == a
        let t = BooleanAnd(true);
        assert_eq!(t.join(&t), t);
        let f = BooleanAnd(false);
        assert_eq!(f.join(&f), f);
        let or_t = BooleanOr(true);
        assert_eq!(or_t.join(&or_t), or_t);
        let s: BTreeSet<i32> = [1, 2].into_iter().collect();
        assert_eq!(s.join(&s), s);
    }

    #[test]
    fn test_join_commutative() {
        // a.join(b) == b.join(a)
        let t = BooleanAnd(true);
        let f = BooleanAnd(false);
        assert_eq!(t.join(&f), f.join(&t));

        let a: BTreeSet<i32> = [1, 2].into_iter().collect();
        let b: BTreeSet<i32> = [2, 3].into_iter().collect();
        assert_eq!(a.join(&b), b.join(&a));
    }

    #[test]
    fn test_leq_reflexive() {
        // a.leq(a) is always true
        assert!(BooleanAnd(true).leq(&BooleanAnd(true)));
        assert!(BooleanAnd(false).leq(&BooleanAnd(false)));
        assert!(BooleanOr(true).leq(&BooleanOr(true)));
        assert!(BooleanOr(false).leq(&BooleanOr(false)));
        let s: BTreeSet<i32> = [1, 2, 3].into_iter().collect();
        assert!(s.leq(&s));
    }

    #[test]
    fn test_leq_antisymmetric() {
        // if a.leq(b) && b.leq(a) then a == b
        let a: BTreeSet<i32> = [1, 2].into_iter().collect();
        let b: BTreeSet<i32> = [1, 2].into_iter().collect();
        assert!(a.leq(&b) && b.leq(&a));
        assert_eq!(a, b);

        let c: BTreeSet<i32> = [1, 2, 3].into_iter().collect();
        assert!(a.leq(&c));
        assert!(!c.leq(&a));
    }
}
