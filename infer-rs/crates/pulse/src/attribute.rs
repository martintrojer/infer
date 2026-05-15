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
use crate::value_history::ValueHistory;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddSemantics {
    KeepExisting,
    ReplaceExisting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum AttributeRank {
    AddressOfCppTemporary,
    AddressOfStackVariable,
    Allocated,
    AlwaysReachable,
    Closure,
    EndOfCollection,
    InReportedRetainCycle,
    Initialized,
    Invalid,
    JavaResourceReleased,
    CSharpResourceReleased,
    AwaitedAwaitable,
    MustBeAwaited,
    MustBeInitialized,
    MustBeValid,
    ReturnedFromUnknown,
    StaticType,
    StdMoved,
    StdVectorReserve,
    UnknownEffect,
    Uninitialized,
    UnreachableAt,
    UsedAsBranchCond,
    WrittenTo,
}

/// Timestamp for ordering events in the analysis.
pub type Timestamp = u64;

/// Error returned when a read requires initialized memory but the address is
/// still marked `Uninitialized`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializationError {
    Uninitialized,
}

/// How an address was allocated.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    Invalid(Invalidation, ValueHistory),
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
    /// May be modified by an unknown/external call.
    UnknownEffect,
    /// Uninitialized value.
    Uninitialized,
    /// Unreachable at this program point.
    UnreachableAt(Location),
    /// Used as a branch condition (for diagnostic traces).
    UsedAsBranchCond(Procname, Location),
    /// Written to at this timestamp.
    WrittenTo(Timestamp, Location),
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Attribute::Invalid(inv, history) => write!(f, "Invalid({inv}, {history})"),
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

impl Attribute {
    fn rank(&self) -> AttributeRank {
        match self {
            Self::AddressOfCppTemporary(_) => AttributeRank::AddressOfCppTemporary,
            Self::AddressOfStackVariable(_, _) => AttributeRank::AddressOfStackVariable,
            Self::Allocated(_, _) => AttributeRank::Allocated,
            Self::AlwaysReachable => AttributeRank::AlwaysReachable,
            Self::Closure(_) => AttributeRank::Closure,
            Self::EndOfCollection => AttributeRank::EndOfCollection,
            Self::InReportedRetainCycle => AttributeRank::InReportedRetainCycle,
            Self::Initialized => AttributeRank::Initialized,
            Self::Invalid(_, _) => AttributeRank::Invalid,
            Self::JavaResourceReleased => AttributeRank::JavaResourceReleased,
            Self::CSharpResourceReleased => AttributeRank::CSharpResourceReleased,
            Self::AwaitedAwaitable => AttributeRank::AwaitedAwaitable,
            Self::MustBeAwaited => AttributeRank::MustBeAwaited,
            Self::MustBeInitialized(_, _) => AttributeRank::MustBeInitialized,
            Self::MustBeValid(_, _, _) => AttributeRank::MustBeValid,
            Self::ReturnedFromUnknown(_) => AttributeRank::ReturnedFromUnknown,
            Self::StaticType(_) => AttributeRank::StaticType,
            Self::StdMoved => AttributeRank::StdMoved,
            Self::StdVectorReserve => AttributeRank::StdVectorReserve,
            Self::UnknownEffect => AttributeRank::UnknownEffect,
            Self::Uninitialized => AttributeRank::Uninitialized,
            Self::UnreachableAt(_) => AttributeRank::UnreachableAt,
            Self::UsedAsBranchCond(_, _) => AttributeRank::UsedAsBranchCond,
            Self::WrittenTo(_, _) => AttributeRank::WrittenTo,
        }
    }

    fn add_semantics(&self) -> AddSemantics {
        match self {
            // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` uses
            // `update` for `WrittenTo` and `Invalid(OptionalEmpty, _)`,
            // replacing any earlier same-rank payload.
            Self::WrittenTo(_, _) | Self::Invalid(Invalidation::OptionalEmpty, _) => {
                AddSemantics::ReplaceExisting
            }
            _ => AddSemantics::KeepExisting,
        }
    }

    /// Cross-ref: OCaml `PulseAttribute.is_suitable_for_pre_summary`.
    pub fn is_suitable_for_pre_summary(&self) -> bool {
        matches!(
            self,
            Attribute::MustBeAwaited
                | Attribute::MustBeInitialized(_, _)
                | Attribute::MustBeValid(_, _, _)
                | Attribute::UsedAsBranchCond(_, _)
        )
    }

    /// Cross-ref: OCaml `PulseAttribute.is_suitable_for_post_summary`.
    pub fn is_suitable_for_post_summary(&self) -> bool {
        !matches!(
            self,
            Attribute::Invalid(Invalidation::ComparedToNullInThisProcedure(_), _)
                | Attribute::MustBeAwaited
                | Attribute::MustBeInitialized(_, _)
                | Attribute::MustBeValid(_, _, _)
                | Attribute::UnreachableAt(_)
                | Attribute::UsedAsBranchCond(_, _)
        )
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
        let rank = attr.rank();
        match attr.add_semantics() {
            AddSemantics::KeepExisting => {
                if self.0.iter().all(|existing| existing.rank() != rank) {
                    self.0.insert(attr);
                }
            }
            AddSemantics::ReplaceExisting => {
                self.0.retain(|existing| existing.rank() != rank);
                self.0.insert(attr);
            }
        }
    }

    pub fn replace_invalid(&mut self, invalidation: Invalidation, history: ValueHistory) {
        self.0
            .retain(|existing| existing.rank() != AttributeRank::Invalid);
        self.0.insert(Attribute::Invalid(invalidation, history));
    }

    pub fn remove(&mut self, attr: &Attribute) {
        self.0.remove(attr);
    }

    pub fn remove_closure_attrs(&mut self) {
        self.0.retain(|attr| !matches!(attr, Attribute::Closure(_)));
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

    pub fn retain_for_pre_summary(&mut self) {
        self.0.retain(Attribute::is_suitable_for_pre_summary);
    }

    pub fn retain_for_post_summary(&mut self) {
        self.0.retain(Attribute::is_suitable_for_post_summary);
    }

    /// Find the `Invalid` attribute, if any.
    pub fn get_invalid(&self) -> Option<(&Invalidation, &ValueHistory)> {
        let mut compared_to_null = None;
        self.0
            .iter()
            .find_map(|a| match a {
                Attribute::Invalid(
                    inv @ Invalidation::ComparedToNullInThisProcedure(_),
                    history,
                ) => {
                    compared_to_null = Some((inv, history));
                    None
                }
                Attribute::Invalid(inv, history) => Some((inv, history)),
                _ => None,
            })
            .or(compared_to_null)
    }

    /// Find the `MustBeValid` attribute, if any.
    pub fn get_must_be_valid(&self) -> Option<(Timestamp, &Location, &Option<MustBeValidReason>)> {
        self.0.iter().find_map(|a| match a {
            Attribute::MustBeValid(ts, loc, reason) => Some((*ts, loc, reason)),
            _ => None,
        })
    }

    /// Find the `MustBeInitialized` attribute, if any.
    pub fn get_must_be_initialized(&self) -> Option<(Timestamp, &Location)> {
        self.0.iter().find_map(|a| match a {
            Attribute::MustBeInitialized(ts, loc) => Some((*ts, loc)),
            _ => None,
        })
    }

    /// Find the `WrittenTo` attribute, if any.
    pub fn get_written_to(&self) -> Option<(Timestamp, &Location)> {
        self.0.iter().find_map(|a| match a {
            Attribute::WrittenTo(ts, loc) => Some((*ts, loc)),
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

    /// Check if this address is still considered uninitialized.
    ///
    /// Cross-ref: OCaml `Attributes.get_uninitialized` ignores the marker once
    /// `Initialized` is present.
    pub fn is_uninitialized(&self) -> bool {
        !self.is_initialized() && self.0.iter().any(|a| matches!(a, Attribute::Uninitialized))
    }

    /// Check if this address should be kept reachable across summary creation.
    pub fn is_always_reachable(&self) -> bool {
        self.0
            .iter()
            .any(|a| matches!(a, Attribute::AlwaysReachable))
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

impl Attribute {
    /// Whether this attribute means the cell was modified by the callee.
    ///
    /// Cross-ref: OCaml `PulseAttribute.Attributes.is_modified`, used by
    /// `PulseInterproc.is_cell_read_only` to avoid replaying read-only summary
    /// cells and overwriting caller-side facts.
    pub(crate) fn is_modified(&self) -> bool {
        matches!(
            self,
            Attribute::WrittenTo(_, _)
                | Attribute::Initialized
                | Attribute::Invalid(_, _)
                | Attribute::UnknownEffect
                | Attribute::Allocated(_, _)
                | Attribute::Uninitialized
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::int_lit::IntLit;
    use sil::source_file::SourceFile;

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
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        ));
        let (inv, _loc) = attrs.get_invalid().unwrap();
        assert!(inv.is_null_deref());
    }

    #[test]
    fn test_invalid_keeps_first_same_rank_attribute_like_ocaml() {
        // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` keeps the
        // first payload for ordinary same-rank attributes.
        let mut attrs = Attributes::empty();
        let loc = Location::dummy();

        attrs.add(Attribute::Invalid(
            Invalidation::ComparedToNullInThisProcedure(loc.clone()),
            ValueHistory::invalidated(
                Invalidation::ComparedToNullInThisProcedure(loc.clone()),
                loc.clone(),
            ),
        ));
        attrs.add(Attribute::Invalid(
            Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(Invalidation::ConstantDereference(IntLit::zero()), loc),
        ));

        let (inv, _history) = attrs.get_invalid().expect("expected an invalidation");
        assert!(matches!(
            inv,
            Invalidation::ComparedToNullInThisProcedure(_)
        ));
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

    #[test]
    fn test_written_to_replaces_previous_same_rank_attribute() {
        // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` uses `update`
        // for `WrittenTo`, so the latest write replaces older payloads.
        let mut attrs = Attributes::empty();
        let loc1 = Location {
            file: SourceFile::new("a.c"),
            line: 10,
            col: 1,
            macro_file_opt: None,
            macro_line: -1,
        };
        let loc2 = Location {
            file: SourceFile::new("a.c"),
            line: 20,
            col: 1,
            macro_file_opt: None,
            macro_line: -1,
        };

        attrs.add(Attribute::WrittenTo(1, loc1.clone()));
        attrs.add(Attribute::WrittenTo(2, loc2.clone()));

        let (ts, loc) = attrs.get_written_to().expect("expected a write marker");
        assert_eq!(ts, 2);
        assert_eq!(loc, &loc2);
        assert_eq!(
            attrs
                .iter()
                .filter(|attr| matches!(attr, Attribute::WrittenTo(_, _)))
                .count(),
            1
        );
    }

    #[test]
    fn test_invalid_optional_empty_replaces_previous_invalid() {
        // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` also uses
        // `update` for `Invalid(OptionalEmpty, _)`.
        let mut attrs = Attributes::empty();
        let loc1 = Location {
            file: SourceFile::new("a.c"),
            line: 10,
            col: 1,
            macro_file_opt: None,
            macro_line: -1,
        };
        let loc2 = Location {
            file: SourceFile::new("a.c"),
            line: 20,
            col: 1,
            macro_file_opt: None,
            macro_line: -1,
        };

        attrs.add(Attribute::Invalid(
            Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(Invalidation::ConstantDereference(IntLit::zero()), loc1),
        ));
        attrs.add(Attribute::Invalid(
            Invalidation::OptionalEmpty,
            ValueHistory::invalidated(Invalidation::OptionalEmpty, loc2),
        ));

        let (inv, _history) = attrs.get_invalid().expect("expected an invalidation");
        assert_eq!(*inv, Invalidation::OptionalEmpty);
        assert_eq!(
            attrs
                .iter()
                .filter(|attr| matches!(attr, Attribute::Invalid(_, _)))
                .count(),
            1
        );
    }
}
