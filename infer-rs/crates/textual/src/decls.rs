// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Declaration environment for Textual modules.
//!
//! Mirrors OCaml's `TextualDecls.t`. Collects all global, struct, and
//! procedure declarations from a module for lookup during verification
//! and conversion.

use std::collections::HashMap;

use crate::ast::*;

/// A procedure entry: either a declaration or a definition.
#[derive(Clone, Debug)]
pub enum ProcEntry {
    Decl(ProcDecl),
    Desc(ProcDesc),
}

impl ProcEntry {
    pub fn procdecl(&self) -> &ProcDecl {
        match self {
            ProcEntry::Decl(d) => d,
            ProcEntry::Desc(d) => &d.procdecl,
        }
    }
}

/// Declaration environment.
///
/// Mirrors OCaml's `TextualDecls.t`.
pub struct DeclEnv {
    pub globals: HashMap<String, Global>,
    pub structs: HashMap<String, Struct>,
    pub procs: HashMap<String, ProcEntry>,
}

impl DeclEnv {
    /// Build a declaration environment from a module.
    ///
    /// Mirrors OCaml's `TextualDecls.make_decls`.
    pub fn from_module(module: &Module) -> (Self, Vec<DeclError>) {
        let mut env = DeclEnv {
            globals: HashMap::new(),
            structs: HashMap::new(),
            procs: HashMap::new(),
        };
        let mut errors = Vec::new();

        for decl in &module.decls {
            match decl {
                Decl::Global(g) => {
                    if env.globals.contains_key(&g.name.value) {
                        // OCaml allows redeclaration of globals, just overwrites
                    }
                    env.globals.insert(g.name.value.clone(), g.clone());
                }
                Decl::Struct(s) => {
                    let key = format!("{}", s.name);
                    if env.structs.contains_key(&key) {
                        errors.push(DeclError::DuplicateStruct(s.name.clone()));
                    }
                    env.structs.insert(key, s.clone());
                }
                Decl::Procdecl(p) => {
                    let key = format!("{}", p.qualified_name);
                    env.procs.insert(key, ProcEntry::Decl(p.clone()));
                }
                Decl::Proc(p) => {
                    let key = format!("{}", p.procdecl.qualified_name);
                    env.procs.insert(key, ProcEntry::Desc(p.clone()));
                }
            }
        }

        (env, errors)
    }

    pub fn get_global(&self, name: &str) -> Option<&Global> {
        self.globals.get(name)
    }

    pub fn get_struct(&self, name: &TypeName) -> Option<&Struct> {
        self.structs.get(&name.to_string())
    }

    pub fn get_proc(&self, name: &QualifiedProcName) -> Option<&ProcEntry> {
        self.procs.get(&name.to_string())
    }

    pub fn get_field(&self, field: &QualifiedFieldName) -> Option<&FieldDecl> {
        let strukt = self.get_struct(&field.enclosing_class)?;
        strukt
            .fields
            .iter()
            .find(|f| f.qualified_name.name.value == field.name.value)
    }
}

/// Errors during declaration collection.
#[derive(Clone, Debug)]
pub enum DeclError {
    DuplicateStruct(TypeName),
}

impl std::fmt::Display for DeclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeclError::DuplicateStruct(name) => write!(f, "duplicate struct: {name}"),
        }
    }
}
