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
use sil::procname::Procname;

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
        self.to_issue_with_context(procedure, None, false, false)
    }

    /// Convert to a diagnostics::Issue for reporting, optionally as latent.
    pub fn to_issue_with_latent(&self, procedure: &str, latent: bool) -> diagnostics::issue::Issue {
        self.to_issue_with_context(procedure, None, latent, false)
    }

    /// Convert to a diagnostics::Issue for reporting, optionally marked as
    /// latent or suppressed.
    pub fn to_issue_with_reporting(
        &self,
        procedure: &str,
        latent: bool,
        suppressed: bool,
    ) -> diagnostics::issue::Issue {
        self.to_issue_with_context(procedure, None, latent, suppressed)
    }

    pub fn to_issue_with_context(
        &self,
        procedure: &str,
        procedure_start_line: Option<u32>,
        latent: bool,
        suppressed: bool,
    ) -> diagnostics::issue::Issue {
        let loc = self.get_location();
        let trace = if suppressed {
            format!("*** SUPPRESSED ***, {}", self.trace_message())
        } else {
            self.trace_message()
        };
        let bug_trace = self.structured_bug_trace();
        let bug_trace_length = bug_trace.as_ref().map(|entries| entries.len() as u32);
        let bug_trace_max_depth = bug_trace
            .as_ref()
            .and_then(|entries| entries.iter().map(|entry| entry.level).max());
        let issue_type = self.build_issue_type(latent);
        let qualifier = format!("{self}");
        let file = format!("{}", loc.file);
        let (key, node_key, hash) = diagnostics::issue::derive_issue_metadata(
            &issue_type,
            &file,
            procedure,
            loc.line as u32,
            loc.col as u32,
            &qualifier,
        );
        diagnostics::issue::Issue {
            bug_type: Some(issue_type.id.clone()),
            bug_type_hum: Some(issue_type.human_name()),
            severity: Some(issue_type.severity.to_string()),
            category: Some(issue_type.category.to_string()),
            issue_type,
            qualifier,
            procedure_start_line,
            key,
            node_key,
            hash,
            file,
            line: loc.line as u32,
            column: loc.col as u32,
            procedure: procedure.to_string(),
            trace,
            bug_trace,
            bug_trace_length,
            bug_trace_max_depth,
            extras: std::collections::BTreeMap::new(),
        }
    }

    fn trace_message(&self) -> String {
        match self {
            Diagnostic::AccessToInvalidAddress {
                access_history,
                invalidation_history,
                ..
            } => {
                let mut parts = vec![format!("{self}")];
                if !invalidation_history.is_epoch() {
                    parts.push(format!(
                        "invalidation history: {}",
                        invalidation_history.signature()
                    ));
                }
                if !access_history.is_epoch() {
                    parts.push(format!("access history: {}", access_history.signature()));
                }
                parts.join("; ")
            }
            Diagnostic::MemoryLeak { .. } | Diagnostic::RetainCycle { .. } => format!("{self}"),
        }
    }

    fn structured_bug_trace(&self) -> Option<Vec<diagnostics::issue::BugTraceEntry>> {
        match self {
            Diagnostic::AccessToInvalidAddress {
                access_location,
                access_history,
                invalidation_history,
                ..
            } => {
                let mut trace = Vec::new();
                if !invalidation_history.is_epoch() {
                    trace.push(make_bug_trace_entry(
                        1,
                        access_location,
                        "invalidation part of the trace starts here".to_string(),
                    ));
                    trace.extend(invalidation_history_to_bug_trace(
                        invalidation_history,
                        access_location,
                        2,
                    ));
                }
                if !access_history.is_epoch() {
                    trace.push(make_bug_trace_entry(
                        1,
                        access_location,
                        access_trace_start_description(self),
                    ));
                    let access_entries =
                        access_history_to_bug_trace(access_history, access_location, 2);
                    let access_depth = access_entries
                        .iter()
                        .map(|entry| entry.level)
                        .max()
                        .unwrap_or(2);
                    trace.extend(access_entries);
                    trace.push(make_bug_trace_entry(
                        access_depth,
                        access_location,
                        "invalid access occurs here".to_string(),
                    ));
                }
                (!trace.is_empty()).then_some(trace)
            }
            Diagnostic::MemoryLeak {
                allocator,
                allocation_location,
                ..
            } => Some(vec![make_bug_trace_entry(
                0,
                allocation_location,
                format!("memory allocated via {allocator:?} here"),
            )]),
            Diagnostic::RetainCycle { location } => Some(vec![make_bug_trace_entry(
                0,
                location,
                "retain cycle here".to_string(),
            )]),
        }
    }

    /// Cross-ref: OCaml `PulseReport.is_constant_deref_without_invalidation`.
    pub fn is_suppressed(&self) -> bool {
        match self {
            Diagnostic::AccessToInvalidAddress {
                invalidation,
                access_history,
                ..
            } => {
                matches!(
                    invalidation,
                    Invalidation::ConstantDereference(_)
                        | Invalidation::ComparedToNullInThisProcedure(_)
                ) && !access_history.contains_invalidation_of_same_type(invalidation)
            }
            Diagnostic::MemoryLeak { .. } | Diagnostic::RetainCycle { .. } => false,
        }
    }
}

fn make_bug_trace_entry(
    level: u32,
    loc: &Location,
    description: String,
) -> diagnostics::issue::BugTraceEntry {
    diagnostics::issue::BugTraceEntry {
        level,
        filename: format!("{}", loc.file),
        line_number: loc.line as u32,
        column_number: loc.col as u32,
        description,
    }
}

fn bug_trace_event_location(
    events: &[crate::value_history::HistoryEvent],
    index: usize,
    fallback: &Location,
) -> Location {
    events[index]
        .location()
        .or_else(|| {
            events[index + 1..]
                .iter()
                .find_map(|event| event.location())
        })
        .or_else(|| {
            events[..index]
                .iter()
                .rev()
                .find_map(|event| event.location())
        })
        .cloned()
        .unwrap_or_else(|| fallback.clone())
}

fn pvar_trace_description(pvar: &sil::pvar::Pvar) -> String {
    match &pvar.kind {
        sil::pvar::PvarKind::Local { proc_name, .. }
        | sil::pvar::PvarKind::Callee(proc_name)
        | sil::pvar::PvarKind::Seed(proc_name) => {
            format!("parameter `{}` of {proc_name}", pvar.name)
        }
        sil::pvar::PvarKind::Global { .. } => format!("global `{}`", pvar.name),
    }
}

fn access_trace_start_description(diagnostic: &Diagnostic) -> String {
    match diagnostic.get_issue_type_id() {
        IssueTypeId::UseAfterFree | IssueTypeId::UseAfterDelete | IssueTypeId::UseAfterLifetime => {
            "use-after-lifetime part of the trace starts here".to_string()
        }
        _ => "access part of the trace starts here".to_string(),
    }
}

fn is_modeled_allocation_proc(proc: &Procname) -> bool {
    let name = format!("{proc}");
    name.contains("malloc") || name.contains("realloc")
}

fn history_events_to_bug_trace(
    events: &[crate::value_history::HistoryEvent],
    fallback: &Location,
    base_level: u32,
) -> Vec<diagnostics::issue::BugTraceEntry> {
    let mut entries = Vec::new();
    let mut level = base_level;
    let mut index = 0usize;
    while let Some(event) = events.get(index) {
        match event {
            crate::value_history::HistoryEvent::ReturnFromCall { .. } => {
                level = level.saturating_sub(1);
                index += 1;
            }
            crate::value_history::HistoryEvent::Call { proc, location }
                if events.get(index + 1).is_some_and(|next| {
                    matches!(next, crate::value_history::HistoryEvent::Returned(_))
                }) && is_modeled_allocation_proc(proc) =>
            {
                entries.push(make_bug_trace_entry(
                    level,
                    location,
                    format!("allocated by call to `{proc}` (modelled)"),
                ));
                index += 2;
            }
            crate::value_history::HistoryEvent::Call { proc, location } => {
                entries.push(make_bug_trace_entry(
                    level,
                    location,
                    format!("when calling `{proc}` here"),
                ));
                level += 1;
                index += 1;
            }
            crate::value_history::HistoryEvent::FormalArgument(pvar) => {
                let loc = bug_trace_event_location(events, index, fallback);
                entries.push(make_bug_trace_entry(
                    level,
                    &loc,
                    pvar_trace_description(pvar),
                ));
                index += 1;
            }
            crate::value_history::HistoryEvent::ActualArgument(pvar) => {
                let loc = bug_trace_event_location(events, index, fallback);
                entries.push(make_bug_trace_entry(
                    level,
                    &loc,
                    pvar_trace_description(pvar),
                ));
                index += 1;
            }
            crate::value_history::HistoryEvent::Assignment(location) => {
                entries.push(make_bug_trace_entry(
                    level,
                    location,
                    "assigned here".to_string(),
                ));
                index += 1;
            }
            crate::value_history::HistoryEvent::Returned(location) => {
                entries.push(make_bug_trace_entry(
                    level,
                    location,
                    "returned here".to_string(),
                ));
                index += 1;
            }
            crate::value_history::HistoryEvent::Invalidated {
                invalidation,
                location,
            } => {
                entries.push(make_bug_trace_entry(
                    level,
                    location,
                    format!("{invalidation}"),
                ));
                index += 1;
            }
        }
    }
    entries
}

fn history_to_bug_trace(
    history: &ValueHistory,
    fallback: &Location,
    base_level: u32,
) -> Vec<diagnostics::issue::BugTraceEntry> {
    let Some(path) = history.primary_path() else {
        return Vec::new();
    };
    history_events_to_bug_trace(path.events(), fallback, base_level)
}

fn invalidation_history_to_bug_trace(
    history: &ValueHistory,
    fallback: &Location,
    base_level: u32,
) -> Vec<diagnostics::issue::BugTraceEntry> {
    let Some(path) = history.primary_path() else {
        return Vec::new();
    };
    let events = path.events();
    if let [crate::value_history::HistoryEvent::Call { proc, location }, crate::value_history::HistoryEvent::FormalArgument(inner_pvar), ..] =
        events
    {
        let inner_proc = match &inner_pvar.kind {
            sil::pvar::PvarKind::Local { proc_name, .. }
            | sil::pvar::PvarKind::Callee(proc_name)
            | sil::pvar::PvarKind::Seed(proc_name) => Some(proc_name),
            sil::pvar::PvarKind::Global { .. } => None,
        };
        if inner_proc.is_some_and(|inner_proc| inner_proc != proc) {
            let outer_description = format!("parameter `{}` of {proc}", inner_pvar.name);
            let mut entries = vec![make_bug_trace_entry(
                base_level,
                location,
                outer_description,
            )];
            entries.push(make_bug_trace_entry(
                base_level,
                location,
                format!("when calling `{}` here", inner_proc.unwrap()),
            ));
            entries.extend(history_events_to_bug_trace(
                &events[1..events.len()
                    - usize::from(matches!(
                        events.last(),
                        Some(crate::value_history::HistoryEvent::ReturnFromCall { .. })
                    ))],
                fallback,
                base_level + 1,
            ));
            return entries;
        }
    }
    history_to_bug_trace(history, fallback, base_level)
}

fn access_history_to_bug_trace(
    history: &ValueHistory,
    fallback: &Location,
    base_level: u32,
) -> Vec<diagnostics::issue::BugTraceEntry> {
    fn rec(
        events: &[crate::value_history::HistoryEvent],
        fallback: &Location,
        level: u32,
    ) -> Vec<diagnostics::issue::BugTraceEntry> {
        let Some((first, rest)) = events.split_first() else {
            return Vec::new();
        };
        match first {
            crate::value_history::HistoryEvent::ActualArgument(pvar) => {
                let mut entries = rec(rest, fallback, level);
                let call_loc = rest
                    .iter()
                    .find_map(|event| event.location())
                    .cloned()
                    .unwrap_or_else(|| fallback.clone());
                let proc_name = match &pvar.kind {
                    sil::pvar::PvarKind::Local { proc_name, .. }
                    | sil::pvar::PvarKind::Callee(proc_name)
                    | sil::pvar::PvarKind::Seed(proc_name) => Some(proc_name),
                    sil::pvar::PvarKind::Global { .. } => None,
                };
                if let Some(proc_name) = proc_name {
                    entries.push(make_bug_trace_entry(
                        level,
                        &call_loc,
                        format!("when calling `{proc_name}` here"),
                    ));
                    entries.push(make_bug_trace_entry(
                        level + 1,
                        &call_loc,
                        pvar_trace_description(pvar),
                    ));
                } else {
                    entries.push(make_bug_trace_entry(
                        level,
                        &call_loc,
                        pvar_trace_description(pvar),
                    ));
                }
                entries
            }
            _ => history_events_to_bug_trace(events, fallback, level),
        }
    }

    let Some(path) = history.primary_path() else {
        return Vec::new();
    };
    rec(path.events(), fallback, base_level)
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

#[cfg(test)]
mod tests {
    use super::*;
    use sil::int_lit::IntLit;
    use sil::mangled::Mangled;
    use sil::procname::Procname;
    use sil::pvar::Pvar;

    use crate::abstract_value::AbstractValue;
    use crate::value_history::HistoryEvent;

    fn loc(line: i32) -> Location {
        Location {
            line,
            col: 1,
            ..Location::dummy()
        }
    }

    #[test]
    fn test_issue_trace_includes_access_and_invalidation_history() {
        let proc = Procname::c_from_string("foo");
        let pvar = Pvar::mk(Mangled::from_string("x"), proc);
        let diag = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: Invalidation::CFree,
            access_location: loc(20),
            access_history: ValueHistory::formal_argument(pvar.clone()).append_assignment(loc(20)),
            invalidation_history: ValueHistory::formal_argument(pvar).append_event(
                HistoryEvent::Invalidated {
                    invalidation: Invalidation::CFree,
                    location: loc(12),
                },
            ),
        };

        let issue = diag.to_issue("foo");
        assert!(issue.trace.contains("invalidation history:"));
        assert!(issue.trace.contains("access history:"));
        assert!(issue.trace.contains("formal("));
        assert!(issue.trace.contains("assign@"));
        assert!(issue
            .bug_trace
            .as_ref()
            .is_some_and(|trace| !trace.is_empty()));
        assert_eq!(
            issue.bug_trace_length,
            issue.bug_trace.as_ref().map(|trace| trace.len() as u32)
        );
        assert!(issue.bug_trace_max_depth.is_some_and(|depth| depth > 1));
        assert!(issue.bug_trace.as_ref().is_some_and(|trace| {
            trace
                .iter()
                .any(|entry| entry.description.contains("parameter `x` of foo"))
        }));
    }

    #[test]
    fn test_suppressed_issue_trace_keeps_prefix_and_history() {
        let diag = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: Invalidation::ConstantDereference(IntLit::zero()),
            access_location: loc(30),
            access_history: ValueHistory::assignment(loc(30)),
            invalidation_history: ValueHistory::invalidated(
                Invalidation::ConstantDereference(IntLit::zero()),
                loc(18),
            ),
        };

        let issue = diag.to_issue_with_reporting("foo", false, true);
        assert!(issue.trace.starts_with("*** SUPPRESSED ***,"));
        assert!(issue.trace.contains("invalidation history:"));
        assert!(issue.trace.contains("access history:"));
        assert!(issue
            .bug_trace
            .as_ref()
            .is_some_and(|trace| !trace.is_empty()));
        assert!(issue.bug_trace.as_ref().is_some_and(|trace| {
            trace
                .iter()
                .any(|entry| entry.description == "access part of the trace starts here")
        }));
    }

    #[test]
    fn test_access_bug_trace_reorders_caller_history_before_synthetic_call() {
        let caller = Procname::c_from_string("caller");
        let callee = Procname::c_from_string("callee");
        let caller_pvar = Pvar::mk(Mangled::from_string("x"), caller.clone());
        let callee_pvar = Pvar::mk(Mangled::from_string("x"), callee.clone());
        let diag = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: Invalidation::CFree,
            access_location: loc(40),
            access_history: ValueHistory::from_event(HistoryEvent::ActualArgument(callee_pvar))
                .append_event(HistoryEvent::FormalArgument(caller_pvar)),
            invalidation_history: ValueHistory::invalidated(Invalidation::CFree, loc(12)),
        };

        let issue = diag.to_issue("caller");
        let trace = issue.bug_trace.expect("expected structured bug trace");
        let descriptions: Vec<_> = trace
            .iter()
            .map(|entry| entry.description.as_str())
            .collect();
        let caller_idx = descriptions
            .iter()
            .position(|desc| *desc == "parameter `x` of caller")
            .expect("caller parameter entry should exist");
        let call_idx = descriptions
            .iter()
            .position(|desc| *desc == "when calling `callee` here")
            .expect("synthetic call entry should exist");
        let callee_idx = descriptions
            .iter()
            .position(|desc| *desc == "parameter `x` of callee")
            .expect("callee parameter entry should exist");
        assert!(caller_idx < call_idx && call_idx < callee_idx);
    }

    #[test]
    fn test_bug_trace_compresses_modelled_allocation_call_and_return() {
        let history =
            ValueHistory::returned(loc(12)).wrap_call(&Procname::c_from_string("malloc"), &loc(12));
        let trace = history_to_bug_trace(&history, &loc(12), 2);
        let descriptions: Vec<_> = trace
            .iter()
            .map(|entry| entry.description.as_str())
            .collect();
        assert_eq!(
            descriptions,
            vec!["allocated by call to `malloc` (modelled)"],
            "modelled allocation call/return should collapse to one trace step"
        );
    }

    #[test]
    fn test_invalidation_bug_trace_synthesizes_outer_parameter_and_inner_call() {
        let outer = Procname::c_from_string("latent_use_after_free");
        let inner = Procname::c_from_string("conditional_free2");
        let inner_pvar = Pvar::mk(Mangled::from_string("x"), inner.clone());
        let history = ValueHistory::from_event(HistoryEvent::FormalArgument(inner_pvar))
            .append_event(HistoryEvent::Invalidated {
                invalidation: Invalidation::CFree,
                location: loc(12),
            })
            .wrap_call(&outer, &loc(25));
        let trace = invalidation_history_to_bug_trace(&history, &loc(25), 2);
        let descriptions: Vec<_> = trace
            .iter()
            .map(|entry| entry.description.as_str())
            .collect();
        assert_eq!(
            descriptions,
            vec![
                "parameter `x` of latent_use_after_free",
                "when calling `conditional_free2` here",
                "parameter `x` of conditional_free2",
                "was invalidated by call to `free()`",
            ]
        );
    }
}
