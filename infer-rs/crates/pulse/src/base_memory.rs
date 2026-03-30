// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Heap domain: the abstract memory graph.
//!
//! Mirrors OCaml's `PulseBaseMemory.ml`.
//!
//! The heap is a graph: `AbstractValue → Edges`, where each edge is
//! `Access → (AbstractValue, History)`. This represents "address A has
//! field F pointing to address B".

use std::collections::BTreeMap;
use std::fmt;

use crate::abstract_value::AbstractValue;
use crate::access::Access;

/// Edges from a single heap address: maps accesses to target addresses.
///
/// OCaml uses `RecencyMap` (bounded map). We use `BTreeMap` (unbounded)
/// for simplicity; can add recency eviction later if needed.
#[derive(Clone, Debug, Default)]
pub struct Edges(BTreeMap<Access, AbstractValue>);

impl Edges {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn add(&mut self, access: Access, target: AbstractValue) {
        self.0.insert(access, target);
    }

    pub fn find(&self, access: &Access) -> Option<AbstractValue> {
        self.0.get(access).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Access, &AbstractValue)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Substitute abstract values in edge targets.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        for target in self.0.values_mut() {
            if *target == old {
                *target = new;
            }
        }
    }
}

/// The heap graph: maps abstract addresses to their outgoing edges.
#[derive(Clone, Debug, Default)]
pub struct BaseMemory {
    graph: BTreeMap<AbstractValue, Edges>,
}

impl BaseMemory {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Register an address in the heap (ensures it has an entry, even if empty).
    pub fn register_address(&mut self, addr: AbstractValue) {
        self.graph.entry(addr).or_default();
    }

    /// Add an edge: `src --access--> target`.
    pub fn add_edge(&mut self, src: AbstractValue, access: Access, target: AbstractValue) {
        self.graph.entry(src).or_default().add(access, target);
    }

    /// Find the target of an edge: `src --access--> ?`.
    pub fn find_edge(&self, src: AbstractValue, access: &Access) -> Option<AbstractValue> {
        self.graph.get(&src).and_then(|edges| edges.find(access))
    }

    /// Check if an edge exists.
    pub fn has_edge(&self, src: AbstractValue, access: &Access) -> bool {
        self.find_edge(src, access).is_some()
    }

    /// Check if an address has any outgoing edges (is "allocated").
    pub fn is_allocated(&self, addr: AbstractValue) -> bool {
        self.graph.get(&addr).is_some_and(|edges| !edges.is_empty())
    }

    /// Get all edges from an address.
    pub fn get_edges(&self, addr: AbstractValue) -> Option<&Edges> {
        self.graph.get(&addr)
    }

    /// Iterate over all addresses and their edges.
    pub fn iter(&self) -> impl Iterator<Item = (&AbstractValue, &Edges)> {
        self.graph.iter()
    }

    /// Number of addresses in the heap.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    /// Substitute abstract values: replace `old` with `new` in both
    /// addresses and edge targets.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        // Substitute in edge targets
        for edges in self.graph.values_mut() {
            edges.subst_var(old, new);
        }
        // Substitute in graph keys
        if let Some(edges) = self.graph.remove(&old) {
            // Merge with existing edges at `new` if any
            let entry = self.graph.entry(new).or_default();
            for (access, target) in edges.iter() {
                entry.add(access.clone(), *target);
            }
        }
    }
}

impl fmt::Display for BaseMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries: Vec<_> = self.graph.iter().collect();
        entries.sort_by_key(|(addr, _)| **addr);
        for (addr, edges) in &entries {
            if !edges.is_empty() {
                for (access, target) in edges.iter() {
                    write!(f, "{addr} -{access}-> {target}; ")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_and_find_edge() {
        let mut mem = BaseMemory::empty();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        mem.add_edge(v1, Access::Dereference, v2);
        assert_eq!(mem.find_edge(v1, &Access::Dereference), Some(v2));
        assert!(mem.is_allocated(v1));
    }

    #[test]
    fn test_no_edge() {
        let mem = BaseMemory::empty();
        let v1 = AbstractValue::of_raw(1);
        assert_eq!(mem.find_edge(v1, &Access::Dereference), None);
        assert!(!mem.is_allocated(v1));
    }

    #[test]
    fn test_subst_var() {
        let mut mem = BaseMemory::empty();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);

        mem.add_edge(v1, Access::Dereference, v2);
        mem.subst_var(v2, v3);

        // v1 --*--> v3 (v2 replaced by v3 in target)
        assert_eq!(mem.find_edge(v1, &Access::Dereference), Some(v3));
    }
}
