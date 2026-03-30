// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Type verification and inference for Textual modules.
//!
//! Mirrors OCaml's `TextualTypeVerification.ml`.
//!
//! Walks each procedure's AST and:
//! - Infers types for identifiers (from Load/Let assignments)
//! - Infers types for variables (from declarations)
//! - Fills in `typ: None` on Load expressions (the `->` syntax)
//!
//! This pass runs BEFORE `remove_effects_in_subexprs` and `to_sil`.

use std::collections::HashMap;

use crate::ast::*;
use crate::decls::DeclEnv;

/// Check if a value of type `given` can be assigned to a variable of type `assigned`.
///
/// Mirrors OCaml's `TextualTypeVerification.compat`.
/// Language-aware: C/Swift allow pointer↔int coercion, void* is universal.
pub fn compat(lang: &str, assigned: &Typ, given: &Typ) -> bool {
    match (assigned, given) {
        (Typ::Int, Typ::Int) => true,
        (Typ::Float, Typ::Float) => true,
        (Typ::Void, _) | (_, Typ::Void) => true,
        (Typ::Int, Typ::Ptr(_, _)) => true,
        (Typ::Ptr(_, _), Typ::Int) if lang == "c" || lang == "swift" => true,
        (Typ::Ptr(t1, _), Typ::Ptr(t2, _)) => compat(lang, t1, t2),
        (Typ::Ptr(_, _), Typ::Null) => true,
        (Typ::Struct(_), Typ::Struct(_)) => true, // no subtyping check yet
        (Typ::Array(t1), Typ::Array(t2)) => compat(lang, t1, t2),
        // void* is compatible with anything in C/Swift
        (_, Typ::Ptr(t, _)) | (Typ::Ptr(t, _), _)
            if matches!(t.as_ref(), Typ::Void) && (lang == "c" || lang == "swift") =>
        {
            true
        }
        _ => false,
    }
}

/// Check if a type is a pointer.
pub fn is_ptr(typ: &Typ) -> bool {
    matches!(typ, Typ::Ptr(_, _) | Typ::Void)
}

/// Check if a type is a pointer to a struct.
pub fn is_ptr_struct(typ: &Typ) -> bool {
    matches!(typ, Typ::Ptr(inner, _) if matches!(inner.as_ref(), Typ::Struct(_) | Typ::Void))
}

/// Type inference state for a single procedure.
struct TypeState<'a> {
    decls: &'a DeclEnv,
    idents: HashMap<Ident, Typ>,
    vars: HashMap<String, Typ>,
}

impl<'a> TypeState<'a> {
    fn new(decls: &'a DeclEnv, pdesc: &ProcDesc) -> Self {
        let mut vars = HashMap::new();

        // Register formal parameters (params + formals_types are parallel)
        if let Some(formals) = &pdesc.procdecl.formals_types {
            for (name, annotated_typ) in pdesc.params.iter().zip(formals.iter()) {
                vars.insert(name.value.clone(), annotated_typ.typ.clone());
            }
        }

        // Register local variables
        for (name, annotated_typ) in &pdesc.locals {
            vars.insert(name.value.clone(), annotated_typ.typ.clone());
        }

        Self {
            decls,
            idents: HashMap::new(),
            vars,
        }
    }

    /// Infer the type of an expression, filling in `typ: None` holes.
    fn typeof_exp(&mut self, exp: &Exp) -> (Exp, Typ) {
        match exp {
            Exp::Var(id) => {
                let typ = self.idents.get(id).cloned().unwrap_or(Typ::Void);
                (exp.clone(), typ)
            }

            Exp::Lvar(name) => {
                let var_typ = self.vars.get(&name.value).cloned().unwrap_or(Typ::Void);
                // Lvar returns Ptr(var_type) — the address of the variable
                (exp.clone(), Typ::Ptr(Box::new(var_typ), Vec::new()))
            }

            Exp::Const(c) => {
                let typ = match c {
                    Const::Int(_) => Typ::Int,
                    Const::Float(_) => Typ::Float,
                    Const::Null => Typ::Null,
                    Const::Str(_) => Typ::Ptr(Box::new(Typ::Void), Vec::new()),
                };
                (exp.clone(), typ)
            }

            Exp::Load { exp: inner, typ } => {
                let (new_inner, inner_typ) = self.typeof_exp(inner);
                let load_typ = match typ {
                    Some(t) => t.clone(),
                    None => {
                        // Infer: if inner is Ptr(T), load gives T
                        match &inner_typ {
                            Typ::Ptr(content, _) => *content.clone(),
                            _ => Typ::Void,
                        }
                    }
                };
                (
                    Exp::Load {
                        exp: Box::new(new_inner),
                        typ: Some(load_typ.clone()),
                    },
                    load_typ,
                )
            }

            Exp::Field { exp: inner, field } => {
                let (new_inner, inner_typ) = self.typeof_exp(inner);
                let field_typ = self
                    .decls
                    .get_field(field)
                    .map(|f| f.typ.clone())
                    .unwrap_or(Typ::Void);

                // If the inner expression is a pointer to a struct (not a struct
                // directly), insert a Load to dereference it first.
                // This handles `l->field` where l is `*struct`: the Field access
                // needs to operate on the dereferenced struct, not the pointer.
                // Matches OCaml: typecheck_exp checks `is_ptr_struct` on the inner
                // expression — if it's Ptr(Struct), the field access is valid.
                // The key is that `Lvar(l)` for a pointer variable `l: *struct`
                // returns Ptr(Ptr(struct)), so we need one dereference.
                // If inner is Ptr(Ptr(Struct(..))), it's a pointer variable
                // pointing to a struct. Insert a Load to dereference one level
                // so the field access operates on the struct, not the pointer.
                // This handles `l->field` where l is `*struct`.
                let needs_deref = matches!(&inner_typ,
                    Typ::Ptr(inner, _) if matches!(inner.as_ref(),
                        Typ::Ptr(inner2, _) if matches!(inner2.as_ref(), Typ::Struct(_))
                    )
                );
                let new_inner = if needs_deref {
                    if let Typ::Ptr(deref_typ, _) = &inner_typ {
                        Exp::Load {
                            exp: Box::new(new_inner),
                            typ: Some(*deref_typ.clone()),
                        }
                    } else {
                        new_inner
                    }
                } else {
                    new_inner
                };

                (
                    Exp::Field {
                        exp: Box::new(new_inner),
                        field: field.clone(),
                    },
                    Typ::Ptr(Box::new(field_typ), Vec::new()),
                )
            }

            Exp::Index(e1, e2) => {
                let (new_e1, _) = self.typeof_exp(e1);
                let (new_e2, _) = self.typeof_exp(e2);
                (
                    Exp::Index(Box::new(new_e1), Box::new(new_e2)),
                    Typ::Void, // simplified
                )
            }

            Exp::Call { proc, args, kind } => {
                let new_args: Vec<Exp> = args.iter().map(|a| self.typeof_exp(a).0).collect();
                // Builtin return types (mirrors OCaml's typeof_allocate_builtin etc.)
                let ret_typ = match proc.name.value.as_str() {
                    "__sil_allocate" => {
                        // __sil_allocate(<Type>) returns *Type
                        if let Some(Exp::Typ(t)) = args.first() {
                            Typ::Ptr(Box::new(t.clone()), Vec::new())
                        } else {
                            Typ::Void
                        }
                    }
                    "__sil_allocate_array" => {
                        // __sil_allocate_array(<Type>, dim) returns *Type
                        if let Some(Exp::Typ(t)) = args.first() {
                            Typ::Ptr(Box::new(t.clone()), Vec::new())
                        } else {
                            Typ::Void
                        }
                    }
                    "__sil_cast" => {
                        // __sil_cast(<Type>, exp) returns Type
                        if let Some(Exp::Typ(t)) = args.first() {
                            t.clone()
                        } else {
                            Typ::Void
                        }
                    }
                    "__sil_instanceof" => Typ::Int,
                    _ => self
                        .decls
                        .get_proc(proc)
                        .map(|entry| entry.procdecl().result_type.typ.clone())
                        .unwrap_or(Typ::Void),
                };
                (
                    Exp::Call {
                        proc: proc.clone(),
                        args: new_args,
                        kind: *kind,
                    },
                    ret_typ,
                )
            }

            Exp::If { cond, then_, else_ } => {
                let (new_then, then_typ) = self.typeof_exp(then_);
                let (new_else, _) = self.typeof_exp(else_);
                (
                    Exp::If {
                        cond: cond.clone(),
                        then_: Box::new(new_then),
                        else_: Box::new(new_else),
                    },
                    then_typ,
                )
            }

            Exp::Apply { closure, args } => {
                let (new_closure, _) = self.typeof_exp(closure);
                let new_args: Vec<Exp> = args.iter().map(|a| self.typeof_exp(a).0).collect();
                (
                    Exp::Apply {
                        closure: Box::new(new_closure),
                        args: new_args,
                    },
                    Typ::Void,
                )
            }

            Exp::Closure {
                proc,
                captured,
                params,
                attributes,
            } => {
                let new_captured: Vec<Exp> =
                    captured.iter().map(|c| self.typeof_exp(c).0).collect();
                (
                    Exp::Closure {
                        proc: proc.clone(),
                        captured: new_captured,
                        params: params.clone(),
                        attributes: attributes.clone(),
                    },
                    Typ::Void,
                )
            }

            Exp::Typ(_) => (exp.clone(), Typ::Void),
        }
    }

    /// Type-check and annotate an instruction.
    fn typecheck_instr(&mut self, instr: &Instr) -> Instr {
        match instr {
            Instr::Load { id, exp, typ, loc } => {
                let (new_exp, exp_typ) = self.typeof_exp(exp);
                let load_typ = match typ {
                    Some(t) => t.clone(),
                    None => match &exp_typ {
                        Typ::Ptr(content, _) => *content.clone(),
                        _ => Typ::Void,
                    },
                };
                self.idents.insert(*id, load_typ.clone());
                Instr::Load {
                    id: *id,
                    exp: new_exp,
                    typ: Some(load_typ),
                    loc: loc.clone(),
                }
            }

            Instr::Store {
                exp1,
                typ,
                exp2,
                loc,
            } => {
                let (new_exp1, _) = self.typeof_exp(exp1);
                let (new_exp2, exp2_typ) = self.typeof_exp(exp2);
                let store_typ = typ.clone().or(Some(exp2_typ));
                Instr::Store {
                    exp1: new_exp1,
                    typ: store_typ,
                    exp2: new_exp2,
                    loc: loc.clone(),
                }
            }

            Instr::Prune { exp, loc } => {
                let (new_exp, _) = self.typeof_exp(exp);
                Instr::Prune {
                    exp: new_exp,
                    loc: loc.clone(),
                }
            }

            Instr::Let { id, exp, loc } => {
                let (new_exp, exp_typ) = self.typeof_exp(exp);
                if let Some(id) = id {
                    self.idents.insert(*id, exp_typ);
                }
                Instr::Let {
                    id: *id,
                    exp: new_exp,
                    loc: loc.clone(),
                }
            }
        }
    }

    fn typecheck_node(&mut self, node: &Node) -> Node {
        // Register SSA parameter idents so downstream instructions can
        // reference them. Mirrors OCaml's `set_ident_type` for ssa_parameters.
        for (id, typ) in &node.ssa_parameters {
            self.idents.insert(*id, typ.clone());
        }

        let new_instrs: Vec<Instr> = node
            .instrs
            .iter()
            .map(|i| self.typecheck_instr(i))
            .collect();
        Node {
            label: node.label.clone(),
            ssa_parameters: node.ssa_parameters.clone(),
            exn_succs: node.exn_succs.clone(),
            last: node.last.clone(),
            instrs: new_instrs,
            last_loc: node.last_loc.clone(),
            label_loc: node.label_loc.clone(),
        }
    }
}

/// Run type verification and inference on a module.
///
/// Fills in `typ: None` holes on Load/Store expressions.
/// Matches OCaml's `TextualTypeVerification.run`.
pub fn run(module: &mut Module, decls: &DeclEnv) {
    for decl in &mut module.decls {
        if let Decl::Proc(pdesc) = decl {
            let mut state = TypeState::new(decls, pdesc);
            let new_nodes: Vec<Node> = pdesc
                .nodes
                .iter()
                .map(|n| state.typecheck_node(n))
                .collect();
            pdesc.nodes = new_nodes;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    #[test]
    fn test_typeof_fills_load_type() {
        let src = r#".source_language = "java"
type list = { header: *int }
define f(l: *list) : void {
  #entry:
    n0 = l->list.header
    ret null
}
"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        run(&mut module, &decls);

        // After type inference, the Load in the Let should have typ filled in
        if let Some(Decl::Proc(pdesc)) = module.decls.last() {
            let has_typed_load = pdesc.nodes.iter().any(|n| {
                n.instrs.iter().any(|i| match i {
                    Instr::Let {
                        exp: Exp::Load { typ: Some(_), .. },
                        ..
                    } => true,
                    _ => false,
                })
            });
            assert!(has_typed_load, "Load typ should be filled in");
        }
    }

    #[test]
    fn test_null_in_store_parses_as_const() {
        let src = r#".source_language = "java"
type list = { header: *int }
define f(l: *list) : void {
  #entry:
    store &l <- null: *list
    ret null
}
"#;
        let module = parse_module(src, "test.sil").unwrap();
        if let Some(Decl::Proc(pdesc)) = module.decls.last() {
            let store = &pdesc.nodes[0].instrs[0];
            if let Instr::Store { exp2, .. } = store {
                assert!(
                    matches!(exp2, Exp::Const(Const::Null)),
                    "store RHS should be Const(Null), got: {exp2:?}"
                );
            } else {
                panic!("expected Store instruction");
            }
        }
    }

    #[test]
    fn test_allocate_returns_ptr_type() {
        let src = r#".source_language = "java"
type Cell = { value: int }
define f() : void {
  #entry:
    n0 = __sil_allocate(<Cell>)
    ret null
}
"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        run(&mut module, &decls);

        // After type_check, re-infer to check what type n0 got
        if let Some(Decl::Proc(pdesc)) = module.decls.last() {
            let mut state = TypeState::new(&decls, pdesc);
            for node in &pdesc.nodes {
                for instr in &node.instrs {
                    state.typecheck_instr(instr);
                }
            }
            let n0_typ = state.idents.get(&0).cloned().unwrap_or(Typ::Void);
            assert!(
                matches!(&n0_typ, Typ::Ptr(inner, _) if matches!(inner.as_ref(), Typ::Struct(_))),
                "__sil_allocate should return *Struct, got: {n0_typ:?}"
            );
        }
    }

    #[test]
    fn test_ssa_params_registered() {
        let src = r#".source_language = "java"
define f(x: int) : void {
  #entry:
    n0: int = load &x
    jmp lab1(n0)
  #lab1(n1: int):
    ret null
}
"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        run(&mut module, &decls);

        // After type inference, n1 (SSA param in lab1) should be registered
        // Verify by checking that the module still has the SSA parameter
        if let Some(Decl::Proc(pdesc)) = module.decls.last() {
            let lab1 = pdesc
                .nodes
                .iter()
                .find(|n| n.label.value == "lab1")
                .unwrap();
            assert_eq!(lab1.ssa_parameters.len(), 1);
            assert_eq!(lab1.ssa_parameters[0].1, Typ::Int);
        }
    }

    #[test]
    fn test_arrow_deref_insertion() {
        let src = r#".source_language = "java"
type cell = { value: int; next: *cell }
type list = { header: *cell }
define f(l: *list) : void {
  #entry:
    store &l <- null: *list
    n1 = l->list.header
    ret null
}
"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        crate::transform::run(&mut module, &decls);

        // After full pipeline, the arrow syntax should produce:
        // 1. Load from &l (deref pointer variable)
        // 2. Load from field (deref struct field)
        if let Some(Decl::Proc(pdesc)) = module.decls.last() {
            let all_instrs: Vec<_> = pdesc.nodes.iter().flat_map(|n| n.instrs.iter()).collect();
            // Should have Load instructions with types filled in
            let load_count = all_instrs
                .iter()
                .filter(|i| matches!(i, Instr::Load { typ: Some(_), .. }))
                .count();
            assert!(
                load_count >= 2,
                "expected at least 2 typed Loads (deref + field), got {load_count}"
            );
        }
    }
}
