// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::qualified_cpp_name::QualifiedCppName;
use crate::typ::{
    CFunctionSig, HackClassName, JavaClassName, ObjcBlockSig, TemplateSpecInfo, Typ, TypeName,
};

// ---- Java procedure names ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum JavaKind {
    NonStatic,
    Static,
}

/// Java procedure name.
///
/// Mirrors OCaml's `Procname.Java.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JavaProcname {
    pub class_name: JavaClassName,
    pub method_name: String,
    pub parameters: Vec<Typ>,
    pub return_type: Option<Typ>,
    pub kind: JavaKind,
}

// ---- C# procedure names ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CSharpKind {
    NonStatic,
    Static,
}

/// C# procedure name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CSharpProcname {
    pub class_name: String,
    pub method_name: String,
    pub kind: CSharpKind,
}

// ---- ObjC/C++ procedure names ----

/// ObjC/C++ method kind.
///
/// Mirrors OCaml's `Procname.ObjC_Cpp.kind`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ObjcCppKind {
    CPPMethod(Option<String>),
    CPPConstructor(Option<String>),
    CPPDestructor(Option<String>),
    ObjCClassMethod,
    ObjCInstanceMethod,
}

/// Parameter type for Clang procedure names.
///
/// `Some(name)` means pointer to struct with that name, `None` means other type.
pub type ClangParameter = Option<TypeName>;

/// ObjC/C++ procedure name.
///
/// Mirrors OCaml's `Procname.ObjC_Cpp.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjcCppProcname {
    pub class_name: TypeName,
    pub kind: ObjcCppKind,
    pub method_name: String,
    pub parameters: Vec<ClangParameter>,
    pub template_args: TemplateSpecInfo,
}

// ---- Erlang procedure names ----

/// Erlang procedure name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ErlangProcname {
    pub module_name: String,
    pub function_name: String,
    pub arity: i32,
}

// ---- Hack procedure names ----

/// Hack procedure name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HackProcname {
    pub class_name: Option<HackClassName>,
    pub function_name: String,
    pub arity: Option<i32>,
}

// ---- Python procedure names ----

/// Python procedure name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PythonProcname {
    pub class_name: Option<String>,
    pub function_name: String,
    pub arity: Option<i32>,
}

// ---- Swift procedure names ----

/// Swift procedure name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SwiftProcname {
    pub class_name: Option<String>,
    pub method_name: String,
    pub mangled: Option<String>,
}

// ---- Top-level Procname ----

/// Procedure name covering all supported languages.
///
/// Mirrors OCaml's `Procname.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Procname {
    /// ObjC block.
    Block(ObjcBlockSig),
    /// C function.
    C(CFunctionSig),
    /// C# method.
    CSharp(CSharpProcname),
    /// Erlang function.
    Erlang(ErlangProcname),
    /// Hack function/method.
    Hack(HackProcname),
    /// Java method.
    Java(JavaProcname),
    /// ObjC/C++ method.
    ObjcCpp(ObjcCppProcname),
    /// Python function/method.
    Python(PythonProcname),
    /// Rust function (uses C function sig).
    Rust(CFunctionSig),
    /// Swift method.
    Swift(SwiftProcname),
}

impl Procname {
    pub fn get_method_name(&self) -> &str {
        match self {
            Procname::Block(sig) => &sig.name,
            Procname::C(sig) | Procname::Rust(sig) => sig.c_name.last().unwrap_or_default(),
            Procname::CSharp(p) => &p.method_name,
            Procname::Erlang(p) => &p.function_name,
            Procname::Hack(p) => &p.function_name,
            Procname::Java(p) => &p.method_name,
            Procname::ObjcCpp(p) => &p.method_name,
            Procname::Python(p) => &p.function_name,
            Procname::Swift(p) => &p.method_name,
        }
    }

    pub fn get_class_type_name(&self) -> Option<&TypeName> {
        match self {
            Procname::ObjcCpp(p) => Some(&p.class_name),
            _ => None,
        }
    }

    pub fn is_java(&self) -> bool {
        matches!(self, Procname::Java(_))
    }

    pub fn is_c(&self) -> bool {
        matches!(self, Procname::C(_))
    }

    /// Create a simple C function procname from a string.
    pub fn c_from_string(name: &str) -> Self {
        Procname::C(CFunctionSig {
            c_name: QualifiedCppName::from_string(name),
            c_mangled: None,
            c_template_args: TemplateSpecInfo::NoTemplate,
        })
    }
}

impl fmt::Display for Procname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Procname::Block(sig) => write!(f, "block:{}", sig.name),
            Procname::C(sig) | Procname::Rust(sig) => write!(f, "{}", sig.c_name),
            Procname::CSharp(p) => write!(f, "{}.{}", p.class_name, p.method_name),
            Procname::Erlang(p) => write!(f, "{}:{}/{}", p.module_name, p.function_name, p.arity),
            Procname::Hack(p) => {
                match &p.class_name {
                    Some(cn) => write!(f, "{}.{}", cn.0, p.function_name)?,
                    None => write!(f, "{}", p.function_name)?,
                }
                if let Some(arity) = p.arity {
                    write!(f, "#{arity}")?;
                }
                Ok(())
            }
            Procname::Java(p) => write!(f, "{}.{}", p.class_name.0, p.method_name),
            Procname::ObjcCpp(p) => write!(f, "{}::{}", p.class_name, p.method_name),
            Procname::Python(p) => {
                match &p.class_name {
                    Some(cn) => write!(f, "{cn}.{}", p.function_name)?,
                    None => write!(f, "{}", p.function_name)?,
                }
                if let Some(arity) = p.arity {
                    write!(f, "#{arity}")?;
                }
                Ok(())
            }
            Procname::Swift(p) => match &p.class_name {
                Some(cn) => write!(f, "{cn}.{}", p.method_name),
                None => write!(f, "{}", p.method_name),
            },
        }
    }
}
