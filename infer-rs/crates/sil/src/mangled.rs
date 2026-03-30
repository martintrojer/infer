// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Mangled name: a plain name with an optional mangled suffix.
///
/// Mirrors OCaml's `Mangled.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Mangled {
    pub plain: String,
    pub mangled: Option<String>,
}

impl Mangled {
    pub fn new(plain: impl Into<String>, mangled: Option<String>) -> Self {
        Self {
            plain: plain.into(),
            mangled,
        }
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        Self {
            plain: s.into(),
            mangled: None,
        }
    }
}

impl fmt::Display for Mangled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mangled {
            Some(m) => write!(f, "{}({})", self.plain, m),
            None => write!(f, "{}", self.plain),
        }
    }
}
