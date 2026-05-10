// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Textual-to-SIL conversion.
//!
//! Mirrors OCaml's `TextualSil.ml`. Converts a Textual `Module` into
//! SIL `Cfg` + `Tenv`.

use sil::annot::AnnotItem;
use sil::binop;
use sil::call_flags::CallFlags;
use sil::cfg::Cfg;
use sil::const_val;
use sil::exp;
use sil::fieldname;
use sil::ident;
use sil::instr;
use sil::int_lit::IntLit;
use sil::location;
use sil::mangled::Mangled;
use sil::procdesc;
use sil::procname;
use sil::pvar;
use sil::qualified_cpp_name::QualifiedCppName;
use sil::source_file::SourceFile;
use sil::strukt;
use sil::tenv::Tenv;
use sil::typ;
use sil::unop;

use std::collections::HashMap;

use crate::ast;
use crate::decls::DeclEnv;

/// Errors during Textual-to-SIL conversion.
#[derive(Clone, Debug)]
pub struct ConvError {
    pub loc: ast::Location,
    pub message: String,
}

impl std::fmt::Display for ConvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "conversion error at {}: {}", self.loc, self.message)
    }
}

/// Source language enum for driving conversion behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    C,
    Hack,
    Java,
    Python,
    Rust,
    Swift,
    ObjectiveC,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "c" | "C" => Some(Lang::C),
            "hack" | "Hack" => Some(Lang::Hack),
            "java" | "Java" => Some(Lang::Java),
            "python" | "Python" => Some(Lang::Python),
            "rust" | "Rust" => Some(Lang::Rust),
            "swift" | "Swift" => Some(Lang::Swift),
            "objc" | "ObjectiveC" => Some(Lang::ObjectiveC),
            _ => None,
        }
    }
}

// ===========================================================================
// Bridge modules — each mirrors a *Bridge module in TextualSil.ml
// ===========================================================================

/// Mirrors `LocationBridge`.
///
/// If a `LineMap` is provided, remaps the textual file line/col to the original
/// source line/col (from `@[line:col]` annotations or `// .line` directives).
fn location_to_sil(
    source_file: &SourceFile,
    loc: &ast::Location,
    line_map: Option<&crate::line_map::LineMap>,
) -> location::Location {
    match loc {
        ast::Location::Known { line, col } => {
            // Try to remap via line map
            if let Some(map) = line_map {
                if let Some(orig) = map.lookup(*line) {
                    return location::Location {
                        file: source_file.clone(),
                        line: orig.line as i32,
                        col: orig.col as i32,
                        macro_file_opt: None,
                        macro_line: -1,
                    };
                }
            }
            location::Location {
                file: source_file.clone(),
                line: *line as i32,
                col: *col as i32,
                macro_file_opt: None,
                macro_line: -1,
            }
        }
        ast::Location::Unknown => location::Location {
            file: source_file.clone(),
            line: -1,
            col: -1,
            macro_file_opt: None,
            macro_line: -1,
        },
    }
}

/// Mirrors `TypeNameBridge.to_sil`.
fn type_name_to_sil(lang: Lang, tname: &ast::TypeName) -> typ::TypeName {
    let value = &tname.name.value;
    match lang {
        Lang::Java => typ::TypeName::JavaClass(typ::JavaClassName(value.replace("::", "."))),
        Lang::Hack => typ::TypeName::HackClass(typ::HackClassName(value.clone())),
        Lang::Python => typ::TypeName::PythonClass(typ::PythonClassName(value.clone())),
        Lang::C | Lang::Rust => typ::TypeName::CStruct(QualifiedCppName::from_string(value)),
        Lang::Swift => typ::TypeName::SwiftClass(typ::SwiftClassName(value.clone())),
        Lang::ObjectiveC => typ::TypeName::ObjcClass(QualifiedCppName::from_string(value)),
    }
}

/// Mirrors `TypBridge.to_sil`.
fn typ_to_sil(lang: Lang, t: &ast::Typ) -> typ::Typ {
    match t {
        ast::Typ::Int => typ::Typ::int(typ::IKind::IInt),
        ast::Typ::Null => typ::Typ::int(typ::IKind::IInt),
        ast::Typ::Float => typ::Typ::float(typ::FKind::FFloat),
        ast::Typ::Void => typ::Typ::void(),
        ast::Typ::Fun(None) => typ::Typ::mk(typ::TypeDesc::Tfun(None)),
        ast::Typ::Fun(Some(proto)) => {
            let params = proto
                .params_type
                .iter()
                .map(|t| typ_to_sil(lang, t))
                .collect();
            let ret = typ_to_sil(lang, &proto.return_type);
            typ::Typ::mk(typ::TypeDesc::Tfun(Some(Box::new(
                typ::FunctionPrototype {
                    params_type: params,
                    return_type: Box::new(ret),
                },
            ))))
        }
        ast::Typ::Ptr(inner, _attrs) => {
            let inner_sil = typ_to_sil(lang, inner);
            typ::Typ::mk_ptr(inner_sil)
        }
        ast::Typ::Struct(name) => {
            let sil_name = type_name_to_sil(lang, name);
            typ::Typ::mk_struct(sil_name)
        }
        ast::Typ::Array(elt) => {
            let elt_sil = typ_to_sil(lang, elt);
            typ::Typ::mk_array(elt_sil, None, None)
        }
    }
}

/// Mirrors `IdentBridge.to_sil`.
fn ident_to_sil(id: ast::Ident) -> ident::Ident {
    ident::Ident::create_normal(ident::IdentName::from_string("n"), id)
}

/// Mirrors `ConstBridge.to_sil`.
fn const_to_sil(c: &ast::Const) -> const_val::Const {
    match c {
        ast::Const::Int(z) => const_val::Const::Cint(IntLit::of_big_int(z.clone())),
        ast::Const::Null => const_val::Const::Cint(IntLit::zero()),
        ast::Const::Str(s) => const_val::Const::Cstr(s.clone()),
        ast::Const::Float(f) => const_val::Const::Cfloat(const_val::OrderedFloat(*f)),
    }
}

/// Mirrors `ProcDeclBridge.to_sil` (simplified — creates a C-style procname).
/// Convert a Textual qualified proc name to a SIL Procname.
///
/// `arity` is the number of formals (for Hack/Python/Erlang, where arity
/// is part of the procname identity and distinguishes overloads).
fn procname_to_sil(
    lang: Lang,
    qname: &ast::QualifiedProcName,
    arity: Option<i32>,
) -> procname::Procname {
    let method = &qname.name.value;
    match lang {
        Lang::Java => {
            let class = match &qname.enclosing_class {
                ast::EnclosingClass::Enclosing(tn) => tn.name.value.replace("::", "."),
                ast::EnclosingClass::TopLevel => "$TOPLEVEL$CLASS$".to_string(),
            };
            procname::Procname::Java(procname::JavaProcname {
                class_name: typ::JavaClassName(class),
                method_name: method.clone(),
                parameters: Vec::new(),
                return_type: None,
                kind: procname::JavaKind::NonStatic,
            })
        }
        Lang::Hack => {
            let class = match &qname.enclosing_class {
                ast::EnclosingClass::Enclosing(tn) => {
                    Some(typ::HackClassName(tn.name.value.clone()))
                }
                ast::EnclosingClass::TopLevel => None,
            };
            procname::Procname::Hack(procname::HackProcname {
                class_name: class,
                function_name: method.clone(),
                arity,
            })
        }
        Lang::Python => {
            let class = match &qname.enclosing_class {
                ast::EnclosingClass::Enclosing(tn) => Some(tn.name.value.clone()),
                ast::EnclosingClass::TopLevel => None,
            };
            procname::Procname::Python(procname::PythonProcname {
                class_name: class,
                function_name: method.clone(),
                arity,
            })
        }
        _ => {
            // C/Rust/Swift/ObjC: arity is not part of the procname
            let full_name = match &qname.enclosing_class {
                ast::EnclosingClass::Enclosing(tn) => format!("{}.{}", tn, method),
                ast::EnclosingClass::TopLevel => method.clone(),
            };
            procname::Procname::c_from_string(&full_name)
        }
    }
}

fn call_result_typ(lang: Lang, decls: &DeclEnv, proc: &ast::QualifiedProcName) -> typ::Typ {
    decls
        .get_proc(proc)
        .map(|entry| typ_to_sil(lang, &entry.procdecl().result_type.typ))
        .unwrap_or_else(typ::Typ::void)
}

fn call_arg_typ(
    lang: Lang,
    decls: &DeclEnv,
    proc: &ast::QualifiedProcName,
    index: usize,
) -> typ::Typ {
    decls
        .get_proc(proc)
        .and_then(|entry| entry.procdecl().formals_types.as_ref())
        .and_then(|formals| formals.get(index))
        .map(|annotated| typ_to_sil(lang, &annotated.typ))
        .unwrap_or_else(typ::Typ::void)
}

/// Mirrors `FieldDeclBridge.to_sil`.
fn field_to_sil(lang: Lang, field: &ast::FieldDecl) -> strukt::Field {
    let class_name = type_name_to_sil(lang, &field.qualified_name.enclosing_class);
    strukt::Field {
        name: fieldname::Fieldname::make(class_name, &field.qualified_name.name.value),
        typ: typ_to_sil(lang, &field.typ),
        annot: AnnotItem::empty(),
    }
}

fn lvar_to_sil_pvar(
    decls: &DeclEnv,
    pname: &procname::Procname,
    name: &ast::VarName,
) -> pvar::Pvar {
    let mangled = Mangled::from_string(&name.value);
    if decls.get_global(&name.value).is_some() {
        pvar::Pvar::mk_global(mangled)
    } else {
        pvar::Pvar::mk(mangled, pname.clone())
    }
}

/// Mirrors `ExpBridge.to_sil` (simplified — handles the common cases).
// `lang` and `source_file` are threaded through for recursive calls where
// deeper expression kinds (Closure, Lfield) need them, even though the
// current simplified implementation only uses them in a subset of arms.
/// Strip __sil_cast for Store RHS only when wrapping non-zero constants.
/// This enables constant propagation (e.g., `uint16_t i = 1` flows as 1)
/// without creating new null paths from zero-casts.
/// Like exp_to_sil but strips __sil_cast for prune contexts.
///
/// In prune expressions, __sil_cast is just a type annotation for
/// comparison. Stripping it preserves the original abstract value,
/// enabling null check pruning. This does NOT affect Store/Load
/// contexts where the cast behavior matters.
fn exp_to_sil_for_prune(
    lang: Lang,
    source_file: &SourceFile,
    decls: &DeclEnv,
    pname: &procname::Procname,
    e: &ast::Exp,
    fallback_loc: &ast::Location,
) -> Result<exp::Exp, ConvError> {
    match e {
        ast::Exp::Call { proc, args, .. }
            if proc.name.value == "__sil_cast" && !args.is_empty() =>
        {
            exp_to_sil_for_prune(
                lang,
                source_file,
                decls,
                pname,
                &args[args.len() - 1],
                fallback_loc,
            )
        }
        ast::Exp::Call { proc, args, .. } => {
            let callee_name = &proc.name.value;
            // Handle __sil_* builtins → BinOp/UnOp with recursive prune handling
            if let Some(bop) = sil_builtin_to_binop(callee_name) {
                if args.len() == 2 {
                    let lhs = exp_to_sil_for_prune(
                        lang,
                        source_file,
                        decls,
                        pname,
                        &args[0],
                        fallback_loc,
                    )?;
                    let rhs = exp_to_sil_for_prune(
                        lang,
                        source_file,
                        decls,
                        pname,
                        &args[1],
                        fallback_loc,
                    )?;
                    return Ok(exp::Exp::BinOp(bop, Box::new(lhs), Box::new(rhs)));
                }
            }
            if let Some(uop) = sil_builtin_to_unop(callee_name) {
                if args.len() == 1 {
                    let inner = exp_to_sil_for_prune(
                        lang,
                        source_file,
                        decls,
                        pname,
                        &args[0],
                        fallback_loc,
                    )?;
                    return Ok(exp::Exp::UnOp(uop, Box::new(inner), None));
                }
            }
            // Fall back to normal exp_to_sil for non-builtin calls
            exp_to_sil(lang, source_file, decls, pname, e, fallback_loc)
        }
        // For non-Call expressions, delegate to normal exp_to_sil
        _ => exp_to_sil(lang, source_file, decls, pname, e, fallback_loc),
    }
}

fn unsupported_expression_error(exp: &ast::Exp, fallback_loc: &ast::Location) -> ConvError {
    let kind = match exp {
        ast::Exp::Closure { .. } => "Closure",
        ast::Exp::Apply { .. } => "Apply",
        ast::Exp::If { .. } => "If",
        _ => unreachable!("called unsupported_expression_error on supported expression"),
    };
    ConvError {
        loc: fallback_loc.clone(),
        message: format!("unsupported Textual expression `{kind}` reached SIL conversion"),
    }
}

#[allow(clippy::only_used_in_recursion)]
fn exp_to_sil(
    lang: Lang,
    source_file: &SourceFile,
    decls: &DeclEnv,
    pname: &procname::Procname,
    e: &ast::Exp,
    fallback_loc: &ast::Location,
) -> Result<exp::Exp, ConvError> {
    match e {
        ast::Exp::Var(id) => Ok(exp::Exp::Var(ident_to_sil(*id))),
        ast::Exp::Const(c) => Ok(exp::Exp::Const(const_to_sil(c))),
        ast::Exp::Lvar(name) => {
            let pv = lvar_to_sil_pvar(decls, pname, name);
            Ok(exp::Exp::Lvar(pv))
        }
        ast::Exp::Load { exp, typ: _ } => {
            // In SIL, loads are instructions, not expressions.
            // During to_sil conversion, the Let-binding that contains
            // this Load expression will be expanded into a Load instruction.
            // For now, just translate the inner expression.
            exp_to_sil(lang, source_file, decls, pname, exp, fallback_loc)
        }
        ast::Exp::Field { exp, field } => {
            let inner = exp_to_sil(lang, source_file, decls, pname, exp, fallback_loc)?;
            let class_name = type_name_to_sil(lang, &field.enclosing_class);
            let sil_field = fieldname::Fieldname::make(class_name.clone(), &field.name.value);
            let struct_typ = typ::Typ::mk_struct(class_name);
            Ok(exp::Exp::Lfield(
                exp::LfieldObjData {
                    exp: Box::new(inner),
                    is_implicit: false,
                },
                sil_field,
                struct_typ,
            ))
        }
        ast::Exp::Index(e1, e2) => {
            let sil_e1 = exp_to_sil(lang, source_file, decls, pname, e1, fallback_loc)?;
            let sil_e2 = exp_to_sil(lang, source_file, decls, pname, e2, fallback_loc)?;
            Ok(exp::Exp::Lindex(Box::new(sil_e1), Box::new(sil_e2)))
        }
        ast::Exp::Call { proc, args, .. } => {
            let callee_name = &proc.name.value;
            // Check for __sil_* builtins → BinOp/UnOp expressions
            if let Some(bop) = sil_builtin_to_binop(callee_name) {
                if args.len() == 2 {
                    let lhs = exp_to_sil(lang, source_file, decls, pname, &args[0], fallback_loc)?;
                    let rhs = exp_to_sil(lang, source_file, decls, pname, &args[1], fallback_loc)?;
                    return Ok(exp::Exp::BinOp(bop, Box::new(lhs), Box::new(rhs)));
                }
            }
            if let Some(uop) = sil_builtin_to_unop(callee_name) {
                if args.len() == 1 {
                    let inner =
                        exp_to_sil(lang, source_file, decls, pname, &args[0], fallback_loc)?;
                    return Ok(exp::Exp::UnOp(uop, Box::new(inner), None));
                }
            }

            // __sil_cast(<typ>, val) → Cast(typ, val)
            // Cross-ref: OCaml TextualSil.ml ExpBridge.to_sil always lowers
            // cast builtins to SilExp.Cast; keep the value connected in all
            // expression contexts, including zero constants.
            if callee_name == "__sil_cast" && args.len() == 2 {
                let typ_arg = exp_to_sil(lang, source_file, decls, pname, &args[0], fallback_loc)?;
                let val_arg = exp_to_sil(lang, source_file, decls, pname, &args[1], fallback_loc)?;
                if let exp::Exp::Sizeof(data) = &typ_arg {
                    return Ok(exp::Exp::Cast(data.typ.clone(), Box::new(val_arg)));
                }
            }

            // __sil_cfun("name") → Const(Cfun(C.from_string(name)))
            // Function pointer constant: preserves procedure identity through
            // the textual roundtrip. Cross-ref: OCaml TextualSil.ml ExpBridge.
            if callee_name == "__sil_cfun" {
                if let Some(ast::Exp::Const(ast::Const::Str(name))) = args.first() {
                    let pn = procname::Procname::c_from_string(name);
                    return Ok(exp::Exp::Const(const_val::Const::Cfun(pn)));
                }
            }

            // Regular calls: create a function constant reference.
            let arity = Some(args.len() as i32);
            let callee = procname_to_sil(lang, proc, arity);
            Ok(exp::Exp::Const(const_val::Const::Cfun(callee)))
        }
        ast::Exp::Typ(t) => {
            // Type expressions like `<Node>` — used for allocate builtins
            let sil_t = typ_to_sil(lang, t);
            Ok(exp::Exp::Sizeof(exp::SizeofData {
                typ: sil_t,
                nbytes: None,
                dynamic_length: None,
                nullable: false,
            }))
        }
        ast::Exp::Closure { .. } | ast::Exp::Apply { .. } | ast::Exp::If { .. } => {
            Err(unsupported_expression_error(e, fallback_loc))
        }
    }
}

// ===========================================================================
// Struct conversion
// ===========================================================================

fn struct_to_sil(lang: Lang, s: &ast::Struct) -> (typ::TypeName, strukt::Struct) {
    let sil_name = type_name_to_sil(lang, &s.name);
    let fields = s.fields.iter().map(|f| field_to_sil(lang, f)).collect();
    let supers = s
        .supers
        .iter()
        .map(|sup| type_name_to_sil(lang, sup))
        .collect();

    let sil_struct = strukt::Struct {
        fields,
        supers,
        ..strukt::Struct::default()
    };

    (sil_name, sil_struct)
}

// ===========================================================================
// Terminator → CFG edges
// ===========================================================================

/// Collect successor node IDs from a terminator.
/// `exit_id` is the exit node ID (for `Ret`/`Unreachable`).
fn terminator_succs(
    term: &ast::Terminator,
    label_to_id: &HashMap<String, procdesc::NodeId>,
    exit_id: procdesc::NodeId,
) -> Vec<procdesc::NodeId> {
    match term {
        ast::Terminator::Ret(_) | ast::Terminator::Unreachable | ast::Terminator::Throw(_) => {
            vec![exit_id]
        }
        ast::Terminator::Jump(calls) if calls.is_empty() => {
            // Empty jmp = fall through to exit (OCaml textual convention)
            vec![exit_id]
        }
        ast::Terminator::Jump(calls) => calls
            .iter()
            .filter_map(|call| label_to_id.get(&call.label.value).copied())
            .collect(),
        ast::Terminator::If { then_, else_, .. } => {
            let mut succs = terminator_succs(then_, label_to_id, exit_id);
            succs.extend(terminator_succs(else_, label_to_id, exit_id));
            succs
        }
    }
}

// ===========================================================================
// Procedure conversion
// ===========================================================================

fn procdesc_to_sil(
    lang: Lang,
    source_file: &SourceFile,
    decls: &DeclEnv,
    pdesc: &ast::ProcDesc,
    line_map: Option<&crate::line_map::LineMap>,
) -> Result<procdesc::Procdesc, Vec<ConvError>> {
    let arity = pdesc
        .procdecl
        .formals_types
        .as_ref()
        .map(|f| f.len() as i32);
    let pname = procname_to_sil(lang, &pdesc.procdecl.qualified_name, arity);
    let ret_type = typ_to_sil(lang, &pdesc.procdecl.result_type.typ);
    let loc = location_to_sil(source_file, &pdesc.exit_loc, line_map);

    let mut sil_pdesc = procdesc::Procdesc::new(pname.clone(), ret_type.clone(), loc);

    // Formals
    sil_pdesc.formals = pdesc
        .params
        .iter()
        .zip(pdesc.procdecl.formals_types.as_deref().unwrap_or_default())
        .map(|(name, at)| {
            (
                Mangled::from_string(&name.value),
                typ_to_sil(lang, &at.typ),
                AnnotItem::empty(),
            )
        })
        .collect();

    // Locals
    sil_pdesc.locals = pdesc
        .locals
        .iter()
        .map(|(name, at)| procdesc::VarData {
            name: Mangled::from_string(&name.value),
            typ: typ_to_sil(lang, &at.typ),
            modify_in_block: false,
            is_constexpr: false,
            is_declared_unused: false,
            is_structured_binding: false,
            has_cleanup_attribute: false,
        })
        .collect();

    // Pass 1: create all nodes and build label→node_id map.
    let mut label_to_id: HashMap<String, procdesc::NodeId> = HashMap::new();
    let mut errors = Vec::new();

    for (i, node) in pdesc.nodes.iter().enumerate() {
        let node_loc = location_to_sil(source_file, &node.label_loc, line_map);
        let kind = procdesc::NodeKind::StmtNode(procdesc::StmtNodeKind::MethodBody);

        let mut sil_instrs = Vec::new();
        for textual_instr in &node.instrs {
            match instr_to_sil(lang, source_file, decls, &pname, textual_instr, line_map) {
                Ok(Some(sil_instr)) => sil_instrs.push(sil_instr),
                Ok(None) => {}
                Err(err) => errors.push(err),
            }
        }

        // Ret(exp) → Store { __return <- exp } before jumping to exit.
        // This mirrors OCaml's `write_to_ret_var` in TextualSil.ml.
        if let ast::Terminator::Ret(ret_exp) = &node.last {
            let ret_pvar = pvar::Pvar::mk(Mangled::from_string("__return"), pname.clone());
            match exp_to_sil(lang, source_file, decls, &pname, ret_exp, &node.last_loc) {
                Ok(sil_ret_exp) => {
                    let ret_loc = location_to_sil(source_file, &node.last_loc, line_map);
                    sil_instrs.push(instr::Instr::Store {
                        e1: Box::new(exp::Exp::Lvar(ret_pvar)),
                        typ: ret_type.clone(),
                        e2: Box::new(sil_ret_exp),
                        loc: ret_loc,
                    });
                }
                Err(err) => errors.push(err),
            }
        }

        let node_id = sil_pdesc.add_node(kind, sil_instrs, node_loc);
        label_to_id.insert(node.label.value.clone(), node_id);

        // Connect start → first node.
        if i == 0 {
            sil_pdesc.set_succs(0, vec![node_id]);
        }
    }

    // Pass 2: wire up CFG edges from terminators.
    for node in &pdesc.nodes {
        let from_id = label_to_id[&node.label.value];
        let succs = terminator_succs(&node.last, &label_to_id, 1);
        if !succs.is_empty() {
            sil_pdesc.set_succs(from_id, succs);
        }
    }

    // Store-textual export can emit declaration-like empty `define`s with the
    // canonical `#node_0: @?; jmp  @?` body shape. Treat those as undefined so
    // merged analysis does not mistake them for real bodies.
    sil_pdesc.is_defined = !sil_pdesc.is_empty_body();

    if errors.is_empty() {
        Ok(sil_pdesc)
    } else {
        Err(errors)
    }
}

fn textual_prune_is_then_branch(exp: &ast::Exp) -> bool {
    !matches!(
        exp,
        ast::Exp::Call { proc, args, .. }
            if proc.name.value == "__sil_lnot" && args.len() == 1
    )
}

/// Convert a single Textual instruction to a SIL instruction.
fn instr_to_sil(
    lang: Lang,
    source_file: &SourceFile,
    decls: &DeclEnv,
    pname: &procname::Procname,
    textual_instr: &ast::Instr,
    line_map: Option<&crate::line_map::LineMap>,
) -> Result<Option<instr::Instr>, ConvError> {
    let loc = |l: &ast::Location| location_to_sil(source_file, l, line_map);

    match textual_instr {
        ast::Instr::Load {
            id,
            exp,
            typ,
            loc: l,
        } => {
            let sil_id = ident_to_sil(*id);
            let sil_exp = exp_to_sil(lang, source_file, decls, pname, exp, l)?;
            let sil_typ = typ
                .as_ref()
                .map(|t| typ_to_sil(lang, t))
                .unwrap_or_else(typ::Typ::void);
            Ok(Some(instr::Instr::Load {
                id: sil_id,
                e: sil_exp,
                typ: sil_typ,
                loc: loc(l),
            }))
        }
        ast::Instr::Store {
            exp1,
            typ,
            exp2,
            loc: l,
        } => {
            let sil_e1 = exp_to_sil(lang, source_file, decls, pname, exp1, l)?;
            let sil_e2 = exp_to_sil(lang, source_file, decls, pname, exp2, l)?;
            let sil_typ = typ
                .as_ref()
                .map(|t| typ_to_sil(lang, t))
                .unwrap_or_else(typ::Typ::void);
            Ok(Some(instr::Instr::Store {
                e1: Box::new(sil_e1),
                typ: sil_typ,
                e2: Box::new(sil_e2),
                loc: loc(l),
            }))
        }
        ast::Instr::Prune { exp, loc: l } => {
            // Use prune-specific expression conversion that strips __sil_cast.
            // This is safe because casts in prune expressions are just type
            // annotations for comparison, and stripping them enables proper
            // null check pruning (e.g., `if (ptr != NULL)` uses
            // `__sil_ne(__sil_cast(<int>, ptr), __sil_cast(<int>, 0))`).
            // Cross-ref: OCaml TextualSil.ml is_cast_builtin +
            // PulseOperations.prune which evaluates without casts.
            let sil_exp = exp_to_sil_for_prune(lang, source_file, decls, pname, exp, l)?;
            Ok(Some(instr::Instr::Prune {
                exp: sil_exp,
                loc: loc(l),
                is_then_branch: textual_prune_is_then_branch(exp),
                if_kind: instr::IfKind::If,
            }))
        }
        ast::Instr::Let { id, exp, loc: l } => {
            // Let bindings in Textual need to be expanded:
            // - If exp is a Call to __sil_* builtin → SIL BinOp/UnOp expression
            // - If exp is a Call → SIL Call instruction
            // - If exp is a Load → SIL Load instruction
            // - Otherwise → SIL Load of the expression
            let ret_id = id
                .map(ident_to_sil)
                .unwrap_or_else(ident::Ident::create_none);
            match exp {
                ast::Exp::Call { proc, args, kind } => {
                    let callee_name = &proc.name.value;

                    // Try __sil_* builtin dispatch (BinOp, UnOp, allocate, cast)
                    if let Some(sil_instr) = sil_builtin_to_instr(
                        callee_name,
                        args,
                        lang,
                        source_file,
                        decls,
                        pname,
                        ret_id.clone(),
                        l,
                        &loc(l),
                    )? {
                        return Ok(Some(sil_instr));
                    }

                    // Regular call
                    let call_arity = Some(args.len() as i32);
                    let callee = procname_to_sil(lang, proc, call_arity);
                    let ret_typ = call_result_typ(lang, decls, proc);
                    let fun_exp = exp::Exp::Const(const_val::Const::Cfun(callee));
                    let sil_args: Vec<(exp::Exp, typ::Typ)> = args
                        .iter()
                        .enumerate()
                        .map(|(i, a)| {
                            Ok((
                                exp_to_sil(lang, source_file, decls, pname, a, l)?,
                                call_arg_typ(lang, decls, proc, i),
                            ))
                        })
                        .collect::<Result<Vec<_>, ConvError>>()?;
                    let flags = CallFlags {
                        cf_virtual: matches!(kind, ast::CallKind::Virtual),
                        ..Default::default()
                    };
                    Ok(Some(instr::Instr::Call {
                        ret: (ret_id, ret_typ),
                        fun_exp,
                        args: sil_args,
                        loc: loc(l),
                        flags,
                    }))
                }
                _ => {
                    let sil_exp = exp_to_sil(lang, source_file, decls, pname, exp, l)?;
                    Ok(Some(instr::Instr::Load {
                        id: ret_id,
                        e: sil_exp,
                        typ: typ::Typ::void(),
                        loc: loc(l),
                    }))
                }
            }
        }
    }
}

// ===========================================================================
// __sil_* builtin dispatch
// ===========================================================================
//
// Unified handler for all `__sil_*` Textual builtins. Mirrors OCaml's
// `Textual.ProcDecl.binop_table`, `unop_table`, and `is_allocate_*_builtin`.
//
// Categories:
// - BinOp: `__sil_eq(x,y)` → `Load { id, BinOp(Eq, x, y) }`
// - UnOp: `__sil_lnot(x)` → `Load { id, UnOp(LNot, x) }`
// - Allocate: `__sil_allocate(<T>)` → `Call { __new, [Sizeof(T)] }`
// - Cast: `__sil_cast(<T>, x)` → `Load { id, Cast(T, x) }`

fn sil_builtin_to_binop(name: &str) -> Option<binop::Binop> {
    // OCaml's dump-textual emits type-suffixed variants like __sil_plusa_int,
    // __sil_plusa_uint, __sil_mult_ulong, etc. The integer kind doesn't affect
    // the formula — we map all variants to the base operation.
    match name {
        "__sil_plusa" | "__sil_plusa_int" | "__sil_plusa_uint" | "__sil_plusa_ulong" => {
            Some(binop::Binop::PlusA(None))
        }
        "__sil_pluspi" => Some(binop::Binop::PlusPI),
        "__sil_minusa" | "__sil_minusa_int" | "__sil_minusa_uint" => {
            Some(binop::Binop::MinusA(None))
        }
        "__sil_minuspi" => Some(binop::Binop::MinusPI),
        "__sil_minuspp" => Some(binop::Binop::MinusPP),
        "__sil_mult" | "__sil_mult_int" | "__sil_mult_ulong" => Some(binop::Binop::Mult(None)),
        "__sil_div" | "__sil_divi" => Some(binop::Binop::DivI),
        "__sil_divf" => Some(binop::Binop::DivF),
        "__sil_mod" => Some(binop::Binop::Mod),
        "__sil_shiftlt" => Some(binop::Binop::Shiftlt),
        "__sil_shiftrt" => Some(binop::Binop::Shiftrt),
        "__sil_lt" => Some(binop::Binop::Lt),
        "__sil_gt" => Some(binop::Binop::Gt),
        "__sil_le" => Some(binop::Binop::Le),
        "__sil_ge" => Some(binop::Binop::Ge),
        "__sil_eq" => Some(binop::Binop::Eq),
        "__sil_ne" => Some(binop::Binop::Ne),
        "__sil_band" => Some(binop::Binop::BAnd),
        "__sil_bxor" => Some(binop::Binop::BXor),
        "__sil_bor" => Some(binop::Binop::BOr),
        "__sil_land" => Some(binop::Binop::LAnd),
        "__sil_lor" => Some(binop::Binop::LOr),
        _ => None,
    }
}

fn sil_builtin_to_unop(name: &str) -> Option<unop::Unop> {
    match name {
        "__sil_neg" => Some(unop::Unop::Neg),
        "__sil_bnot" => Some(unop::Unop::BNot),
        "__sil_lnot" => Some(unop::Unop::LNot),
        _ => None,
    }
}

fn metadata_builtin_error(
    textual_loc: &ast::Location,
    name: &str,
    message: impl Into<String>,
) -> ConvError {
    ConvError {
        loc: textual_loc.clone(),
        message: format!("invalid `{name}` metadata builtin: {}", message.into()),
    }
}

fn metadata_i32_arg(
    name: &str,
    arg_name: &str,
    exp: &ast::Exp,
    textual_loc: &ast::Location,
) -> Result<i32, ConvError> {
    match exp {
        ast::Exp::Const(ast::Const::Int(value)) => value.to_string().parse::<i32>().map_err(|_| {
            metadata_builtin_error(textual_loc, name, format!("{arg_name} must fit in i32"))
        }),
        _ => Err(metadata_builtin_error(
            textual_loc,
            name,
            format!("{arg_name} must be an integer constant, got {exp:?}"),
        )),
    }
}

fn metadata_pvar_arg(
    name: &str,
    arg_name: &str,
    decls: &DeclEnv,
    pname: &procname::Procname,
    exp: &ast::Exp,
    textual_loc: &ast::Location,
) -> Result<pvar::Pvar, ConvError> {
    match exp {
        ast::Exp::Lvar(var_name) => Ok(lvar_to_sil_pvar(decls, pname, var_name)),
        _ => Err(metadata_builtin_error(
            textual_loc,
            name,
            format!("{arg_name} must be an lvar, got {exp:?}"),
        )),
    }
}

fn metadata_var_arg(
    name: &str,
    decls: &DeclEnv,
    pname: &procname::Procname,
    exp: &ast::Exp,
    textual_loc: &ast::Location,
) -> Result<sil::var::Var, ConvError> {
    match exp {
        ast::Exp::Lvar(var_name) => Ok(sil::var::Var::of_pvar(lvar_to_sil_pvar(
            decls, pname, var_name,
        ))),
        ast::Exp::Var(id) => Ok(sil::var::Var::of_id(ident_to_sil(*id))),
        _ => Err(metadata_builtin_error(
            textual_loc,
            name,
            format!("exit_scope args must be lvars or logical vars, got {exp:?}"),
        )),
    }
}

fn metadata_typ_arg(
    name: &str,
    arg_name: &str,
    lang: Lang,
    exp: &ast::Exp,
    textual_loc: &ast::Location,
) -> Result<typ::Typ, ConvError> {
    match exp {
        ast::Exp::Typ(textual_typ) => Ok(typ_to_sil(lang, textual_typ)),
        _ => Err(metadata_builtin_error(
            textual_loc,
            name,
            format!("{arg_name} must be a type expression, got {exp:?}"),
        )),
    }
}

fn sil_builtin_to_metadata(
    name: &str,
    args: &[ast::Exp],
    lang: Lang,
    decls: &DeclEnv,
    pname: &procname::Procname,
    textual_loc: &ast::Location,
    loc: &location::Location,
) -> Result<Option<instr::InstrMetadata>, ConvError> {
    // Cross-ref: OCaml TextualOfSil.ml `InstrBridge.of_sil_metadata`.
    let metadata = match name {
        "__sil_metadata_abstract" => {
            if !args.is_empty() {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 0 args, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::Abstract(loc.clone())
        }
        "__sil_metadata_catch_entry" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::CatchEntry {
                try_id: metadata_i32_arg(name, "try_id", &args[0], textual_loc)?,
                loc: loc.clone(),
            }
        }
        "__sil_metadata_exit_scope" => instr::InstrMetadata::ExitScope(
            args.iter()
                .map(|arg| metadata_var_arg(name, decls, pname, arg, textual_loc))
                .collect::<Result<Vec<_>, ConvError>>()?,
            loc.clone(),
        ),
        "__sil_metadata_nullify" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::Nullify(
                metadata_pvar_arg(name, "pvar", decls, pname, &args[0], textual_loc)?,
                loc.clone(),
            )
        }
        "__sil_metadata_loop_back_edge" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::LoopBackEdge {
                header_id: metadata_i32_arg(name, "header_id", &args[0], textual_loc)?,
            }
        }
        "__sil_metadata_loop_entry" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::LoopEntry {
                header_id: metadata_i32_arg(name, "header_id", &args[0], textual_loc)?,
            }
        }
        "__sil_metadata_loop_exit" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::LoopExit {
                header_id: metadata_i32_arg(name, "header_id", &args[0], textual_loc)?,
            }
        }
        "__sil_metadata_skip" => {
            if !args.is_empty() {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 0 args, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::Skip
        }
        "__sil_metadata_try_entry" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::TryEntry {
                try_id: metadata_i32_arg(name, "try_id", &args[0], textual_loc)?,
                loc: loc.clone(),
            }
        }
        "__sil_metadata_try_exit" => {
            if args.len() != 1 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 1 arg, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::TryExit {
                try_id: metadata_i32_arg(name, "try_id", &args[0], textual_loc)?,
                loc: loc.clone(),
            }
        }
        "__sil_metadata_variable_lifetime_begins" => {
            if args.len() != 2 {
                return Err(metadata_builtin_error(
                    textual_loc,
                    name,
                    format!("expected 2 args, got {}", args.len()),
                ));
            }
            instr::InstrMetadata::VariableLifetimeBegins {
                pvar: metadata_pvar_arg(name, "pvar", decls, pname, &args[0], textual_loc)?,
                typ: metadata_typ_arg(name, "typ", lang, &args[1], textual_loc)?,
                loc: loc.clone(),
                is_cpp_structured_binding: false,
            }
        }
        _ => return Ok(None),
    };

    Ok(Some(metadata))
}

#[allow(clippy::too_many_arguments)]
fn sil_builtin_to_instr(
    name: &str,
    args: &[ast::Exp],
    lang: Lang,
    source_file: &SourceFile,
    decls: &DeclEnv,
    pname: &procname::Procname,
    ret_id: ident::Ident,
    textual_loc: &ast::Location,
    loc: &location::Location,
) -> Result<Option<instr::Instr>, ConvError> {
    if let Some(bop) = sil_builtin_to_binop(name) {
        if args.len() == 2 {
            let lhs = exp_to_sil(lang, source_file, decls, pname, &args[0], textual_loc)?;
            let rhs = exp_to_sil(lang, source_file, decls, pname, &args[1], textual_loc)?;
            return Ok(Some(instr::Instr::Load {
                id: ret_id,
                e: exp::Exp::BinOp(bop, Box::new(lhs), Box::new(rhs)),
                typ: typ::Typ::void(),
                loc: loc.clone(),
            }));
        }
    }

    if let Some(uop) = sil_builtin_to_unop(name) {
        if args.len() == 1 {
            let inner = exp_to_sil(lang, source_file, decls, pname, &args[0], textual_loc)?;
            return Ok(Some(instr::Instr::Load {
                id: ret_id,
                e: exp::Exp::UnOp(uop, Box::new(inner), None),
                typ: typ::Typ::void(),
                loc: loc.clone(),
            }));
        }
    }

    if let Some(metadata) =
        sil_builtin_to_metadata(name, args, lang, decls, pname, textual_loc, loc)?
    {
        return Ok(Some(instr::Instr::Metadata(metadata)));
    }

    // Allocate builtins → BuiltinDecl.__new / __new_array
    match name {
        "__sil_allocate" | "__sil_allocate_array" => {
            let sil_args: Vec<(exp::Exp, typ::Typ)> = args
                .iter()
                .map(|a| {
                    Ok((
                        exp_to_sil(lang, source_file, decls, pname, a, textual_loc)?,
                        typ::Typ::void(),
                    ))
                })
                .collect::<Result<Vec<_>, ConvError>>()?;
            let builtin = if name == "__sil_allocate" {
                sil::builtin_decl::__new()
            } else {
                sil::builtin_decl::__new_array()
            };
            return Ok(Some(instr::Instr::Call {
                ret: (ret_id, typ::Typ::void()),
                fun_exp: exp::Exp::Const(const_val::Const::Cfun(builtin)),
                args: sil_args,
                loc: loc.clone(),
                flags: CallFlags::default(),
            }));
        }
        _ => {}
    }

    // __sil_cfun("name") → Load { Const(Cfun(name)) }
    // Function pointer constant, preserving procedure identity through textual.
    // Cross-ref: OCaml TextualSil.ml is_cfun_builtin.
    if name == "__sil_cfun" {
        if let Some(ast::Exp::Const(ast::Const::Str(fname))) = args.first() {
            let pn = procname::Procname::c_from_string(fname);
            return Ok(Some(instr::Instr::Load {
                id: ret_id,
                e: exp::Exp::Const(const_val::Const::Cfun(pn)),
                typ: typ::Typ::void(),
                loc: loc.clone(),
            }));
        }
    }

    // Cast builtin
    if name == "__sil_cast" && args.len() == 2 {
        let typ_arg = exp_to_sil(lang, source_file, decls, pname, &args[0], textual_loc)?;
        let val_arg = exp_to_sil(lang, source_file, decls, pname, &args[1], textual_loc)?;
        // Cast(typ, val) — extract the type from the Sizeof expression
        if let exp::Exp::Sizeof(data) = &typ_arg {
            return Ok(Some(instr::Instr::Load {
                id: ret_id,
                e: exp::Exp::Cast(data.typ.clone(), Box::new(val_arg)),
                typ: typ::Typ::void(),
                loc: loc.clone(),
            }));
        }
    }

    Ok(None)
}

// ===========================================================================
// Module conversion — the main entry point
// ===========================================================================

/// Convert a Textual module into SIL Cfg + Tenv.
///
/// Mirrors OCaml's `TextualSil.module_to_sil`.
///
/// If `line_map` is provided, locations are remapped from textual line numbers
/// to original source line numbers (from `@[line:col]` or `// .line` directives).
pub fn module_to_sil(module: &ast::Module, decls: &DeclEnv) -> Result<(Cfg, Tenv), Vec<ConvError>> {
    module_to_sil_with_line_map(module, decls, None)
}

/// Convert a Textual module into SIL Cfg + Tenv with an optional line map.
pub fn module_to_sil_with_line_map(
    module: &ast::Module,
    decls: &DeclEnv,
    line_map: Option<&crate::line_map::LineMap>,
) -> Result<(Cfg, Tenv), Vec<ConvError>> {
    let lang_str = module.lang().unwrap_or("c");
    let lang = Lang::parse(lang_str).unwrap_or(Lang::C);
    let source_file = SourceFile::new(&module.source_file);

    let mut cfg = Cfg::new();
    let mut tenv = Tenv::new();
    let mut errors = Vec::new();

    for decl in &module.decls {
        match decl {
            ast::Decl::Struct(s) => {
                let (sil_name, sil_struct) = struct_to_sil(lang, s);
                tenv.insert(sil_name, sil_struct);
            }
            ast::Decl::Proc(pdesc) => {
                match procdesc_to_sil(lang, &source_file, decls, pdesc, line_map) {
                    Ok(sil_pdesc) => cfg.add_proc_desc(sil_pdesc),
                    Err(mut proc_errors) => errors.append(&mut proc_errors),
                }
            }
            ast::Decl::Global(_) | ast::Decl::Procdecl(_) => {
                // Globals and declarations don't produce CFG entries
            }
        }
    }

    if errors.is_empty() {
        Ok((cfg, tenv))
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;
    use std::collections::BTreeSet;

    #[test]
    fn test_basic_conversion() {
        let src = r#".source_language = "java"

type node = { val: int; next: *node }

define f(x: int) : int {
  #entry:
    n0 : int = load &x
    ret n0
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _errors) = DeclEnv::from_module(&module);
        let (cfg, tenv) = module_to_sil(&module, &decls).unwrap();

        assert_eq!(cfg.num_procs(), 1);
        assert_eq!(tenv.len(), 1);

        // Check the procedure
        let pdesc = cfg.iter_proc_descs().next().unwrap();
        assert_eq!(pdesc.formals.len(), 1);
        // start(0) + exit(1) + entry block(2) = 3 nodes
        assert_eq!(pdesc.nodes.len(), 3);
    }

    #[test]
    fn test_conversion_with_calls() {
        let src = r#".source_language = "java"

declare plus(int, int) : int

define f(x: int, y: int) : int {
  #entry:
    n0 : int = load &x
    n1 : int = load &y
    n2 = plus(n0, n1)
    ret n2
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _tenv) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().unwrap();
        // 4 instructions: 2 loads + 1 call + 1 store(__return)
        let instrs: Vec<_> = pdesc.iter_instrs().collect();
        assert_eq!(instrs.len(), 4);
        match instrs[2].1 {
            instr::Instr::Call {
                ret: (_, ref ret_typ),
                ref args,
                ..
            } => {
                assert!(
                    ret_typ.is_int(),
                    "declared call result type should be preserved"
                );
                assert_eq!(args.len(), 2);
                assert!(
                    args.iter().all(|(_, typ)| typ.is_int()),
                    "declared formal types should be preserved on call arguments"
                );
            }
            other => panic!("expected call instruction, got {other:?}"),
        }
    }

    #[test]
    fn test_virtual_call_sets_call_flag() {
        let src = r#".source_language = "hack"

type Num .abstract {}
declare Num.value(*Num): int

define f(arg: *Num) : int {
  #entry:
    n0: *Num = load &arg
    n1 = n0.Num.value()
    ret n1
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg
            .iter_proc_descs()
            .find(|pdesc| format!("{}", pdesc.proc_name).contains("f#1"))
            .expect("f proc should exist");
        let instrs: Vec<_> = pdesc.iter_instrs().collect();
        assert!(instrs.iter().any(|(_, instr)| matches!(
            instr,
            instr::Instr::Call { flags, .. } if flags.cf_virtual
        )));
    }

    #[test]
    fn test_struct_with_supers() {
        let src = r#".source_language = "java"

type Base = { x: int }
type Child extends Base = { y: float }
"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (_, tenv) = module_to_sil(&module, &decls).unwrap();

        assert_eq!(tenv.len(), 2);
        // Child should have Base as a super
        let child_name = typ::TypeName::JavaClass(typ::JavaClassName("Child".into()));
        let child = tenv.lookup(&child_name).unwrap();
        assert_eq!(child.supers.len(), 1);
        assert_eq!(child.fields.len(), 1);
    }

    #[test]
    fn test_store_instruction() {
        let src = r#".source_language = "java"

define f(x: int) : void {
  #entry:
    store &x <- 42 : int
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().unwrap();
        let instrs: Vec<_> = pdesc.iter_instrs().collect();
        // 2 instructions: 1 store + 1 store(__return for `ret null`)
        assert_eq!(instrs.len(), 2);
        assert!(matches!(instrs[0].1, instr::Instr::Store { .. }));
    }

    #[test]
    fn test_multiple_procedures() {
        let src = r#".source_language = "java"

define f() : void {
  #entry:
    ret null
}

define g() : void {
  #entry:
    ret null
}
"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        assert_eq!(cfg.num_procs(), 2);
    }

    #[test]
    fn test_hack_type_names() {
        let src = r#".source_language = "hack"

type MyClass = { field: int }
"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (_, tenv) = module_to_sil(&module, &decls).unwrap();

        let name = typ::TypeName::HackClass(typ::HackClassName("MyClass".into()));
        assert!(tenv.lookup(&name).is_some());
    }

    #[test]
    fn test_global_lvar_is_lowered_to_global_pvar() {
        let src = r#".source_language = "c"

global fp : *int

define f() : *int {
  #entry:
    n0 : *int = load &fp
    ret n0
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().unwrap();
        let instrs: Vec<_> = pdesc.iter_instrs().collect();
        let global_pvar = match instrs[0].1 {
            instr::Instr::Load {
                e: exp::Exp::Lvar(ref pv),
                ..
            } => pv,
            other => panic!("expected load from global lvar, got {other:?}"),
        };

        assert!(
            global_pvar.is_global(),
            "expected global pvar, got {global_pvar:?}"
        );
    }

    #[test]
    fn test_empty_define_marks_proc_undefined() {
        let src = r#".source_language = "c"

define f() : void {
  #node_0: @?
      jmp  @?

} @[1:1]
"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().unwrap();
        assert!(pdesc.is_empty_body());
        assert!(!pdesc.is_defined);
    }

    #[test]
    fn test_metadata_builtins_lower_to_sil_metadata() {
        let src = r#".source_language = "c"

define f(x: int) : void {
  #entry:
    _ = __sil_metadata_abstract()
    _ = __sil_metadata_nullify(&x)
    _ = __sil_metadata_exit_scope(&x, n0)
    _ = __sil_metadata_loop_entry(7)
    _ = __sil_metadata_loop_back_edge(7)
    _ = __sil_metadata_loop_exit(7)
    _ = __sil_metadata_skip()
    _ = __sil_metadata_try_entry(3)
    _ = __sil_metadata_catch_entry(3)
    _ = __sil_metadata_try_exit(3)
    _ = __sil_metadata_variable_lifetime_begins(&x, <int>)
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _) = module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().unwrap();
        let metadata_instrs: Vec<_> = pdesc
            .iter_instrs()
            .filter_map(|(_, instr)| match instr {
                instr::Instr::Metadata(metadata) => Some(metadata),
                _ => None,
            })
            .collect();

        assert_eq!(metadata_instrs.len(), 11);
        assert!(matches!(
            metadata_instrs[0],
            instr::InstrMetadata::Abstract(_)
        ));
        assert!(matches!(
            metadata_instrs[1],
            instr::InstrMetadata::Nullify(_, _)
        ));
        match metadata_instrs[2] {
            instr::InstrMetadata::ExitScope(vars, _) => {
                assert_eq!(vars.len(), 2);
                assert!(matches!(vars[0], sil::var::Var::ProgramVar(_)));
                assert!(matches!(vars[1], sil::var::Var::LogicalVar(_)));
            }
            other => panic!("expected ExitScope metadata, got {other:?}"),
        }
        assert!(matches!(
            metadata_instrs[3],
            instr::InstrMetadata::LoopEntry { header_id: 7 }
        ));
        assert!(matches!(
            metadata_instrs[4],
            instr::InstrMetadata::LoopBackEdge { header_id: 7 }
        ));
        assert!(matches!(
            metadata_instrs[5],
            instr::InstrMetadata::LoopExit { header_id: 7 }
        ));
        assert!(matches!(metadata_instrs[6], instr::InstrMetadata::Skip));
        assert!(matches!(
            metadata_instrs[7],
            instr::InstrMetadata::TryEntry { try_id: 3, .. }
        ));
        assert!(matches!(
            metadata_instrs[8],
            instr::InstrMetadata::CatchEntry { try_id: 3, .. }
        ));
        assert!(matches!(
            metadata_instrs[9],
            instr::InstrMetadata::TryExit { try_id: 3, .. }
        ));
        assert!(matches!(
            metadata_instrs[10],
            instr::InstrMetadata::VariableLifetimeBegins {
                is_cpp_structured_binding: false,
                ..
            }
        ));
    }

    #[test]
    fn test_prune_then_branch_metadata_tracks_textual_negation() {
        let src = r#".source_language = "c"

define f(x: int) : void {
  #entry:
    n0: int = load &x
    prune n0
    prune ! n0
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        let (cfg, _) = module_to_sil(&module, &decls).unwrap();
        let pdesc = cfg.iter_proc_descs().next().unwrap();
        let branch_metadata: Vec<_> = pdesc
            .iter_instrs()
            .filter_map(|(_, instr)| match instr {
                instr::Instr::Prune { is_then_branch, .. } => Some(*is_then_branch),
                _ => None,
            })
            .collect();

        assert_eq!(
            branch_metadata,
            vec![true, false],
            "plain textual prune should be marked as the then branch and `prune !` as the else branch"
        );
    }

    #[test]
    fn test_transform_preserves_metadata_after_prune() {
        let src = r#".source_language = "c"

define f(x: int) : void {
  #entry:
    n0: int = load &x
    prune __sil_lt(n0, 10)
    _ = __sil_metadata_exit_scope(n0)
    jmp done
  #done:
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        crate::transform::run(&mut module, &decls);

        let (cfg, _) = module_to_sil(&module, &decls).unwrap();
        let pdesc = cfg.iter_proc_descs().next().unwrap();
        let prune_node = pdesc
            .nodes
            .iter()
            .find(|node| {
                node.instrs
                    .iter()
                    .any(|instr| matches!(instr, instr::Instr::Prune { .. }))
            })
            .expect("expected a node containing the prune instruction");

        let has_prune_then_exit_scope =
            prune_node
                .instrs
                .windows(2)
                .any(|pair| match (&pair[0], &pair[1]) {
                    (
                        instr::Instr::Prune { .. },
                        instr::Instr::Metadata(instr::InstrMetadata::ExitScope(vars, _)),
                    ) => {
                        vars.len() == 1
                            && matches!(&vars[0], sil::var::Var::LogicalVar(id) if id.stamp == 0)
                    }
                    _ => false,
                });

        assert!(
            has_prune_then_exit_scope,
            "Cross-ref: OCaml `TextualTransform` + `TextualSil` preserve metadata calls after \
prune blocks so later Pulse cleanup sees `ExitScope`. pdesc={pdesc:?}"
        );
    }

    #[test]
    fn test_invalid_metadata_builtin_reports_conversion_error() {
        let src = r#".source_language = "c"

define f() : void {
  #entry:
    _ = __sil_metadata_nullify(0)
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        let errors = module_to_sil(&module, &decls).unwrap_err();
        assert!(
            errors.iter().any(|err| err
                .message
                .contains("invalid `__sil_metadata_nullify` metadata builtin")),
            "expected metadata builtin conversion error, got: {errors:?}"
        );
    }

    #[test]
    fn test_conversion_rejects_apply_after_transform() {
        let src = r#".source_language = "hack"

define f(callback: *HackMixed) : void {
  #entry:
    n0 = callback(1, 2)
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        crate::transform::run(&mut module, &decls);

        let errors = module_to_sil(&module, &decls).unwrap_err();
        assert!(
            errors.iter().any(|err| err.message.contains("Apply")),
            "expected Apply conversion error, got: {errors:?}"
        );
    }

    #[test]
    fn test_conversion_rejects_residual_if_expression() {
        let src = r#".source_language = "c"

define f(flag: int) : int {
  #entry:
    n0 = (if flag then 1 else 2)
    ret n0
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        let errors = module_to_sil(&module, &decls).unwrap_err();
        assert!(
            errors.iter().any(|err| err.message.contains("If")),
            "expected If conversion error, got: {errors:?}"
        );
    }

    #[test]
    fn test_conversion_rejects_closure_after_transform() {
        let loc = ast::Location::known(1, 1);
        let callee = ast::QualifiedProcName::top_level(ast::ProcName::plain("captured_target"));
        let closure_proc = ast::ProcDesc {
            procdecl: ast::ProcDecl {
                qualified_name: ast::QualifiedProcName::top_level(ast::ProcName::plain("f")),
                formals_types: Some(Vec::new()),
                result_type: ast::AnnotatedTyp::without_attrs(ast::Typ::Void),
                attributes: Vec::new(),
            },
            nodes: vec![ast::Node {
                label: ast::NodeName::plain("entry"),
                ssa_parameters: Vec::new(),
                exn_succs: BTreeSet::new(),
                instrs: vec![ast::Instr::Let {
                    id: Some(0),
                    exp: ast::Exp::Closure {
                        proc: callee.clone(),
                        captured: vec![ast::Exp::Const(ast::Const::Int(1.into()))],
                        params: vec![ast::VarName::plain("x")],
                        attributes: Vec::new(),
                    },
                    loc: loc.clone(),
                }],
                last: ast::Terminator::Ret(ast::Exp::Const(ast::Const::Null)),
                last_loc: loc.clone(),
                label_loc: loc.clone(),
            }],
            start: ast::NodeName::plain("entry"),
            params: Vec::new(),
            locals: Vec::new(),
            exit_loc: loc.clone(),
        };
        let callee_decl = ast::ProcDecl {
            qualified_name: callee,
            formals_types: Some(Vec::new()),
            result_type: ast::AnnotatedTyp::without_attrs(ast::Typ::Void),
            attributes: Vec::new(),
        };
        let mut module = ast::Module {
            attrs: vec![ast::Attr::new(
                "source_language",
                vec!["hack".into()],
                loc.clone(),
            )],
            decls: vec![
                ast::Decl::Procdecl(callee_decl),
                ast::Decl::Proc(closure_proc),
            ],
            source_file: "test.sil".into(),
        };
        let (decls, decl_errors) = DeclEnv::from_module(&module);
        assert!(decl_errors.is_empty());

        crate::transform::run(&mut module, &decls);

        let errors = module_to_sil(&module, &decls).unwrap_err();
        assert!(
            errors.iter().any(|err| err.message.contains("Closure")),
            "expected Closure conversion error, got: {errors:?}"
        );
    }
}
