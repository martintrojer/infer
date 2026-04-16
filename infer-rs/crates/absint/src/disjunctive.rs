// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Disjunctive domain: a bounded set of abstract states.
//!
//! Mirrors OCaml's `MakeDisjunctiveTransferFunctions.Domain`.
//!
//! The domain is a list of disjuncts representing `x1 ∨ x2 ∨ ... ∨ xN`.
//! Join = union of disjunct lists using a cheap equality predicate
//! (bounded by `max_disjuncts`).
//! Widen = at loop heads, keep semantic subsumption checks and stop adding new
//! disjuncts after `max_widen_iters`.
//! Leq = subset check.

use crate::domain::{AbstractDomain, Comparable, WithBottom};
use std::fmt;

/// A bounded disjunctive domain.
///
/// Wraps a list of abstract states representing alternative execution paths.
/// The number of disjuncts is bounded to prevent exponential blowup.
#[derive(Clone, Debug)]
pub struct DisjunctiveDomain<D: Comparable> {
    pub disjuncts: Vec<D>,
    pub max_disjuncts: usize,
    pub max_widen_iters: usize,
    pub had_dropped_disjuncts: bool,
}

impl<D: Comparable> DisjunctiveDomain<D> {
    /// Create a domain with a single initial disjunct.
    pub fn singleton(d: D, max_disjuncts: usize, max_widen_iters: usize) -> Self {
        Self {
            disjuncts: vec![d],
            max_disjuncts,
            max_widen_iters,
            had_dropped_disjuncts: false,
        }
    }

    /// Create an empty (bottom) domain.
    pub fn empty(max_disjuncts: usize, max_widen_iters: usize) -> Self {
        Self {
            disjuncts: Vec::new(),
            max_disjuncts,
            max_widen_iters,
            had_dropped_disjuncts: false,
        }
    }

    /// Bound the number of disjuncts, dropping oldest when over the limit.
    pub fn bound(&mut self) {
        if self.disjuncts.len() > self.max_disjuncts {
            let excess = self.disjuncts.len() - self.max_disjuncts;
            self.disjuncts.drain(..excess);
            self.had_dropped_disjuncts = true;
        }
    }

    /// Remove duplicate disjuncts while preserving the first occurrence.
    ///
    /// Cross-ref: OCaml `AbstractInterpreter.MakeDisjunctiveTransferFunctions`
    /// uses `equal_fast` for instruction-level disjunct joins and keeps the
    /// earlier (left-hand) disjunct.
    pub fn dedup(&mut self) {
        self.dedup_with(|lhs, rhs| lhs.equal_fast(rhs));
    }

    fn dedup_with<F>(&mut self, mut subsumes: F)
    where
        F: FnMut(&D, &D) -> bool,
    {
        let mut kept = Vec::with_capacity(self.disjuncts.len());
        for disjunct in self.disjuncts.drain(..) {
            if kept.iter().any(|existing| subsumes(&disjunct, existing)) {
                self.had_dropped_disjuncts = true;
            } else {
                kept.push(disjunct);
            }
        }
        self.disjuncts = kept;
    }

    fn join_with<F>(&self, other: &Self, mut subsumes: F) -> Self
    where
        F: FnMut(&D, &D) -> bool,
    {
        let mut result = self.clone();
        result.had_dropped_disjuncts |= other.had_dropped_disjuncts;
        for d in &other.disjuncts {
            if !result
                .disjuncts
                .iter()
                .any(|existing| subsumes(d, existing))
            {
                result.disjuncts.push(d.clone());
            } else {
                result.had_dropped_disjuncts = true;
            }
        }
        result.bound();
        result
    }

    /// Cross-ref: OCaml `AbstractInterpreter.MakeDisjunctiveTransferFunctions`
    /// first checks whether the left disjunct list appears in the right list
    /// in the same order using `equal_fast` before falling back to semantic
    /// subset checks. This keeps loop-head convergence from paying the full
    /// semantic comparison cost when the disjunct sequence is unchanged.
    fn is_trivial_subset(&self, rhs: &Self) -> bool {
        if self.disjuncts.len() > rhs.disjuncts.len() {
            return false;
        }
        let mut rhs_iter = rhs.disjuncts.iter();
        for lhs in &self.disjuncts {
            loop {
                match rhs_iter.next() {
                    Some(rhs_disjunct) if lhs.equal_fast(rhs_disjunct) => break,
                    Some(_) => continue,
                    None => return false,
                }
            }
        }
        true
    }
}

impl<D: Comparable> PartialEq for DisjunctiveDomain<D> {
    fn eq(&self, other: &Self) -> bool {
        self.disjuncts == other.disjuncts
            && self.had_dropped_disjuncts == other.had_dropped_disjuncts
    }
}

impl<D: Comparable> Comparable for DisjunctiveDomain<D> {
    /// Subset check: lhs ≤ rhs if every disjunct in lhs has a matching
    /// disjunct in rhs under the inner domain ordering.
    fn leq(&self, rhs: &Self) -> bool {
        if self.had_dropped_disjuncts && !rhs.had_dropped_disjuncts {
            return false;
        }
        if self.equal_fast(rhs) || self.is_trivial_subset(rhs) {
            return true;
        }
        self.disjuncts
            .iter()
            .all(|d| rhs.disjuncts.iter().any(|r| d.leq(r)))
    }

    fn equal_fast(&self, rhs: &Self) -> bool {
        self.had_dropped_disjuncts == rhs.had_dropped_disjuncts
            && self.disjuncts.len() == rhs.disjuncts.len()
            && self
                .disjuncts
                .iter()
                .zip(&rhs.disjuncts)
                .all(|(lhs, rhs)| lhs.equal_fast(rhs))
    }
}

impl<D: Comparable> AbstractDomain for DisjunctiveDomain<D> {
    /// Join = union of disjuncts using cheap equality, bounded by
    /// `max_disjuncts`.
    fn join(&self, other: &Self) -> Self {
        self.join_with(other, |lhs, rhs| lhs.equal_fast(rhs))
    }

    /// Widen = at loop heads, stop adding after max_widen_iters.
    /// Under-approximation: once we exceed the iteration limit, keep prev.
    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        if num_iters > self.max_widen_iters {
            // Stop exploring new paths — keep previous state
            let mut result = self.clone();
            if !next.leq(self) {
                result.had_dropped_disjuncts = true;
            }
            return result;
        }
        // Cross-ref: OCaml widens with semantic `leq` at loop heads even
        // though its ordinary disjunctive join uses `equal_fast`.
        self.join_with(next, |lhs, rhs| lhs.leq(rhs))
    }
}

impl<D: Comparable> WithBottom for DisjunctiveDomain<D> {
    fn bottom() -> Self {
        // Can't know the config at bottom() — use sensible defaults.
        // The actual values get overridden when singleton() is used.
        Self::empty(20, 3)
    }

    fn is_bottom(&self) -> bool {
        self.disjuncts.is_empty()
    }
}

impl<D: Comparable> fmt::Display for DisjunctiveDomain<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} disjuncts", self.disjuncts.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestDisjunct(u8);

    impl Comparable for TestDisjunct {
        fn leq(&self, rhs: &Self) -> bool {
            self == rhs
        }
    }

    impl AbstractDomain for TestDisjunct {
        fn join(&self, other: &Self) -> Self {
            if self == other {
                self.clone()
            } else {
                other.clone()
            }
        }

        fn widen(&self, next: &Self, _num_iters: usize) -> Self {
            self.join(next)
        }
    }

    #[test]
    fn test_join_marks_duplicate_disjuncts_as_dropped() {
        let lhs = DisjunctiveDomain::singleton(TestDisjunct(1), 20, 3);
        let rhs = DisjunctiveDomain::singleton(TestDisjunct(1), 20, 3);

        let joined = lhs.join(&rhs);

        assert_eq!(joined.disjuncts, vec![TestDisjunct(1)]);
        assert!(
            joined.had_dropped_disjuncts,
            "deduplicated join should preserve OCaml-style dropped-disjunct metadata"
        );
    }

    #[test]
    fn test_join_keeps_drop_flag_clear_when_nothing_is_discarded() {
        let lhs = DisjunctiveDomain::singleton(TestDisjunct(1), 20, 3);
        let rhs = DisjunctiveDomain::singleton(TestDisjunct(2), 20, 3);

        let joined = lhs.join(&rhs);

        assert_eq!(joined.disjuncts, vec![TestDisjunct(1), TestDisjunct(2)]);
        assert!(!joined.had_dropped_disjuncts);
    }

    #[test]
    fn test_dedup_marks_duplicate_disjuncts_as_dropped() {
        let mut domain = DisjunctiveDomain {
            disjuncts: vec![TestDisjunct(1), TestDisjunct(1), TestDisjunct(2)],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };

        domain.dedup();

        assert_eq!(domain.disjuncts, vec![TestDisjunct(1), TestDisjunct(2)]);
        assert!(domain.had_dropped_disjuncts);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct GroupedDisjunct {
        raw_id: u8,
        semantic_class: u8,
    }

    impl Comparable for GroupedDisjunct {
        fn leq(&self, rhs: &Self) -> bool {
            self.semantic_class == rhs.semantic_class
        }

        fn equal_fast(&self, rhs: &Self) -> bool {
            self.raw_id == rhs.raw_id
        }
    }

    impl AbstractDomain for GroupedDisjunct {
        fn join(&self, other: &Self) -> Self {
            if self == other {
                self.clone()
            } else {
                other.clone()
            }
        }

        fn widen(&self, next: &Self, _num_iters: usize) -> Self {
            self.join(next)
        }
    }

    #[test]
    fn test_join_uses_fast_equality_instead_of_semantic_leq() {
        let lhs = DisjunctiveDomain::singleton(
            GroupedDisjunct {
                raw_id: 1,
                semantic_class: 7,
            },
            20,
            3,
        );
        let rhs = DisjunctiveDomain::singleton(
            GroupedDisjunct {
                raw_id: 2,
                semantic_class: 7,
            },
            20,
            3,
        );

        let joined = lhs.join(&rhs);

        assert_eq!(joined.disjuncts.len(), 2);
        assert!(!joined.had_dropped_disjuncts);
    }

    #[test]
    fn test_widen_keeps_semantic_subsumption() {
        let lhs = DisjunctiveDomain::singleton(
            GroupedDisjunct {
                raw_id: 1,
                semantic_class: 7,
            },
            20,
            3,
        );
        let rhs = DisjunctiveDomain::singleton(
            GroupedDisjunct {
                raw_id: 2,
                semantic_class: 7,
            },
            20,
            3,
        );

        let widened = lhs.widen(&rhs, 1);

        assert_eq!(widened.disjuncts.len(), 1);
        assert!(widened.had_dropped_disjuncts);
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct PanicOnSemanticLeq(u8);

    impl Comparable for PanicOnSemanticLeq {
        fn leq(&self, _rhs: &Self) -> bool {
            panic!("semantic leq should not run when trivial equal_fast subset applies")
        }

        fn equal_fast(&self, rhs: &Self) -> bool {
            self.0 == rhs.0
        }
    }

    impl AbstractDomain for PanicOnSemanticLeq {
        fn join(&self, other: &Self) -> Self {
            if self == other {
                self.clone()
            } else {
                other.clone()
            }
        }

        fn widen(&self, next: &Self, _num_iters: usize) -> Self {
            self.join(next)
        }
    }

    #[test]
    fn test_leq_uses_trivial_equal_fast_subset_before_semantic_leq() {
        let lhs = DisjunctiveDomain {
            disjuncts: vec![PanicOnSemanticLeq(1), PanicOnSemanticLeq(3)],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };
        let rhs = DisjunctiveDomain {
            disjuncts: vec![
                PanicOnSemanticLeq(1),
                PanicOnSemanticLeq(2),
                PanicOnSemanticLeq(3),
            ],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };

        assert!(lhs.leq(&rhs));
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct PanicOnEqualFast;

    impl Comparable for PanicOnEqualFast {
        fn leq(&self, _rhs: &Self) -> bool {
            false
        }

        fn equal_fast(&self, _rhs: &Self) -> bool {
            panic!("equal_fast should not run when lhs cannot fit in rhs")
        }
    }

    impl AbstractDomain for PanicOnEqualFast {
        fn join(&self, other: &Self) -> Self {
            other.clone()
        }

        fn widen(&self, next: &Self, _num_iters: usize) -> Self {
            next.clone()
        }
    }

    #[test]
    fn test_trivial_subset_short_circuits_when_lhs_is_longer_than_rhs() {
        let lhs = DisjunctiveDomain {
            disjuncts: vec![PanicOnEqualFast, PanicOnEqualFast],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };
        let rhs = DisjunctiveDomain {
            disjuncts: vec![PanicOnEqualFast],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };

        assert!(!lhs.is_trivial_subset(&rhs));
    }
}
