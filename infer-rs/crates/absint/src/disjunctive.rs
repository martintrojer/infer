// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Disjunctive domain: a bounded set of abstract states.
//!
//! Mirrors OCaml's `MakeDisjunctiveTransferFunctions.Domain`.
//!
//! The domain is a list of disjuncts representing `x1 ∨ x2 ∨ ... ∨ xN`.
//! Join = union of disjunct lists (bounded by `max_disjuncts`).
//! Widen = at loop heads, stop adding new disjuncts after `max_widen_iters`.
//! Leq = subset check.

use std::fmt;

use crate::domain::{AbstractDomain, Comparable, WithBottom};

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

    /// Remove duplicate/subsumed disjuncts while preserving the first
    /// occurrence, mirroring OCaml's "favor the left-hand disjuncts" join
    /// strategy.
    pub fn dedup(&mut self) {
        let mut kept = Vec::with_capacity(self.disjuncts.len());
        for disjunct in self.disjuncts.drain(..) {
            if kept.iter().any(|existing| disjunct.leq(existing)) {
                self.had_dropped_disjuncts = true;
            } else {
                kept.push(disjunct);
            }
        }
        self.disjuncts = kept;
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
        self.disjuncts
            .iter()
            .all(|d| rhs.disjuncts.iter().any(|r| d.leq(r)))
            && (!self.had_dropped_disjuncts || rhs.had_dropped_disjuncts)
    }
}

impl<D: Comparable> AbstractDomain for DisjunctiveDomain<D> {
    /// Join = union of disjuncts, bounded by max_disjuncts.
    fn join(&self, other: &Self) -> Self {
        let mut result = self.clone();
        result.had_dropped_disjuncts |= other.had_dropped_disjuncts;
        for d in &other.disjuncts {
            // Favor keeping existing disjuncts, but drop semantically
            // equivalent ones modulo the inner domain ordering.
            if !result.disjuncts.iter().any(|existing| d.leq(existing)) {
                result.disjuncts.push(d.clone());
            } else {
                result.had_dropped_disjuncts = true;
            }
        }
        result.bound();
        result
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
        self.join(next)
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
}
