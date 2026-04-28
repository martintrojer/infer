// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Issue type definitions and severity levels.
//!
//! Mirrors OCaml's `IssueType.ml`.

use serde::{Deserialize, Serialize};

/// Issue severity level.
///
/// Mirrors OCaml's `IssueType.severity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Advice,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "ERROR"),
            Severity::Warning => write!(f, "WARNING"),
            Severity::Info => write!(f, "INFO"),
            Severity::Advice => write!(f, "ADVICE"),
        }
    }
}

/// Issue category for grouping related issue types.
///
/// Mirrors OCaml's `IssueType.category`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    LogicError,
    MemoryError,
    NullPointerDereference,
    ResourceLeak,
    RaceCondition,
    Perf,
    Other,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Category::LogicError => write!(f, "Logic error"),
            Category::MemoryError => write!(f, "Memory error"),
            Category::NullPointerDereference => write!(f, "Null pointer dereference"),
            Category::ResourceLeak => write!(f, "Resource leak"),
            Category::RaceCondition => write!(f, "Race condition"),
            Category::Perf => write!(f, "Performance issue"),
            Category::Other => write!(f, "Other"),
        }
    }
}

/// The checker that produced an issue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Checker(pub String);

/// Well-known issue type identifiers.
///
/// Each variant's `id()` returns the exact string OCaml uses in `IssueType.ml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IssueTypeId {
    ComparedToNullAndDereferenced,
    DeadStore,
    NullptrDereference,
    MemoryLeakC,
    UseAfterFree,
    UseAfterDelete,
    UseAfterLifetime,
    OptionalEmptyAccess,
    VectorInvalidation,
    RetainCycle,
    PulseError,
}

impl IssueTypeId {
    /// The string identifier matching OCaml's `IssueType.ml`.
    pub fn id(self) -> &'static str {
        match self {
            Self::ComparedToNullAndDereferenced => "COMPARED_TO_NULL_AND_DEREFERENCED",
            Self::DeadStore => "DEAD_STORE",
            Self::NullptrDereference => "NULLPTR_DEREFERENCE",
            Self::MemoryLeakC => "MEMORY_LEAK_C",
            Self::UseAfterFree => "USE_AFTER_FREE",
            Self::UseAfterDelete => "USE_AFTER_DELETE",
            Self::UseAfterLifetime => "USE_AFTER_LIFETIME",
            Self::OptionalEmptyAccess => "OPTIONAL_EMPTY_ACCESS",
            Self::VectorInvalidation => "VECTOR_INVALIDATION",
            Self::RetainCycle => "RETAIN_CYCLE",
            Self::PulseError => "PULSE_ERROR",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::MemoryLeakC => Severity::Warning,
            _ => Severity::Error,
        }
    }

    pub fn category(self) -> Category {
        match self {
            Self::ComparedToNullAndDereferenced => Category::NullPointerDereference,
            Self::DeadStore => Category::LogicError,
            Self::NullptrDereference => Category::NullPointerDereference,
            Self::MemoryLeakC => Category::ResourceLeak,
            Self::UseAfterFree
            | Self::UseAfterDelete
            | Self::UseAfterLifetime
            | Self::VectorInvalidation
            | Self::RetainCycle => Category::MemoryError,
            Self::OptionalEmptyAccess | Self::PulseError => Category::Other,
        }
    }
}

impl std::fmt::Display for IssueTypeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id())
    }
}

/// An issue type definition.
///
/// Each distinct kind of bug has its own `IssueType` with a unique ID.
/// Mirrors OCaml's `IssueType.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueType {
    /// Unique identifier string, e.g. "DEAD_STORE", "NULLPTR_DEREFERENCE".
    pub id: String,
    /// Severity level.
    pub severity: Severity,
    /// Category for grouping.
    pub category: Category,
    /// Checker that produces this issue type.
    pub checker: Checker,
}

impl std::fmt::Display for IssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

impl IssueType {
    /// Create an IssueType from a well-known ID and checker name.
    pub fn from_id(id: IssueTypeId, checker: &str) -> Self {
        Self {
            id: id.id().to_string(),
            severity: id.severity(),
            category: id.category(),
            checker: Checker(checker.to_string()),
        }
    }

    pub fn dead_store() -> Self {
        Self::from_id(IssueTypeId::DeadStore, "Liveness")
    }

    pub fn null_dereference() -> Self {
        Self::from_id(IssueTypeId::NullptrDereference, "Pulse")
    }

    pub fn memory_leak() -> Self {
        Self::from_id(IssueTypeId::MemoryLeakC, "Pulse")
    }

    pub fn human_name(&self) -> String {
        self.id
            .strip_suffix("_LATENT")
            .unwrap_or(&self.id)
            .split('_')
            .map(|part| {
                let mut chars = part.chars();
                let Some(first) = chars.next() else {
                    return String::new();
                };
                let rest = chars.as_str().to_ascii_lowercase();
                format!("{}{}", first.to_ascii_uppercase(), rest)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}
