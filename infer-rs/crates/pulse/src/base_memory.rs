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
use std::sync::Arc;

use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::value_history::{ValueHistory, ValueWithHistory};

/// Edges from a single heap address: maps accesses to target addresses.
///
/// Mirrors OCaml's `RecencyMap`: keep the most recently modified batch plus
/// the previous batch, each bounded by `pulse-recency-limit`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    new_keys: Vec<Access>,
    old_keys: Vec<Access>,
    values: BTreeMap<Access, ValueWithHistory>,
}

impl Edges {
    pub fn empty() -> Self {
        Self::default()
    }

    fn configured_limit() -> Option<usize> {
        config::get().pulse_recency_limit.map(|limit| limit.max(1))
    }

    fn remove_key(keys: &mut Vec<Access>, access: &Access) -> bool {
        let Some(index) = keys.iter().position(|existing| existing == access) else {
            return false;
        };
        keys.remove(index);
        true
    }

    fn retain_tracked_values(&mut self) {
        let tracked_new = self.new_keys.clone();
        let tracked_old = self.old_keys.clone();
        self.values
            .retain(|access, _| tracked_new.contains(access) || tracked_old.contains(access));
    }

    fn recency_bindings(&self) -> Vec<(&Access, &ValueWithHistory)> {
        if self.new_keys.is_empty() && self.old_keys.is_empty() {
            return self.values.iter().collect();
        }
        let mut bindings = Vec::with_capacity(self.values.len());
        for access in &self.new_keys {
            if let Some(value) = self.values.get(access) {
                bindings.push((access, value));
            }
        }
        for access in &self.old_keys {
            if self.new_keys.contains(access) {
                continue;
            }
            if let Some(value) = self.values.get(access) {
                bindings.push((access, value));
            }
        }
        bindings
    }

    fn recency_bindings_cloned(&self) -> Vec<(Access, ValueWithHistory)> {
        self.recency_bindings()
            .into_iter()
            .map(|(access, value)| (access.clone(), value.clone()))
            .collect()
    }

    fn from_recency_bindings_limited(
        bindings_in_recency_order: Vec<(Access, ValueWithHistory)>,
        limit: usize,
    ) -> Self {
        let mut edges = Self::empty();
        for (access, value) in bindings_in_recency_order.into_iter().rev() {
            edges.add_with_history_limited(access, value, limit);
        }
        edges
    }

    pub fn add(&mut self, access: Access, target: AbstractValue) {
        self.add_with_history(access, ValueWithHistory::new(target, ValueHistory::epoch()));
    }

    pub fn add_with_history(&mut self, access: Access, value: ValueWithHistory) {
        let Some(limit) = Self::configured_limit() else {
            self.values.insert(access, value);
            return;
        };
        self.add_with_history_limited(access, value, limit);
    }

    fn add_with_history_limited(&mut self, access: Access, value: ValueWithHistory, limit: usize) {
        let limit = limit.max(1);
        Self::remove_key(&mut self.old_keys, &access);
        Self::remove_key(&mut self.new_keys, &access);
        self.values.insert(access.clone(), value);

        let next_count_new = self.new_keys.len() + 1;
        if next_count_new > limit {
            self.old_keys = std::mem::take(&mut self.new_keys);
            self.new_keys = vec![access];
            self.retain_tracked_values();
        } else {
            self.new_keys.insert(0, access);
        }
    }

    pub fn remove(&mut self, access: &Access) {
        self.values.remove(access);
        Self::remove_key(&mut self.new_keys, access);
        Self::remove_key(&mut self.old_keys, access);
    }

    pub fn find(&self, access: &Access) -> Option<AbstractValue> {
        self.find_with_history(access).map(|value| value.addr)
    }

    pub fn find_with_history(&self, access: &Access) -> Option<&ValueWithHistory> {
        self.values.get(access)
    }

    /// OCaml-style `find_edge_opt` fallback for `ArrayAccess`: if the direct
    /// lookup misses and `access` is an `ArrayAccess`, canonicalize the index
    /// (and the indices of every existing edge) through `get_var_repr` and
    /// retry. This lets two reads `arr[i]` and `arr[j]` share the same edge
    /// when the formula proves `i = j`, which is the dominant value-sharing
    /// mechanism Pulse uses to keep per-disjunct unique-value counts small
    /// inside encryption-style byte loops.
    ///
    /// Cross-ref: OCaml `PulseBaseMemory.find_edge_opt`.
    pub fn find_with_history_canonicalized(
        &self,
        access: &Access,
        get_var_repr: impl Fn(AbstractValue) -> AbstractValue,
    ) -> Option<&ValueWithHistory> {
        if let Some(direct) = self.values.get(access) {
            return Some(direct);
        }
        match access {
            Access::ArrayAccess(_, _) => {
                let canonical_access = access.canonicalize(&get_var_repr);
                self.values.iter().find_map(|(existing, value)| {
                    let canonical_existing = existing.canonicalize(&get_var_repr);
                    if canonical_existing == canonical_access {
                        Some(value)
                    } else {
                        None
                    }
                })
            }
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Access, &AbstractValue)> {
        self.values
            .iter()
            .map(|(access, value)| (access, &value.addr))
    }

    pub fn iter_with_history(&self) -> impl Iterator<Item = (&Access, &ValueWithHistory)> {
        self.values.iter()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    fn first_mapping_change(
        &self,
        mut f: impl FnMut(AbstractValue) -> AbstractValue,
    ) -> Option<(AbstractValue, AbstractValue)> {
        for (access, value) in self.iter_with_history() {
            if let Access::ArrayAccess(_, index) = access {
                let new_index = f(*index);
                if new_index != *index {
                    return Some((*index, new_index));
                }
            }
            let new_addr = f(value.addr);
            if new_addr != value.addr {
                return Some((value.addr, new_addr));
            }
        }
        None
    }

    fn mapped_values(&self, mut f: impl FnMut(AbstractValue) -> AbstractValue) -> Option<Self> {
        let mut changed = false;
        let mut access_changed = false;
        let mut rewritten = Vec::with_capacity(self.values.len());

        for (access, value) in self.recency_bindings() {
            let access = match access {
                Access::ArrayAccess(typ, index) => {
                    let index = *index;
                    let new_index = f(index);
                    if new_index != index {
                        changed = true;
                        access_changed = true;
                    }
                    Access::ArrayAccess(typ.clone(), new_index)
                }
                Access::FieldAccess(_) | Access::Dereference => access.clone(),
            };
            let mut value = value.clone();
            let new_addr = f(value.addr);
            if new_addr != value.addr {
                changed = true;
                value.addr = new_addr;
            }
            rewritten.push((access, value));
        }

        if !changed {
            return None;
        }

        if !access_changed {
            let mut edges = self.clone();
            for (access, value) in rewritten {
                if let Some(existing) = edges.values.get_mut(&access) {
                    existing.addr = value.addr;
                }
            }
            return Some(edges);
        }

        Some(match Self::configured_limit() {
            Some(limit) => Self::from_recency_bindings_limited(rewritten, limit),
            None => Self {
                new_keys: Vec::new(),
                old_keys: Vec::new(),
                values: rewritten.into_iter().collect(),
            },
        })
    }

    /// Rewrite edge targets/access indices through an arbitrary value mapper.
    pub fn map_values(&mut self, f: impl FnMut(AbstractValue) -> AbstractValue) -> bool {
        let Some(rewritten) = self.mapped_values(f) else {
            return false;
        };
        *self = rewritten;
        true
    }

    /// Substitute abstract values in edge targets.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        let rewritten = self
            .recency_bindings_cloned()
            .into_iter()
            .map(|(access, mut value)| {
                let access = access.canonicalize(|v| if v == old { new } else { v });
                if value.addr == old {
                    value.addr = new;
                }
                (access, value)
            })
            .collect();
        *self = match Self::configured_limit() {
            Some(limit) => Self::from_recency_bindings_limited(rewritten, limit),
            None => Self {
                new_keys: Vec::new(),
                old_keys: Vec::new(),
                values: rewritten.into_iter().collect(),
            },
        };
    }
}

/// The heap graph: maps abstract addresses to their outgoing edges.
///
/// Two layers of structural sharing:
/// - the outer `BTreeMap<AbstractValue, Arc<Edges>>` is itself wrapped in
///   `Arc<...>` so cloning the surrounding abductive state never deep-copies
///   the heap graph eagerly; mutating accesses use `Arc::make_mut`
///   (clone-on-write) to keep the same `&mut self` API while preserving
///   sharing across disjuncts and retained invariant snapshots.
/// - each address still points to its own `Arc<Edges>` so per-address edge
///   bundles stay refcount-shared after the outer map is cloned-on-write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseMemory {
    graph: Arc<BTreeMap<AbstractValue, Arc<Edges>>>,
}

impl BaseMemory {
    pub fn empty() -> Self {
        Self::default()
    }

    fn graph_mut(&mut self) -> &mut BTreeMap<AbstractValue, Arc<Edges>> {
        Arc::make_mut(&mut self.graph)
    }

    fn entry_mut(&mut self, addr: AbstractValue) -> &mut Edges {
        let arc = self.graph_mut().entry(addr).or_default();
        Arc::make_mut(arc)
    }

    /// Register an address in the heap (ensures it has an entry, even if empty).
    pub fn register_address(&mut self, addr: AbstractValue) {
        if self.graph.contains_key(&addr) {
            return;
        }
        self.graph_mut().entry(addr).or_default();
    }

    /// Add an edge: `src --access--> target`.
    pub fn add_edge(&mut self, src: AbstractValue, access: Access, target: AbstractValue) {
        self.entry_mut(src).add(access, target);
    }

    /// Add an edge together with the target provenance.
    pub fn add_edge_with_history(
        &mut self,
        src: AbstractValue,
        access: Access,
        value: ValueWithHistory,
    ) {
        self.entry_mut(src).add_with_history(access, value);
    }

    /// Find the target of an edge: `src --access--> ?`.
    pub fn find_edge(&self, src: AbstractValue, access: &Access) -> Option<AbstractValue> {
        self.graph.get(&src).and_then(|edges| edges.find(access))
    }

    /// Find the target of an edge together with its provenance.
    pub fn find_edge_with_history(
        &self,
        src: AbstractValue,
        access: &Access,
    ) -> Option<&ValueWithHistory> {
        self.graph
            .get(&src)
            .and_then(|edges| edges.find_with_history(access))
    }

    /// OCaml-style `find_edge_opt` with `get_var_repr`: if the direct lookup
    /// misses on an `ArrayAccess`, canonicalize and retry. See
    /// [`Edges::find_with_history_canonicalized`] for rationale.
    pub fn find_edge_with_history_canonicalized(
        &self,
        src: AbstractValue,
        access: &Access,
        get_var_repr: impl Fn(AbstractValue) -> AbstractValue,
    ) -> Option<&ValueWithHistory> {
        self.graph
            .get(&src)
            .and_then(|edges| edges.find_with_history_canonicalized(access, get_var_repr))
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
        self.graph.get(&addr).map(Arc::as_ref)
    }

    /// Remove all outgoing edges for an address.
    pub fn remove(&mut self, addr: AbstractValue) {
        if !self.graph.contains_key(&addr) {
            return;
        }
        self.graph_mut().remove(&addr);
    }

    /// Replace all outgoing edges for an address.
    pub fn set_edges(&mut self, addr: AbstractValue, edges: Edges) {
        if edges.is_empty() {
            self.remove(addr);
        } else {
            self.graph_mut().insert(addr, Arc::new(edges));
        }
    }

    /// Keep only reachable heap cells that still have outgoing edges.
    ///
    /// Mirrors the summary filtering in OCaml's `discard_unreachable_`,
    /// which drops dead heap cells entirely instead of leaving empty nodes
    /// behind in exported summaries.
    pub fn retain_reachable(&mut self, reachable: &std::collections::HashSet<AbstractValue>) {
        if self
            .graph
            .iter()
            .all(|(addr, edges)| reachable.contains(addr) && !edges.is_empty())
        {
            return;
        }
        self.graph_mut()
            .retain(|addr, edges| reachable.contains(addr) && !edges.is_empty());
    }

    /// Iterate over all addresses and their edges.
    pub fn iter(&self) -> impl Iterator<Item = (&AbstractValue, &Edges)> {
        self.graph
            .iter()
            .map(|(addr, edges)| (addr, edges.as_ref()))
    }

    /// Number of addresses in the heap.
    pub fn len(&self) -> usize {
        self.graph.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graph.is_empty()
    }

    pub fn first_mapping_change(
        &self,
        mut f: impl FnMut(AbstractValue) -> AbstractValue,
    ) -> Option<(AbstractValue, AbstractValue)> {
        for (src, edges) in self.graph.iter() {
            let new_src = f(*src);
            if new_src != *src {
                return Some((*src, new_src));
            }
            if let Some(change) = edges.first_mapping_change(&mut f) {
                return Some(change);
            }
        }
        None
    }

    /// Rewrite every heap source/target/index through an arbitrary value mapper.
    pub fn map_values(&mut self, mut f: impl FnMut(AbstractValue) -> AbstractValue) -> bool {
        let mut changed = false;
        let mut src_changed = false;
        let mut rewritten = Vec::with_capacity(self.graph.len());

        for (src, edges) in self.graph.iter() {
            let mapped_edges = edges.mapped_values(&mut f);
            let new_src = f(*src);
            if new_src != *src {
                changed = true;
                src_changed = true;
            }
            if mapped_edges.is_some() {
                changed = true;
            }
            rewritten.push((*src, new_src, mapped_edges, Arc::clone(edges)));
        }

        if !changed {
            return false;
        }

        if src_changed {
            // This is a wholesale key rewrite. Assign a fresh Arc instead of
            // going through graph_mut()/Arc::make_mut(), which would first
            // clone the old BTreeMap only to overwrite it immediately.
            self.graph = Arc::new(
                rewritten
                    .into_iter()
                    .map(|(_src, new_src, mapped_edges, old_edges)| {
                        (new_src, mapped_edges.map_or(old_edges, Arc::new))
                    })
                    .collect(),
            );
        } else {
            // OCaml's persistent maps return the original node when an update
            // is physically unchanged. Mirror that sharing here: when heap
            // roots stay canonical, replace only the addresses whose edge
            // bundle actually changed instead of rebuilding the whole graph.
            let graph = self.graph_mut();
            for (src, _new_src, mapped_edges, _old_edges) in rewritten {
                if let Some(edges) = mapped_edges {
                    graph.insert(src, Arc::new(edges));
                }
            }
        }
        true
    }

    /// Substitute abstract values: replace `old` with `new` in both
    /// addresses and edge targets.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        let needs_target_subst = self.graph.values().any(|edges| {
            edges
                .iter_with_history()
                .any(|(access, value)| value.addr == old || access_mentions(access, old))
        });
        let needs_key_subst = self.graph.contains_key(&old);
        if !needs_target_subst && !needs_key_subst {
            return;
        }
        let graph = self.graph_mut();
        for edges in graph.values_mut() {
            Arc::make_mut(edges).subst_var(old, new);
        }
        if let Some(edges) = graph.remove(&old) {
            // Merge with existing edges at `new` if any.
            let entry_arc = graph.entry(new).or_default();
            let entry = Arc::make_mut(entry_arc);
            for (access, value) in edges.iter_with_history() {
                entry.add_with_history(access.clone(), value.clone());
            }
        }
    }
}

fn access_mentions(access: &Access, old: AbstractValue) -> bool {
    matches!(access, Access::ArrayAccess(_, idx) if *idx == old)
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
    use sil::fieldname::Fieldname;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::typ::TypeName;

    fn field_access(name: &str) -> Access {
        Access::FieldAccess(Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("S")),
            name,
        ))
    }

    #[test]
    fn test_edges_recency_spills_old_batch() {
        let mut edges = Edges::empty();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);
        let v4 = AbstractValue::of_raw(4);
        let v5 = AbstractValue::of_raw(5);

        edges.add_with_history_limited(
            field_access("a"),
            ValueWithHistory::new(v1, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("b"),
            ValueWithHistory::new(v2, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("c"),
            ValueWithHistory::new(v3, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("d"),
            ValueWithHistory::new(v4, ValueHistory::epoch()),
            2,
        );
        assert_eq!(edges.len(), 4);

        edges.add_with_history_limited(
            field_access("e"),
            ValueWithHistory::new(v5, ValueHistory::epoch()),
            2,
        );

        assert_eq!(edges.len(), 3);
        assert_eq!(edges.find(&field_access("a")), None);
        assert_eq!(edges.find(&field_access("b")), None);
        assert_eq!(edges.find(&field_access("c")), Some(v3));
        assert_eq!(edges.find(&field_access("d")), Some(v4));
        assert_eq!(edges.find(&field_access("e")), Some(v5));
    }

    #[test]
    fn test_edges_recency_update_from_old_promotes_binding() {
        let mut edges = Edges::empty();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);
        let v4 = AbstractValue::of_raw(4);

        edges.add_with_history_limited(
            field_access("a"),
            ValueWithHistory::new(v1, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("b"),
            ValueWithHistory::new(v2, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("c"),
            ValueWithHistory::new(v3, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            field_access("a"),
            ValueWithHistory::new(v4, ValueHistory::epoch()),
            2,
        );

        assert_eq!(edges.len(), 3);
        assert_eq!(edges.find(&field_access("a")), Some(v4));
        assert_eq!(edges.find(&field_access("b")), Some(v2));
        assert_eq!(edges.find(&field_access("c")), Some(v3));
    }

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

    #[test]
    fn test_subst_var_rewrites_array_index_access() {
        let mut mem = BaseMemory::empty();
        let base = AbstractValue::of_raw(1);
        let old_idx = AbstractValue::of_raw(2);
        let new_idx = AbstractValue::of_raw(3);
        let target = AbstractValue::of_raw(4);

        mem.add_edge(
            base,
            Access::ArrayAccess(sil::typ::Typ::void(), old_idx),
            target,
        );
        mem.subst_var(old_idx, new_idx);

        assert_eq!(
            mem.find_edge(base, &Access::ArrayAccess(sil::typ::Typ::void(), new_idx)),
            Some(target)
        );
        assert_eq!(
            mem.find_edge(base, &Access::ArrayAccess(sil::typ::Typ::void(), old_idx)),
            None
        );
    }
}
