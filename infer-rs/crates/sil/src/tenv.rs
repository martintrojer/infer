// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::HashMap;

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
