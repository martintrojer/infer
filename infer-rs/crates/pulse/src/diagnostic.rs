// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse diagnostics: error classification and reporting.
//!
//! Mirrors OCaml's `PulseDiagnostic.ml` (simplified).

use std::fmt;

use sil::location::Location;

use crate::abstract_value::AbstractValue;
use crate::invalidation::Invalidation;

/// A Pulse diagnostic — a bug found during analysis.
#[derive(Clone, Debug)]
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
                self.get_issue_type().to_string(),
                invalidation_location.clone(),
            ),
            Diagnostic::MemoryLeak {
                allocation_location,
                ..
            } => (
                self.get_issue_type().to_string(),
                allocation_location.clone(),
            ),
            Diagnostic::RetainCycle { location } => {
                (self.get_issue_type().to_string(), location.clone())
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

    /// Get the issue type string for reporting.
    pub fn get_issue_type(&self) -> &str {
        match self {
            Diagnostic::AccessToInvalidAddress { invalidation, .. } => {
                if invalidation.is_null_deref() {
                    "NULL_DEREFERENCE"
                } else {
                    match invalidation {
                        Invalidation::CFree | Invalidation::FClose => "USE_AFTER_FREE",
                        Invalidation::CppDelete | Invalidation::CppDeleteArray => {
                            "USE_AFTER_DELETE"
                        }
                        Invalidation::GoneOutOfScope(_, _) => "USE_AFTER_LIFETIME",
                        Invalidation::OptionalEmpty => "OPTIONAL_EMPTY_ACCESS",
                        Invalidation::StdVector(_) => "VECTOR_INVALIDATION",
                        _ => "PULSE_ERROR",
                    }
                }
            }
            Diagnostic::MemoryLeak { .. } => "MEMORY_LEAK_C",
            Diagnostic::RetainCycle { .. } => "RETAIN_CYCLE",
        }
    }

    /// Get the severity.
    pub fn get_severity(&self) -> diagnostics::issue_type::Severity {
        match self {
            Diagnostic::MemoryLeak { .. } => diagnostics::issue_type::Severity::Warning,
            _ => diagnostics::issue_type::Severity::Error,
        }
    }

    /// Get the issue category.
    pub fn get_category(&self) -> diagnostics::issue_type::Category {
        match self {
            Diagnostic::AccessToInvalidAddress { invalidation, .. } => {
                if invalidation.is_null_deref() {
                    diagnostics::issue_type::Category::NullDereference
                } else {
                    diagnostics::issue_type::Category::MemoryError
                }
            }
            Diagnostic::MemoryLeak { .. } => diagnostics::issue_type::Category::ResourceLeak,
            Diagnostic::RetainCycle { .. } => diagnostics::issue_type::Category::MemoryError,
        }
    }

    /// Convert to a diagnostics::Issue for reporting.
    pub fn to_issue(&self, procedure: &str) -> diagnostics::issue::Issue {
        let loc = self.get_location();
        diagnostics::issue::Issue {
            issue_type: diagnostics::issue_type::IssueType {
                id: self.get_issue_type().to_string(),
                severity: self.get_severity(),
                category: self.get_category(),
                checker: diagnostics::issue_type::Checker("Pulse".to_string()),
            },
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
