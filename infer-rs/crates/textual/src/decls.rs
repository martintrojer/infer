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
pub struct ProcNameKey {
    pub enclosing_class: EnclosingClassKey,
    pub name: String,
}

impl ProcNameKey {
    pub fn from_ast(q: &QualifiedProcName) -> Self {
        Self {
            enclosing_class: EnclosingClassKey::from(&q.enclosing_class),
            name: q.name.value.clone(),
        }
    }
}

impl From<&QualifiedProcName> for ProcNameKey {
    fn from(q: &QualifiedProcName) -> Self {
        Self::from_ast(q)
    }
}

/// Procedure signature key.
///
/// OCaml `TextualDecls` keys Hack procedures by `(qualified name, arity)` and
/// all other languages by qualified name only. This prevents Hack overloads
/// that differ only in arity from overwriting each other while preserving the
/// original non-Hack lookup behavior.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProcKey {
    Hack {
        qualified_name: ProcNameKey,
        arity: Option<usize>,
    },
    Other {
        qualified_name: ProcNameKey,
    },
}

impl ProcKey {
    pub fn from_decl(lang: &str, decl: &ProcDecl) -> Self {
        Self::from_parts(
            lang,
            &decl.qualified_name,
            decl.formals_types.as_ref().map(Vec::len),
        )
    }

    pub fn from_call(lang: &str, q: &QualifiedProcName, arity: usize) -> Self {
        Self::from_parts(lang, q, Some(arity))
    }

    pub fn from_name(lang: &str, q: &QualifiedProcName) -> Self {
        Self::from_parts(lang, q, None)
    }

    fn from_parts(lang: &str, q: &QualifiedProcName, arity: Option<usize>) -> Self {
        let qualified_name = ProcNameKey::from_ast(q);
        if lang.eq_ignore_ascii_case("hack") {
            ProcKey::Hack {
                qualified_name,
                arity,
            }
        } else {
            ProcKey::Other { qualified_name }
        }
    }

    pub fn qualified_name(&self) -> &ProcNameKey {
        match self {
            ProcKey::Hack { qualified_name, .. } | ProcKey::Other { qualified_name } => {
                qualified_name
            }
        }
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

    pub fn from_name(name: &str) -> Self {
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

    fn without_formals_types(&self) -> Self {
        match self {
            ProcEntry::Decl(d) => {
                let mut decl = d.clone();
                decl.formals_types = None;
                ProcEntry::Decl(decl)
            }
            ProcEntry::Desc(d) => {
                let mut desc = d.clone();
                desc.procdecl.formals_types = None;
                ProcEntry::Desc(desc)
            }
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
    pub variadic_procs: HashMap<ProcNameKey, ProcDesc>,
    lang: String,
}

impl DeclEnv {
    /// Build a declaration environment from a module.
    ///
    /// Mirrors OCaml's `TextualDecls.make_decls`.
    pub fn from_module(module: &Module) -> (Self, Vec<DeclError>) {
        let lang = module.lang().unwrap_or("c").to_string();
        let mut env = DeclEnv {
            globals: HashMap::new(),
            structs: HashMap::new(),
            procs: HashMap::new(),
            variadic_procs: HashMap::new(),
            lang,
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
                Decl::Procdecl(p) => env.declare_proc(ProcEntry::Decl(p.clone())),
                Decl::Proc(p) => env.declare_proc(ProcEntry::Desc(p.clone())),
            }
        }

        (env, errors)
    }

    fn declare_proc(&mut self, proc: ProcEntry) {
        self.declare_variadic_proc_if_necessary(&proc);
        self.declare_default_if_necessary(&proc);

        let key = ProcKey::from_decl(&self.lang, proc.procdecl());
        match (self.procs.get(&key), &proc) {
            // Match OCaml `declare_proc`: a declaration must not overwrite an
            // existing definition with the same signature.
            (Some(ProcEntry::Desc(_)), ProcEntry::Decl(_)) => {}
            _ => {
                self.procs.insert(key, proc);
            }
        }
    }

    fn declare_variadic_proc_if_necessary(&mut self, proc: &ProcEntry) {
        if let ProcEntry::Desc(pdesc) = proc {
            if pdesc.procdecl.is_variadic() {
                // OCaml records only ProcDesc entries as variadic. The
                // annotated formal itself is preserved on `formals_types`.
                self.variadic_procs.insert(
                    ProcNameKey::from_ast(&pdesc.procdecl.qualified_name),
                    pdesc.clone(),
                );
            }
        }
    }

    fn declare_default_if_necessary(&mut self, proc: &ProcEntry) {
        if !self.lang.eq_ignore_ascii_case("hack") {
            return;
        }
        let ProcKey::Hack {
            qualified_name,
            arity: Some(_),
        } = ProcKey::from_decl(&self.lang, proc.procdecl())
        else {
            return;
        };
        let default_key = ProcKey::Hack {
            qualified_name,
            arity: None,
        };
        self.procs
            .entry(default_key)
            .or_insert_with(|| proc.without_formals_types());
    }

    pub fn lang(&self) -> &str {
        &self.lang
    }

    pub fn get_global(&self, name: &str) -> Option<&Global> {
        self.globals.get(&GlobalKey::from_name(name))
    }

    pub fn get_struct(&self, name: &TypeName) -> Option<&Struct> {
        self.structs.get(&TypeNameKey::from_ast(name))
    }

    pub fn get_proc(&self, name: &QualifiedProcName) -> Option<&ProcEntry> {
        self.get_proc_with_arity(name, None)
    }

    pub fn get_proc_for_call(&self, name: &QualifiedProcName, arity: usize) -> Option<&ProcEntry> {
        self.get_proc_with_arity(name, Some(arity))
    }

    fn get_exact_proc_for_call(
        &self,
        name: &QualifiedProcName,
        arity: usize,
    ) -> Option<&ProcEntry> {
        self.procs.get(&ProcKey::from_call(&self.lang, name, arity))
    }

    fn get_proc_with_arity(
        &self,
        name: &QualifiedProcName,
        arity: Option<usize>,
    ) -> Option<&ProcEntry> {
        if self.lang.eq_ignore_ascii_case("hack") {
            if let Some(arity) = arity {
                self.procs
                    .get(&ProcKey::from_call(&self.lang, name, arity))
                    .or_else(|| self.procs.get(&ProcKey::from_name(&self.lang, name)))
            } else {
                self.procs.get(&ProcKey::from_name(&self.lang, name))
            }
        } else {
            self.procs.get(&ProcKey::from_name(&self.lang, name))
        }
    }

    pub fn has_proc_named(&self, name: &QualifiedProcName) -> bool {
        let qualified_name = ProcNameKey::from_ast(name);
        self.procs
            .keys()
            .any(|key| key.qualified_name() == &qualified_name)
            || self.variadic_procs.contains_key(&qualified_name)
    }

    pub fn get_variadic_procdesc(&self, name: &QualifiedProcName) -> Option<&ProcDesc> {
        self.variadic_procs.get(&ProcNameKey::from_ast(name))
    }

    /// Resolve a call to the declaration that should be used for arity/type
    /// checks, including OCaml's Textual variadic lookup for definitions with
    /// a `.variadic` annotated formal.
    pub fn get_procdecl_for_call(
        &self,
        name: &QualifiedProcName,
        num_args: usize,
    ) -> Option<ResolvedProcDecl<'_>> {
        let exact_non_variadic =
            self.get_exact_proc_for_call(name, num_args)
                .map(|entry| ResolvedProcDecl {
                    variadic: None,
                    decl: entry.procdecl(),
                });
        let non_variadic = self
            .get_proc_for_call(name, num_args)
            .map(|entry| ResolvedProcDecl {
                variadic: None,
                decl: entry.procdecl(),
            });

        match self.get_variadic_procdesc(name) {
            Some(pdesc) => match pdesc.procdecl.formals_types.as_ref() {
                Some(formals) => {
                    let variadic_index = pdesc
                        .procdecl
                        .variadic_formal_index()
                        .unwrap_or_else(|| formals.len().saturating_sub(1));
                    if num_args + 1 >= formals.len() {
                        Some(ResolvedProcDecl {
                            variadic: Some(VariadicInfo {
                                index: variadic_index,
                            }),
                            decl: &pdesc.procdecl,
                        })
                    } else {
                        // Too few arguments for the variadic definition.
                        // Prefer a real arity-specific overload if it exists,
                        // but do not silently accept the synthetic Hack
                        // arity-less fallback added to avoid capture misses.
                        exact_non_variadic.or(Some(ResolvedProcDecl {
                            variadic: Some(VariadicInfo {
                                index: variadic_index,
                            }),
                            decl: &pdesc.procdecl,
                        }))
                    }
                }
                None => non_variadic,
            },
            None => non_variadic,
        }
    }

    pub fn is_defined_in_a_trait(&self, proc: &QualifiedProcName) -> bool {
        match &proc.enclosing_class {
            EnclosingClass::Enclosing(tname) => self.get_struct(tname).is_some_and(|s| {
                s.attributes
                    .iter()
                    .any(|attr| attr.name == "kind" && attr.values == ["trait"])
            }),
            EnclosingClass::TopLevel => false,
        }
    }

    pub fn is_trait_method(&self, proc: &QualifiedProcName) -> bool {
        self.is_defined_in_a_trait(proc) && !is_hack_init(proc)
    }

    pub fn get_field(&self, field: &QualifiedFieldName) -> Option<&FieldDecl> {
        let strukt = self.get_struct(&field.enclosing_class)?;
        strukt
            .fields
            .iter()
            .find(|f| f.qualified_name.name.value == field.name.value)
    }
}

fn is_hack_init(proc: &QualifiedProcName) -> bool {
    matches!(proc.name.value.as_str(), "_86pinit" | "_86constinit")
}

/// Information about a `.variadic` formal in a resolved call target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VariadicInfo {
    pub index: usize,
}

/// Result of resolving a call through [`DeclEnv`].
#[derive(Clone, Copy, Debug)]
pub struct ResolvedProcDecl<'a> {
    pub variadic: Option<VariadicInfo>,
    pub decl: &'a ProcDecl,
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
    fn proc_name_key_ignores_location_and_distinguishes_class() {
        let p1 = QualifiedProcName::top_level(Name::new("foo", loc(1, 1)));
        let p2 = QualifiedProcName::top_level(Name::new("foo", loc(50, 3)));
        assert_eq!(ProcNameKey::from_ast(&p1), ProcNameKey::from_ast(&p2));

        let p3 = QualifiedProcName::with_class(TypeName::plain("C"), Name::plain("foo"));
        assert_ne!(ProcNameKey::from_ast(&p1), ProcNameKey::from_ast(&p3));
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

    #[test]
    fn hack_proc_lookup_uses_arity_and_preserves_unknown_default() {
        let name = QualifiedProcName::top_level(Name::plain("foo"));
        let decl1 = ProcDecl {
            qualified_name: name.clone(),
            formals_types: Some(vec![AnnotatedTyp::without_attrs(Typ::Int)]),
            result_type: AnnotatedTyp::without_attrs(Typ::Int),
            attributes: vec![],
        };
        let decl2 = ProcDecl {
            qualified_name: name.clone(),
            formals_types: Some(vec![
                AnnotatedTyp::without_attrs(Typ::Int),
                AnnotatedTyp::without_attrs(Typ::Float),
            ]),
            result_type: AnnotatedTyp::without_attrs(Typ::Float),
            attributes: vec![],
        };
        let module = Module {
            attrs: vec![Attr::new("source_language", vec!["hack".into()], loc(1, 1))],
            decls: vec![Decl::Procdecl(decl1), Decl::Procdecl(decl2)],
            source_file: String::new(),
        };

        let (env, errors) = DeclEnv::from_module(&module);
        assert!(errors.is_empty());
        assert_eq!(
            env.get_proc_for_call(&name, 1)
                .unwrap()
                .procdecl()
                .result_type
                .typ,
            Typ::Int
        );
        assert_eq!(
            env.get_proc_for_call(&name, 2)
                .unwrap()
                .procdecl()
                .result_type
                .typ,
            Typ::Float
        );
        assert!(env
            .get_proc_for_call(&name, 3)
            .unwrap()
            .procdecl()
            .formals_types
            .is_none());
    }

    #[test]
    fn variadic_procdesc_is_resolved_for_extra_args() {
        let name = QualifiedProcName::top_level(Name::plain("foo"));
        let pdesc = ProcDesc {
            procdecl: ProcDecl {
                qualified_name: name.clone(),
                formals_types: Some(vec![
                    AnnotatedTyp::without_attrs(Typ::Int),
                    AnnotatedTyp {
                        typ: Typ::Float,
                        attributes: vec![Attr::new("variadic", vec![], loc(1, 1))],
                    },
                ]),
                result_type: AnnotatedTyp::without_attrs(Typ::Void),
                attributes: vec![],
            },
            nodes: vec![],
            start: Name::plain("entry"),
            params: vec![Name::plain("x"), Name::plain("xs")],
            locals: vec![],
            exit_loc: Location::Unknown,
        };
        let module = Module {
            attrs: vec![Attr::new("source_language", vec!["hack".into()], loc(1, 1))],
            decls: vec![Decl::Proc(pdesc)],
            source_file: String::new(),
        };

        let (env, errors) = DeclEnv::from_module(&module);
        assert!(errors.is_empty());
        let resolved = env.get_procdecl_for_call(&name, 4).unwrap();
        assert_eq!(resolved.variadic.unwrap().index, 1);
    }
}
