// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::{hash_map::Entry, HashMap};

use serde::{Deserialize, Serialize};

use crate::strukt::Struct;
use crate::typ::TypeName;

/// Type environment: maps type names to struct definitions.
///
/// Mirrors OCaml's `Tenv.t`, which is `Struct.t TypenameHash.t`.
/// The OCaml version uses a concurrent hash table; this uses a standard HashMap
/// since Rust handles concurrency at a higher level (e.g., via Arc<RwLock<Tenv>>).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Tenv {
    types: HashMap<TypeName, Struct>,
}

impl Tenv {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn merge(&mut self, other: Tenv) {
        for (name, newer) in other.types {
            match self.types.entry(name) {
                Entry::Vacant(entry) => {
                    entry.insert(newer);
                }
                Entry::Occupied(mut entry) => {
                    let typename = entry.key().clone();
                    let current_slot = entry.get_mut();
                    let current = std::mem::take(current_slot);
                    *current_slot = Struct::merge(&typename, newer, current);
                }
            }
        }
    }

    /// Look up a struct definition by type name.
    pub fn lookup(&self, name: &TypeName) -> Option<&Struct> {
        self.types.get(name)
    }

    /// Add or replace a struct definition.
    pub fn insert(&mut self, name: TypeName, strukt: Struct) {
        self.types.insert(name, strukt);
    }

    /// Check if a type name is in the environment.
    pub fn contains(&self, name: &TypeName) -> bool {
        self.types.contains_key(name)
    }

    /// Iterate over all type definitions.
    pub fn iter(&self) -> impl Iterator<Item = (&TypeName, &Struct)> {
        self.types.iter()
    }

    /// Number of types in the environment.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Get all supertypes of a given type (transitive closure).
    pub fn get_supers(&self, name: &TypeName) -> Vec<TypeName> {
        let mut result = Vec::new();
        let mut worklist = vec![name.clone()];
        let mut visited = std::collections::HashSet::new();

        while let Some(current) = worklist.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(strukt) = self.lookup(&current) {
                for super_name in &strukt.supers {
                    result.push(super_name.clone());
                    worklist.push(super_name.clone());
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annot::AnnotItem;
    use crate::fieldname::Fieldname;
    use crate::qualified_cpp_name::QualifiedCppName;
    use crate::strukt::Field;
    use crate::typ::{IKind, JavaClassName, Typ};

    fn mk_type(name: &str) -> TypeName {
        TypeName::CStruct(QualifiedCppName::from_string(name))
    }

    #[test]
    fn test_merge_extends_types() {
        let mut lhs = Tenv::new();
        lhs.insert(mk_type("left"), Struct::default());

        let mut rhs = Tenv::new();
        rhs.insert(mk_type("right"), Struct::default());

        lhs.merge(rhs);

        assert!(lhs.contains(&mk_type("left")));
        assert!(lhs.contains(&mk_type("right")));
        assert_eq!(lhs.len(), 2);
    }

    #[test]
    fn test_merge_prefers_non_dummy_c_struct() {
        let name = mk_type("dup");
        let mut lhs = Tenv::new();
        let left = Struct { dummy: false, ..Struct::default() };
        lhs.insert(name.clone(), left);

        let mut rhs = Tenv::new();
        let right = Struct { dummy: true, ..Struct::default() };
        rhs.insert(name.clone(), right);

        lhs.merge(rhs);

        assert!(!lhs.lookup(&name).expect("merged type should exist").dummy);
    }

    #[test]
    fn test_merge_combines_duplicate_java_type() {
        let name = TypeName::JavaClass(JavaClassName("Example".to_string()));
        let mut lhs = Tenv::new();
        lhs.insert(
            name.clone(),
            Struct {
                fields: vec![Field {
                    name: Fieldname::make(name.clone(), "lhs_field"),
                    typ: Typ::int(IKind::IInt),
                    annot: AnnotItem::empty(),
                }],
                ..Struct::default()
            },
        );

        let mut rhs = Tenv::new();
        rhs.insert(
            name.clone(),
            Struct {
                fields: vec![Field {
                    name: Fieldname::make(name.clone(), "rhs_field"),
                    typ: Typ::void(),
                    annot: AnnotItem::empty(),
                }],
                ..Struct::default()
            },
        );

        lhs.merge(rhs);

        let merged = lhs.lookup(&name).expect("merged type should exist");
        assert_eq!(merged.fields.len(), 2);
        assert!(merged
            .fields
            .iter()
            .any(|field| field.name.field_name == "lhs_field"));
        assert!(merged
            .fields
            .iter()
            .any(|field| field.name.field_name == "rhs_field"));
    }
}
