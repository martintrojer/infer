// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Address attributes map: tracks properties of each abstract address.
//!
//! Mirrors OCaml's `PulseBaseAddressAttributes.ml`.

use std::collections::BTreeMap;
use std::fmt;

use sil::location::Location;

use crate::abstract_value::AbstractValue;
use crate::attribute::{Attribute, Attributes};
use crate::invalidation::Invalidation;

/// Maps abstract addresses to their attribute sets.
#[derive(Clone, Debug, Default)]
pub struct BaseAddressAttributes {
    map: BTreeMap<AbstractValue, Attributes>,
}

impl BaseAddressAttributes {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Add a single attribute to an address.
    pub fn add_one(&mut self, addr: AbstractValue, attr: Attribute) {
        self.map.entry(addr).or_default().add(attr);
    }

    /// Get all attributes for an address.
    pub fn get(&self, addr: &AbstractValue) -> Option<&Attributes> {
        self.map.get(addr)
    }

    /// Check if an address is valid (not invalid).
    ///
    /// Returns `Ok(())` if valid, or `Err(Box<(invalidation, location)>)` if invalid.
    /// This is THE null-dereference / use-after-free check.
    pub fn check_valid(&self, addr: AbstractValue) -> Result<(), Box<(Invalidation, Location)>> {
        if let Some(attrs) = self.map.get(&addr) {
            if let Some((inv, loc)) = attrs.get_invalid() {
                return Err(Box::new((inv.clone(), loc.clone())));
            }
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
    pub fn invalidate(&mut self, addr: AbstractValue, inv: Invalidation, loc: Location) {
        self.add_one(addr, Attribute::Invalid(inv, loc));
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
    pub fn initialize(&mut self, addr: AbstractValue) {
        self.add_one(addr, Attribute::Initialized);
    }

    /// Mark an address as written to.
    pub fn mark_written_to(&mut self, addr: AbstractValue, timestamp: u64, loc: Location) {
        self.add_one(addr, Attribute::WrittenTo(timestamp, loc));
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
        if let Some(attrs) = self.map.get_mut(&addr) {
            attrs.remove_allocated();
        }
    }

    /// Iterate over all addresses and their attributes.
    pub fn iter(&self) -> impl Iterator<Item = (&AbstractValue, &Attributes)> {
        self.map.iter()
    }

    /// Remove attributes on addresses not in the reachable set.
    /// Used during summary normalization to strip spurious attrs.
    pub fn retain_reachable(&mut self, reachable: &std::collections::HashSet<AbstractValue>) {
        self.map.retain(|addr, _| reachable.contains(addr));
    }

    /// Substitute abstract values.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        if let Some(attrs) = self.map.remove(&old) {
            let entry = self.map.entry(new).or_default();
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
        for (addr, attrs) in &self.map {
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
            Location::dummy(),
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
        attrs.invalidate(v, Invalidation::CFree, Location::dummy());
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
