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
pub struct DisjunctiveDomain<D: Clone + fmt::Debug + PartialEq> {
    pub disjuncts: Vec<D>,
    pub max_disjuncts: usize,
    pub max_widen_iters: usize,
}

impl<D: Clone + fmt::Debug + PartialEq> DisjunctiveDomain<D> {
    /// Create a domain with a single initial disjunct.
    pub fn singleton(d: D, max_disjuncts: usize, max_widen_iters: usize) -> Self {
        Self {
            disjuncts: vec![d],
            max_disjuncts,
            max_widen_iters,
        }
    }

    /// Create an empty (bottom) domain.
    pub fn empty(max_disjuncts: usize, max_widen_iters: usize) -> Self {
        Self {
            disjuncts: Vec::new(),
            max_disjuncts,
            max_widen_iters,
        }
    }

    /// Bound the number of disjuncts, dropping oldest when over the limit.
    pub fn bound(&mut self) {
        if self.disjuncts.len() > self.max_disjuncts {
            let excess = self.disjuncts.len() - self.max_disjuncts;
            self.disjuncts.drain(..excess);
        }
    }
}

impl<D: Clone + fmt::Debug + PartialEq> PartialEq for DisjunctiveDomain<D> {
    fn eq(&self, other: &Self) -> bool {
        self.disjuncts == other.disjuncts
    }
}

impl<D: Clone + fmt::Debug + PartialEq> Comparable for DisjunctiveDomain<D> {
    /// Subset check: lhs ≤ rhs if every disjunct in lhs has a matching
    /// disjunct in rhs (by equality). This is a fast approximation —
    /// OCaml uses pointer equality for speed.
    fn leq(&self, rhs: &Self) -> bool {
        self.disjuncts
            .iter()
            .all(|d| rhs.disjuncts.iter().any(|r| d == r))
    }
}

impl<D: Clone + fmt::Debug + PartialEq> AbstractDomain for DisjunctiveDomain<D> {
    /// Join = union of disjuncts, bounded by max_disjuncts.
    fn join(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for d in &other.disjuncts {
            // Skip duplicates (deduplication)
            if !result.disjuncts.iter().any(|existing| existing == d) {
                result.disjuncts.push(d.clone());
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
            return self.clone();
        }
        self.join(next)
    }
}

impl<D: Clone + fmt::Debug + PartialEq> WithBottom for DisjunctiveDomain<D> {
    fn bottom() -> Self {
        // Can't know the config at bottom() — use sensible defaults.
        // The actual values get overridden when singleton() is used.
        Self::empty(20, 3)
    }

    fn is_bottom(&self) -> bool {
        self.disjuncts.is_empty()
    }
}

impl<D: Clone + fmt::Debug + PartialEq> fmt::Display for DisjunctiveDomain<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} disjuncts", self.disjuncts.len())
    }
}
