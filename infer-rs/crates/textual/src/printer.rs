// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pretty printer for Textual IR.
//!
//! Produces output that can be parsed back by the parser (roundtrip).

use std::fmt;

use crate::ast::*;

/// Print a complete Textual module.
pub fn print_module(module: &Module) -> String {
    let mut out = String::new();
    let mut printer = Printer::new(&mut out);
    printer.print_module(module);
    out
}

struct Printer<'a> {
    out: &'a mut String,
}

impl<'a> Printer<'a> {
    fn new(out: &'a mut String) -> Self {
        Self { out }
    }

    fn write(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn writeln(&mut self, s: &str) {
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn print_module(&mut self, module: &Module) {
        for attr in &module.attrs {
            self.print_attr(attr);
            self.out.push('\n');
        }
        if !module.attrs.is_empty() {
            self.out.push('\n');
        }
        for decl in &module.decls {
            self.print_decl(decl);
            self.out.push('\n');
        }
    }

    fn print_attr(&mut self, attr: &Attr) {
        self.write(&format!(".{}", attr.name));
        if !attr.values.is_empty() {
            self.write(" = ");
            for (i, v) in attr.values.iter().enumerate() {
                if i > 0 {
                    self.write(", ");
                }
                self.write(&format!("\"{}\"", v));
            }
        }
    }

    fn print_annots(&mut self, attrs: &[Attr]) {
        for attr in attrs {
            self.write(" ");
            self.print_attr(attr);
        }
    }

    fn print_typ(&mut self, typ: &Typ) {
        self.write(&format!("{}", typ));
    }

    fn print_annotated_typ(&mut self, at: &AnnotatedTyp) {
        self.print_annots(&at.attributes);
        if !at.attributes.is_empty() {
            self.write(" ");
        }
        self.print_typ(&at.typ);
    }

    fn print_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Global(g) => {
                self.write(&format!("global {} : ", g.name));
                self.print_typ(&g.typ);
                self.writeln("");
            }
            Decl::Struct(s) => {
                self.write(&format!("type {}", s.name));
                if !s.supers.is_empty() {
                    self.write(" extends ");
                    for (i, sup) in s.supers.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.write(&format!("{}", sup));
                    }
                }
                self.print_annots(&s.attributes);
                self.write(" { ");
                for (i, field) in s.fields.iter().enumerate() {
                    if i > 0 {
                        self.write("; ");
                    }
                    self.write(&format!("{}: ", field.qualified_name.name));
                    self.print_annots(&field.attributes);
                    if !field.attributes.is_empty() {
                        self.write(" ");
                    }
                    self.print_typ(&field.typ);
                }
                self.writeln(" }");
            }
            Decl::Procdecl(p) => {
                self.write("declare");
                self.print_annots(&p.attributes);
                self.write(&format!(" {}(", p.qualified_name));
                match &p.formals_types {
                    None => self.write("..."),
                    Some(types) => {
                        for (i, t) in types.iter().enumerate() {
                            if i > 0 {
                                self.write(", ");
                            }
                            self.print_annotated_typ(t);
                        }
                    }
                }
                self.write(") : ");
                self.print_annotated_typ(&p.result_type);
                self.writeln("");
            }
            Decl::Proc(p) => {
                self.write("define");
                self.print_annots(&p.procdecl.attributes);
                self.write(&format!(" {}(", p.procdecl.qualified_name));
                for (i, (param, typ)) in p
                    .params
                    .iter()
                    .zip(
                        p.procdecl
                            .formals_types
                            .as_ref()
                            .map(|v| v.iter())
                            .unwrap_or_default(),
                    )
                    .enumerate()
                {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&format!("{}: ", param));
                    self.print_annotated_typ(typ);
                }
                self.write(") : ");
                self.print_annotated_typ(&p.procdecl.result_type);
                self.writeln(" {");

                if !p.locals.is_empty() {
                    self.write("  local ");
                    for (i, (name, typ)) in p.locals.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.write(&format!("{}: ", name));
                        self.print_annotated_typ(typ);
                    }
                    self.writeln("");
                }

                for node in &p.nodes {
                    self.print_node(node);
                }
                self.writeln("}");
            }
        }
    }

    fn print_node(&mut self, node: &Node) {
        self.write(&format!("  #{}:", node.label));
        if !node.ssa_parameters.is_empty() {
            // Not standard syntax for ssa params in labels, but included for completeness
        }
        self.writeln("");

        for instr in &node.instrs {
            self.write("    ");
            self.print_instr(instr);
            self.writeln("");
        }

        self.write("    ");
        self.print_terminator(&node.last);
        self.writeln("");

        if !node.exn_succs.is_empty() {
            self.write("    .handlers ");
            for (i, h) in node.exn_succs.iter().enumerate() {
                if i > 0 {
                    self.write(", ");
                }
                self.write(&format!("{}", h));
            }
            self.writeln("");
        }
    }

    fn print_instr(&mut self, instr: &Instr) {
        match instr {
            Instr::Load { id, exp, typ, .. } => {
                self.write(&format!("n{}", id));
                if let Some(t) = typ {
                    self.write(" : ");
                    self.print_typ(t);
                }
                self.write(" = load ");
                self.print_exp(exp);
            }
            Instr::Store {
                exp1, typ, exp2, ..
            } => {
                self.write("store ");
                self.print_exp(exp1);
                self.write(" <- ");
                self.print_exp(exp2);
                if let Some(t) = typ {
                    self.write(" : ");
                    self.print_typ(t);
                }
            }
            Instr::Prune { exp, .. } => {
                self.write("prune ");
                self.print_exp(exp);
            }
            Instr::Let { id, exp, .. } => {
                if let Some(id) = id {
                    self.write(&format!("n{} = ", id));
                } else {
                    self.write("_ = ");
                }
                self.print_exp(exp);
            }
        }
    }

    fn print_terminator(&mut self, term: &Terminator) {
        match term {
            Terminator::Ret(exp) => {
                self.write("ret ");
                self.print_exp(exp);
            }
            Terminator::Jump(calls) => {
                self.write("jmp");
                for (i, call) in calls.iter().enumerate() {
                    if i > 0 {
                        self.write(",");
                    }
                    self.write(&format!(" {}", call.label));
                    if !call.ssa_args.is_empty() {
                        self.write("(");
                        for (j, arg) in call.ssa_args.iter().enumerate() {
                            if j > 0 {
                                self.write(", ");
                            }
                            self.print_exp(arg);
                        }
                        self.write(")");
                    }
                }
            }
            Terminator::If { bexp, then_, else_ } => {
                self.write("if ");
                self.print_bool_exp(bexp);
                self.write(" then ");
                self.print_terminator(then_);
                self.write(" else ");
                self.print_terminator(else_);
            }
            Terminator::Throw(exp) => {
                self.write("throw ");
                self.print_exp(exp);
            }
            Terminator::Unreachable => {
                self.write("unreachable");
            }
        }
    }

    fn print_exp(&mut self, exp: &Exp) {
        match exp {
            Exp::Var(id) => self.write(&format!("n{}", id)),
            Exp::Load { exp, typ } => {
                self.write("[");
                self.print_exp(exp);
                if let Some(t) = typ {
                    self.write(" : ");
                    self.print_typ(t);
                }
                self.write("]");
            }
            Exp::Lvar(name) => self.write(&format!("&{}", name)),
            Exp::Field { exp, field } => {
                self.print_exp(exp);
                self.write(&format!(".{}", field));
            }
            Exp::Index(e1, e2) => {
                self.print_exp(e1);
                self.write("[");
                self.print_exp(e2);
                self.write("]");
            }
            Exp::Const(c) => match c {
                Const::Int(i) => self.write(&format!("{}", i)),
                Const::Null => self.write("null"),
                Const::Str(s) => self.write(&format!("\"{}\"", s)),
                Const::Float(f) => self.write(&format!("{}", f)),
            },
            Exp::If { cond, then_, else_ } => {
                self.write("(if ");
                self.print_bool_exp(cond);
                self.write(" then ");
                self.print_exp(then_);
                self.write(" else ");
                self.print_exp(else_);
                self.write(")");
            }
            Exp::Call { proc, args, kind } => {
                if *kind == CallKind::Virtual {
                    // Virtual calls: first arg is receiver
                    if let Some((recv, rest)) = args.split_first() {
                        self.print_exp(recv);
                        self.write(&format!(".{}(", proc));
                        for (i, arg) in rest.iter().enumerate() {
                            if i > 0 {
                                self.write(", ");
                            }
                            self.print_exp(arg);
                        }
                        self.write(")");
                    }
                } else {
                    self.write(&format!("{}(", proc));
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.print_exp(arg);
                    }
                    self.write(")");
                }
            }
            Exp::Closure {
                proc,
                captured,
                params,
                ..
            } => {
                // Closure printing: fun (params) -> proc(captured, params)
                self.write("fun (");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&format!("{p}"));
                }
                self.write(&format!(") -> {proc}("));
                for (i, arg) in captured.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.print_exp(arg);
                }
                self.write(")");
            }
            Exp::Apply { closure, args } => {
                self.print_exp(closure);
                self.write("(");
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.print_exp(arg);
                }
                self.write(")");
            }
            Exp::Typ(t) => {
                self.write("<");
                self.print_typ(t);
                self.write(">");
            }
        }
    }

    fn print_bool_exp(&mut self, bexp: &BoolExp) {
        match bexp {
            BoolExp::Exp(exp) => self.print_exp(exp),
            BoolExp::Not(inner) => {
                self.write("!");
                self.print_bool_exp(inner);
            }
            BoolExp::And(a, b) => {
                self.print_bool_exp(a);
                self.write(" && ");
                self.print_bool_exp(b);
            }
            BoolExp::Or(a, b) => {
                self.print_bool_exp(a);
                self.write(" || ");
                self.print_bool_exp(b);
            }
        }
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", print_module(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_module;

    #[test]
    fn test_roundtrip_simple() {
        let src = r#".source_language = "java"

global I : int
type node = { val: int; next: *node }
declare cons(int, *node) : node
define f(x: int) : int {
  #entry:
    n0 : int = load &x
    ret n0
}
"#;
        let module = parse_module(src, "test.sil").unwrap();
        let printed = print_module(&module);
        let reparsed =
            parse_module(&printed, "test.sil").unwrap_or_else(|e| panic!("reparsed failed: {e}"));

        // Verify structural equivalence: same number and kinds of declarations
        assert_eq!(
            module.attrs.len(),
            reparsed.attrs.len(),
            "attr count mismatch"
        );
        assert_eq!(
            module.decls.len(),
            reparsed.decls.len(),
            "decl count mismatch"
        );
        for (orig, re) in module.decls.iter().zip(reparsed.decls.iter()) {
            assert_eq!(
                std::mem::discriminant(orig),
                std::mem::discriminant(re),
                "declaration kind mismatch"
            );
        }
    }
}
