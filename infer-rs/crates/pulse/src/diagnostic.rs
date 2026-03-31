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
use crate::value_history::ValueHistory;

/// A Pulse diagnostic — a bug found during analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diagnostic {
    /// Accessing an address that has been invalidated (null deref, use-after-free, etc.).
    AccessToInvalidAddress {
        addr: AbstractValue,
        invalidation: Invalidation,
        access_location: Location,
        access_history: ValueHistory,
        invalidation_history: ValueHistory,
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
    pub fn dedup_key(&self) -> String {
        match self {
            Diagnostic::AccessToInvalidAddress {
                access_location,
                access_history,
                invalidation_history,
                ..
            } => format!(
                "{}|{}|{}|{}",
                self.get_issue_type_id().id(),
                access_location,
                access_history.signature(),
                invalidation_history.signature()
            ),
            Diagnostic::MemoryLeak {
                allocation_location,
                ..
            } => format!("{}|{}", self.get_issue_type_id().id(), allocation_location),
            Diagnostic::RetainCycle { location } => {
                format!("{}|{}", self.get_issue_type_id().id(), location)
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
                        Invalidation::ComparedToNullInThisProcedure(_) => {
                            IssueTypeId::ComparedToNullAndDereferenced
                        }
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

    fn build_issue_type(&self, latent: bool) -> diagnostics::issue_type::IssueType {
        let type_id = self.get_issue_type_id();
        let id = if latent {
            format!("{}_LATENT", type_id.id())
        } else {
            type_id.id().to_string()
        };
        diagnostics::issue_type::IssueType {
            id,
            severity: type_id.severity(),
            category: type_id.category(),
            checker: diagnostics::issue_type::Checker("Pulse".to_string()),
        }
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
        self.to_issue_with_latent(procedure, false)
    }

    /// Convert to a diagnostics::Issue for reporting, optionally as latent.
    pub fn to_issue_with_latent(&self, procedure: &str, latent: bool) -> diagnostics::issue::Issue {
        let loc = self.get_location();
        diagnostics::issue::Issue {
            issue_type: self.build_issue_type(latent),
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
                access_history,
                invalidation_history,
                ..
            } => {
                if invalidation.is_null_deref() {
                    if let Some((_inv, loc)) = access_history.caller_argument_invalidation() {
                        return write!(
                            f,
                            "address could be null (null value originating from line {}) and is dereferenced",
                            loc.line
                        );
                    }
                    if let Some((_inv, loc)) = access_history.first_invalidation_before_call() {
                        return write!(
                            f,
                            "address could be null (null value originating from line {}) and is dereferenced",
                            loc.line
                        );
                    }
                    if let Some((proc, loc)) = access_history.first_call_before_invalidation() {
                        return write!(
                            f,
                            "address could be null (from the call to `{proc}` on line {}) and is dereferenced",
                            loc.line
                        );
                    }
                    if let Some((_inv, loc)) = invalidation_history.first_invalidation() {
                        return write!(
                            f,
                            "address could be null (null value originating from line {}) and is dereferenced",
                            loc.line
                        );
                    }
                }

                if let Some((_inv, loc)) = invalidation_history.first_invalidation() {
                    write!(
                        f,
                        "accessing address that {invalidation} from line {}",
                        loc.line
                    )
                } else {
                    write!(f, "accessing address that {invalidation}")
                }
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
