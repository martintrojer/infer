// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::annot::{Annot, AnnotItem};
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

impl Struct {
    /// Mirrors OCaml's `Struct.merge`, used by `Tenv.merge` when the same
    /// type name is contributed by multiple translated units.
    pub fn merge(typename: &TypeName, newer: Self, current: Self) -> Self {
        match typename {
            TypeName::CStruct(_)
            | TypeName::CUnion(_)
            | TypeName::CppClass { .. }
            | TypeName::ErlangType(_)
            | TypeName::ObjcClass(_)
            | TypeName::ObjcProtocol(_)
            | TypeName::SwiftClass(_)
            | TypeName::ObjcBlock(_)
            | TypeName::CFunction(_)
            | TypeName::SwiftClosure(_) => {
                if newer.dummy {
                    current
                } else {
                    newer
                }
            }
            TypeName::JavaClass(_)
            | TypeName::CSharpClass(_)
            | TypeName::HackClass(_)
            | TypeName::PythonClass(_) => {
                if newer.dummy {
                    current
                } else if current.dummy {
                    newer
                } else {
                    Self::full_merge(newer, current)
                }
            }
        }
    }

    fn full_merge(newer: Self, mut current: Self) -> Self {
        Self::merge_fields(&mut current.fields, newer.fields);
        Self::merge_fields(&mut current.statics, newer.statics);
        current.supers.extend(newer.supers);
        Self::merge_methods(&mut current.methods, newer.methods);
        Self::merge_annots(&mut current.annots.0, newer.annots.0);
        current.class_info = Self::merge_class_info(newer.class_info, current.class_info);
        current.source_file = Self::merge_source_file(newer.source_file, current.source_file);
        current
    }

    fn merge_fields(current: &mut Vec<Field>, newer: Vec<Field>) {
        for field in newer {
            if current.iter().any(|existing| existing.name == field.name) {
                continue;
            }
            current.push(field);
        }
    }

    fn merge_methods(current: &mut Vec<TenvMethod>, newer: Vec<TenvMethod>) {
        for method in newer {
            if let Some(existing) = current
                .iter_mut()
                .find(|existing| existing.proc_name == method.proc_name)
            {
                // Rust carries a defined/declaration bit that OCaml's tenv
                // method entries do not. Preserve a definition if either side
                // says the method is defined.
                existing.is_defined |= method.is_defined;
            } else {
                current.push(method);
            }
        }
    }

    fn merge_annots(current: &mut Vec<Annot>, newer: Vec<Annot>) {
        for annot in newer {
            if current.contains(&annot) {
                continue;
            }
            current.push(annot);
        }
    }

    fn merge_class_info(newer: ClassInfo, current: ClassInfo) -> ClassInfo {
        match (newer, current) {
            (ClassInfo::NoInfo, current) | (current, ClassInfo::NoInfo) => current,
            (
                ClassInfo::JavaClassInfo { kind: newer_kind },
                ClassInfo::JavaClassInfo { kind: current_kind },
            ) => ClassInfo::JavaClassInfo {
                kind: if Self::java_kind_rank(newer_kind) > Self::java_kind_rank(current_kind) {
                    newer_kind
                } else {
                    current_kind
                },
            },
            (ClassInfo::HackClassInfo { .. }, current @ ClassInfo::HackClassInfo { .. }) => current,
            (ClassInfo::CppClassInfo, ClassInfo::CppClassInfo) => ClassInfo::CppClassInfo,
            (newer, current) => {
                debug_assert_eq!(
                    std::mem::discriminant(&newer),
                    std::mem::discriminant(&current),
                    "incompatible class info merge: {newer:?} vs {current:?}",
                );
                current
            }
        }
    }

    fn merge_source_file(
        newer: Option<SourceFile>,
        current: Option<SourceFile>,
    ) -> Option<SourceFile> {
        match (newer, current) {
            (None, current) | (current, None) => current,
            (Some(newer), Some(current)) => {
                if newer < current {
                    Some(newer)
                } else {
                    Some(current)
                }
            }
        }
    }

    fn java_kind_rank(kind: JavaClassKind) -> u8 {
        match kind {
            JavaClassKind::Interface => 0,
            JavaClassKind::AbstractClass => 1,
            JavaClassKind::ConcreteClass => 2,
            JavaClassKind::Enum => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fieldname::Fieldname;
    use crate::qualified_cpp_name::QualifiedCppName;
    use crate::typ::{IKind, JavaClassName};

    fn mk_field(class_name: &TypeName, field_name: &str, typ: Typ) -> Field {
        Field {
            name: Fieldname::make(class_name.clone(), field_name),
            typ,
            annot: AnnotItem::empty(),
        }
    }

    fn mk_method(name: &str, is_defined: bool) -> TenvMethod {
        TenvMethod {
            proc_name: Procname::c_from_string(name),
            is_defined,
        }
    }

    #[test]
    fn test_merge_c_struct_prefers_non_dummy_newer() {
        let name = TypeName::CStruct(QualifiedCppName::from_string("Cell"));
        let current = Struct {
            source_file: Some(SourceFile::new("current.c")),
            ..Struct::default()
        };

        let merged_dummy = Struct::merge(
            &name,
            Struct {
                dummy: true,
                source_file: Some(SourceFile::new("dummy.c")),
                ..Struct::default()
            },
            current.clone(),
        );
        assert_eq!(merged_dummy, current);

        let newer = Struct {
            source_file: Some(SourceFile::new("newer.c")),
            ..Struct::default()
        };
        let merged_real = Struct::merge(&name, newer.clone(), current);
        assert_eq!(merged_real, newer);
    }

    #[test]
    fn test_merge_java_structs_full_merges_non_dummy_entries() {
        let name = TypeName::JavaClass(JavaClassName("Example".to_string()));
        let current = Struct {
            fields: vec![
                mk_field(&name, "dup", Typ::int(IKind::IInt)),
                mk_field(&name, "keep", Typ::void()),
            ],
            supers: BTreeSet::from([TypeName::JavaClass(JavaClassName("Base".to_string()))]),
            methods: vec![mk_method("dup_method", false)],
            class_info: ClassInfo::JavaClassInfo {
                kind: JavaClassKind::Interface,
            },
            source_file: Some(SourceFile::new("z_current.java")),
            ..Struct::default()
        };

        let newer = Struct {
            fields: vec![
                mk_field(&name, "dup", Typ::void()),
                mk_field(&name, "new", Typ::int(IKind::IBool)),
            ],
            supers: BTreeSet::from([TypeName::JavaClass(JavaClassName("Mixin".to_string()))]),
            methods: vec![
                mk_method("dup_method", true),
                mk_method("new_method", false),
            ],
            class_info: ClassInfo::JavaClassInfo {
                kind: JavaClassKind::ConcreteClass,
            },
            source_file: Some(SourceFile::new("a_newer.java")),
            ..Struct::default()
        };

        let merged = Struct::merge(&name, newer, current);

        assert_eq!(merged.fields.len(), 3);
        assert_eq!(
            merged
                .fields
                .iter()
                .find(|field| field.name.field_name == "dup")
                .expect("duplicate field should remain")
                .typ,
            Typ::int(IKind::IInt),
        );
        assert!(merged
            .fields
            .iter()
            .any(|field| field.name.field_name == "new"));
        assert_eq!(merged.supers.len(), 2);
        assert_eq!(merged.methods.len(), 2);
        assert!(
            merged
                .methods
                .iter()
                .find(|method| method.proc_name == Procname::c_from_string("dup_method"))
                .expect("duplicate method should remain")
                .is_defined
        );
        assert_eq!(
            merged.class_info,
            ClassInfo::JavaClassInfo {
                kind: JavaClassKind::ConcreteClass,
            }
        );
        assert_eq!(merged.source_file, Some(SourceFile::new("a_newer.java")));
    }
}
