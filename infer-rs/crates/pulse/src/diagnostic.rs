// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse diagnostics: error classification and reporting.
//!
//! Mirrors OCaml's `PulseDiagnostic.ml` (simplified).

use std::fmt;

use diagnostics::issue_type::IssueTypeId;
use sil::location::Location;

use crate::abstract_value::AbstractValue;
use crate::invalidation::Invalidation;

/// A Pulse diagnostic — a bug found during analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    /// Accessing an address that has been invalidated (null deref, use-after-free, etc.).
    AccessToInvalidAddress {
        addr: AbstractValue,
        invalidation: Invalidation,
        access_location: Location,
        invalidation_location: Location,
    },
    /// Memory leak: allocated but never freed.
    MemoryLeak {
        addr: AbstractValue,
        allocator: crate::attribute::Allocator,
        allocation_location: Location,
    },
    /// Retain cycle detected (ObjC/Swift).
    RetainCycle { location: Location },
}

impl Diagnostic {
    /// Deduplication key: identifies the same bug across different SIL nodes.
    ///
    /// Uses (issue_type, invalidation_location) rather than access_location,
    /// because the same null pointer can be dereferenced at multiple SIL nodes
    /// that map to the same C source location (e.g., short-circuit `&&`/`||`
    /// generates duplicate load nodes).
    pub fn dedup_key(&self) -> (String, Location) {
        match self {
            Diagnostic::AccessToInvalidAddress {
                invalidation_location,
                ..
            } => (
                self.get_issue_type_id().id().to_string(),
                invalidation_location.clone(),
            ),
            Diagnostic::MemoryLeak {
                allocation_location,
                ..
            } => (
                self.get_issue_type_id().id().to_string(),
                allocation_location.clone(),
            ),
            Diagnostic::RetainCycle { location } => {
                (self.get_issue_type_id().id().to_string(), location.clone())
            }
        }
    }

    /// Get the location where the bug manifests.
    pub fn get_location(&self) -> &Location {
        match self {
            Diagnostic::AccessToInvalidAddress {
                access_location, ..
            } => access_location,
            Diagnostic::MemoryLeak {
                allocation_location,
                ..
            } => allocation_location,
            Diagnostic::RetainCycle { location } => location,
        }
    }

    /// Get the well-known issue type ID.
    pub fn get_issue_type_id(&self) -> IssueTypeId {
        match self {
            Diagnostic::AccessToInvalidAddress { invalidation, .. } => {
                if invalidation.is_null_deref() {
                    IssueTypeId::NullptrDereference
                } else {
                    match invalidation {
                        Invalidation::CFree | Invalidation::FClose => IssueTypeId::UseAfterFree,
                        Invalidation::CppDelete | Invalidation::CppDeleteArray => {
                            IssueTypeId::UseAfterDelete
                        }
                        Invalidation::GoneOutOfScope(_, _) => IssueTypeId::UseAfterLifetime,
                        Invalidation::OptionalEmpty => IssueTypeId::OptionalEmptyAccess,
                        Invalidation::StdVector(_) => IssueTypeId::VectorInvalidation,
                        _ => IssueTypeId::PulseError,
                    }
                }
            }
            Diagnostic::MemoryLeak { .. } => IssueTypeId::MemoryLeakC,
            Diagnostic::RetainCycle { .. } => IssueTypeId::RetainCycle,
        }
    }

    /// Get the issue type string for reporting. Matches OCaml's issue type IDs.
    pub fn get_issue_type(&self) -> &'static str {
        self.get_issue_type_id().id()
    }

    /// Get the severity.
    pub fn get_severity(&self) -> diagnostics::issue_type::Severity {
        self.get_issue_type_id().severity()
    }

    /// Get the issue category.
    pub fn get_category(&self) -> diagnostics::issue_type::Category {
        self.get_issue_type_id().category()
    }

    /// Convert to a diagnostics::Issue for reporting.
    pub fn to_issue(&self, procedure: &str) -> diagnostics::issue::Issue {
        let loc = self.get_location();
        let type_id = self.get_issue_type_id();
        diagnostics::issue::Issue {
            issue_type: diagnostics::issue_type::IssueType::from_id(type_id, "Pulse"),
            qualifier: format!("{self}"),
            file: format!("{}", loc.file),
            line: loc.line as u32,
            column: loc.col as u32,
            procedure: procedure.to_string(),
            trace: format!("{self}"),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Diagnostic::AccessToInvalidAddress {
                invalidation,
                access_location,
                ..
            } => {
                write!(
                    f,
                    "accessing address that {invalidation} at {access_location}"
                )
            }
            Diagnostic::MemoryLeak { allocator, .. } => {
                write!(f, "memory allocated via {allocator:?} is leaked")
            }
            Diagnostic::RetainCycle { location } => {
                write!(f, "retain cycle at {location}")
            }
        }
    }
}
