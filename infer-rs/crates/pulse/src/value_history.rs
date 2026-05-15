// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Minimal value provenance for Pulse diagnostics.
//!
//! This is a reduced Rust analogue of OCaml's `PulseValueHistory` and
//! `PulseTrace`: we keep one or more event paths per value, then derive
//! access/invalidation provenance from those paths when building diagnostics.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use sil::location::Location;
use sil::procname::Procname;
use sil::pvar::Pvar;

use crate::abstract_value::AbstractValue;
use crate::invalidation::Invalidation;

/// Keep Rust's eager path-set representation bounded at hot merge sites.
///
/// Cross-ref: OCaml `PulseValueHistory.binary_op` and `multiplex` keep a
/// history tree/list of existing histories. They do not eagerly clone and
/// flatten all event paths every time two operands are combined. Rust's
/// simplified `BTreeSet<HistoryPath>` representation otherwise grows without
/// bound in straight-line hash bodies where every arithmetic `BinOp` unions two
/// already-large histories and then assignment appends another event to every
/// path.
const MAX_MERGED_HISTORY_PATHS: usize = 2;
const MAX_HISTORY_EVENTS_PER_PATH: usize = 8;

/// One step in a value's provenance path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HistoryEvent {
    /// Formal placeholder to be replaced by the caller actual history.
    FormalArgument(Pvar, Option<Location>),
    /// Marker that a substituted path came from a caller actual.
    ActualArgument(Pvar, Option<Location>),
    Assignment(Location),
    Call {
        proc: Procname,
        location: Location,
    },
    ReturnFromCall {
        proc: Procname,
        location: Location,
    },
    Returned(Location),
    Invalidated {
        invalidation: Invalidation,
        location: Location,
    },
}

impl HistoryEvent {
    pub(crate) fn location(&self) -> Option<&Location> {
        match self {
            Self::FormalArgument(_, location) | Self::ActualArgument(_, location) => {
                location.as_ref()
            }
            Self::Assignment(location)
            | Self::Returned(location)
            | Self::Invalidated { location, .. }
            | Self::Call { location, .. }
            | Self::ReturnFromCall { location, .. } => Some(location),
        }
    }
}

/// One concrete provenance path from origin to the current value.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HistoryPath(pub Vec<HistoryEvent>);

impl HistoryPath {
    pub fn empty() -> Self {
        Self::default()
    }

    fn new_capped(events: Vec<HistoryEvent>) -> Self {
        Self(cap_history_events(events))
    }

    pub(crate) fn events(&self) -> &[HistoryEvent] {
        &self.0
    }

    fn append(&self, event: HistoryEvent) -> Self {
        let mut events = self.0.clone();
        events.push(event);
        Self::new_capped(events)
    }

    fn prepend(&self, event: HistoryEvent) -> Self {
        let mut events = Vec::with_capacity(self.0.len() + 1);
        events.push(event);
        events.extend(self.0.clone());
        Self::new_capped(events)
    }

    fn wrap_call(&self, proc: &Procname, location: &Location) -> Self {
        let mut events = Vec::with_capacity(self.0.len() + 2);
        events.push(HistoryEvent::Call {
            proc: proc.clone(),
            location: location.clone(),
        });
        events.extend(self.0.clone());
        events.push(HistoryEvent::ReturnFromCall {
            proc: proc.clone(),
            location: location.clone(),
        });
        Self::new_capped(events)
    }

    fn first_invalidation_before_call(&self) -> Option<(&Invalidation, &Location)> {
        for event in &self.0 {
            match event {
                HistoryEvent::Invalidated {
                    invalidation,
                    location,
                } => return Some((invalidation, location)),
                HistoryEvent::Call { .. } => return None,
                HistoryEvent::ActualArgument(_, _) => {}
                _ => {}
            }
        }
        None
    }

    fn first_call_before_invalidation(&self) -> Option<(&Procname, &Location)> {
        for event in &self.0 {
            match event {
                HistoryEvent::Call { proc, location } => return Some((proc, location)),
                HistoryEvent::Invalidated { .. } => return None,
                _ => {}
            }
        }
        None
    }

    fn last_call_before_invalidation(&self) -> Option<(&Procname, &Location)> {
        let mut last = None;
        for event in &self.0 {
            match event {
                HistoryEvent::Call { proc, location } => last = Some((proc, location)),
                HistoryEvent::Invalidated { .. } => return last,
                _ => {}
            }
        }
        None
    }

    fn has_call_at_location_before_invalidation(&self, location: &Location) -> bool {
        self.first_call_before_invalidation()
            .is_some_and(|(_proc, call_loc)| call_loc == location)
    }

    fn first_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        self.0.iter().find_map(|event| match event {
            HistoryEvent::Invalidated {
                invalidation,
                location,
            } => Some((invalidation, location)),
            _ => None,
        })
    }

    fn caller_argument_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        let mut saw_actual_argument = false;
        for event in &self.0 {
            match event {
                HistoryEvent::ActualArgument(_, _) => saw_actual_argument = true,
                HistoryEvent::Invalidated {
                    invalidation,
                    location,
                } if saw_actual_argument => return Some((invalidation, location)),
                HistoryEvent::Call { .. } if saw_actual_argument => return None,
                _ => {}
            }
        }
        None
    }

    fn contains_formal_origin(&self) -> bool {
        self.0.iter().any(|event| {
            matches!(
                event,
                HistoryEvent::FormalArgument(_, _) | HistoryEvent::ActualArgument(_, _)
            )
        })
    }

    fn contains_invalidation(&self, invalidation: &Invalidation) -> bool {
        self.0.iter().any(|event| match event {
            HistoryEvent::Invalidated {
                invalidation: found,
                ..
            } => found == invalidation,
            _ => false,
        })
    }

    fn contains_invalidation_of_same_type(&self, invalidation: &Invalidation) -> bool {
        self.0.iter().any(|event| match event {
            HistoryEvent::Invalidated {
                invalidation: found,
                ..
            } => found.is_same_type(invalidation),
            _ => false,
        })
    }

    fn last_location(&self) -> Option<&Location> {
        self.0.iter().rev().find_map(HistoryEvent::location)
    }
}

fn is_diagnostic_event(event: &HistoryEvent) -> bool {
    !matches!(event, HistoryEvent::Assignment(_))
}

/// Keep at most `MAX_HISTORY_EVENTS_PER_PATH` events in a materialized path.
/// Prefer non-assignment events because they drive issue classification and
/// traces, then retain the latest assignment events as a fallback location for
/// straight-line value flow. This approximates OCaml's lazy nested histories:
/// diagnostics remain anchored while hash-body assignment chains stop growing
/// linearly through every arithmetic/store step.
fn cap_history_events(events: Vec<HistoryEvent>) -> Vec<HistoryEvent> {
    if events.len() <= MAX_HISTORY_EVENTS_PER_PATH {
        return events;
    }

    let mut keep = vec![false; events.len()];
    let mut count = 0usize;

    for (index, event) in events.iter().enumerate() {
        if is_diagnostic_event(event) {
            keep[index] = true;
            count += 1;
        }
    }

    for index in (0..events.len()).rev() {
        if count >= MAX_HISTORY_EVENTS_PER_PATH {
            break;
        }
        if !keep[index] {
            keep[index] = true;
            count += 1;
        }
    }

    events
        .into_iter()
        .enumerate()
        .filter_map(|(index, event)| keep[index].then_some(event))
        .take(MAX_HISTORY_EVENTS_PER_PATH)
        .collect()
}

fn merge_path_score(path: &HistoryPath) -> u8 {
    let mut score = 0u8;
    for event in &path.0 {
        score = score.saturating_add(match event {
            HistoryEvent::Invalidated { .. } => 8,
            HistoryEvent::FormalArgument(_, _) | HistoryEvent::ActualArgument(_, _) => 4,
            HistoryEvent::Call { .. } | HistoryEvent::ReturnFromCall { .. } => 2,
            HistoryEvent::Returned(_) => 1,
            HistoryEvent::Assignment(_) => 0,
        });
    }
    score
}

fn cap_merged_paths(mut paths: BTreeSet<Arc<HistoryPath>>) -> BTreeSet<Arc<HistoryPath>> {
    if paths.len() <= MAX_MERGED_HISTORY_PATHS {
        return paths;
    }

    let mut candidates: Vec<_> = std::mem::take(&mut paths).into_iter().collect();
    candidates.sort_by(|lhs, rhs| {
        merge_path_score(rhs)
            .cmp(&merge_path_score(lhs))
            .then_with(|| lhs.0.len().cmp(&rhs.0.len()))
            .then_with(|| lhs.cmp(rhs))
    });
    candidates.truncate(MAX_MERGED_HISTORY_PATHS);
    candidates.into_iter().collect()
}

impl fmt::Display for HistoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, event) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, " -> ")?;
            }
            match event {
                HistoryEvent::FormalArgument(pvar, _) => write!(f, "formal({pvar})")?,
                HistoryEvent::ActualArgument(pvar, _) => write!(f, "actual({pvar})")?,
                HistoryEvent::Assignment(location) => write!(f, "assign@{location}")?,
                HistoryEvent::Call { proc, location } => write!(f, "call {proc}@{location}")?,
                HistoryEvent::ReturnFromCall { proc, location } => {
                    write!(f, "return {proc}@{location}")?
                }
                HistoryEvent::Returned(location) => write!(f, "returned@{location}")?,
                HistoryEvent::Invalidated {
                    invalidation,
                    location,
                } => write!(f, "{invalidation}@{location}")?,
            }
        }
        Ok(())
    }
}

/// A value can have more than one provenance path after branch or equality
/// merges, so keep a small set of canonical paths.
///
/// Wrap both the path set and the individual paths in `Arc` to mirror OCaml's
/// cheap sharing of immutable history values. `BaseMemory`/`BaseStack` cloning
/// is frequent at hot fixpoint nodes; with an owned `BTreeSet`, every
/// `ValueWithHistory` clone recursively copied every path/event vector even
/// when the clone was only being retained as an identical snapshot. Hot
/// arithmetic merge sites need one more level of sharing: OCaml
/// `PulseValueHistory.binary_op` / `multiplex` links existing histories instead
/// of materializing fresh copies of their event lists, so Rust should not deep
/// clone every `HistoryPath` just to union two provenance sets.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ValueHistory(Arc<BTreeSet<Arc<HistoryPath>>>);

impl Clone for ValueHistory {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl Serialize for ValueHistory {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ValueHistory {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let paths = BTreeSet::<HistoryPath>::deserialize(deserializer)?
            .into_iter()
            .map(Arc::new)
            .collect();
        Ok(Self(Arc::new(paths)))
    }
}

impl Default for ValueHistory {
    fn default() -> Self {
        Self::epoch()
    }
}

impl ValueHistory {
    pub fn epoch() -> Self {
        let mut paths = BTreeSet::new();
        paths.insert(Arc::new(HistoryPath::empty()));
        Self(Arc::new(paths))
    }

    pub fn from_event(event: HistoryEvent) -> Self {
        let mut paths = BTreeSet::new();
        paths.insert(Arc::new(HistoryPath::new_capped(vec![event])));
        Self(Arc::new(paths))
    }

    pub fn formal_argument(pvar: Pvar) -> Self {
        Self::from_event(HistoryEvent::FormalArgument(pvar, None))
    }

    pub fn formal_argument_at(pvar: Pvar, location: Location) -> Self {
        Self::from_event(HistoryEvent::FormalArgument(pvar, Some(location)))
    }

    pub fn assignment(location: Location) -> Self {
        Self::from_event(HistoryEvent::Assignment(location))
    }

    pub fn returned(location: Location) -> Self {
        Self::from_event(HistoryEvent::Returned(location))
    }

    pub fn invalidated(invalidation: Invalidation, location: Location) -> Self {
        Self::from_event(HistoryEvent::Invalidated {
            invalidation,
            location,
        })
    }

    pub fn is_epoch(&self) -> bool {
        self.0.len() == 1 && self.0.iter().next().is_some_and(|path| path.0.is_empty())
    }

    pub fn append_event(&self, event: HistoryEvent) -> Self {
        let paths = self
            .0
            .iter()
            .map(|path| Arc::new(path.append(event.clone())))
            .collect();
        Self(Arc::new(paths))
    }

    pub fn prepend_event(&self, event: HistoryEvent) -> Self {
        let paths = self
            .0
            .iter()
            .map(|path| Arc::new(path.prepend(event.clone())))
            .collect();
        Self(Arc::new(paths))
    }

    pub fn append_assignment(&self, location: Location) -> Self {
        self.append_event(HistoryEvent::Assignment(location))
    }

    pub fn append_returned(&self, location: Location) -> Self {
        self.append_event(HistoryEvent::Returned(location))
    }

    pub fn wrap_call(&self, proc: &Procname, location: &Location) -> Self {
        let paths = self
            .0
            .iter()
            .map(|path| Arc::new(path.wrap_call(proc, location)))
            .collect();
        Self(Arc::new(paths))
    }

    pub fn merge(&self, other: &Self) -> Self {
        if Arc::ptr_eq(&self.0, &other.0) || self == other {
            return self.clone();
        }

        let self_len = self.0.len();
        let other_len = other.0.len();
        if other_len <= self_len && other.0.is_subset(&self.0) {
            return self.clone();
        }
        if self_len <= other_len && self.0.is_subset(&other.0) {
            return other.clone();
        }

        let (base, extra) = if self_len >= other_len {
            (&self.0, &other.0)
        } else {
            (&other.0, &self.0)
        };
        let mut paths = base.as_ref().clone();
        paths.extend(extra.iter().cloned());
        Self(Arc::new(cap_merged_paths(paths)))
    }

    /// Variant for hot left-fold style call sites. When the accumulator is
    /// uniquely owned, extend it in place with `Arc::make_mut` and only copy
    /// path handles from the other history.
    pub fn merge_owned(mut self, other: &Self) -> Self {
        if Arc::ptr_eq(&self.0, &other.0) || self == *other {
            return self;
        }

        let self_len = self.0.len();
        let other_len = other.0.len();
        if other_len <= self_len && other.0.is_subset(&self.0) {
            return self;
        }
        if self_len <= other_len && self.0.is_subset(&other.0) {
            return other.clone();
        }

        if Arc::strong_count(&self.0) == 1 {
            let paths = Arc::make_mut(&mut self.0);
            paths.extend(other.0.iter().cloned());
            if paths.len() <= MAX_MERGED_HISTORY_PATHS {
                return self;
            }
            self.0 = Arc::new(cap_merged_paths(std::mem::take(paths)));
            return self;
        }

        if self_len >= other_len {
            let mut paths = self.0.as_ref().clone();
            paths.extend(other.0.iter().cloned());
            Self(Arc::new(cap_merged_paths(paths)))
        } else {
            let mut paths = other.0.as_ref().clone();
            paths.extend(self.0.iter().cloned());
            Self(Arc::new(cap_merged_paths(paths)))
        }
    }

    #[cfg(test)]
    fn path_count(&self) -> usize {
        self.0.len()
    }

    pub fn map_formals(
        &self,
        formal_histories: &std::collections::BTreeMap<Pvar, ValueHistory>,
    ) -> Self {
        self.map_formals_with_callsite(formal_histories, None)
    }

    pub fn map_formals_with_callsite(
        &self,
        formal_histories: &std::collections::BTreeMap<Pvar, ValueHistory>,
        callsite: Option<Location>,
    ) -> Self {
        let mut translated = BTreeSet::new();
        for path in self.0.iter() {
            let mut partials = vec![HistoryPath::empty()];
            for event in &path.0 {
                match event {
                    HistoryEvent::FormalArgument(pvar, formal_loc) => {
                        if let Some(history) = formal_histories.get(pvar) {
                            let mut next = Vec::new();
                            for prefix in &partials {
                                for suffix in history.0.iter() {
                                    let mut events = prefix.0.clone();
                                    events.push(HistoryEvent::ActualArgument(
                                        pvar.clone(),
                                        callsite.clone().or_else(|| formal_loc.clone()),
                                    ));
                                    events.extend(suffix.0.clone());
                                    next.push(HistoryPath::new_capped(events));
                                }
                            }
                            partials = next;
                        } else {
                            for partial in &mut partials {
                                partial.0.push(event.clone());
                            }
                        }
                    }
                    _ => {
                        for partial in &mut partials {
                            partial.0.push(event.clone());
                        }
                    }
                }
            }
            translated.extend(partials.into_iter().map(Arc::new));
        }
        Self(Arc::new(translated))
    }

    pub fn first_invalidation_before_call(&self) -> Option<(&Invalidation, &Location)> {
        self.0
            .iter()
            .find_map(|path| path.first_invalidation_before_call())
    }

    pub fn caller_argument_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        self.0
            .iter()
            .find_map(|path| path.caller_argument_invalidation())
    }

    pub fn first_call_before_invalidation(&self) -> Option<(&Procname, &Location)> {
        self.0
            .iter()
            .find_map(|path| path.first_call_before_invalidation())
    }

    pub fn last_call_before_invalidation(&self) -> Option<(&Procname, &Location)> {
        self.0
            .iter()
            .find_map(|path| path.last_call_before_invalidation())
    }

    pub fn has_call_at_location_before_invalidation(&self, location: &Location) -> bool {
        self.0
            .iter()
            .any(|path| path.has_call_at_location_before_invalidation(location))
    }

    pub fn first_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        self.0.iter().find_map(|path| path.first_invalidation())
    }

    pub fn contains_invalidation(&self, invalidation: &Invalidation) -> bool {
        self.0
            .iter()
            .any(|path| path.contains_invalidation(invalidation))
    }

    pub fn contains_invalidation_of_same_type(&self, invalidation: &Invalidation) -> bool {
        self.0
            .iter()
            .any(|path| path.contains_invalidation_of_same_type(invalidation))
    }

    pub fn contains_formal_origin(&self) -> bool {
        self.0.iter().any(|path| path.contains_formal_origin())
    }

    pub fn last_location(&self) -> Option<&Location> {
        self.0.iter().find_map(|path| path.last_location())
    }

    pub fn signature(&self) -> String {
        let parts: Vec<String> = self.0.iter().map(|path| path.to_string()).collect();
        parts.join(" || ")
    }

    pub(crate) fn primary_path(&self) -> Option<&HistoryPath> {
        self.0.iter().next().map(Arc::as_ref)
    }

    pub(crate) fn first_actual_argument(&self) -> Option<&Pvar> {
        self.primary_path()?
            .events()
            .iter()
            .find_map(|event| match event {
                HistoryEvent::ActualArgument(pvar, _) => Some(pvar),
                _ => None,
            })
    }

    pub(crate) fn first_formal_argument(&self) -> Option<&Pvar> {
        self.primary_path()?
            .events()
            .iter()
            .find_map(|event| match event {
                HistoryEvent::FormalArgument(pvar, _) => Some(pvar),
                _ => None,
            })
    }
}

impl fmt::Display for ValueHistory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.signature())
    }
}

/// Pair an abstract value with its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueWithHistory {
    pub addr: AbstractValue,
    pub history: ValueHistory,
}

impl ValueWithHistory {
    pub fn new(addr: AbstractValue, history: ValueHistory) -> Self {
        Self { addr, history }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::source_file::SourceFile;

    use super::{
        HistoryEvent, ValueHistory, MAX_HISTORY_EVENTS_PER_PATH, MAX_MERGED_HISTORY_PATHS,
    };
    use crate::invalidation::Invalidation;

    fn loc(line: i32) -> Location {
        Location {
            file: SourceFile::new("test.c"),
            line,
            col: 0,
            macro_file_opt: None,
            macro_line: -1,
        }
    }

    #[test]
    fn test_merge_caps_path_count_and_keeps_invalid_history() {
        let mut merged = ValueHistory::assignment(loc(1));
        for line in 2..24 {
            merged = merged.merge_owned(&ValueHistory::assignment(loc(line)));
        }
        let invalidation = Invalidation::ConstantDereference(sil::int_lit::IntLit::zero());
        merged = merged.merge_owned(&ValueHistory::invalidated(invalidation, loc(99)));

        assert_eq!(merged.path_count(), MAX_MERGED_HISTORY_PATHS);
        assert!(merged.signature().contains("test.c:99:0"));
    }

    #[test]
    fn test_append_caps_path_length_and_keeps_diagnostic_events() {
        let invalidation = Invalidation::ConstantDereference(sil::int_lit::IntLit::zero());
        let mut history = ValueHistory::invalidated(invalidation, loc(1));
        for line in 2..40 {
            history = history.append_assignment(loc(line));
        }
        let path = history.primary_path().expect("history should have a path");

        assert_eq!(path.events().len(), MAX_HISTORY_EVENTS_PER_PATH);
        assert!(history.signature().contains("test.c:1:0"));
        assert!(history.signature().contains("test.c:39:0"));
    }

    #[test]
    fn test_map_formals_prefers_callsite_for_actual_argument_location() {
        let callee = Procname::c_from_string("callee");
        let formal = Pvar::mk(Mangled::from_string("x"), callee);
        let formal_decl = loc(3);
        let callsite = loc(20);
        let caller_assignment = loc(18);

        let history = ValueHistory::from_event(HistoryEvent::FormalArgument(
            formal.clone(),
            Some(formal_decl),
        ));
        let formal_histories = BTreeMap::from([(
            formal.clone(),
            ValueHistory::assignment(caller_assignment.clone()),
        )]);

        let mapped = history.map_formals_with_callsite(&formal_histories, Some(callsite.clone()));

        assert_eq!(
            mapped.signature(),
            format!("actual({formal}) -> assign@{caller_assignment}"),
            "caller diagnostics should point the actual-argument step at the callsite, not the callee formal declaration"
        );
        let path = mapped
            .primary_path()
            .expect("mapped history should be non-empty");
        assert!(matches!(
            path.events().first(),
            Some(HistoryEvent::ActualArgument(_, Some(location))) if location == &callsite
        ));
    }
}
