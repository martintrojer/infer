// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Stack domain: maps variables to their abstract addresses.
//!
//! Mirrors OCaml's `PulseBaseStack.ml`.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use sil::var::Var;

use crate::abstract_value::AbstractValue;
use crate::value_history::{ValueHistory, ValueWithHistory};

/// The stack: maps program/logical variables to their abstract addresses.
///
/// In OCaml this also carries `ValueOrigin` for provenance tracking.
/// We simplify to just the address for now.
///
/// The inner `HashMap` is wrapped in `Arc` so cloning the surrounding
/// abductive state shares it between disjuncts and retained invariant
/// snapshots without deep-copying it. Mutating helpers go through
/// `Arc::make_mut` (clone-on-write) so the public `BaseStack` API is
/// unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseStack {
    map: Arc<HashMap<Var, ValueWithHistory>>,
}

impl BaseStack {
    pub fn empty() -> Self {
        Self::default()
    }

    fn map_mut(&mut self) -> &mut HashMap<Var, ValueWithHistory> {
        Arc::make_mut(&mut self.map)
    }

    /// Look up a variable's abstract address.
    pub fn find(&self, var: &Var) -> Option<AbstractValue> {
        self.find_with_history(var).map(|value| value.addr)
    }

    /// Look up a variable together with its provenance.
    pub fn find_with_history(&self, var: &Var) -> Option<&ValueWithHistory> {
        self.map.get(var)
    }

    /// Bind a variable to an abstract address.
    pub fn add(&mut self, var: Var, addr: AbstractValue) {
        self.add_with_history(var, ValueWithHistory::new(addr, ValueHistory::epoch()));
    }

    /// Bind a variable to an abstract value and provenance.
    pub fn add_with_history(&mut self, var: Var, value: ValueWithHistory) {
        self.map_mut().insert(var, value);
    }

    /// Remove a variable binding.
    pub fn remove(&mut self, var: &Var) {
        if self.map.contains_key(var) {
            self.map_mut().remove(var);
        }
    }

    /// Iterate over all bindings.
    pub fn iter(&self) -> impl Iterator<Item = (&Var, &AbstractValue)> {
        self.map.iter().map(|(var, value)| (var, &value.addr))
    }

    /// Iterate over all bindings with provenance.
    pub fn iter_with_history(&self) -> impl Iterator<Item = (&Var, &ValueWithHistory)> {
        self.map.iter()
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Substitute abstract values: replace `old` with `new` wherever it appears.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        if !self.map.values().any(|value| value.addr == old) {
            return;
        }
        for value in self.map_mut().values_mut() {
            if value.addr == old {
                value.addr = new;
            }
        }
    }
}

impl fmt::Display for BaseStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries: Vec<_> = self.map.iter().collect();
        entries.sort_by_key(|(var, _)| format!("{var}"));
        for (i, (var, value)) in entries.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "&{var}={}", value.addr)?;
        }
        Ok(())
    }
}
