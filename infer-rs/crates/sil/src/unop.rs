// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Unary operations.
///
/// Mirrors OCaml's `Unop.t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Unop {
    /// Unary minus.
    Neg,
    /// Bitwise complement (~).
    BNot,
    /// Logical Not (!).
    LNot,
}

impl fmt::Display for Unop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unop::Neg => write!(f, "-"),
            Unop::BNot => write!(f, "~"),
            Unop::LNot => write!(f, "!"),
        }
    }
}
