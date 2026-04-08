// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! How an abstract address became invalid.
//!
//! Mirrors OCaml's `PulseInvalidation.ml`.

use std::fmt;

use serde::{Deserialize, Serialize};

use sil::int_lit::IntLit;
use sil::location::Location;
use sil::pvar::Pvar;
use sil::typ::Typ;

/// How a std::vector was invalidated.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StdVectorFunction {
    Assign,
    Clear,
    Emplace,
    EmplaceBack,
    Insert,
    PushBack,
    Reserve,
    ShrinkToFit,
}

/// How an address became invalid.
///
/// Mirrors OCaml's `Invalidation.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Invalidation {
    /// `free()` in C.
    CFree,
    /// Compared to null in this procedure (and thus known to be null on this path).
    ComparedToNullInThisProcedure(Location),
    /// Dereference of a constant address (0 = null).
    ConstantDereference(IntLit),
    /// C++ `delete`.
    CppDelete,
    /// C++ `delete[]`.
    CppDeleteArray,
    /// Past-the-end iterator.
    EndIterator,
    /// `fclose()`.
    FClose,
    /// Stack variable went out of scope.
    GoneOutOfScope(Box<Pvar>, Typ),
    /// `std::optional` is empty.
    OptionalEmpty,
    /// std::vector invalidation.
    StdVector(StdVectorFunction),
}

impl Invalidation {
    fn disc_index(&self) -> u8 {
        match self {
            Self::CFree => 0,
            Self::ComparedToNullInThisProcedure(_) => 1,
            Self::ConstantDereference(_) => 2,
            Self::CppDelete => 3,
            Self::CppDeleteArray => 4,
            Self::EndIterator => 5,
            Self::FClose => 6,
            Self::GoneOutOfScope(_, _) => 7,
            Self::OptionalEmpty => 8,
            Self::StdVector(_) => 9,
        }
    }
}

impl PartialOrd for Invalidation {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Invalidation {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let ord = self.disc_index().cmp(&other.disc_index());
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
        match (self, other) {
            (Self::ConstantDereference(a), Self::ConstantDereference(b)) => a.cmp(b),
            (Self::ComparedToNullInThisProcedure(a), Self::ComparedToNullInThisProcedure(b)) => {
                a.line.cmp(&b.line).then(a.col.cmp(&b.col))
            }
            (Self::GoneOutOfScope(a, _), Self::GoneOutOfScope(b, _)) => {
                format!("{a}").cmp(&format!("{b}"))
            }
            (Self::StdVector(a), Self::StdVector(b)) => a.cmp(b),
            _ => std::cmp::Ordering::Equal,
        }
    }
}

impl MustBeValidReason {
    fn disc_index(&self) -> u8 {
        match self {
            Self::BlockCall => 0,
            Self::InsertionIntoCollectionKey => 1,
            Self::InsertionIntoCollectionValue => 2,
            Self::SelfOfNonPODReturnMethod(_) => 3,
            Self::NullArgumentWhereNonNullExpected => 4,
        }
    }
}

impl PartialOrd for MustBeValidReason {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MustBeValidReason {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let ord = self.disc_index().cmp(&other.disc_index());
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
        match (self, other) {
            (Self::SelfOfNonPODReturnMethod(a), Self::SelfOfNonPODReturnMethod(b)) => a.cmp(b),
            _ => std::cmp::Ordering::Equal,
        }
    }
}

impl Invalidation {
    /// Whether this represents a null pointer dereference.
    pub fn is_null_deref(&self) -> bool {
        matches!(self, Invalidation::ConstantDereference(i) if i.is_zero())
    }

    /// Cross-ref: OCaml `PulseInvalidation.is_same_type`.
    pub fn is_same_type(&self, other: &Self) -> bool {
        self.disc_index() == other.disc_index()
    }
}

impl fmt::Display for Invalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Invalidation::CFree => write!(f, "was invalidated by call to `free()`"),
            Invalidation::ComparedToNullInThisProcedure(loc) => {
                write!(f, "was compared to null at {loc}")
            }
            Invalidation::ConstantDereference(i) if i.is_zero() => {
                write!(f, "is assigned to the null pointer")
            }
            Invalidation::ConstantDereference(i) => {
                write!(f, "is assigned to the constant {i}")
            }
            Invalidation::CppDelete => write!(f, "was invalidated by `delete`"),
            Invalidation::CppDeleteArray => write!(f, "was invalidated by `delete[]`"),
            Invalidation::EndIterator => write!(f, "is pointed to by the `end()` iterator"),
            Invalidation::FClose => write!(f, "was closed with `fclose()`"),
            Invalidation::GoneOutOfScope(pvar, _) => {
                write!(f, "is the address of `{pvar}` whose lifetime has ended")
            }
            Invalidation::OptionalEmpty => write!(f, "is assigned an empty value"),
            Invalidation::StdVector(func) => {
                write!(f, "was potentially invalidated by `std::vector::{func:?}`")
            }
        }
    }
}

/// Why an address must be valid when accessed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MustBeValidReason {
    BlockCall,
    InsertionIntoCollectionKey,
    InsertionIntoCollectionValue,
    SelfOfNonPODReturnMethod(Typ),
    NullArgumentWhereNonNullExpected,
}
