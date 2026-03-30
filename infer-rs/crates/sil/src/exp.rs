// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::binop::Binop;
use crate::captured_var::CapturedVar;
use crate::const_val::Const;
use crate::fieldname::Fieldname;
use crate::ident::Ident;
use crate::int_lit::IntLit;
use crate::procname::Procname;
use crate::pvar::Pvar;
use crate::typ::{Typ, TypeDesc};
use crate::unop::Unop;

/// Closure value.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Closure {
    pub name: Procname,
    pub captured_vars: Vec<(Exp, CapturedVar)>,
}

/// Data for `Sizeof` expressions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SizeofData {
    pub typ: Typ,
    pub nbytes: Option<i32>,
    pub dynamic_length: Option<Box<Exp>>,
    pub nullable: bool,
}

/// Data for `Lfield` expressions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LfieldObjData {
    pub exp: Box<Exp>,
    pub is_implicit: bool,
}

/// Program expressions.
///
/// Mirrors OCaml's `Exp.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exp {
    /// Pure variable: it is not an lvalue.
    Var(Ident),
    /// Unary operator with optional result type.
    UnOp(Unop, Box<Exp>, Option<Typ>),
    /// Binary operator.
    BinOp(Binop, Box<Exp>, Box<Exp>),
    /// Exception.
    Exn(Box<Exp>),
    /// Anonymous function (closure).
    Closure(Closure),
    /// Constant.
    Const(Const),
    /// Type cast.
    Cast(Typ, Box<Exp>),
    /// The address of a program variable.
    Lvar(Pvar),
    /// A field offset. The type is the surrounding struct type.
    Lfield(LfieldObjData, Fieldname, Typ),
    /// An array index offset: `exp1[exp2]`.
    Lindex(Box<Exp>, Box<Exp>),
    /// Sizeof expression.
    Sizeof(SizeofData),
}

impl Exp {
    pub fn zero() -> Self {
        Exp::Const(Const::Cint(IntLit::zero()))
    }

    pub fn one() -> Self {
        Exp::Const(Const::Cint(IntLit::one()))
    }

    pub fn null() -> Self {
        Exp::Const(Const::Cint(IntLit::null()))
    }

    pub fn int(v: IntLit) -> Self {
        Exp::Const(Const::Cint(v))
    }

    pub fn bool(b: bool) -> Self {
        Exp::Const(Const::Cint(if b { IntLit::one() } else { IntLit::zero() }))
    }

    pub fn is_null_literal(&self) -> bool {
        matches!(self, Exp::Const(Const::Cint(i)) if i.is_null())
    }

    pub fn is_zero(&self) -> bool {
        matches!(self, Exp::Const(c) if c.is_zero())
    }

    pub fn is_const(&self) -> bool {
        matches!(self, Exp::Const(_))
    }

    /// Create expression `e1 == e2`.
    pub fn eq(e1: Exp, e2: Exp) -> Self {
        Exp::BinOp(Binop::Eq, Box::new(e1), Box::new(e2))
    }

    /// Create expression `e1 != e2`.
    pub fn ne(e1: Exp, e2: Exp) -> Self {
        Exp::BinOp(Binop::Ne, Box::new(e1), Box::new(e2))
    }

    /// Returns the zero value for a type, if applicable.
    pub fn zero_of_type(typ: &Typ) -> Option<Self> {
        match &*typ.desc {
            TypeDesc::Tint(_) => Some(Exp::zero()),
            TypeDesc::Tfloat(_) => Some(Exp::Const(Const::Cfloat(crate::const_val::OrderedFloat(
                0.0,
            )))),
            TypeDesc::Tptr(_, _) => Some(Exp::null()),
            _ => None,
        }
    }
}

impl fmt::Display for Exp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exp::Var(id) => write!(f, "{id}"),
            Exp::UnOp(op, e, _) => write!(f, "{op}({e})"),
            Exp::BinOp(op, e1, e2) => write!(f, "({e1} {op} {e2})"),
            Exp::Exn(e) => write!(f, "exn({e})"),
            Exp::Closure(c) => write!(f, "closure({})", c.name),
            Exp::Const(c) => write!(f, "{c}"),
            Exp::Cast(t, e) => write!(f, "({t}){e}"),
            Exp::Lvar(pv) => write!(f, "&{pv}"),
            Exp::Lfield(data, fld, _) => write!(f, "{}.{fld}", data.exp),
            Exp::Lindex(e1, e2) => write!(f, "{e1}[{e2}]"),
            Exp::Sizeof(data) => write!(f, "sizeof({})", data.typ),
        }
    }
}
