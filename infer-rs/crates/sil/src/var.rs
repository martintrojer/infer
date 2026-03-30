// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ident::Ident;
use crate::pvar::Pvar;

/// Single abstraction for all the kinds of variables in SIL.
///
/// Mirrors OCaml's `Var.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Var {
    /// Logical (temporary) variable.
    LogicalVar(Ident),
    /// Program variable.
    ProgramVar(Box<Pvar>),
}

impl Var {
    pub fn of_id(id: Ident) -> Self {
        Var::LogicalVar(id)
    }

    pub fn of_pvar(pvar: Pvar) -> Self {
        Var::ProgramVar(Box::new(pvar))
    }

    pub fn is_global(&self) -> bool {
        matches!(self, Var::ProgramVar(pv) if pv.is_global())
    }

    pub fn is_return(&self) -> bool {
        matches!(self, Var::ProgramVar(pv) if pv.is_return())
    }

    pub fn get_ident(&self) -> Option<&Ident> {
        match self {
            Var::LogicalVar(id) => Some(id),
            _ => None,
        }
    }

    pub fn get_pvar(&self) -> Option<&Pvar> {
        match self {
            Var::ProgramVar(pv) => Some(pv),
            _ => None,
        }
    }
}

impl fmt::Display for Var {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Var::LogicalVar(id) => write!(f, "{id}"),
            Var::ProgramVar(pv) => write!(f, "{pv}"),
        }
    }
}
