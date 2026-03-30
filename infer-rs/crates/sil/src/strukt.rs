// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::annot::AnnotItem;
use crate::fieldname::Fieldname;
use crate::procname::Procname;
use crate::source_file::SourceFile;
use crate::typ::{Typ, TypeName};

/// A struct/class field.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Field {
    pub name: Fieldname,
    pub typ: Typ,
    pub annot: AnnotItem,
}

/// Language-specific class information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ClassInfo {
    #[default]
    NoInfo,
    CppClassInfo,
    JavaClassInfo {
        kind: JavaClassKind,
    },
    HackClassInfo {
        kind: HackClassKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JavaClassKind {
    AbstractClass,
    ConcreteClass,
    Interface,
    Enum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HackClassKind {
    Class,
    Interface,
    Trait,
    Enum,
    Abstract,
}

/// A method known to the type environment.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TenvMethod {
    pub proc_name: Procname,
    pub is_defined: bool,
}

/// Struct type definition.
///
/// Mirrors OCaml's `Struct.t`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Struct {
    /// Non-static fields.
    pub fields: Vec<Field>,
    /// Static fields.
    pub statics: Vec<Field>,
    /// Superclasses. Set semantics: no duplicates, `contains()` is a core operation.
    pub supers: BTreeSet<TypeName>,
    /// ObjC protocols. Set semantics: no duplicates.
    pub objc_protocols: BTreeSet<TypeName>,
    /// Defined methods.
    pub methods: Vec<TenvMethod>,
    /// Exported ObjC methods.
    pub exported_objc_methods: Vec<Procname>,
    /// Annotations.
    pub annots: AnnotItem,
    /// Language-specific class info.
    pub class_info: ClassInfo,
    /// Dummy struct for static methods.
    pub dummy: bool,
    /// Source file where this struct is defined.
    pub source_file: Option<SourceFile>,
}

impl Default for Struct {
    fn default() -> Self {
        Self {
            fields: Vec::new(),
            statics: Vec::new(),
            supers: BTreeSet::new(),
            objc_protocols: BTreeSet::new(),
            methods: Vec::new(),
            exported_objc_methods: Vec::new(),
            annots: AnnotItem::empty(),
            class_info: ClassInfo::NoInfo,
            dummy: false,
            source_file: None,
        }
    }
}
