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

// ---- Typed, location-insensitive keys ----
//
// The AST `TypeName`, `QualifiedProcName`, and `VarName` all derive `Eq`
// and `Hash`, but they embed `Location`, which means two otherwise-identical
// names with different source positions hash differently. The `DeclEnv`
// historically worked around this by using `format!("{}", ...)` strings as
// keys. That is brittle (any change to `Display` silently changes keying)
// and inefficient. The wrapper key types below mirror the structural shape
// of the relevant AST nodes but drop locations, giving us proper typed
// keys for all `DeclEnv` maps.

/// Location-insensitive key for a [`TypeName`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeNameKey {
    pub name: String,
    pub args: Vec<TypeNameKey>,
}

impl TypeNameKey {
    pub fn from_ast(t: &TypeName) -> Self {
        Self {
            name: t.name.value.clone(),
            args: t.args.iter().map(Self::from_ast).collect(),
        }
    }
}

impl From<&TypeName> for TypeNameKey {
    fn from(t: &TypeName) -> Self {
        Self::from_ast(t)
    }
}

/// Location-insensitive key for an [`EnclosingClass`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EnclosingClassKey {
    TopLevel,
    Enclosing(TypeNameKey),
}

impl From<&EnclosingClass> for EnclosingClassKey {
    fn from(e: &EnclosingClass) -> Self {
        match e {
            EnclosingClass::TopLevel => EnclosingClassKey::TopLevel,
            EnclosingClass::Enclosing(t) => EnclosingClassKey::Enclosing(TypeNameKey::from_ast(t)),
        }
    }
}

/// Location-insensitive key for a [`QualifiedProcName`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcKey {
    pub enclosing_class: EnclosingClassKey,
    pub name: String,
}

impl ProcKey {
    pub fn from_ast(q: &QualifiedProcName) -> Self {
        Self {
            enclosing_class: EnclosingClassKey::from(&q.enclosing_class),
            name: q.name.value.clone(),
        }
    }
}

impl From<&QualifiedProcName> for ProcKey {
    fn from(q: &QualifiedProcName) -> Self {
        Self::from_ast(q)
    }
}

/// Location-insensitive key for a global variable. Globals live in a flat
/// namespace keyed by their textual name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GlobalKey(pub String);

impl GlobalKey {
    pub fn from_var(name: &VarName) -> Self {
        Self(name.value.clone())
    }

    pub fn from_str(name: &str) -> Self {
        Self(name.to_string())
    }
}

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
    pub globals: HashMap<GlobalKey, Global>,
    pub structs: HashMap<TypeNameKey, Struct>,
    pub procs: HashMap<ProcKey, ProcEntry>,
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
                    // OCaml allows redeclaration of globals; just overwrite.
                    env.globals.insert(GlobalKey::from_var(&g.name), g.clone());
                }
                Decl::Struct(s) => {
                    let key = TypeNameKey::from_ast(&s.name);
                    if env.structs.contains_key(&key) {
                        errors.push(DeclError::DuplicateStruct(s.name.clone()));
                    }
                    env.structs.insert(key, s.clone());
                }
                Decl::Procdecl(p) => {
                    let key = ProcKey::from_ast(&p.qualified_name);
                    env.procs.insert(key, ProcEntry::Decl(p.clone()));
                }
                Decl::Proc(p) => {
                    let key = ProcKey::from_ast(&p.procdecl.qualified_name);
                    env.procs.insert(key, ProcEntry::Desc(p.clone()));
                }
            }
        }

        (env, errors)
    }

    pub fn get_global(&self, name: &str) -> Option<&Global> {
        self.globals.get(&GlobalKey::from_str(name))
    }

    pub fn get_struct(&self, name: &TypeName) -> Option<&Struct> {
        self.structs.get(&TypeNameKey::from_ast(name))
    }

    pub fn get_proc(&self, name: &QualifiedProcName) -> Option<&ProcEntry> {
        self.procs.get(&ProcKey::from_ast(name))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(line: usize, col: usize) -> Location {
        Location::known(line, col)
    }

    #[test]
    fn type_name_key_ignores_location() {
        let a = TypeName::new("Foo", loc(1, 1));
        let b = TypeName::new("Foo", loc(99, 7));
        assert_eq!(TypeNameKey::from_ast(&a), TypeNameKey::from_ast(&b));
    }

    #[test]
    fn type_name_key_distinguishes_args() {
        let a = TypeName::with_args(Name::plain("List"), vec![TypeName::plain("Int")]);
        let b = TypeName::with_args(Name::plain("List"), vec![TypeName::plain("Bool")]);
        assert_ne!(TypeNameKey::from_ast(&a), TypeNameKey::from_ast(&b));
    }

    #[test]
    fn proc_key_ignores_location_and_distinguishes_class() {
        let p1 = QualifiedProcName::top_level(Name::new("foo", loc(1, 1)));
        let p2 = QualifiedProcName::top_level(Name::new("foo", loc(50, 3)));
        assert_eq!(ProcKey::from_ast(&p1), ProcKey::from_ast(&p2));

        let p3 = QualifiedProcName::with_class(TypeName::plain("C"), Name::plain("foo"));
        assert_ne!(ProcKey::from_ast(&p1), ProcKey::from_ast(&p3));
    }

    #[test]
    fn lookup_works_with_different_locations() {
        let g = Global {
            name: Name::new("g", loc(1, 1)),
            typ: Typ::Int,
            attributes: vec![],
        };
        let s = Struct {
            name: TypeName::new("S", loc(2, 1)),
            supers: Default::default(),
            fields: vec![],
            attributes: vec![],
        };
        let module = Module {
            attrs: vec![],
            decls: vec![Decl::Global(g), Decl::Struct(s)],
            source_file: String::new(),
        };
        let (env, errors) = DeclEnv::from_module(&module);
        assert!(errors.is_empty());

        // Lookup with a TypeName that has a different location should still
        // hit the entry.
        let q = TypeName::new("S", loc(999, 999));
        assert!(env.get_struct(&q).is_some());
        assert!(env.get_global("g").is_some());
    }
}
