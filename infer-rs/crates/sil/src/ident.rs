// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Kind of identifiers.
///
/// Mirrors OCaml's `Ident.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IdentKind {
    /// Normal temporary variable.
    Normal,
    /// Primed variable (for abduction).
    Primed,
    /// Footprint variable.
    Footprint,
    /// No specific kind (placeholder).
    None,
}

/// Identifier name.
///
/// Mirrors OCaml's `Ident.name`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IdentName(pub String);

impl IdentName {
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for IdentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Program and logical variables (temporaries).
///
/// Mirrors OCaml's `Ident.t`. An identifier has a kind, a name, and a stamp
/// (unique integer) to distinguish identifiers with the same name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ident {
    pub kind: IdentKind,
    pub name: IdentName,
    pub stamp: i32,
}

impl Ident {
    pub fn new(kind: IdentKind, name: IdentName, stamp: i32) -> Self {
        Self { kind, name, stamp }
    }

    pub fn create_normal(name: IdentName, stamp: i32) -> Self {
        Self::new(IdentKind::Normal, name, stamp)
    }

    pub fn create_none() -> Self {
        Self::new(IdentKind::None, IdentName::from_string("_"), 0)
    }

    pub fn is_normal(&self) -> bool {
        self.kind == IdentKind::Normal
    }

    pub fn is_footprint(&self) -> bool {
        self.kind == IdentKind::Footprint
    }

    pub fn is_none(&self) -> bool {
        self.kind == IdentKind::None
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.kind {
            IdentKind::Normal => "n",
            IdentKind::Primed => "p",
            IdentKind::Footprint => "f",
            IdentKind::None => "_",
        };
        write!(f, "{}${}:{}", prefix, self.name, self.stamp)
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.stamp
            .cmp(&other.stamp)
            .then_with(|| self.name.cmp(&other.name))
    }
}
