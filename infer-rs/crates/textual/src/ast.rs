// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Textual AST types.
//!
//! These mirror the OCaml types in `Textual.mli`. Textual types are simpler
//! than SIL types -- they omit integer widths, float kinds, pointer kinds, etc.

use std::collections::BTreeSet;
use std::fmt;

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

// ---- Location ----

/// Source location in a Textual file.
///
/// Mirrors OCaml's `Textual.Location.t`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Location {
    Known {
        line: usize,
        col: usize,
    },
    #[default]
    Unknown,
}

impl Location {
    pub fn known(line: usize, col: usize) -> Self {
        Location::Known { line, col }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Known { line, col } => write!(f, "{}:{}", line, col),
            Location::Unknown => write!(f, "<unknown>"),
        }
    }
}

// ---- Names ----

/// A named identifier with source location.
///
/// Used for ProcName, VarName, FieldName, NodeName, BaseTypeName.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Name {
    pub value: String,
    pub loc: Location,
}

impl Name {
    pub fn new(value: impl Into<String>, loc: Location) -> Self {
        Self {
            value: value.into(),
            loc,
        }
    }

    pub fn plain(value: &str) -> Self {
        Self {
            value: value.to_string(),
            loc: Location::Unknown,
        }
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

// Type aliases for different name kinds.
pub type ProcName = Name;
pub type VarName = Name;
pub type FieldName = Name;
pub type NodeName = Name;

// ---- TypeName ----

/// Type name with optional generic arguments.
///
/// Mirrors OCaml's `Textual.TypeName.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TypeName {
    pub name: Name,
    pub args: Vec<TypeName>,
}

impl TypeName {
    pub fn new(value: impl Into<String>, loc: Location) -> Self {
        Self {
            name: Name::new(value, loc),
            args: Vec::new(),
        }
    }

    pub fn plain(value: &str) -> Self {
        Self {
            name: Name::plain(value),
            args: Vec::new(),
        }
    }

    pub fn with_args(name: Name, args: Vec<TypeName>) -> Self {
        Self { name, args }
    }
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)?;
        if !self.args.is_empty() {
            write!(f, "<")?;
            for (i, arg) in self.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", arg)?;
            }
            write!(f, ">")?;
        }
        Ok(())
    }
}

// ---- Enclosing class ----

/// Enclosing class for a qualified procedure name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EnclosingClass {
    TopLevel,
    Enclosing(TypeName),
}

// ---- QualifiedProcName ----

/// Qualified procedure name: optional enclosing class + name.
///
/// Mirrors OCaml's `Textual.QualifiedProcName.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedProcName {
    pub enclosing_class: EnclosingClass,
    pub name: ProcName,
}

impl QualifiedProcName {
    pub fn top_level(name: ProcName) -> Self {
        Self {
            enclosing_class: EnclosingClass::TopLevel,
            name,
        }
    }

    pub fn with_class(class: TypeName, name: ProcName) -> Self {
        Self {
            enclosing_class: EnclosingClass::Enclosing(class),
            name,
        }
    }
}

impl fmt::Display for QualifiedProcName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.enclosing_class {
            EnclosingClass::TopLevel => write!(f, "{}", self.name),
            EnclosingClass::Enclosing(class) => write!(f, "{}.{}", class, self.name),
        }
    }
}

// ---- Qualified field name ----

/// A field name with its enclosing class.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QualifiedFieldName {
    pub enclosing_class: TypeName,
    pub name: FieldName,
}

impl fmt::Display for QualifiedFieldName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.enclosing_class, self.name)
    }
}

// ---- Attr ----

/// An attribute (e.g., `.source_language = "java"`).
///
/// Mirrors OCaml's `Textual.Attr.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Attr {
    pub name: String,
    pub values: Vec<String>,
    pub loc: Location,
}

impl Attr {
    pub fn new(name: impl Into<String>, values: Vec<String>, loc: Location) -> Self {
        Self {
            name: name.into(),
            values,
            loc,
        }
    }
}

// ---- Typ ----

/// Textual types.
///
/// Mirrors OCaml's `Textual.Typ.t`. Simpler than SIL types.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Typ {
    Int,
    Float,
    Null,
    Void,
    Fun(Option<FunctionPrototype>),
    Ptr(Box<Typ>, Vec<Attr>),
    Struct(TypeName),
    Array(Box<Typ>),
}

/// Function prototype: parameter types and return type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FunctionPrototype {
    pub params_type: Vec<Typ>,
    pub return_type: Box<Typ>,
}

/// Type with annotations.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotatedTyp {
    pub typ: Typ,
    pub attributes: Vec<Attr>,
}

impl AnnotatedTyp {
    pub fn without_attrs(typ: Typ) -> Self {
        Self {
            typ,
            attributes: Vec::new(),
        }
    }
}

impl fmt::Display for Typ {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Typ::Int => write!(f, "int"),
            Typ::Float => write!(f, "float"),
            Typ::Null => write!(f, "null"),
            Typ::Void => write!(f, "void"),
            Typ::Fun(None) => write!(f, "(fun _ -> _)"),
            Typ::Fun(Some(proto)) => {
                write!(f, "(fun ")?;
                for (i, t) in proto.params_type.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, ") -> {}", proto.return_type)?;
                write!(f, ")")
            }
            Typ::Ptr(t, _) => write!(f, "*{}", t),
            Typ::Struct(name) => write!(f, "{}", name),
            Typ::Array(t) => write!(f, "{}[]", t),
        }
    }
}

// ---- Ident ----

/// Textual identifier (SSA variable), represented as `nN` (e.g., `n0`, `n1`).
pub type Ident = i32;

// ---- Const ----

/// Constants in Textual.
///
/// Mirrors OCaml's `Textual.Const.t`.
#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(BigInt),
    Null,
    Str(String),
    Float(f64),
}

impl Eq for Const {}

impl std::hash::Hash for Const {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
        match self {
            Const::Int(i) => i.hash(state),
            Const::Null => {}
            Const::Str(s) => s.hash(state),
            Const::Float(f) => f.to_bits().hash(state),
        }
    }
}

impl Serialize for Const {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStructVariant;
        match self {
            Const::Int(i) => {
                let mut sv = serializer.serialize_struct_variant("Const", 0, "Int", 1)?;
                sv.serialize_field("value", &i.to_string())?;
                sv.end()
            }
            Const::Null => serializer.serialize_unit_variant("Const", 1, "Null"),
            Const::Str(s) => serializer.serialize_newtype_variant("Const", 2, "Str", s),
            Const::Float(f) => serializer.serialize_newtype_variant("Const", 3, "Float", f),
        }
    }
}

impl<'de> Deserialize<'de> for Const {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Simplified: use an untagged approach
        #[derive(Deserialize)]
        #[serde(tag = "type")]
        enum ConstHelper {
            Int { value: String },
            Null,
            Str(String),
            Float(f64),
        }
        let helper = ConstHelper::deserialize(deserializer)?;
        match helper {
            ConstHelper::Int { value } => {
                let i = value.parse::<BigInt>().map_err(serde::de::Error::custom)?;
                Ok(Const::Int(i))
            }
            ConstHelper::Null => Ok(Const::Null),
            ConstHelper::Str(s) => Ok(Const::Str(s)),
            ConstHelper::Float(f) => Ok(Const::Float(f)),
        }
    }
}

// ---- BoolExp ----

/// Boolean expressions (used in terminators).
///
/// Mirrors OCaml's `Textual.BoolExp.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BoolExp {
    Exp(Box<Exp>),
    Not(Box<BoolExp>),
    And(Box<BoolExp>, Box<BoolExp>),
    Or(Box<BoolExp>, Box<BoolExp>),
}

// ---- Exp ----

/// Call kind: virtual or non-virtual.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallKind {
    Virtual,
    NonVirtual,
}

/// Textual expressions.
///
/// Mirrors OCaml's `Textual.Exp.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Exp {
    /// SSA variable: `nN`.
    Var(Ident),
    /// Heap load: `[exp : typ]` or `[exp]`.
    Load { exp: Box<Exp>, typ: Option<Typ> },
    /// Address of a program variable: `&name`.
    Lvar(VarName),
    /// Field access: `exp.class.field`.
    Field {
        exp: Box<Exp>,
        field: QualifiedFieldName,
    },
    /// Array index: `exp1[exp2]`.
    Index(Box<Exp>, Box<Exp>),
    /// Constant.
    Const(Const),
    /// Conditional expression: `(if cond then e1 else e2)`.
    If {
        cond: BoolExp,
        then_: Box<Exp>,
        else_: Box<Exp>,
    },
    /// Function call: `proc(args)`.
    Call {
        proc: QualifiedProcName,
        args: Vec<Exp>,
        kind: CallKind,
    },
    /// Closure: `fun params -> proc(captured, params)`.
    Closure {
        proc: QualifiedProcName,
        captured: Vec<Exp>,
        params: Vec<VarName>,
        attributes: Vec<Attr>,
    },
    /// Closure application: `closure(args)`.
    Apply { closure: Box<Exp>, args: Vec<Exp> },
    /// Type expression: `<typ>`.
    Typ(Typ),
}

impl Exp {
    pub fn logical_not(exp: Exp) -> Exp {
        Exp::Call {
            proc: QualifiedProcName::top_level(ProcName::plain("__sil_lnot")),
            args: vec![exp],
            kind: CallKind::NonVirtual,
        }
    }

    pub fn call_virtual(proc: QualifiedProcName, recv: Exp, args: Vec<Exp>) -> Exp {
        let mut all_args = vec![recv];
        all_args.extend(args);
        Exp::Call {
            proc,
            args: all_args,
            kind: CallKind::Virtual,
        }
    }
}

// ---- Instr ----

/// Textual instructions.
///
/// Mirrors OCaml's `Textual.Instr.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Instr {
    /// `id : typ = load exp` or `id = load exp`.
    Load {
        id: Ident,
        exp: Exp,
        typ: Option<Typ>,
        loc: Location,
    },
    /// `store exp1 <- exp2 : typ` or `store exp1 <- exp2`.
    Store {
        exp1: Exp,
        typ: Option<Typ>,
        exp2: Exp,
        loc: Location,
    },
    /// `prune exp` or `prune ! exp`.
    Prune { exp: Exp, loc: Location },
    /// `id = exp` (let-binding, including calls).
    Let {
        id: Option<Ident>,
        exp: Exp,
        loc: Location,
    },
}

// ---- Terminator ----

/// A node call in a jump terminator.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeCall {
    pub label: NodeName,
    pub ssa_args: Vec<Exp>,
}

/// Textual terminators.
///
/// Mirrors OCaml's `Textual.Terminator.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Terminator {
    If {
        bexp: BoolExp,
        then_: Box<Terminator>,
        else_: Box<Terminator>,
    },
    Ret(Exp),
    Jump(Vec<NodeCall>),
    Throw(Exp),
    Unreachable,
}

// ---- Node ----

/// A basic block in a Textual procedure.
///
/// Mirrors OCaml's `Textual.Node.t`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub label: NodeName,
    pub ssa_parameters: Vec<(Ident, Typ)>,
    /// Exception handler successors. Set semantics: no duplicate handlers.
    pub exn_succs: BTreeSet<NodeName>,
    pub last: Terminator,
    pub instrs: Vec<Instr>,
    pub last_loc: Location,
    pub label_loc: Location,
}

// ---- Global ----

/// A global variable declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Global {
    pub name: VarName,
    pub typ: Typ,
    pub attributes: Vec<Attr>,
}

// ---- FieldDecl ----

/// A field declaration in a struct.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDecl {
    pub qualified_name: QualifiedFieldName,
    pub typ: Typ,
    pub attributes: Vec<Attr>,
}

// ---- Struct ----

/// A struct type definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Struct {
    pub name: TypeName,
    /// Supertypes. Set semantics: no duplicates, `contains()` for hierarchy checks.
    pub supers: BTreeSet<TypeName>,
    pub fields: Vec<FieldDecl>,
    pub attributes: Vec<Attr>,
}

// ---- ProcDecl ----

/// A procedure declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcDecl {
    pub qualified_name: QualifiedProcName,
    /// `None` means unknown formals (ellipsis `...` syntax).
    pub formals_types: Option<Vec<AnnotatedTyp>>,
    pub result_type: AnnotatedTyp,
    pub attributes: Vec<Attr>,
}

// ---- ProcDesc ----

/// A procedure definition (declaration + body).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcDesc {
    pub procdecl: ProcDecl,
    pub nodes: Vec<Node>,
    pub start: NodeName,
    pub params: Vec<VarName>,
    pub locals: Vec<(VarName, AnnotatedTyp)>,
    pub exit_loc: Location,
}

// ---- Module ----

/// A top-level declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decl {
    Global(Global),
    Struct(Struct),
    Procdecl(ProcDecl),
    Proc(ProcDesc),
}

/// A complete Textual module (compilation unit).
///
/// Mirrors OCaml's `Textual.Module.t`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub attrs: Vec<Attr>,
    pub decls: Vec<Decl>,
    pub source_file: String,
}

impl Module {
    /// Get the source language from module attributes.
    pub fn lang(&self) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name == "source_language")
            .and_then(|a| a.values.first())
            .map(|s| s.as_str())
    }
}
