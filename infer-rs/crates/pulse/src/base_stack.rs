// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Stack domain: maps variables to their abstract addresses.
//!
//! Mirrors OCaml's `PulseBaseStack.ml`.

use std::collections::HashMap;
use std::fmt;

use sil::var::Var;

use crate::abstract_value::AbstractValue;

/// The stack: maps program/logical variables to their abstract addresses.
///
/// In OCaml this also carries `ValueOrigin` for provenance tracking.
/// We simplify to just the address for now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseStack {
    map: HashMap<Var, AbstractValue>,
}

impl BaseStack {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Look up a variable's abstract address.
    pub fn find(&self, var: &Var) -> Option<AbstractValue> {
        self.map.get(var).copied()
    }

    /// Bind a variable to an abstract address.
    pub fn add(&mut self, var: Var, addr: AbstractValue) {
        self.map.insert(var, addr);
    }

    /// Remove a variable binding.
    pub fn remove(&mut self, var: &Var) {
        self.map.remove(var);
    }

    /// Iterate over all bindings.
    pub fn iter(&self) -> impl Iterator<Item = (&Var, &AbstractValue)> {
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
        for addr in self.map.values_mut() {
            if *addr == old {
                *addr = new;
            }
        }
    }
}

impl fmt::Display for BaseStack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries: Vec<_> = self.map.iter().collect();
        entries.sort_by_key(|(var, _)| format!("{var}"));
        for (i, (var, addr)) in entries.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "&{var}={addr}")?;
        }
        Ok(())
    }
}
