// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ident::IdentName;
use crate::int_lit::IntLit;
use crate::procname::Procname;

/// Constants.
///
/// Mirrors OCaml's `Const.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Const {
    /// Integer constant.
    Cint(IntLit),
    /// Function name constant.
    Cfun(Procname),
    /// String constant.
    Cstr(String),
    /// Float constant.
    Cfloat(OrderedFloat),
    /// Class constant.
    Cclass(IdentName),
}

/// Float wrapper that implements Eq and Hash (by using bit representation).
///
/// Needed because `f64` doesn't implement `Eq` or `Hash`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrderedFloat(pub f64);

impl PartialEq for OrderedFloat {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for OrderedFloat {}

impl std::hash::Hash for OrderedFloat {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

impl Const {
    pub fn is_zero(&self) -> bool {
        match self {
            Const::Cint(i) => i.is_zero(),
            Const::Cfloat(f) => f.0 == 0.0,
            _ => false,
        }
    }

    pub fn is_one(&self) -> bool {
        match self {
            Const::Cint(i) => i.is_one(),
            Const::Cfloat(f) => f.0 == 1.0,
            _ => false,
        }
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Const::Cint(i) => write!(f, "{i}"),
            Const::Cfun(pn) => write!(f, "_{pn}_"),
            Const::Cstr(s) => write!(f, "\"{s}\""),
            Const::Cfloat(fl) => write!(f, "{}", fl.0),
            Const::Cclass(name) => write!(f, "{name}"),
        }
    }
}
