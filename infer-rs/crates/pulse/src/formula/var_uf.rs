// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Union-Find for variable equality classes.
//!
//! Mirrors OCaml's `VarUF` (via `UnionFind.Make`).
//!
//! Tracks which abstract values are known to be equal. The canonical
//! representative of each class mirrors OCaml's `PulseFormulaVar` order.

use std::collections::HashMap;

use crate::abstract_value::AbstractValue;

/// Union-Find with path compression and union-by-rank.
#[derive(Clone, Debug, Default)]
pub struct VarUF {
    parent: HashMap<AbstractValue, AbstractValue>,
    rank: HashMap<AbstractValue, usize>,
}

impl PartialEq for VarUF {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_equalities() == other.canonical_equalities()
    }
}

impl Eq for VarUF {}

impl VarUF {
    pub fn new() -> Self {
        Self::default()
    }

    /// Find the canonical representative of a variable (with path compression).
    pub fn find(&mut self, v: AbstractValue) -> AbstractValue {
        let p = self.parent.get(&v).copied();
        match p {
            None => v, // v is its own representative
            Some(parent) if parent == v => v,
            Some(parent) => {
                let root = self.find(parent);
                // Path compression
                self.parent.insert(v, root);
                root
            }
        }
    }

    /// Find without mutation (for read-only contexts).
    pub fn find_immut(&self, v: AbstractValue) -> AbstractValue {
        let mut current = v;
        loop {
            match self.parent.get(&current) {
                None => return current,
                Some(&p) if p == current => return current,
                Some(&p) => current = p,
            }
        }
    }

    /// Union two variables into the same equivalence class.
    ///
    /// The simpler variable (lower raw id) becomes the representative.
    /// Returns `Some((old_repr, new_repr))` if the classes were different,
    /// or `None` if they were already in the same class.
    pub fn union(
        &mut self,
        v1: AbstractValue,
        v2: AbstractValue,
    ) -> Option<(AbstractValue, AbstractValue)> {
        let r1 = self.find(v1);
        let r2 = self.find(v2);
        if r1 == r2 {
            return None; // already in same class
        }

        // The "simpler" variable becomes the representative.
        let (keep, merge) = if is_simpler(r1, r2) {
            (r1, r2)
        } else {
            (r2, r1)
        };

        let rank_keep = self.rank.get(&keep).copied().unwrap_or(0);
        let rank_merge = self.rank.get(&merge).copied().unwrap_or(0);

        self.parent.insert(merge, keep);
        if rank_keep == rank_merge {
            self.rank.insert(keep, rank_keep + 1);
        }

        Some((merge, keep))
    }

    /// Get the canonical representative without mutation.
    pub fn get_repr(&self, v: AbstractValue) -> AbstractValue {
        self.find_immut(v)
    }

    /// Number of parent links tracked by the union-find.
    pub fn len(&self) -> usize {
        self.parent.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parent.is_empty()
    }

    /// Iterate over all non-trivial equivalence classes.
    pub fn iter_equalities(&self) -> impl Iterator<Item = (AbstractValue, AbstractValue)> + '_ {
        self.parent
            .iter()
            .filter(|(k, v)| k != v)
            .map(|(&k, &v)| (k, self.find_immut(v)))
    }

    /// Canonicalize all non-trivial equalities, ignoring implementation
    /// details like path-compression shape and union ranks.
    fn canonical_equalities(&self) -> HashMap<AbstractValue, AbstractValue> {
        self.parent
            .iter()
            .filter_map(|(&child, &parent)| {
                if child == parent {
                    None
                } else {
                    Some((child, self.find_immut(parent)))
                }
            })
            .collect()
    }
}

/// Mirror OCaml `PulseFormulaVar.is_simpler_than`, which delegates to
/// `PulseAbstractValue.compare` over raw ids. Restricted values are negative
/// there, so a restricted representative is preferred over an unrestricted
/// one. This matters when a field cursor is proven equal to its root: OCaml
/// keeps the first field-derived value as the summary representative instead
/// of rewriting the cycle target back to the root.
fn is_simpler(v1: AbstractValue, v2: AbstractValue) -> bool {
    v1.raw() < v2.raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_self() {
        let mut uf = VarUF::new();
        let v = AbstractValue::of_raw(1);
        assert_eq!(uf.find(v), v);
    }

    #[test]
    fn test_union_and_find() {
        let mut uf = VarUF::new();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        let result = uf.union(v1, v2);
        assert!(result.is_some());

        // Both should now have the same representative
        assert_eq!(uf.find(v1), uf.find(v2));
        // v1 is simpler (lower raw id), so it should be the representative
        assert_eq!(uf.find(v2), v1);
    }

    #[test]
    fn test_union_idempotent() {
        let mut uf = VarUF::new();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        uf.union(v1, v2);
        let result = uf.union(v1, v2);
        assert!(result.is_none()); // already same class
    }

    #[test]
    fn test_transitive_union() {
        let mut uf = VarUF::new();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);

        uf.union(v1, v2);
        uf.union(v2, v3);

        // All three should be in the same class
        let r = uf.find(v1);
        assert_eq!(uf.find(v2), r);
        assert_eq!(uf.find(v3), r);
    }

    #[test]
    fn test_union_prefers_ocaml_raw_order_for_restricted_values() {
        let mut uf = VarUF::new();
        let unrestricted = AbstractValue::of_raw(1);
        let restricted = AbstractValue::of_raw(-1);

        assert_eq!(
            uf.union(unrestricted, restricted),
            Some((unrestricted, restricted))
        );
        assert_eq!(uf.find(unrestricted), restricted);
        assert_eq!(uf.find(restricted), restricted);
    }

    #[test]
    fn test_equality_ignores_path_compression_shape() {
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);

        let mut left = VarUF::new();
        left.union(v2, v3);
        left.union(v1, v2);

        let mut right = VarUF::new();
        right.union(v1, v2);
        right.union(v1, v3);

        assert_eq!(left, right);
    }
}
