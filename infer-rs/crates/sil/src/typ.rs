// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::int_lit::IntLit;
use crate::mangled::Mangled;
use crate::qualified_cpp_name::QualifiedCppName;

// ---- Integer kinds ----

/// Kinds of integers.
///
/// Mirrors OCaml's `Typ.ikind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IKind {
    IChar,
    ISChar,
    IUChar,
    IBool,
    IInt,
    IUInt,
    IShort,
    IUShort,
    ILong,
    IULong,
    ILongLong,
    IULongLong,
    I128,
    IU128,
}

impl IKind {
    pub fn is_unsigned(&self) -> bool {
        matches!(
            self,
            IKind::IUChar
                | IKind::IUInt
                | IKind::IUShort
                | IKind::IULong
                | IKind::IULongLong
                | IKind::IU128
        )
    }

    pub fn is_char(&self) -> bool {
        matches!(self, IKind::IChar | IKind::ISChar | IKind::IUChar)
    }
}

// ---- Float kinds ----

/// Kinds of floating-point numbers.
///
/// Mirrors OCaml's `Typ.fkind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum FKind {
    FFloat,
    FDouble,
    FLongDouble,
}

// ---- Pointer kinds ----

/// Kind of pointer.
///
/// Mirrors OCaml's `Typ.ptr_kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PtrKind {
    /// C/C++, Java, ObjC standard/__strong pointer.
    Pointer,
    /// C++ lvalue reference.
    LvalueReference,
    /// C++ rvalue reference.
    RvalueReference,
    /// ObjC __weak pointer.
    ObjcWeak,
    /// ObjC __unsafe_unretained pointer.
    ObjcUnsafeUnretained,
    /// ObjC __autoreleasing pointer.
    ObjcAutoreleasing,
    /// ObjC block annotated with nullable.
    ObjcNullableBlock,
    /// ObjC block annotated with nonnull.
    ObjcNonnullBlock,
}

// ---- Type qualifiers ----

/// Type qualifiers (const, volatile, restrict, reference).
///
/// Mirrors OCaml's `Typ.type_quals`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TypeQuals {
    pub is_const: bool,
    pub is_reference: bool,
    pub is_restrict: bool,
    pub is_volatile: bool,
}

// ---- Template arguments ----

/// Template argument.
///
/// Mirrors OCaml's `Typ.template_arg`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TemplateArg {
    TType(Typ),
    TInt(i64),
    TNull,
    TNullPtr,
    TOpaque,
}

/// Template specialization info.
///
/// Mirrors OCaml's `Typ.template_spec_info`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TemplateSpecInfo {
    #[default]
    NoTemplate,
    Template {
        mangled: Option<String>,
        args: Vec<TemplateArg>,
    },
}

// ---- Type names ----

/// Language-specific class name types.
/// These mirror the various `*ClassName.t` types in OCaml.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JavaClassName(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CSharpClassName(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HackClassName(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PythonClassName(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ErlangTypeName(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SwiftClassName(pub String);

/// ObjC block signature.
///
/// Mirrors OCaml's `Typ.objc_block_sig`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ObjcBlockSig {
    pub class_name: Option<Box<TypeName>>,
    pub name: String,
    pub mangled: String,
}

/// C function signature.
///
/// Mirrors OCaml's `Typ.c_function_sig`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CFunctionSig {
    pub c_name: QualifiedCppName,
    pub c_mangled: Option<String>,
    pub c_template_args: TemplateSpecInfo,
}

/// Named types (struct, class, union, etc).
///
/// Mirrors OCaml's `Typ.name`. Covers all supported languages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TypeName {
    CStruct(QualifiedCppName),
    CUnion(QualifiedCppName),
    CppClass {
        name: QualifiedCppName,
        template_spec_info: TemplateSpecInfo,
        is_union: bool,
    },
    CSharpClass(CSharpClassName),
    ErlangType(ErlangTypeName),
    HackClass(HackClassName),
    JavaClass(JavaClassName),
    ObjcClass(QualifiedCppName),
    ObjcProtocol(QualifiedCppName),
    PythonClass(PythonClassName),
    SwiftClass(SwiftClassName),
    ObjcBlock(ObjcBlockSig),
    CFunction(CFunctionSig),
    SwiftClosure(Mangled),
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeName::CStruct(n) | TypeName::CUnion(n) => write!(f, "{n}"),
            TypeName::CppClass { name, .. } => write!(f, "{name}"),
            TypeName::CSharpClass(n) => write!(f, "{}", n.0),
            TypeName::ErlangType(n) => write!(f, "{}", n.0),
            TypeName::HackClass(n) => write!(f, "{}", n.0),
            TypeName::JavaClass(n) => write!(f, "{}", n.0),
            TypeName::ObjcClass(n) | TypeName::ObjcProtocol(n) => write!(f, "{n}"),
            TypeName::PythonClass(n) => write!(f, "{}", n.0),
            TypeName::SwiftClass(n) => write!(f, "{}", n.0),
            TypeName::ObjcBlock(sig) => write!(f, "{}", sig.name),
            TypeName::CFunction(sig) => write!(f, "{}", sig.c_name),
            TypeName::SwiftClosure(m) => write!(f, "{m}"),
        }
    }
}

// ---- Function prototype ----

/// Function prototype (parameter types and return type).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FunctionPrototype {
    pub params_type: Vec<Typ>,
    pub return_type: Box<Typ>,
}

// ---- Type descriptor ----

/// Type descriptor.
///
/// Mirrors OCaml's `Typ.desc`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TypeDesc {
    /// Integer type.
    Tint(IKind),
    /// Float type.
    Tfloat(FKind),
    /// Void type.
    Tvoid,
    /// Function type.
    Tfun(Option<Box<FunctionPrototype>>),
    /// Pointer type.
    Tptr(Box<Typ>, PtrKind),
    /// Structured value type name.
    Tstruct(TypeName),
    /// Type variable (C++ template variables).
    TVar(String),
    /// Array type with optional static length and stride.
    Tarray {
        elt: Box<Typ>,
        length: Option<IntLit>,
        stride: Option<IntLit>,
    },
}

// ---- Typ ----

/// Types for SIL expressions.
///
/// Mirrors OCaml's `Typ.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Typ {
    pub desc: Box<TypeDesc>,
    pub quals: TypeQuals,
}

impl Typ {
    pub fn mk(desc: TypeDesc) -> Self {
        Self {
            desc: Box::new(desc),
            quals: TypeQuals::default(),
        }
    }

    pub fn mk_with_quals(desc: TypeDesc, quals: TypeQuals) -> Self {
        Self {
            desc: Box::new(desc),
            quals,
        }
    }

    pub fn void() -> Self {
        Self::mk(TypeDesc::Tvoid)
    }

    pub fn int(ikind: IKind) -> Self {
        Self::mk(TypeDesc::Tint(ikind))
    }

    pub fn float(fkind: FKind) -> Self {
        Self::mk(TypeDesc::Tfloat(fkind))
    }

    pub fn mk_ptr(pointee: Typ) -> Self {
        Self::mk(TypeDesc::Tptr(Box::new(pointee), PtrKind::Pointer))
    }

    pub fn mk_struct(name: TypeName) -> Self {
        Self::mk(TypeDesc::Tstruct(name))
    }

    pub fn mk_array(elt: Typ, length: Option<IntLit>, stride: Option<IntLit>) -> Self {
        Self::mk(TypeDesc::Tarray {
            elt: Box::new(elt),
            length,
            stride,
        })
    }

    /// Size of this type in bytes, if statically known.
    ///
    /// Returns None for structs (need Tenv), void, and arrays without
    /// a known length. Uses typical 64-bit C ABI sizes.
    pub fn size_in_bytes(&self) -> Option<i64> {
        match &*self.desc {
            TypeDesc::Tint(ik) => Some(match ik {
                IKind::IChar | IKind::ISChar | IKind::IUChar | IKind::IBool => 1,
                IKind::IShort | IKind::IUShort => 2,
                IKind::IInt | IKind::IUInt => 4,
                IKind::ILong | IKind::IULong => 8,
                IKind::ILongLong | IKind::IULongLong => 8,
                IKind::I128 | IKind::IU128 => 16,
            }),
            TypeDesc::Tfloat(fk) => Some(match fk {
                FKind::FFloat => 4,
                FKind::FDouble | FKind::FLongDouble => 8,
            }),
            TypeDesc::Tptr(..) => Some(8), // 64-bit pointers
            TypeDesc::Tarray {
                elt,
                length,
                stride,
                ..
            } => {
                let elt_size = stride
                    .as_ref()
                    .and_then(|s| s.to_i64())
                    .or_else(|| elt.size_in_bytes())?;
                let len = length.as_ref()?.to_i64()?;
                Some(elt_size * len)
            }
            _ => None,
        }
    }

    pub fn is_void(&self) -> bool {
        matches!(*self.desc, TypeDesc::Tvoid)
    }

    pub fn is_pointer(&self) -> bool {
        matches!(*self.desc, TypeDesc::Tptr(..))
    }

    pub fn is_int(&self) -> bool {
        matches!(*self.desc, TypeDesc::Tint(_))
    }

    pub fn is_struct(&self) -> bool {
        matches!(*self.desc, TypeDesc::Tstruct(_))
    }

    pub fn name(&self) -> Option<&TypeName> {
        match &*self.desc {
            TypeDesc::Tstruct(name) => Some(name),
            _ => None,
        }
    }

    pub fn strip_ptr(&self) -> Option<&Typ> {
        match &*self.desc {
            TypeDesc::Tptr(t, _) => Some(t),
            _ => None,
        }
    }

    /// Whether this type is a pointer whose pointee is `const`-qualified.
    ///
    /// Mirrors OCaml's `Typ.is_ptr_to_const`.
    pub fn is_ptr_to_const(&self) -> bool {
        match &*self.desc {
            TypeDesc::Tptr(t, _) => t.quals.is_const,
            _ => false,
        }
    }
}

impl fmt::Display for Typ {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &*self.desc {
            TypeDesc::Tint(ik) => write!(f, "{ik:?}"),
            TypeDesc::Tfloat(fk) => write!(f, "{fk:?}"),
            TypeDesc::Tvoid => write!(f, "void"),
            TypeDesc::Tfun(_) => write!(f, "fun"),
            TypeDesc::Tptr(t, _) => write!(f, "*{t}"),
            TypeDesc::Tstruct(name) => write!(f, "{name}"),
            TypeDesc::TVar(s) => write!(f, "'{s}"),
            TypeDesc::Tarray { elt, length, .. } => match length {
                Some(len) => write!(f, "{elt}[{len}]"),
                None => write!(f, "{elt}[]"),
            },
        }
    }
}
