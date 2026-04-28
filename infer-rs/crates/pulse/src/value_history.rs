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

use serde::{Deserialize, Serialize};

use sil::location::Location;
use sil::procname::Procname;
use sil::pvar::Pvar;

use crate::abstract_value::AbstractValue;
use crate::invalidation::Invalidation;

/// One step in a value's provenance path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HistoryEvent {
    /// Formal placeholder to be replaced by the caller actual history.
    FormalArgument(Pvar),
    /// Marker that a substituted path came from a caller actual.
    ActualArgument(Pvar),
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
            Self::FormalArgument(_) | Self::ActualArgument(_) => None,
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

    pub(crate) fn events(&self) -> &[HistoryEvent] {
        &self.0
    }

    fn append(&self, event: HistoryEvent) -> Self {
        let mut events = self.0.clone();
        events.push(event);
        Self(events)
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
        Self(events)
    }

    fn first_invalidation_before_call(&self) -> Option<(&Invalidation, &Location)> {
        for event in &self.0 {
            match event {
                HistoryEvent::Invalidated {
                    invalidation,
                    location,
                } => return Some((invalidation, location)),
                HistoryEvent::Call { .. } => return None,
                HistoryEvent::ActualArgument(_) => {}
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
                HistoryEvent::ActualArgument(_) => saw_actual_argument = true,
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
                HistoryEvent::FormalArgument(_) | HistoryEvent::ActualArgument(_)
            )
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

impl fmt::Display for HistoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, event) in self.0.iter().enumerate() {
            if index > 0 {
                write!(f, " -> ")?;
            }
            match event {
                HistoryEvent::FormalArgument(pvar) => write!(f, "formal({pvar})")?,
                HistoryEvent::ActualArgument(pvar) => write!(f, "actual({pvar})")?,
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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ValueHistory(BTreeSet<HistoryPath>);

impl Default for ValueHistory {
    fn default() -> Self {
        Self::epoch()
    }
}

impl ValueHistory {
    pub fn epoch() -> Self {
        let mut paths = BTreeSet::new();
        paths.insert(HistoryPath::empty());
        Self(paths)
    }

    pub fn from_event(event: HistoryEvent) -> Self {
        let mut paths = BTreeSet::new();
        paths.insert(HistoryPath(vec![event]));
        Self(paths)
    }

    pub fn formal_argument(pvar: Pvar) -> Self {
        Self::from_event(HistoryEvent::FormalArgument(pvar))
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
            .map(|path| path.append(event.clone()))
            .collect();
        Self(paths)
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
            .map(|path| path.wrap_call(proc, location))
            .collect();
        Self(paths)
    }

    pub fn merge(&self, other: &Self) -> Self {
        let mut paths = self.0.clone();
        paths.extend(other.0.iter().cloned());
        Self(paths)
    }

    pub fn map_formals(
        &self,
        formal_histories: &std::collections::BTreeMap<Pvar, ValueHistory>,
    ) -> Self {
        let mut translated = BTreeSet::new();
        for path in &self.0 {
            let mut partials = vec![HistoryPath::empty()];
            for event in &path.0 {
                match event {
                    HistoryEvent::FormalArgument(pvar) => {
                        if let Some(history) = formal_histories.get(pvar) {
                            let mut next = Vec::new();
                            for prefix in &partials {
                                for suffix in &history.0 {
                                    let mut events = prefix.0.clone();
                                    events.push(HistoryEvent::ActualArgument(pvar.clone()));
                                    events.extend(suffix.0.clone());
                                    next.push(HistoryPath(events));
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
            translated.extend(partials);
        }
        Self(translated)
    }

    pub fn first_invalidation_before_call(&self) -> Option<(&Invalidation, &Location)> {
        self.0
            .iter()
            .find_map(HistoryPath::first_invalidation_before_call)
    }

    pub fn caller_argument_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        self.0
            .iter()
            .find_map(HistoryPath::caller_argument_invalidation)
    }

    pub fn first_call_before_invalidation(&self) -> Option<(&Procname, &Location)> {
        self.0
            .iter()
            .find_map(HistoryPath::first_call_before_invalidation)
    }

    pub fn has_call_at_location_before_invalidation(&self, location: &Location) -> bool {
        self.0
            .iter()
            .any(|path| path.has_call_at_location_before_invalidation(location))
    }

    pub fn first_invalidation(&self) -> Option<(&Invalidation, &Location)> {
        self.0.iter().find_map(HistoryPath::first_invalidation)
    }

    pub fn contains_invalidation_of_same_type(&self, invalidation: &Invalidation) -> bool {
        self.0
            .iter()
            .any(|path| path.contains_invalidation_of_same_type(invalidation))
    }

    pub fn contains_formal_origin(&self) -> bool {
        self.0.iter().any(HistoryPath::contains_formal_origin)
    }

    pub fn last_location(&self) -> Option<&Location> {
        self.0.iter().find_map(HistoryPath::last_location)
    }

    pub fn signature(&self) -> String {
        let parts: Vec<String> = self.0.iter().map(ToString::to_string).collect();
        parts.join(" || ")
    }

    pub(crate) fn primary_path(&self) -> Option<&HistoryPath> {
        self.0.iter().next()
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
