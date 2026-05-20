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

fn map_value_after_first_change(
    value: AbstractValue,
    first_change: &mut Option<(AbstractValue, AbstractValue)>,
    f: &mut impl FnMut(AbstractValue) -> AbstractValue,
) -> AbstractValue {
    if let Some((old, new)) = *first_change {
        if value == old {
            *first_change = None;
            new
        } else {
            value
        }
    } else {
        f(value)
    }
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

    /// OCaml `RecencyMap.union_left_biased`: keep bindings from `self` when
    /// both edge sets mention the same access, then fill remaining recency
    /// budget from `right`.
    pub fn union_left_biased(&self, right: &Self) -> Self {
        let Some(limit) = Self::configured_limit() else {
            let mut values = right.values.clone();
            for (access, value) in self.iter_with_history() {
                values.insert(access.clone(), value.clone());
            }
            return Self {
                new_keys: Vec::new(),
                old_keys: Vec::new(),
                values,
            };
        };
        let limit = limit.max(1);

        fn concat_and_spill(
            mut head: Vec<Access>,
            mut count: usize,
            tail: &[Access],
            limit: usize,
        ) -> (Vec<Access>, usize, Vec<Access>) {
            if count == 0 {
                return (tail.to_vec(), tail.len(), Vec::new());
            }
            if count == limit {
                return (head, count, tail.to_vec());
            }

            let mut take = Vec::new();
            let mut rest = Vec::new();
            for access in tail {
                if count + take.len() >= limit {
                    rest.push(access.clone());
                } else if head.iter().any(|existing| existing == access)
                    || take.iter().any(|existing| existing == access)
                {
                    // Preserve OCaml's left bias when filling `new_`.
                } else {
                    take.push(access.clone());
                }
            }
            count += take.len();
            head.extend(take);
            (head, count, rest)
        }

        // Rust's `recency_bindings` yields the same newest-first order as
        // OCaml `bindings`; OCaml reverses it here because callers care about
        // the recency partitioning, not only the map contents.
        let mut left_keys: Vec<_> = self
            .recency_bindings()
            .into_iter()
            .map(|(access, _)| access.clone())
            .collect();
        left_keys.reverse();
        let left_count = left_keys.len();
        let (mut new_keys, mut count_new, mut old_keys) = if left_count <= limit {
            (left_keys, left_count, Vec::new())
        } else {
            let old_keys = left_keys.split_off(limit);
            (left_keys, limit, old_keys)
        };

        if count_new < limit {
            (new_keys, count_new, old_keys) =
                concat_and_spill(new_keys, count_new, &right.new_keys, limit);
        }
        if count_new < limit {
            let (filled_new_keys, _filled_count_new, filled_old_keys) =
                concat_and_spill(new_keys, count_new, &right.old_keys, limit);
            new_keys = filled_new_keys;
            old_keys = filled_old_keys;
        }

        let mut values = BTreeMap::new();
        for access in new_keys.iter().chain(old_keys.iter()) {
            if let Some(value) = self
                .find_with_history(access)
                .or_else(|| right.find_with_history(access))
            {
                values.insert(access.clone(), value.clone());
            }
        }

        Self {
            new_keys,
            old_keys,
            values,
        }
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
        let bindings = self.recency_bindings();
        let mut target_only_edges = None;
        let mut rewritten: Option<Vec<(Access, ValueWithHistory)>> = None;

        for (ordinal, (access, value)) in bindings.iter().copied().enumerate() {
            let mapped_access = match access {
                Access::ArrayAccess(typ, index) => {
                    let index = *index;
                    let new_index = f(index);
                    if new_index != index {
                        Some(Access::ArrayAccess(typ.clone(), new_index))
                    } else {
                        None
                    }
                }
                Access::FieldAccess(_) | Access::Dereference => None,
            };
            let new_addr = f(value.addr);

            if let Some(rewritten) = rewritten.as_mut() {
                let mut value = value.clone();
                value.addr = new_addr;
                rewritten.push((mapped_access.unwrap_or_else(|| access.clone()), value));
                continue;
            }

            if let Some(mapped_access) = mapped_access {
                let mut new_rewritten = Vec::with_capacity(bindings.len());
                for (prefix_access, prefix_value) in bindings.iter().copied().take(ordinal) {
                    let prefix_value = target_only_edges
                        .as_ref()
                        .and_then(|edges: &Self| edges.values.get(prefix_access))
                        .unwrap_or(prefix_value)
                        .clone();
                    new_rewritten.push((prefix_access.clone(), prefix_value));
                }

                let mut value = value.clone();
                value.addr = new_addr;
                new_rewritten.push((mapped_access, value));
                rewritten = Some(new_rewritten);
            } else if new_addr != value.addr {
                let edges = target_only_edges.get_or_insert_with(|| self.clone());
                if let Some(existing) = edges.values.get_mut(access) {
                    existing.addr = new_addr;
                }
            }
        }

        if let Some(rewritten) = rewritten {
            return Some(match Self::configured_limit() {
                Some(limit) => Self::from_recency_bindings_limited(rewritten, limit),
                None => Self {
                    new_keys: Vec::new(),
                    old_keys: Vec::new(),
                    values: rewritten.into_iter().collect(),
                },
            });
        }

        target_only_edges
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
    pub fn map_values(&mut self, f: impl FnMut(AbstractValue) -> AbstractValue) -> bool {
        self.map_values_after_first_change(None, f)
    }

    /// Rewrite every heap source/target/index through an arbitrary value mapper,
    /// optionally reusing a previously observed changed mapping. This keeps
    /// callers that already probed the heap from paying another redundant
    /// mapping call on the first changed value.
    pub fn map_values_after_first_change(
        &mut self,
        mut first_change: Option<(AbstractValue, AbstractValue)>,
        mut f: impl FnMut(AbstractValue) -> AbstractValue,
    ) -> bool {
        let mut src_changed = false;
        let mut edge_changed = false;
        let mut rewritten = Vec::new();

        for (src, edges) in self.graph.iter() {
            let new_src = map_value_after_first_change(*src, &mut first_change, &mut f);
            if new_src != *src {
                src_changed = true;
            }

            let mapped_edges = if edges
                .first_mapping_change(|value| {
                    map_value_after_first_change(value, &mut first_change, &mut f)
                })
                .is_some()
            {
                edge_changed = true;
                edges.mapped_values(|value| {
                    map_value_after_first_change(value, &mut first_change, &mut f)
                })
            } else {
                None
            };
            rewritten.push((*src, new_src, mapped_edges, Arc::clone(edges)));
        }

        if !src_changed && !edge_changed {
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
            // Merge with existing edges at `new` if any. Summary export uses
            // `subst_var_or_unsat` below when it needs OCaml's aliasing-
            // contradiction pruning instead.
            let entry_arc = graph.entry(new).or_default();
            let entry = Arc::make_mut(entry_arc);
            for (access, value) in edges.iter_with_history() {
                entry.add_with_history(access.clone(), value.clone());
            }
        }
    }

    /// Substitution with OCaml's aliasing-contradiction check.
    ///
    /// Cross-ref: `PulseBaseMemory.subst_var` / `PulseBaseMemory.canonicalize`.
    /// If two distinct non-empty heap roots collapse to the same representative,
    /// the path was treating equal values as disjoint allocated memory and is
    /// unsatisfiable. The plain Rust `subst_var` above preserves the historical
    /// merge behaviour for callers that cannot consume `Unsat`; equality
    /// incorporation and summary export use this variant to prune the path.
    pub fn subst_var_or_unsat(
        &mut self,
        old: AbstractValue,
        new: AbstractValue,
    ) -> crate::sat_unsat::SatUnsat<()> {
        let needs_target_subst = self.graph.values().any(|edges| {
            edges
                .iter_with_history()
                .any(|(access, value)| value.addr == old || access_mentions(access, old))
        });
        let needs_key_subst = self.graph.contains_key(&old);
        if !needs_target_subst && !needs_key_subst {
            return crate::sat_unsat::SatUnsat::Sat(());
        }
        let graph = self.graph_mut();
        for edges in graph.values_mut() {
            Arc::make_mut(edges).subst_var(old, new);
        }
        if let Some(edges) = graph.remove(&old) {
            match graph.get(&new) {
                Some(existing) if !existing.is_empty() && !edges.is_empty() => {
                    graph.insert(old, edges);
                    return crate::sat_unsat::SatUnsat::Unsat;
                }
                Some(existing) if !edges.is_empty() && existing.is_empty() => {
                    graph.insert(new, edges);
                }
                Some(_) => {}
                None if !edges.is_empty() => {
                    graph.insert(new, edges);
                }
                None => {}
            }
        }
        crate::sat_unsat::SatUnsat::Sat(())
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
    fn test_edges_mapped_values_target_only_preserves_recency_and_history() {
        let mut edges = Edges::empty();
        let a = field_access("a");
        let b = field_access("b");
        let c = field_access("c");
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);
        let v4 = AbstractValue::of_raw(4);
        edges.add_with_history_limited(
            a.clone(),
            ValueWithHistory::new(v1, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            b.clone(),
            ValueWithHistory::new(v2, ValueHistory::epoch()),
            2,
        );
        edges.add_with_history_limited(
            c.clone(),
            ValueWithHistory::new(v3, ValueHistory::epoch()),
            2,
        );

        let mapped = edges
            .mapped_values(|value| if value == v2 { v4 } else { value })
            .expect("target rewrite should change edges");

        assert_eq!(mapped.new_keys, edges.new_keys);
        assert_eq!(mapped.old_keys, edges.old_keys);
        assert_eq!(mapped.find(&a), Some(v1));
        assert_eq!(mapped.find(&b), Some(v4));
        assert_eq!(mapped.find(&c), Some(v3));
        assert_eq!(
            mapped.find_with_history(&b).unwrap().history,
            edges.find_with_history(&b).unwrap().history
        );
    }

    #[test]
    fn test_edges_mapped_values_array_index_rebuilds() {
        let mut edges = Edges::empty();
        let old_idx = AbstractValue::of_raw(1);
        let new_idx = AbstractValue::of_raw(2);
        let target = AbstractValue::of_raw(3);
        edges.add_with_history(
            Access::ArrayAccess(sil::typ::Typ::void(), old_idx),
            ValueWithHistory::new(target, ValueHistory::epoch()),
        );

        let mapped = edges
            .mapped_values(|value| if value == old_idx { new_idx } else { value })
            .expect("index rewrite should change edges");

        assert_eq!(
            mapped.find(&Access::ArrayAccess(sil::typ::Typ::void(), new_idx)),
            Some(target)
        );
        assert_eq!(
            mapped.find(&Access::ArrayAccess(sil::typ::Typ::void(), old_idx)),
            None
        );
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
    fn test_edges_union_left_biased_prefers_left() {
        let mut left = Edges::empty();
        let mut right = Edges::empty();
        let a = field_access("a");
        let b = field_access("b");
        let c = field_access("c");
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let v3 = AbstractValue::of_raw(3);
        let v20 = AbstractValue::of_raw(20);

        left.add_with_history_limited(
            a.clone(),
            ValueWithHistory::new(v1, ValueHistory::epoch()),
            32,
        );
        left.add_with_history_limited(
            b.clone(),
            ValueWithHistory::new(v2, ValueHistory::epoch()),
            32,
        );
        right.add_with_history_limited(
            b.clone(),
            ValueWithHistory::new(v20, ValueHistory::epoch()),
            32,
        );
        right.add_with_history_limited(
            c.clone(),
            ValueWithHistory::new(v3, ValueHistory::epoch()),
            32,
        );

        let union = left.union_left_biased(&right);

        assert_eq!(union.find(&a), Some(v1));
        assert_eq!(union.find(&b), Some(v2), "left edge should win");
        assert_eq!(union.find(&c), Some(v3));
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

    #[test]
    fn test_subst_var_or_unsat_rejects_allocated_root_alias() {
        let mut mem = BaseMemory::empty();
        let old = AbstractValue::of_raw(1);
        let new = AbstractValue::of_raw(2);
        let old_target = AbstractValue::of_raw(3);
        let new_target = AbstractValue::of_raw(4);

        mem.add_edge(old, Access::Dereference, old_target);
        mem.add_edge(new, Access::Dereference, new_target);

        assert!(mem.subst_var_or_unsat(old, new).is_unsat());
    }
}
