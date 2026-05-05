// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::typ::IKind;

/// Binary operations.
///
/// Mirrors OCaml's `Binop.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Binop {
    /// Arithmetic +.
    PlusA(Option<IKind>),
    /// Pointer + integer.
    PlusPI,
    /// Arithmetic -.
    MinusA(Option<IKind>),
    /// Pointer - integer.
    MinusPI,
    /// Pointer - pointer.
    MinusPP,
    /// Multiplication.
    Mult(Option<IKind>),
    /// Integer division.
    DivI,
    /// Float division.
    DivF,
    /// Modulo.
    Mod,
    /// Shift left.
    Shiftlt,
    /// Shift right.
    Shiftrt,
    /// < (arithmetic comparison).
    Lt,
    /// > (arithmetic comparison).
    Gt,
    /// <= (arithmetic comparison).
    Le,
    /// >= (arithmetic comparison).
    Ge,
    /// == (arithmetic comparison).
    Eq,
    /// != (arithmetic comparison).
    Ne,
    /// Bitwise and.
    BAnd,
    /// Exclusive-or.
    BXor,
    /// Inclusive-or.
    BOr,
    /// Logical and. Does not always evaluate both operands.
    LAnd,
    /// Logical or. Does not always evaluate both operands.
    LOr,
}

impl fmt::Display for Binop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Binop::PlusA(_) | Binop::PlusPI => write!(f, "+"),
            Binop::MinusA(_) | Binop::MinusPI | Binop::MinusPP => write!(f, "-"),
            Binop::Mult(_) => write!(f, "*"),
            Binop::DivI | Binop::DivF => write!(f, "/"),
            Binop::Mod => write!(f, "%"),
            Binop::Shiftlt => write!(f, "<<"),
            Binop::Shiftrt => write!(f, ">>"),
            Binop::Lt => write!(f, "<"),
            Binop::Gt => write!(f, ">"),
            Binop::Le => write!(f, "<="),
            Binop::Ge => write!(f, ">="),
            Binop::Eq => write!(f, "=="),
            Binop::Ne => write!(f, "!="),
            Binop::BAnd => write!(f, "&"),
            Binop::BXor => write!(f, "^"),
            Binop::BOr => write!(f, "|"),
            Binop::LAnd => write!(f, "&&"),
            Binop::LOr => write!(f, "||"),
        }
    }
}
