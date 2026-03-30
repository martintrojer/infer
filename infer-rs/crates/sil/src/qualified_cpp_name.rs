// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Qualified C++ name, represented as a list of name components.
///
/// For example, `std::vector::push_back` is represented as `["std", "vector", "push_back"]`.
///
/// Mirrors OCaml's `QualifiedCppName.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QualifiedCppName {
    pub parts: Vec<String>,
}

impl QualifiedCppName {
    pub fn from_parts(parts: Vec<String>) -> Self {
        Self { parts }
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        let s = s.into();
        Self {
            parts: s.split("::").map(String::from).collect(),
        }
    }

    pub fn last(&self) -> Option<&str> {
        self.parts.last().map(|s| s.as_str())
    }
}

impl fmt::Display for QualifiedCppName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.parts.join("::"))
    }
}
