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
    NullDereference,
    ResourceLeak,
    RaceCondition,
    Perf,
    Other,
}

/// The checker that produced an issue.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Checker(pub String);

/// An issue type definition.
///
/// Each distinct kind of bug has its own `IssueType` with a unique ID.
/// Mirrors OCaml's `IssueType.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IssueType {
    /// Unique identifier string, e.g. "DEAD_STORE", "NULL_DEREFERENCE".
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

// ---------------------------------------------------------------------------
// Well-known issue types
// ---------------------------------------------------------------------------

/// Dead store: a value is written to a variable but never read.
pub const DEAD_STORE: &str = "DEAD_STORE";

/// Null dereference: a null pointer is dereferenced.
pub const NULL_DEREFERENCE: &str = "NULL_DEREFERENCE";

/// Memory leak (C): allocated memory is not freed on all paths.
/// Matches OCaml's MEMORY_LEAK_C issue type.
pub const MEMORY_LEAK_C: &str = "MEMORY_LEAK_C";

/// Use after free: memory accessed after being freed.
pub const USE_AFTER_FREE: &str = "USE_AFTER_FREE";

impl IssueType {
    pub fn dead_store() -> Self {
        Self {
            id: DEAD_STORE.to_string(),
            severity: Severity::Error,
            category: Category::LogicError,
            checker: Checker("Liveness".to_string()),
        }
    }

    pub fn null_dereference() -> Self {
        Self {
            id: NULL_DEREFERENCE.to_string(),
            severity: Severity::Error,
            category: Category::NullDereference,
            checker: Checker("Pulse".to_string()),
        }
    }

    pub fn memory_leak() -> Self {
        Self {
            id: MEMORY_LEAK_C.to_string(),
            severity: Severity::Error,
            category: Category::ResourceLeak,
            checker: Checker("Pulse".to_string()),
        }
    }
}
