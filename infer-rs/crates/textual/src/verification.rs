// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Verification passes for Textual modules.
//!
//! Mirrors OCaml's `TextualBasicVerification.ml` and `TextualVerification.ml`.
//! Checks structural well-formedness and reports errors without aborting.

use std::collections::HashSet;

use crate::ast::*;
use crate::decls::DeclEnv;

/// Verification error.
///
/// Mirrors OCaml's `TextualBasicVerification.error`.
#[derive(Clone, Debug)]
pub enum VerifError {
    /// A field is referenced but not declared in any struct.
    UnknownField {
        enclosing_class: TypeName,
        field: FieldName,
        loc: Location,
    },
    /// A procedure is called but not declared or defined.
    UnknownProc {
        proc: QualifiedProcName,
        args: usize,
        loc: Location,
    },
    /// A label is referenced in a jump but not declared in the procedure.
    UnknownLabel {
        label: NodeName,
        pname: QualifiedProcName,
    },
    /// A call has the wrong number of arguments.
    WrongArgNumber {
        proc: QualifiedProcName,
        args: usize,
        formals: usize,
        loc: Location,
    },
    /// A variadic call did not pass enough arguments to activate the
    /// `.variadic` parameter.
    VariadicNotEnoughArgs {
        proc: QualifiedProcName,
        args: usize,
        formals: usize,
        loc: Location,
    },
    /// A `.variadic` formal appears in an invalid position.
    VariadicWrongParam {
        proc: QualifiedProcName,
        loc: Location,
    },
}

impl std::fmt::Display for VerifError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifError::UnknownField {
                enclosing_class,
                field,
                ..
            } => write!(f, "field {enclosing_class}.{field} is not declared"),
            VerifError::UnknownProc { proc, args, .. } => {
                write!(
                    f,
                    "function {proc} called with {args} arguments is not declared"
                )
            }
            VerifError::UnknownLabel { label, pname } => {
                write!(f, "label {label} is not declared in function {pname}")
            }
            VerifError::WrongArgNumber {
                proc,
                args,
                formals,
                ..
            } => write!(
                f,
                "function {proc} called with {args} arguments but declared with {formals} parameters"
            ),
            VerifError::VariadicNotEnoughArgs {
                proc,
                args,
                formals,
                ..
            } => write!(
                f,
                "variadic function {proc} expects at least {} arguments but is called with {args}",
                formals.saturating_sub(1)
            ),
            VerifError::VariadicWrongParam { proc, .. } => {
                write!(f, "variadic parameter is in an invalid position in function {proc}")
            }
        }
    }
}

impl VerifError {
    pub fn loc(&self) -> &Location {
        match self {
            VerifError::UnknownField { loc, .. } => loc,
            VerifError::UnknownProc { loc, .. } => loc,
            VerifError::UnknownLabel { label, .. } => &label.loc,
            VerifError::WrongArgNumber { loc, .. } => loc,
            VerifError::VariadicNotEnoughArgs { loc, .. } => loc,
            VerifError::VariadicWrongParam { loc, .. } => loc,
        }
    }
}

/// Run basic verification on a module.
///
/// Mirrors OCaml's `TextualBasicVerification.run`.
/// Checks:
/// - All jump labels reference declared labels within the procedure
/// - All field accesses reference declared fields
/// - All procedure calls reference declared procedures with matching arity
pub fn verify(module: &Module, decls: &DeclEnv) -> Vec<VerifError> {
    let mut errors = Vec::new();

    for decl in &module.decls {
        if let Decl::Proc(pdesc) = decl {
            verify_procdesc(pdesc, decls, &mut errors);
        }
    }

    errors
}

fn verify_procdesc(pdesc: &ProcDesc, decls: &DeclEnv, errors: &mut Vec<VerifError>) {
    verify_variadic_position(pdesc, decls, errors);

    // Collect declared labels.
    let declared_labels: HashSet<&str> =
        pdesc.nodes.iter().map(|n| n.label.value.as_str()).collect();

    for node in &pdesc.nodes {
        // Verify instructions.
        for instr in &node.instrs {
            verify_instr(instr, decls, errors);
        }
        // Verify terminator.
        verify_terminator(
            &node.last,
            &pdesc.procdecl.qualified_name,
            &declared_labels,
            decls,
            errors,
        );
    }
}

fn verify_instr(instr: &Instr, decls: &DeclEnv, errors: &mut Vec<VerifError>) {
    match instr {
        Instr::Load { exp, .. } | Instr::Prune { exp, .. } | Instr::Let { exp, .. } => {
            verify_exp(exp, decls, errors);
        }
        Instr::Store { exp1, exp2, .. } => {
            verify_exp(exp1, decls, errors);
            verify_exp(exp2, decls, errors);
        }
    }
}

fn verify_exp(exp: &Exp, decls: &DeclEnv, errors: &mut Vec<VerifError>) {
    match exp {
        Exp::Var(_) | Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => {}
        Exp::Load { exp, .. } => verify_exp(exp, decls, errors),
        Exp::Field { exp, field } => {
            verify_exp(exp, decls, errors);
            // Check field exists (unless wildcard class).
            if field.enclosing_class.name.value != "?" && decls.get_field(field).is_none() {
                errors.push(VerifError::UnknownField {
                    enclosing_class: field.enclosing_class.clone(),
                    field: field.name.clone(),
                    loc: field.name.loc.clone(),
                });
            }
        }
        Exp::Index(e1, e2) => {
            verify_exp(e1, decls, errors);
            verify_exp(e2, decls, errors);
        }
        Exp::If { cond, then_, else_ } => {
            verify_boolexp(cond, decls, errors);
            verify_exp(then_, decls, errors);
            verify_exp(else_, decls, errors);
        }
        Exp::Call { proc, args, .. } => {
            for arg in args {
                verify_exp(arg, decls, errors);
            }
            verify_call(proc, args.len(), decls, errors);
        }
        Exp::Closure { captured, .. } => {
            for cap in captured {
                verify_exp(cap, decls, errors);
            }
        }
        Exp::Apply { closure, args } => {
            verify_exp(closure, decls, errors);
            for arg in args {
                verify_exp(arg, decls, errors);
            }
        }
    }
}

fn verify_boolexp(bexp: &BoolExp, decls: &DeclEnv, errors: &mut Vec<VerifError>) {
    match bexp {
        BoolExp::Exp(exp) => verify_exp(exp, decls, errors),
        BoolExp::Not(inner) => verify_boolexp(inner, decls, errors),
        BoolExp::And(a, b) | BoolExp::Or(a, b) => {
            verify_boolexp(a, decls, errors);
            verify_boolexp(b, decls, errors);
        }
    }
}

fn verify_call(
    proc: &QualifiedProcName,
    num_args: usize,
    decls: &DeclEnv,
    errors: &mut Vec<VerifError>,
) {
    // Skip builtins (names starting with `__sil_`).
    if proc.name.value.starts_with("__sil_") {
        return;
    }
    // Skip wildcard classes.
    if let EnclosingClass::Enclosing(tn) = &proc.enclosing_class {
        if tn.name.value == "?" {
            return;
        }
    }

    match decls.get_procdecl_for_call(proc, num_args) {
        Some(resolved) => {
            if let Some(formals) = &resolved.decl.formals_types {
                if resolved.variadic.is_some() {
                    if num_args < formals.len().saturating_sub(1) {
                        errors.push(VerifError::VariadicNotEnoughArgs {
                            proc: proc.clone(),
                            args: num_args,
                            formals: formals.len(),
                            loc: proc.name.loc.clone(),
                        });
                    }
                } else {
                    let adjusted_args = if decls.is_trait_method(proc) {
                        num_args + 1
                    } else {
                        num_args
                    };
                    if formals.len() != adjusted_args {
                        errors.push(VerifError::WrongArgNumber {
                            proc: proc.clone(),
                            args: adjusted_args,
                            formals: formals.len(),
                            loc: proc.name.loc.clone(),
                        });
                    }
                }
            }
            // If formals_types is None (ellipsis), any arity is fine.
        }
        None => {
            errors.push(VerifError::UnknownProc {
                proc: proc.clone(),
                args: num_args,
                loc: proc.name.loc.clone(),
            });
        }
    }
}

fn verify_variadic_position(pdesc: &ProcDesc, decls: &DeclEnv, errors: &mut Vec<VerifError>) {
    let Some(formals) = &pdesc.procdecl.formals_types else {
        return;
    };
    let Some(variadic_index) = pdesc.procdecl.variadic_formal_index() else {
        return;
    };

    let in_trait = decls.is_defined_in_a_trait(&pdesc.procdecl.qualified_name);
    let has_reified_generics_param = pdesc
        .params
        .iter()
        .any(|param| param.value == "$0ReifiedGenerics");
    let expected_index_from_end = match (in_trait, has_reified_generics_param) {
        (true, true) => 2,
        (true, false) | (false, true) => 1,
        (false, false) => 0,
    };

    if formals
        .len()
        .checked_sub(1 + expected_index_from_end)
        .is_none_or(|expected| variadic_index != expected)
    {
        errors.push(VerifError::VariadicWrongParam {
            proc: pdesc.procdecl.qualified_name.clone(),
            loc: pdesc.procdecl.qualified_name.name.loc.clone(),
        });
    }
}

fn verify_terminator(
    term: &Terminator,
    pname: &QualifiedProcName,
    declared_labels: &HashSet<&str>,
    decls: &DeclEnv,
    errors: &mut Vec<VerifError>,
) {
    match term {
        Terminator::If { bexp, then_, else_ } => {
            verify_boolexp(bexp, decls, errors);
            verify_terminator(then_, pname, declared_labels, decls, errors);
            verify_terminator(else_, pname, declared_labels, decls, errors);
        }
        Terminator::Ret(exp) | Terminator::Throw(exp) => {
            verify_exp(exp, decls, errors);
        }
        Terminator::Jump(calls) => {
            for call in calls {
                if !declared_labels.contains(call.label.value.as_str()) {
                    errors.push(VerifError::UnknownLabel {
                        label: call.label.clone(),
                        pname: pname.clone(),
                    });
                }
                for arg in &call.ssa_args {
                    verify_exp(arg, decls, errors);
                }
            }
        }
        Terminator::Unreachable => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decls::DeclEnv;
    use crate::parser::parse_module;

    #[test]
    fn test_verify_valid_module() {
        let src = r#".source_language = "java"

type node = { val: int; next: *node }

declare cons(int, *node) : node

define f(x: int) : int {
  #entry:
    n0 : int = load &x
    ret n0
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_verify_unknown_label() {
        let src = r#".source_language = "java"

define f(x: int) : void {
  #entry:
    jmp nonexistent
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert_eq!(errors.len(), 1);
        assert!(
            matches!(&errors[0], VerifError::UnknownLabel { label, .. } if label.value == "nonexistent")
        );
    }

    #[test]
    fn test_verify_unknown_field() {
        let src = r#".source_language = "java"

type A = { x: int }

define f(a: *A) : void {
  #entry:
    n0 : *A = load &a
    store n0.A.nonexistent <- 1 : int
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            VerifError::UnknownField { field, .. } if field.value == "nonexistent"
        ));
    }

    #[test]
    fn test_verify_valid_control_flow() {
        let src = r#".source_language = "java"

define f(x: int) : void {
  #entry:
    n0: int = load &x
    jmp lab1, lab2
  #lab1:
    ret null
  #lab2:
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_verify_multiple_errors() {
        let src = r#".source_language = "java"

declare foo(int) : void

define f() : void {
  #entry:
    n0 = foo(1, 2, 3)
    jmp missing_label
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert_eq!(errors.len(), 2);
        assert!(errors
            .iter()
            .any(|e| matches!(e, VerifError::WrongArgNumber { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, VerifError::UnknownLabel { .. })));
    }

    #[test]
    fn test_verify_wildcard_field_ok() {
        let src = r#".source_language = "java"

define f(a: *A) : void {
  #entry:
    n0 : *A = load &a
    store n0.?.field <- 1 : int
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        // Wildcard class `?` should not produce an error.
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_verify_wrong_arg_count() {
        let src = r#".source_language = "java"

declare foo(int) : void

define f() : void {
  #entry:
    n0 = foo(1, 2)
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            &errors[0],
            VerifError::WrongArgNumber {
                args: 2,
                formals: 1,
                ..
            }
        ));
    }

    #[test]
    fn test_verify_variadic_definition_allows_extra_args() {
        let src = r#".source_language = "hack"

define foo(x: int, xs: .variadic int) : void {
  #entry:
    ret null
}

define f() : void {
  #entry:
    n0 = foo(1, 2, 3, 4)
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    }

    #[test]
    fn test_verify_variadic_definition_rejects_too_few_args() {
        let src = r#".source_language = "hack"

define foo(x: int, xs: .variadic int) : void {
  #entry:
    ret null
}

define f() : void {
  #entry:
    n0 = foo()
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors
            .iter()
            .any(|e| matches!(e, VerifError::VariadicNotEnoughArgs { .. })));
    }

    #[test]
    fn test_verify_variadic_wrong_position() {
        let src = r#".source_language = "hack"

define foo(xs: .variadic int, x: int) : void {
  #entry:
    ret null
}"#;
        let module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors
            .iter()
            .any(|e| matches!(e, VerifError::VariadicWrongParam { .. })));
    }
}
