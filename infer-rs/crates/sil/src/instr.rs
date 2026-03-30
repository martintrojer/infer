// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::call_flags::CallFlags;
use crate::exp::Exp;
use crate::ident::Ident;
use crate::location::Location;
use crate::pvar::Pvar;
use crate::typ::Typ;
use crate::var::Var;

/// Kind of prune instruction.
///
/// Mirrors OCaml's `Sil.if_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IfKind {
    /// Boolean expressions, and `exp ? exp : exp`.
    Bexp,
    /// Used in atomic compare exchange expressions.
    CompExch,
    DoWhile,
    For,
    If,
    /// Obtained from translation of `&&` or `||`.
    LandLor,
    While,
    Switch,
}

/// Instruction metadata -- hints about the program that are not strictly needed
/// to understand its semantics.
///
/// Mirrors OCaml's `Sil.instr_metadata`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstrMetadata {
    /// A good place to apply abstraction (biabduction).
    Abstract(Location),
    /// Entry of C++ catch blocks.
    CatchEntry { try_id: i32, loc: Location },
    /// Remove temporaries and dead program variables.
    ExitScope(Vec<Var>, Location),
    /// Nullify stack variable.
    Nullify(Pvar, Location),
    /// Next node is the loop header of the current loop.
    LoopBackEdge { header_id: i32 },
    /// Next node is the loop header of a nested loop.
    LoopEntry { header_id: i32 },
    /// Reaching the current node requires exiting this loop header.
    LoopExit { header_id: i32 },
    /// No-op.
    Skip,
    /// Entry of C++ try block.
    TryEntry { try_id: i32, loc: Location },
    /// Exit of C++ try block.
    TryExit { try_id: i32, loc: Location },
    /// Stack variable declared.
    VariableLifetimeBegins {
        pvar: Pvar,
        typ: Typ,
        loc: Location,
        is_cpp_structured_binding: bool,
    },
}

/// A SIL instruction.
///
/// Mirrors OCaml's `Sil.instr`. SIL has exactly 5 instruction kinds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Instr {
    /// Load a value from the heap into an identifier.
    /// `id = *e:typ`
    Load {
        id: Ident,
        e: Exp,
        typ: Typ,
        loc: Location,
    },
    /// Store the value of an expression into the heap.
    /// `*e1:typ = e2`
    Store {
        e1: Box<Exp>,
        typ: Typ,
        e2: Box<Exp>,
        loc: Location,
    },
    /// Prune the state if expression evaluates to zero.
    /// Used with CFG structure to encode control flow.
    Prune {
        exp: Exp,
        loc: Location,
        is_then_branch: bool,
        if_kind: IfKind,
    },
    /// Function call: `ret_id = e_fun(args)`.
    Call {
        ret: (Ident, Typ),
        fun_exp: Exp,
        args: Vec<(Exp, Typ)>,
        loc: Location,
        flags: CallFlags,
    },
    /// Metadata: hints about the program.
    Metadata(InstrMetadata),
}

impl Instr {
    /// Get the location of the instruction.
    pub fn location(&self) -> &Location {
        match self {
            Instr::Load { loc, .. }
            | Instr::Store { loc, .. }
            | Instr::Prune { loc, .. }
            | Instr::Call { loc, .. } => loc,
            Instr::Metadata(md) => match md {
                InstrMetadata::Abstract(loc) => loc,
                InstrMetadata::CatchEntry { loc, .. } => loc,
                InstrMetadata::ExitScope(_, loc) => loc,
                InstrMetadata::Nullify(_, loc) => loc,
                InstrMetadata::TryEntry { loc, .. } => loc,
                InstrMetadata::TryExit { loc, .. } => loc,
                InstrMetadata::VariableLifetimeBegins { loc, .. } => loc,
                InstrMetadata::LoopBackEdge { .. }
                | InstrMetadata::LoopEntry { .. }
                | InstrMetadata::LoopExit { .. }
                | InstrMetadata::Skip => &DUMMY_LOCATION,
            },
        }
    }

    /// Create a Skip metadata instruction.
    pub fn skip() -> Self {
        Instr::Metadata(InstrMetadata::Skip)
    }

    /// Check if this instruction is auxiliary (metadata) rather than semantic.
    pub fn is_auxiliary(&self) -> bool {
        matches!(self, Instr::Metadata(_))
    }
}

/// A static dummy location for metadata instructions that lack one.
static DUMMY_LOCATION: std::sync::LazyLock<Location> = std::sync::LazyLock::new(Location::dummy);

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Instr::Load { id, e, typ, .. } => write!(f, "{id} = *{e}:{typ}"),
            Instr::Store { e1, typ, e2, .. } => write!(f, "*{e1}:{typ} = {e2}"),
            Instr::Prune { exp, .. } => write!(f, "prune({exp})"),
            Instr::Call {
                ret: (id, _),
                fun_exp,
                args,
                ..
            } => {
                write!(f, "{id} = {fun_exp}(")?;
                for (i, (arg, _)) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
            Instr::Metadata(md) => write!(f, "{md:?}"),
        }
    }
}
