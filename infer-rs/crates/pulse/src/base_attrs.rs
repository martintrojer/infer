// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Address attributes map: tracks properties of each abstract address.
//!
//! Mirrors OCaml's `PulseBaseAddressAttributes.ml`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use sil::location::Location;

use crate::abstract_value::AbstractValue;
use crate::attribute::{Attribute, Attributes, InitializationError};
use crate::invalidation::Invalidation;
use crate::value_history::ValueHistory;

/// Maps abstract addresses to their attribute sets.
///
/// Two layers of structural sharing:
/// - the outer `BTreeMap<AbstractValue, Arc<Attributes>>` is wrapped in
///   `Arc<...>` so cloning the surrounding abductive state never deep-copies
///   the attribute graph eagerly; mutating accesses use `Arc::make_mut`
///   (clone-on-write) to keep the same `&mut self` API while preserving
///   sharing across disjuncts and retained invariant snapshots.
/// - each per-address `Attributes` set is also wrapped in its own `Arc`,
///   so per-address attribute bundles stay refcount-shared after the outer
///   map is cloned-on-write.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseAddressAttributes {
    map: Arc<BTreeMap<AbstractValue, Arc<Attributes>>>,
}

impl BaseAddressAttributes {
    pub fn empty() -> Self {
        Self::default()
    }

    fn map_mut(&mut self) -> &mut BTreeMap<AbstractValue, Arc<Attributes>> {
        Arc::make_mut(&mut self.map)
    }

    fn entry_mut(&mut self, addr: AbstractValue) -> &mut Attributes {
        let arc = self.map_mut().entry(addr).or_default();
        Arc::make_mut(arc)
    }

    /// Add a single attribute to an address.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.AddressAttributes.add_one`
    /// treats `WrittenTo` as a write-side initialization event too.
    pub fn add_one(&mut self, addr: AbstractValue, attr: Attribute) {
        let attrs = self.entry_mut(addr);
        if matches!(attr, Attribute::WrittenTo(_, _)) {
            attrs.add(Attribute::Initialized);
        }
        attrs.add(attr);
    }

    /// Get all attributes for an address.
    pub fn get(&self, addr: &AbstractValue) -> Option<&Attributes> {
        self.map.get(addr).map(Arc::as_ref)
    }

    /// Get mutable access to the attributes for an address.
    pub fn get_mut(&mut self, addr: &AbstractValue) -> Option<&mut Attributes> {
        if !self.map.contains_key(addr) {
            return None;
        }
        self.map_mut().get_mut(addr).map(Arc::make_mut)
    }

    /// Check if an address is valid (not invalid).
    ///
    /// Returns `Ok(())` if valid, or `Err(Box<(invalidation, history)>)` if invalid.
    /// This is THE null-dereference / use-after-free check.
    pub fn check_valid(
        &self,
        addr: AbstractValue,
    ) -> Result<(), Box<(Invalidation, ValueHistory)>> {
        if let Some(attrs) = self.map.get(&addr) {
            if let Some((inv, history)) = attrs.get_invalid() {
                return Err(Box::new((inv.clone(), history.clone())));
            }
        }
        Ok(())
    }

    /// Check if an address is initialized.
    ///
    /// Cross-ref: OCaml `PulseBaseAddressAttributes.check_initialized`.
    pub fn check_initialized(&self, addr: AbstractValue) -> Result<(), InitializationError> {
        if self
            .map
            .get(&addr)
            .is_some_and(|attrs| attrs.is_uninitialized())
        {
            return Err(InitializationError::Uninitialized);
        }
        Ok(())
    }

    /// Get the closure/function-pointer procedure name for an abstract value.
    pub fn get_closure_proc_name(&self, addr: AbstractValue) -> Option<&sil::procname::Procname> {
        self.map
            .get(&addr)
            .and_then(|attrs| attrs.get_closure_proc_name())
    }

    /// Mark an address as invalid.
    pub fn invalidate(&mut self, addr: AbstractValue, inv: Invalidation, history: ValueHistory) {
        self.add_one(addr, Attribute::Invalid(inv, history));
    }

    /// Replace any existing invalidation payload for this address.
    pub fn replace_invalid(
        &mut self,
        addr: AbstractValue,
        inv: Invalidation,
        history: ValueHistory,
    ) {
        self.entry_mut(addr).replace_invalid(inv, history);
    }

    /// Mark an address as allocated.
    pub fn allocate(
        &mut self,
        addr: AbstractValue,
        allocator: crate::attribute::Allocator,
        loc: Location,
    ) {
        self.add_one(addr, Attribute::Allocated(allocator, loc));
    }

    /// Mark an address as initialized.
    ///
    /// Cross-ref: OCaml `PulseBaseAddressAttributes.initialize` adds
    /// `Initialized` and removes any `Uninitialized` marker.
    pub fn initialize(&mut self, addr: AbstractValue) {
        self.add_one(addr, Attribute::Initialized);
        let has_uninit = self.map.get(&addr).is_some_and(|attrs| {
            attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::Uninitialized))
        });
        if has_uninit {
            if let Some(attrs) = self.map_mut().get_mut(&addr) {
                Arc::make_mut(attrs).remove_uninitialized();
            }
        }
    }

    /// Mark an address as always reachable.
    pub fn always_reachable(&mut self, addr: AbstractValue) {
        self.add_one(addr, Attribute::AlwaysReachable);
    }

    /// Mark an address as written to.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.add_one` calls `initialize` when
    /// adding a `WrittenTo` attribute, so a write both records the marker and
    /// clears any `Uninitialized` state on the value.
    pub fn mark_written_to(&mut self, addr: AbstractValue, timestamp: u64, loc: Location) {
        self.add_one(addr, Attribute::WrittenTo(timestamp, loc));
        self.initialize(addr);
    }

    /// Check if an address is allocated.
    pub fn is_allocated(&self, addr: AbstractValue) -> bool {
        self.map
            .get(&addr)
            .is_some_and(|attrs| attrs.is_allocated())
    }

    /// Remove the Allocated attribute from an address.
    /// Used during unknown call havoc to prevent false leak reports.
    pub fn remove_allocated(&mut self, addr: AbstractValue) {
        if !self
            .map
            .get(&addr)
            .is_some_and(|attrs| attrs.is_allocated())
        {
            return;
        }
        if let Some(attrs) = self.map_mut().get_mut(&addr) {
            Arc::make_mut(attrs).remove_allocated();
        }
    }

    pub fn remove_must_be_initialized(&mut self, addr: AbstractValue) {
        if !self.map.get(&addr).is_some_and(|attrs| {
            attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeInitialized(_, _)))
        }) {
            return;
        }
        if let Some(attrs) = self.map_mut().get_mut(&addr) {
            Arc::make_mut(attrs).remove_must_be_initialized();
        }
    }

    /// Iterate over all addresses and their attributes.
    pub fn iter(&self) -> impl Iterator<Item = (&AbstractValue, &Attributes)> {
        self.map.iter().map(|(addr, attrs)| (addr, attrs.as_ref()))
    }

    /// Remove all attributes for an address.
    pub fn remove_addr(&mut self, addr: &AbstractValue) {
        if !self.map.contains_key(addr) {
            return;
        }
        self.map_mut().remove(addr);
    }

    /// Remove attributes on addresses not in the reachable set.
    /// Used during summary normalization to strip spurious attrs.
    pub fn retain_reachable(&mut self, reachable: &std::collections::HashSet<AbstractValue>) {
        if self.map.keys().all(|addr| reachable.contains(addr)) {
            return;
        }
        self.map_mut().retain(|addr, _| reachable.contains(addr));
    }

    pub fn retain_for_pre_summary(&mut self) {
        let needs_update = self.map.values().any(|attrs| {
            attrs.is_empty() || attrs.iter().any(|attr| !attr.is_suitable_for_pre_summary())
        });
        if !needs_update {
            return;
        }
        self.map_mut().retain(|_addr, attrs| {
            let mutable = Arc::make_mut(attrs);
            mutable.retain_for_pre_summary();
            !mutable.is_empty()
        });
    }

    pub fn retain_for_post_summary(&mut self) {
        let needs_update = self.map.values().any(|attrs| {
            attrs.is_empty()
                || attrs
                    .iter()
                    .any(|attr| !attr.is_suitable_for_post_summary())
        });
        if !needs_update {
            return;
        }
        self.map_mut().retain(|_addr, attrs| {
            let mutable = Arc::make_mut(attrs);
            mutable.retain_for_post_summary();
            !mutable.is_empty()
        });
    }

    pub fn remove_empty_entries(&mut self) {
        if self.map.values().all(|attrs| !attrs.is_empty()) {
            return;
        }
        self.map_mut().retain(|_addr, attrs| !attrs.is_empty());
    }

    /// Substitute abstract values.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        if !self.map.contains_key(&old) {
            return;
        }
        let map = self.map_mut();
        if let Some(attrs) = map.remove(&old) {
            let entry_arc = map.entry(new).or_default();
            let entry = Arc::make_mut(entry_arc);
            for attr in attrs.iter() {
                entry.add(attr.clone());
            }
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl fmt::Display for BaseAddressAttributes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (addr, attrs) in self.map.iter() {
            if !attrs.is_empty() {
                write!(f, "{addr}: ")?;
                for attr in attrs.iter() {
                    write!(f, "{attr} ")?;
                }
                write!(f, "; ")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::int_lit::IntLit;

    #[test]
    fn test_check_valid_ok() {
        let attrs = BaseAddressAttributes::empty();
        let v = AbstractValue::of_raw(1);
        assert!(attrs.check_valid(v).is_ok());
    }

    #[test]
    fn test_check_valid_null() {
        let mut attrs = BaseAddressAttributes::empty();
        let v = AbstractValue::of_raw(1);
        attrs.invalidate(
            v,
            Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        let err = attrs.check_valid(v);
        assert!(err.is_err());
        let (inv, _) = *err.unwrap_err();
        assert!(inv.is_null_deref());
    }

    #[test]
    fn test_check_valid_freed() {
        let mut attrs = BaseAddressAttributes::empty();
        let v = AbstractValue::of_raw(1);
        attrs.invalidate(
            v,
            Invalidation::CFree,
            ValueHistory::invalidated(Invalidation::CFree, Location::dummy()),
        );
        assert!(attrs.check_valid(v).is_err());
    }

    #[test]
    fn test_allocate_and_check() {
        let mut attrs = BaseAddressAttributes::empty();
        let v = AbstractValue::of_raw(1);
        attrs.allocate(v, crate::attribute::Allocator::CMalloc, Location::dummy());
        assert!(attrs.is_allocated(v));
        assert!(attrs.check_valid(v).is_ok());
    }
}
