// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Address attributes — properties tracked per abstract value.
//!
//! Mirrors OCaml's `PulseAttribute.ml`.
//!
//! Each abstract value can have a set of attributes describing its state:
//! whether it's been allocated, freed, initialized, tainted, etc.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use sil::location::Location;
use sil::procname::Procname;
use sil::typ;
use sil::var::Var;

use crate::abstract_value::AbstractValue;
use crate::invalidation::{Invalidation, MustBeValidReason};

/// Timestamp for ordering events in the analysis.
pub type Timestamp = u64;

/// How an address was allocated.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Allocator {
    CMalloc,
    CRealloc,
    CppNew,
    CppNewArray,
    JavaResource(Procname),
    CSharpResource(Procname),
    HackAsync,
    CustomMalloc(Procname),
    CustomRealloc(Procname),
    CustomFree(Procname),
}

/// An attribute on an abstract value.
///
/// Mirrors OCaml's `Attribute.t`. All variants from the OCaml code are
/// included to avoid artificial limitations.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Attribute {
    /// Address of a C++ temporary variable.
    AddressOfCppTemporary(Var),
    /// Address of a stack variable (with its declaration location).
    AddressOfStackVariable(Var, Location),
    /// This address was allocated (and how).
    Allocated(Allocator, Location),
    /// Always reachable (prevents garbage collection of this address).
    AlwaysReachable,
    /// This is a closure value.
    Closure(Procname),
    /// End of a collection (past-the-end iterator target).
    EndOfCollection,
    /// In a reported retain cycle (suppresses duplicate reports).
    InReportedRetainCycle,
    /// This address has been initialized.
    Initialized,
    /// This address is invalid (freed, null, etc.).
    Invalid(Invalidation, Location),
    /// Java resource has been released.
    JavaResourceReleased,
    /// C# resource has been released.
    CSharpResourceReleased,
    /// This awaitable has been awaited.
    AwaitedAwaitable,
    /// Must be awaited before procedure returns.
    MustBeAwaited,
    /// Must be initialized before use.
    MustBeInitialized(Timestamp, Location),
    /// Must be valid (dereferenceable) when accessed.
    MustBeValid(Timestamp, Location, Option<MustBeValidReason>),
    /// Returned from an unknown function.
    ReturnedFromUnknown(Vec<AbstractValue>),
    /// Static type information.
    StaticType(typ::TypeName),
    /// Has been std::move'd.
    StdMoved,
    /// std::vector::reserve was called.
    StdVectorReserve,
    /// Uninitialized value.
    Uninitialized,
    /// Unreachable at this program point.
    UnreachableAt(Location),
    /// Used as a branch condition (for diagnostic traces).
    UsedAsBranchCond(Procname, Location),
    /// Written to at this timestamp.
    WrittenTo(Timestamp, Location),
}

impl PartialOrd for Allocator {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Allocator {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.disc_index().cmp(&other.disc_index())
    }
}

impl PartialOrd for Attribute {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Attribute {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.disc_index().cmp(&other.disc_index())
    }
}

impl Allocator {
    fn disc_index(&self) -> u8 {
        match self {
            Self::CMalloc => 0,
            Self::CRealloc => 1,
            Self::CppNew => 2,
            Self::CppNewArray => 3,
            Self::JavaResource(_) => 4,
            Self::CSharpResource(_) => 5,
            Self::HackAsync => 6,
            Self::CustomMalloc(_) => 7,
            Self::CustomRealloc(_) => 8,
            Self::CustomFree(_) => 9,
        }
    }
}

impl Attribute {
    fn disc_index(&self) -> u8 {
        match self {
            Self::AddressOfCppTemporary(_) => 0,
            Self::AddressOfStackVariable(_, _) => 1,
            Self::Allocated(_, _) => 2,
            Self::AlwaysReachable => 3,
            Self::Closure(_) => 4,
            Self::EndOfCollection => 5,
            Self::InReportedRetainCycle => 6,
            Self::Initialized => 7,
            Self::Invalid(_, _) => 8,
            Self::JavaResourceReleased => 9,
            Self::CSharpResourceReleased => 10,
            Self::AwaitedAwaitable => 11,
            Self::MustBeAwaited => 12,
            Self::MustBeInitialized(_, _) => 13,
            Self::MustBeValid(_, _, _) => 14,
            Self::ReturnedFromUnknown(_) => 15,
            Self::StaticType(_) => 16,
            Self::StdMoved => 17,
            Self::StdVectorReserve => 18,
            Self::Uninitialized => 19,
            Self::UnreachableAt(_) => 20,
            Self::UsedAsBranchCond(_, _) => 21,
            Self::WrittenTo(_, _) => 22,
        }
    }
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Attribute::Invalid(inv, loc) => write!(f, "Invalid({inv}, {loc})"),
            Attribute::MustBeValid(ts, loc, reason) => {
                write!(f, "MustBeValid({ts}, {loc}, {reason:?})")
            }
            Attribute::Allocated(alloc, loc) => write!(f, "Allocated({alloc:?}, {loc})"),
            Attribute::Initialized => write!(f, "Initialized"),
            Attribute::Closure(proc) => write!(f, "Closure({proc})"),
            Attribute::WrittenTo(ts, loc) => write!(f, "WrittenTo({ts}, {loc})"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// A set of attributes on an abstract value.
///
/// Uses `BTreeSet` for deterministic iteration order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attributes(BTreeSet<Attribute>);

impl Attributes {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn add(&mut self, attr: Attribute) {
        self.0.insert(attr);
    }

    pub fn remove(&mut self, attr: &Attribute) {
        self.0.remove(attr);
    }

    pub fn contains(&self, attr: &Attribute) -> bool {
        self.0.contains(attr)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Attribute> {
        self.0.iter()
    }

    /// Find the `Invalid` attribute, if any.
    pub fn get_invalid(&self) -> Option<(&Invalidation, &Location)> {
        self.0.iter().find_map(|a| match a {
            Attribute::Invalid(inv, loc) => Some((inv, loc)),
            _ => None,
        })
    }

    /// Find the `MustBeValid` attribute, if any.
    pub fn get_must_be_valid(&self) -> Option<(Timestamp, &Location, &Option<MustBeValidReason>)> {
        self.0.iter().find_map(|a| match a {
            Attribute::MustBeValid(ts, loc, reason) => Some((*ts, loc, reason)),
            _ => None,
        })
    }

    /// Check if this address is allocated.
    pub fn is_allocated(&self) -> bool {
        self.0
            .iter()
            .any(|a| matches!(a, Attribute::Allocated(_, _)))
    }

    /// Remove the `Allocated` attribute, if any.
    pub fn remove_allocated(&mut self) {
        self.0.retain(|a| !matches!(a, Attribute::Allocated(_, _)));
    }

    /// Find the `Allocated` attribute, if any.
    pub fn get_allocated(&self) -> Option<(&Allocator, &Location)> {
        self.0.iter().find_map(|a| match a {
            Attribute::Allocated(alloc, loc) => Some((alloc, loc)),
            _ => None,
        })
    }

    /// Check if this address is initialized.
    pub fn is_initialized(&self) -> bool {
        self.0.iter().any(|a| matches!(a, Attribute::Initialized))
    }

    /// Check if this address has been std::move'd.
    pub fn is_std_moved(&self) -> bool {
        self.0.iter().any(|a| matches!(a, Attribute::StdMoved))
    }

    /// Get the closure/function-pointer procedure name, if any.
    /// Cross-ref: OCaml `Attributes.get_closure_proc_name`.
    pub fn get_closure_proc_name(&self) -> Option<&Procname> {
        self.0.iter().find_map(|a| match a {
            Attribute::Closure(pname) => Some(pname),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::int_lit::IntLit;

    #[test]
    fn test_attributes_basic() {
        let mut attrs = Attributes::empty();
        assert!(attrs.is_empty());

        attrs.add(Attribute::Initialized);
        assert!(!attrs.is_empty());
        assert!(attrs.is_initialized());
        assert!(!attrs.is_allocated());
    }

    #[test]
    fn test_get_invalid() {
        let mut attrs = Attributes::empty();
        assert!(attrs.get_invalid().is_none());

        attrs.add(Attribute::Invalid(
            Invalidation::ConstantDereference(IntLit::zero()),
            Location::dummy(),
        ));
        let (inv, _loc) = attrs.get_invalid().unwrap();
        assert!(inv.is_null_deref());
    }

    #[test]
    fn test_multiple_attributes() {
        let mut attrs = Attributes::empty();
        attrs.add(Attribute::Initialized);
        attrs.add(Attribute::Allocated(Allocator::CMalloc, Location::dummy()));
        attrs.add(Attribute::WrittenTo(1, Location::dummy()));

        assert!(attrs.is_initialized());
        assert!(attrs.is_allocated());
        assert_eq!(attrs.iter().count(), 3);
    }
}
