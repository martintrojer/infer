// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! AST transformations for Textual modules.
//!
//! Mirrors OCaml's `TextualTransform.ml`. Transformations are applied after
//! parsing and before Textual-to-SIL conversion.
//!
//! The pipeline (matching OCaml's `TextualTransform.run`):
//! 1. `fix_closure_app` — resolve Call vs Apply ambiguity
//! 2. `remove_effects_in_subexprs` — flatten nested effectful sub-expressions
//!    (internally calls `remove_if_exp_and_terminator` iteratively)
//! 3. `let_propagation` — inline side-effect-free Let bindings
//! 4. `out_of_ssa` — convert SSA parameters to stores/loads

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::ast::*;
use crate::decls::DeclEnv;

// ===========================================================================
// Helpers
// ===========================================================================

/// Find the next unused ident in a ProcDesc.
fn get_fresh_ident(pdesc: &ProcDesc) -> Ident {
    let mut max_id: Ident = -1;
    for node in &pdesc.nodes {
        for (id, _) in &node.ssa_parameters {
            max_id = max_id.max(*id);
        }
        for instr in &node.instrs {
            match instr {
                Instr::Load { id, .. } => max_id = max_id.max(*id),
                Instr::Let { id: Some(id), .. } => max_id = max_id.max(*id),
                _ => {}
            }
        }
    }
    max_id + 1
}

/// Create a fresh label generator that avoids collisions with existing labels.
fn fresh_label_gen(pdesc: &ProcDesc, prefix: &str) -> impl FnMut() -> NodeName {
    let existing: HashSet<String> = pdesc.nodes.iter().map(|n| n.label.value.clone()).collect();
    let mut counter = 0u32;
    let prefix = prefix.to_string();
    move || loop {
        let name = format!("{prefix}{counter}");
        counter += 1;
        if !existing.contains(&name) {
            return NodeName::new(name, Location::Unknown);
        }
    }
}

// ===========================================================================
// 1. fix_closure_app
// ===========================================================================

/// Fix closure application ambiguity.
///
/// Mirrors OCaml's `TextualTransform.fix_closure_app`.
pub fn fix_closure_app(module: &mut Module, decls: &DeclEnv) {
    for decl in &mut module.decls {
        if let Decl::Proc(pdesc) = decl {
            let local_vars = collect_local_vars(pdesc);
            for node in &mut pdesc.nodes {
                for instr in &mut node.instrs {
                    fix_closure_app_instr(instr, &local_vars, decls);
                }
            }
        }
    }
}

fn collect_local_vars(pdesc: &ProcDesc) -> HashSet<String> {
    let mut vars = HashSet::new();
    for param in &pdesc.params {
        vars.insert(param.value.clone());
    }
    for (local, _) in &pdesc.locals {
        vars.insert(local.value.clone());
    }
    vars
}

fn fix_closure_app_instr(instr: &mut Instr, local_vars: &HashSet<String>, decls: &DeclEnv) {
    match instr {
        Instr::Load { exp, .. } | Instr::Prune { exp, .. } | Instr::Let { exp, .. } => {
            fix_closure_app_exp(exp, local_vars, decls);
        }
        Instr::Store { exp1, exp2, .. } => {
            fix_closure_app_exp(exp1, local_vars, decls);
            fix_closure_app_exp(exp2, local_vars, decls);
        }
    }
}

fn fix_closure_app_exp(exp: &mut Exp, local_vars: &HashSet<String>, decls: &DeclEnv) {
    match exp {
        Exp::Call {
            proc,
            args,
            kind: CallKind::NonVirtual,
        } => {
            for arg in args.iter_mut() {
                fix_closure_app_exp(arg, local_vars, decls);
            }
            if let EnclosingClass::TopLevel = &proc.enclosing_class {
                let name = &proc.name.value;
                if local_vars.contains(name) && decls.get_proc(proc).is_none() {
                    let closure_var = VarName::new(name.clone(), proc.name.loc.clone());
                    let closure_exp = Exp::Load {
                        exp: Box::new(Exp::Lvar(closure_var)),
                        typ: None,
                    };
                    *exp = Exp::Apply {
                        closure: Box::new(closure_exp),
                        args: std::mem::take(args),
                    };
                }
            }
        }
        Exp::Load { exp: inner, .. } => fix_closure_app_exp(inner, local_vars, decls),
        Exp::Field { exp: inner, .. } => fix_closure_app_exp(inner, local_vars, decls),
        Exp::Index(e1, e2) => {
            fix_closure_app_exp(e1, local_vars, decls);
            fix_closure_app_exp(e2, local_vars, decls);
        }
        Exp::If { cond, then_, else_ } => {
            fix_closure_app_boolexp(cond, local_vars, decls);
            fix_closure_app_exp(then_, local_vars, decls);
            fix_closure_app_exp(else_, local_vars, decls);
        }
        Exp::Call { args, .. } => {
            for arg in args {
                fix_closure_app_exp(arg, local_vars, decls);
            }
        }
        Exp::Closure { captured, .. } => {
            for cap in captured {
                fix_closure_app_exp(cap, local_vars, decls);
            }
        }
        Exp::Apply { closure, args } => {
            fix_closure_app_exp(closure, local_vars, decls);
            for arg in args {
                fix_closure_app_exp(arg, local_vars, decls);
            }
        }
        Exp::Var(_) | Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => {}
    }
}

fn fix_closure_app_boolexp(bexp: &mut BoolExp, local_vars: &HashSet<String>, decls: &DeclEnv) {
    match bexp {
        BoolExp::Exp(exp) => fix_closure_app_exp(exp, local_vars, decls),
        BoolExp::Not(inner) => fix_closure_app_boolexp(inner, local_vars, decls),
        BoolExp::And(a, b) | BoolExp::Or(a, b) => {
            fix_closure_app_boolexp(a, local_vars, decls);
            fix_closure_app_boolexp(b, local_vars, decls);
        }
    }
}

// ===========================================================================
// 2. remove_effects_in_subexprs
// ===========================================================================

/// Check if a call is a side-effect-free SIL expression (unop, binop, cast).
fn is_sil_builtin(proc: &QualifiedProcName) -> bool {
    if proc.enclosing_class != EnclosingClass::TopLevel {
        return false;
    }
    let name = &proc.name.value;
    // Cast builtin
    if name == "__sil_cast" {
        return true;
    }
    // Unary operators
    if matches!(name.as_str(), "__sil_neg" | "__sil_bnot" | "__sil_lnot") {
        return true;
    }
    // Binary operators (all __sil_ prefixed arithmetic/comparison/bitwise)
    if name.starts_with("__sil_")
        && !name.starts_with("__sil_allocate")
        && !name.starts_with("__sil_get_lazy")
        && !name.starts_with("__sil_lazy_class")
        && !name.starts_with("__sil_instanceof")
        && !name.starts_with("__sil_metadata")
    {
        return true;
    }
    false
}

/// Check if an expression contains sub-expressions that need flattening.
fn exp_needs_flattening(exp: &Exp) -> bool {
    match exp {
        Exp::Var(_) | Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => false,
        Exp::Load { exp, .. } => exp_needs_flattening(exp),
        Exp::Field { exp, .. } => exp_needs_flattening(exp),
        Exp::Index(e1, e2) => exp_needs_flattening(e1) || exp_needs_flattening(e2),
        Exp::Call { args, .. } => args.iter().any(exp_needs_flattening),
        Exp::If { .. } => true,
        Exp::Closure { .. } | Exp::Apply { .. } => true,
    }
}

fn bexp_needs_flattening(bexp: &BoolExp) -> bool {
    match bexp {
        BoolExp::Exp(e) => exp_needs_flattening(e),
        BoolExp::Not(inner) => bexp_needs_flattening(inner),
        BoolExp::And(a, b) | BoolExp::Or(a, b) => {
            bexp_needs_flattening(a) || bexp_needs_flattening(b)
        }
    }
}

fn terminator_needs_flattening(term: &Terminator) -> bool {
    match term {
        Terminator::If { bexp, then_, else_ } => {
            bexp_needs_flattening(bexp)
                || terminator_needs_flattening(then_)
                || terminator_needs_flattening(else_)
        }
        Terminator::Ret(e) | Terminator::Throw(e) => exp_needs_flattening(e),
        Terminator::Jump(calls) => calls
            .iter()
            .any(|c| c.ssa_args.iter().any(exp_needs_flattening)),
        Terminator::Unreachable => false,
    }
}

/// Flattening state: accumulates instructions and tracks fresh idents.
struct FlattenState {
    instrs_rev: Vec<Instr>,
    fresh_ident: Ident,
    may_need_iteration: bool,
}

impl FlattenState {
    fn new(fresh_ident: Ident) -> Self {
        Self {
            instrs_rev: Vec::new(),
            fresh_ident,
            may_need_iteration: false,
        }
    }

    fn alloc_fresh(&mut self) -> Ident {
        let id = self.fresh_ident;
        self.fresh_ident += 1;
        id
    }

    fn push_instr(&mut self, instr: Instr) {
        // If a Let has id=None, assign a fresh ident (mirrors OCaml's State.push_instr)
        let instr = match instr {
            Instr::Let {
                id: None, exp, loc, ..
            } => {
                let fresh = self.alloc_fresh();
                Instr::Let {
                    id: Some(fresh),
                    exp,
                    loc,
                }
            }
            other => other,
        };
        self.instrs_rev.push(instr);
    }

    fn into_instrs(self) -> (Vec<Instr>, Ident, bool) {
        let instrs = self.instrs_rev;
        // instrs were pushed in order (not reversed) since we push_instr sequentially
        (instrs, self.fresh_ident, self.may_need_iteration)
    }
}

/// Flatten an expression, hoisting effectful sub-expressions into separate instructions.
fn flatten_exp(exp: &Exp, loc: &Location, toplevel: bool, state: &mut FlattenState) -> Exp {
    match exp {
        Exp::Var(_) | Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => exp.clone(),

        Exp::Load { exp: inner, typ } => {
            let flat_inner = flatten_exp(inner, loc, false, state);
            let fresh = state.alloc_fresh();
            state.push_instr(Instr::Load {
                id: fresh,
                exp: flat_inner,
                typ: typ.clone(),
                loc: loc.clone(),
            });
            Exp::Var(fresh)
        }

        Exp::Field { exp: inner, field } => {
            let flat_inner = flatten_exp(inner, loc, false, state);
            Exp::Field {
                exp: Box::new(flat_inner),
                field: field.clone(),
            }
        }

        Exp::Index(e1, e2) => {
            let flat_e1 = flatten_exp(e1, loc, false, state);
            let flat_e2 = flatten_exp(e2, loc, false, state);
            Exp::Index(Box::new(flat_e1), Box::new(flat_e2))
        }

        Exp::If { cond, then_, else_ } if toplevel => {
            // At toplevel: flatten only the condition, leave branches for RemoveIf
            let flat_cond = flatten_bexp(cond, loc, state);
            let result = Exp::If {
                cond: flat_cond,
                then_: then_.clone(),
                else_: else_.clone(),
            };
            if exp_needs_flattening(&result) {
                state.may_need_iteration = true;
            }
            result
        }

        Exp::If { cond, then_, else_ } => {
            // Nested: hoist to a separate Let, RemoveIf will split later
            let flat_cond = flatten_bexp(cond, loc, state);
            let fresh = state.alloc_fresh();
            let if_exp = Exp::If {
                cond: flat_cond,
                then_: then_.clone(),
                else_: else_.clone(),
            };
            if exp_needs_flattening(&if_exp) {
                state.may_need_iteration = true;
            }
            state.push_instr(Instr::Let {
                id: Some(fresh),
                exp: if_exp,
                loc: loc.clone(),
            });
            Exp::Var(fresh)
        }

        Exp::Call { proc, args, kind } => {
            let flat_args: Vec<Exp> = args
                .iter()
                .map(|a| flatten_exp(a, loc, false, state))
                .collect();
            if is_sil_builtin(proc) {
                // Side-effect-free: keep inline
                Exp::Call {
                    proc: proc.clone(),
                    args: flat_args,
                    kind: *kind,
                }
            } else {
                // Effectful: hoist to separate Let
                let fresh = state.alloc_fresh();
                state.push_instr(Instr::Let {
                    id: Some(fresh),
                    exp: Exp::Call {
                        proc: proc.clone(),
                        args: flat_args,
                        kind: *kind,
                    },
                    loc: loc.clone(),
                });
                Exp::Var(fresh)
            }
        }

        Exp::Closure { .. } => {
            // Simplified: don't transform closures into object allocations
            // (that requires generating new types and procedures).
            // Just return as-is; the to_sil pass handles closures directly.
            exp.clone()
        }

        Exp::Apply { closure, args } => {
            let flat_closure = flatten_exp(closure, loc, false, state);
            let flat_args: Vec<Exp> = args
                .iter()
                .map(|a| flatten_exp(a, loc, false, state))
                .collect();
            let fresh = state.alloc_fresh();
            state.push_instr(Instr::Let {
                id: Some(fresh),
                exp: Exp::Apply {
                    closure: Box::new(flat_closure),
                    args: flat_args,
                },
                loc: loc.clone(),
            });
            Exp::Var(fresh)
        }
    }
}

/// Flatten a boolean expression. Only the first operand of And/Or is flattened
/// (short-circuit semantics — second operand may not be evaluated).
fn flatten_bexp(bexp: &BoolExp, loc: &Location, state: &mut FlattenState) -> BoolExp {
    match bexp {
        BoolExp::Exp(e) => BoolExp::Exp(Box::new(flatten_exp(e, loc, false, state))),
        BoolExp::Not(inner) => BoolExp::Not(Box::new(flatten_bexp(inner, loc, state))),
        BoolExp::And(a, b) => {
            let flat_a = flatten_bexp(a, loc, state);
            // Leave b un-flattened (short-circuit)
            BoolExp::And(Box::new(flat_a), b.clone())
        }
        BoolExp::Or(a, b) => {
            let flat_a = flatten_bexp(a, loc, state);
            BoolExp::Or(Box::new(flat_a), b.clone())
        }
    }
}

/// Flatten instructions within a single instruction.
fn flatten_in_instr(instr: &Instr, state: &mut FlattenState) {
    match instr {
        Instr::Load { id, exp, typ, loc } => {
            let flat_exp = flatten_exp(exp, loc, false, state);
            state.push_instr(Instr::Load {
                id: *id,
                exp: flat_exp,
                typ: typ.clone(),
                loc: loc.clone(),
            });
        }
        Instr::Store {
            exp1,
            typ,
            exp2,
            loc,
        } => {
            let flat_exp1 = flatten_exp(exp1, loc, false, state);
            let flat_exp2 = flatten_exp(exp2, loc, false, state);
            state.push_instr(Instr::Store {
                exp1: flat_exp1,
                typ: typ.clone(),
                exp2: flat_exp2,
                loc: loc.clone(),
            });
        }
        Instr::Prune { exp, loc } => {
            let flat_exp = flatten_exp(exp, loc, false, state);
            state.push_instr(Instr::Prune {
                exp: flat_exp,
                loc: loc.clone(),
            });
        }
        Instr::Let { id, exp, loc } => {
            // For non-builtin calls: flatten only the args, keep the Call at top level
            if let Exp::Call { proc, args, kind } = exp {
                if !is_sil_builtin(proc) {
                    let flat_args: Vec<Exp> = args
                        .iter()
                        .map(|a| flatten_exp(a, loc, false, state))
                        .collect();
                    state.push_instr(Instr::Let {
                        id: *id,
                        exp: Exp::Call {
                            proc: proc.clone(),
                            args: flat_args,
                            kind: *kind,
                        },
                        loc: loc.clone(),
                    });
                    return;
                }
            }
            // For everything else: flatten with toplevel=true
            let flat_exp = flatten_exp(exp, loc, true, state);
            state.push_instr(Instr::Let {
                id: *id,
                exp: flat_exp,
                loc: loc.clone(),
            });
        }
    }
}

/// Flatten expressions in a terminator.
fn flatten_in_terminator(
    term: &Terminator,
    loc: &Location,
    state: &mut FlattenState,
) -> Terminator {
    match term {
        Terminator::If { bexp, then_, else_ } => {
            let flat_bexp = flatten_bexp(bexp, loc, state);
            let flat_then = flatten_in_terminator(then_, loc, state);
            let flat_else = flatten_in_terminator(else_, loc, state);
            let result = Terminator::If {
                bexp: flat_bexp,
                then_: Box::new(flat_then),
                else_: Box::new(flat_else),
            };
            if terminator_needs_flattening(&result) {
                state.may_need_iteration = true;
            }
            result
        }
        Terminator::Ret(e) => {
            let flat = flatten_exp(e, loc, false, state);
            Terminator::Ret(flat)
        }
        Terminator::Throw(e) => {
            let flat = flatten_exp(e, loc, false, state);
            Terminator::Throw(flat)
        }
        Terminator::Jump(calls) => {
            let flat_calls: Vec<NodeCall> = calls
                .iter()
                .map(|call| {
                    let flat_args: Vec<Exp> = call
                        .ssa_args
                        .iter()
                        .map(|a| flatten_exp(a, loc, false, state))
                        .collect();
                    NodeCall {
                        label: call.label.clone(),
                        ssa_args: flat_args,
                    }
                })
                .collect();
            Terminator::Jump(flat_calls)
        }
        Terminator::Unreachable => Terminator::Unreachable,
    }
}

/// Flatten a single procedure, iterating until stable.
///
/// Mirrors OCaml's `flatten_pdesc`. After each pass, runs `remove_if_terminators`
/// to convert If expressions/terminators into prune nodes. Iterates if the
/// flattening was incomplete (e.g. If branches or And/Or second operands
/// still need processing).
fn flatten_pdesc(pdesc: &mut ProcDesc) {
    loop {
        let fresh_ident = get_fresh_ident(pdesc);
        let mut may_need_iteration = false;

        let mut new_nodes = Vec::new();
        for node in &pdesc.nodes {
            let mut state = FlattenState::new(fresh_ident);

            // Flatten all instructions
            for instr in &node.instrs {
                flatten_in_instr(instr, &mut state);
            }

            // Flatten terminator
            let flat_term = flatten_in_terminator(&node.last, &node.last_loc, &mut state);

            let (instrs, _next_fresh, needs_iter) = state.into_instrs();
            may_need_iteration |= needs_iter;

            new_nodes.push(Node {
                label: node.label.clone(),
                ssa_parameters: node.ssa_parameters.clone(),
                exn_succs: node.exn_succs.clone(),
                last: flat_term,
                instrs,
                last_loc: node.last_loc.clone(),
                label_loc: node.label_loc.clone(),
            });
        }

        pdesc.nodes = new_nodes;

        // Run RemoveIf after flattening
        remove_if_terminators(pdesc);

        if !may_need_iteration {
            break;
        }
    }
}

/// Remove effectful sub-expressions by flattening them into separate instructions.
///
/// Mirrors OCaml's `TextualTransform.remove_effects_in_subexprs`.
///
/// Nested calls like `foo(bar(x))` become `n1 = bar(x); n2 = foo(n1)`.
/// SIL builtins (`__sil_*` operators) are kept inline since they're pure.
/// After flattening, `remove_if_terminators` is called to handle any
/// If expressions that were introduced.
pub fn remove_effects_in_subexprs(module: &mut Module) {
    for decl in &mut module.decls {
        if let Decl::Proc(pdesc) = decl {
            flatten_pdesc(pdesc);
        }
    }
}

// ===========================================================================
// 3. remove_if_exp_and_terminator
// ===========================================================================

/// Remove `If` terminators by decomposing compound boolean conditions into
/// explicit prune nodes, and remove `If` expressions by splitting nodes.
///
/// Mirrors OCaml's `RemoveIf.transform_pdesc`.
pub fn remove_if_terminators(pdesc: &mut ProcDesc) {
    let mut fresh_label = fresh_label_gen(pdesc, "if");

    // Process If terminators → prune nodes
    let mut new_nodes = Vec::new();
    for node in &mut pdesc.nodes {
        if let Terminator::If { bexp, then_, else_ } = &node.last {
            let mut targets = Vec::new();

            // True branch: NNF → DNF → prune nodes
            let true_bexp = bexp.clone();
            let true_dnf = to_dnf(to_nnf(true_bexp));
            for conjuncts in &true_dnf {
                let label = fresh_label();
                let prune_instrs: Vec<Instr> = conjuncts
                    .iter()
                    .map(|e| Instr::Prune {
                        exp: e.clone(),
                        loc: Location::Unknown,
                    })
                    .collect();
                new_nodes.push(Node {
                    label: label.clone(),
                    ssa_parameters: Vec::new(),
                    exn_succs: node.exn_succs.clone(),
                    last: *then_.clone(),
                    instrs: prune_instrs,
                    last_loc: Location::Unknown,
                    label_loc: Location::Unknown,
                });
                targets.push(NodeCall {
                    label,
                    ssa_args: Vec::new(),
                });
            }

            // False branch: NNF(Not(bexp)) → DNF → prune nodes
            let false_bexp = BoolExp::Not(Box::new(bexp.clone()));
            let false_dnf = to_dnf(to_nnf(false_bexp));
            for conjuncts in &false_dnf {
                let label = fresh_label();
                let prune_instrs: Vec<Instr> = conjuncts
                    .iter()
                    .map(|e| Instr::Prune {
                        exp: e.clone(),
                        loc: Location::Unknown,
                    })
                    .collect();
                new_nodes.push(Node {
                    label: label.clone(),
                    ssa_parameters: Vec::new(),
                    exn_succs: node.exn_succs.clone(),
                    last: *else_.clone(),
                    instrs: prune_instrs,
                    last_loc: Location::Unknown,
                    label_loc: Location::Unknown,
                });
                targets.push(NodeCall {
                    label,
                    ssa_args: Vec::new(),
                });
            }

            // Replace the If terminator with Jump to all prune nodes
            node.last = Terminator::Jump(targets);
        }
    }

    pdesc.nodes.append(&mut new_nodes);

    // Process If expressions in Let instructions → split into diamond CFG.
    // `Let { id, exp: If { cond, then_, else_ } }` becomes:
    //   current_node → If { cond, then: jmp then_node, else: jmp else_node }
    //   then_node: jmp next_node(then_)
    //   else_node: jmp next_node(else_)
    //   next_node: (id as SSA param) + remaining instructions
    let mut fresh_label_exp = fresh_label_gen(pdesc, "if_exp");
    let mut result_nodes = Vec::new();
    let nodes = std::mem::take(&mut pdesc.nodes);
    for node in nodes {
        // Find first Let with If expression
        let if_pos = node.instrs.iter().position(|i| {
            matches!(
                i,
                Instr::Let {
                    exp: Exp::If { .. },
                    id: Some(_),
                    ..
                }
            )
        });

        if let Some(pos) = if_pos {
            let Instr::Let {
                id: Some(ret_id),
                exp: Exp::If { cond, then_, else_ },
                loc,
            } = &node.instrs[pos]
            else {
                unreachable!()
            };

            let next_label = fresh_label_exp();
            let then_label = fresh_label_exp();
            let else_label = fresh_label_exp();

            // then_node: jump to next with then_ value
            let then_node = Node {
                label: then_label.clone(),
                ssa_parameters: Vec::new(),
                exn_succs: BTreeSet::new(),
                last: Terminator::Jump(vec![NodeCall {
                    label: next_label.clone(),
                    ssa_args: vec![*then_.clone()],
                }]),
                instrs: Vec::new(),
                last_loc: loc.clone(),
                label_loc: loc.clone(),
            };

            // else_node: jump to next with else_ value
            let else_node = Node {
                label: else_label.clone(),
                ssa_parameters: Vec::new(),
                exn_succs: BTreeSet::new(),
                last: Terminator::Jump(vec![NodeCall {
                    label: next_label.clone(),
                    ssa_args: vec![*else_.clone()],
                }]),
                instrs: Vec::new(),
                last_loc: loc.clone(),
                label_loc: loc.clone(),
            };

            // interrupted_node: instructions before the If, terminator becomes the If
            let interrupted_node = Node {
                label: node.label.clone(),
                ssa_parameters: node.ssa_parameters.clone(),
                exn_succs: node.exn_succs.clone(),
                last: Terminator::If {
                    bexp: cond.clone(),
                    then_: Box::new(Terminator::Jump(vec![NodeCall {
                        label: then_label,
                        ssa_args: Vec::new(),
                    }])),
                    else_: Box::new(Terminator::Jump(vec![NodeCall {
                        label: else_label,
                        ssa_args: Vec::new(),
                    }])),
                },
                instrs: node.instrs[..pos].to_vec(),
                last_loc: loc.clone(),
                label_loc: node.label_loc.clone(),
            };

            // next_node: receives the SSA param, has remaining instructions
            let next_node = Node {
                label: next_label,
                ssa_parameters: vec![(*ret_id, Typ::Ptr(Box::new(Typ::Void), Vec::new()))],
                exn_succs: BTreeSet::new(),
                last: node.last.clone(),
                instrs: node.instrs[pos + 1..].to_vec(),
                last_loc: node.last_loc.clone(),
                label_loc: loc.clone(),
            };

            result_nodes.push(interrupted_node);
            result_nodes.push(then_node);
            result_nodes.push(else_node);
            result_nodes.push(next_node);
        } else {
            result_nodes.push(node);
        }
    }
    pdesc.nodes = result_nodes;
}

/// Convert a BoolExp to Negation Normal Form (push Not inward).
fn to_nnf(bexp: BoolExp) -> BoolExp {
    match bexp {
        BoolExp::Not(inner) => match *inner {
            BoolExp::Not(x) => to_nnf(*x),
            BoolExp::And(a, b) => BoolExp::Or(
                Box::new(to_nnf(BoolExp::Not(a))),
                Box::new(to_nnf(BoolExp::Not(b))),
            ),
            BoolExp::Or(a, b) => BoolExp::And(
                Box::new(to_nnf(BoolExp::Not(a))),
                Box::new(to_nnf(BoolExp::Not(b))),
            ),
            BoolExp::Exp(e) => BoolExp::Exp(Box::new(Exp::logical_not(*e))),
        },
        BoolExp::And(a, b) => BoolExp::And(Box::new(to_nnf(*a)), Box::new(to_nnf(*b))),
        BoolExp::Or(a, b) => BoolExp::Or(Box::new(to_nnf(*a)), Box::new(to_nnf(*b))),
        BoolExp::Exp(_) => bexp,
    }
}

/// Convert a BoolExp (in NNF) to Disjunctive Normal Form.
/// Returns a list of disjuncts, each being a list of conjunct expressions.
fn to_dnf(bexp: BoolExp) -> Vec<Vec<Exp>> {
    match bexp {
        BoolExp::Exp(e) => vec![vec![*e]],
        BoolExp::Not(_) => {
            // Should not happen after NNF
            panic!("to_dnf called on non-NNF expression")
        }
        BoolExp::Or(a, b) => {
            let mut result = to_dnf(*a);
            result.extend(to_dnf(*b));
            result
        }
        BoolExp::And(a, b) => {
            let left = to_dnf(*a);
            let right = to_dnf(*b);
            let mut result = Vec::new();
            for l in &left {
                for r in &right {
                    let mut conj = l.clone();
                    conj.extend(r.iter().cloned());
                    result.push(conj);
                }
            }
            result
        }
    }
}

// ===========================================================================
// 3. let_propagation
// ===========================================================================

/// Inline side-effect-free `Let` bindings.
///
/// For every `Let {id=Some id; exp}` where `exp` is a side-effect-free
/// expression (not an effectful Call), substitutes `exp` for `Var id`
/// everywhere and removes the `Let` instruction.
///
/// Mirrors OCaml's `TextualTransform.let_propagation`.
pub fn let_propagation(pdesc: &mut ProcDesc) {
    // Build equations: id → exp for side-effect-free lets
    let mut equations: HashMap<Ident, Exp> = HashMap::new();
    for node in &pdesc.nodes {
        for instr in &node.instrs {
            if let Instr::Let {
                id: Some(id), exp, ..
            } = instr
            {
                if is_side_effect_free(exp) {
                    equations.insert(*id, exp.clone());
                }
            }
        }
    }

    if equations.is_empty() {
        return;
    }

    // Compute dependencies and topologically sort
    let domain: HashSet<Ident> = equations.keys().copied().collect();
    let sorted = topo_sort_equations(&equations, &domain);

    // Saturate: substitute each equation's dependencies
    let mut saturated: HashMap<Ident, Exp> = HashMap::new();
    for id in &sorted {
        if let Some(exp) = equations.get(id) {
            let subst_exp = subst_exp(exp, &saturated);
            saturated.insert(*id, subst_exp);
        }
    }

    // Apply substitution to entire ProcDesc
    for node in &mut pdesc.nodes {
        // Remove Let instructions that were inlined
        node.instrs.retain(|instr| {
            if let Instr::Let { id: Some(id), .. } = instr {
                !saturated.contains_key(id)
            } else {
                true
            }
        });

        // Substitute in remaining instructions
        for instr in &mut node.instrs {
            subst_in_instr(instr, &saturated);
        }

        // Substitute in terminator
        subst_in_terminator(&mut node.last, &saturated);

        // Substitute in SSA args of jumps (already handled by subst_in_terminator)
    }
}

/// Check if an expression is side-effect-free (safe to inline).
fn is_side_effect_free(exp: &Exp) -> bool {
    match exp {
        Exp::Call {
            proc,
            kind: CallKind::NonVirtual,
            ..
        } => {
            // SIL builtins are side-effect-free
            proc.enclosing_class == EnclosingClass::TopLevel
                && proc.name.value.starts_with("__sil_")
        }
        Exp::Call { .. } => false,
        Exp::Apply { .. } => false,
        Exp::Var(_) | Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => true,
        Exp::Load { .. } => true,
        Exp::Field { .. } | Exp::Index(..) => true,
        Exp::If { .. } => true,
        Exp::Closure { .. } => false,
    }
}

/// Collect free idents from an expression.
fn free_idents(exp: &Exp) -> HashSet<Ident> {
    let mut ids = HashSet::new();
    collect_idents(exp, &mut ids);
    ids
}

fn collect_idents(exp: &Exp, ids: &mut HashSet<Ident>) {
    match exp {
        Exp::Var(id) => {
            ids.insert(*id);
        }
        Exp::Load { exp, .. } | Exp::Field { exp, .. } => collect_idents(exp, ids),
        Exp::Index(e1, e2) => {
            collect_idents(e1, ids);
            collect_idents(e2, ids);
        }
        Exp::Call { args, .. } => {
            for a in args {
                collect_idents(a, ids);
            }
        }
        Exp::If { then_, else_, .. } => {
            collect_idents(then_, ids);
            collect_idents(else_, ids);
        }
        Exp::Closure { captured, .. } => {
            for c in captured {
                collect_idents(c, ids);
            }
        }
        Exp::Apply { closure, args } => {
            collect_idents(closure, ids);
            for a in args {
                collect_idents(a, ids);
            }
        }
        Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => {}
    }
}

/// Topologically sort equation idents by dependency.
fn topo_sort_equations(equations: &HashMap<Ident, Exp>, domain: &HashSet<Ident>) -> Vec<Ident> {
    let mut deps: HashMap<Ident, HashSet<Ident>> = HashMap::new();
    for (id, exp) in equations {
        let free = free_idents(exp);
        let dep: HashSet<Ident> = free.intersection(domain).copied().collect();
        deps.insert(*id, dep);
    }

    let mut sorted = Vec::new();
    let mut visited = HashSet::new();
    let mut in_stack = HashSet::new();

    fn visit(
        id: Ident,
        deps: &HashMap<Ident, HashSet<Ident>>,
        visited: &mut HashSet<Ident>,
        in_stack: &mut HashSet<Ident>,
        sorted: &mut Vec<Ident>,
    ) {
        if visited.contains(&id) {
            return;
        }
        if in_stack.contains(&id) {
            return; // cycle — skip
        }
        in_stack.insert(id);
        if let Some(d) = deps.get(&id) {
            for &dep_id in d {
                visit(dep_id, deps, visited, in_stack, sorted);
            }
        }
        in_stack.remove(&id);
        visited.insert(id);
        sorted.push(id);
    }

    for &id in domain {
        visit(id, &deps, &mut visited, &mut in_stack, &mut sorted);
    }

    sorted
}

/// Substitute idents in an expression using the saturated equation map.
fn subst_exp(exp: &Exp, subst: &HashMap<Ident, Exp>) -> Exp {
    match exp {
        Exp::Var(id) => {
            if let Some(replacement) = subst.get(id) {
                replacement.clone()
            } else {
                exp.clone()
            }
        }
        Exp::Load { exp: inner, typ } => Exp::Load {
            exp: Box::new(subst_exp(inner, subst)),
            typ: typ.clone(),
        },
        Exp::Field { exp: inner, field } => Exp::Field {
            exp: Box::new(subst_exp(inner, subst)),
            field: field.clone(),
        },
        Exp::Index(e1, e2) => Exp::Index(
            Box::new(subst_exp(e1, subst)),
            Box::new(subst_exp(e2, subst)),
        ),
        Exp::Call { proc, args, kind } => Exp::Call {
            proc: proc.clone(),
            args: args.iter().map(|a| subst_exp(a, subst)).collect(),
            kind: *kind,
        },
        Exp::If { cond, then_, else_ } => Exp::If {
            cond: subst_boolexp(cond, subst),
            then_: Box::new(subst_exp(then_, subst)),
            else_: Box::new(subst_exp(else_, subst)),
        },
        Exp::Apply { closure, args } => Exp::Apply {
            closure: Box::new(subst_exp(closure, subst)),
            args: args.iter().map(|a| subst_exp(a, subst)).collect(),
        },
        Exp::Closure {
            proc,
            captured,
            params,
            attributes,
        } => Exp::Closure {
            proc: proc.clone(),
            captured: captured.iter().map(|c| subst_exp(c, subst)).collect(),
            params: params.clone(),
            attributes: attributes.clone(),
        },
        Exp::Lvar(_) | Exp::Const(_) | Exp::Typ(_) => exp.clone(),
    }
}

fn subst_boolexp(bexp: &BoolExp, subst: &HashMap<Ident, Exp>) -> BoolExp {
    match bexp {
        BoolExp::Exp(e) => BoolExp::Exp(Box::new(subst_exp(e, subst))),
        BoolExp::Not(inner) => BoolExp::Not(Box::new(subst_boolexp(inner, subst))),
        BoolExp::And(a, b) => BoolExp::And(
            Box::new(subst_boolexp(a, subst)),
            Box::new(subst_boolexp(b, subst)),
        ),
        BoolExp::Or(a, b) => BoolExp::Or(
            Box::new(subst_boolexp(a, subst)),
            Box::new(subst_boolexp(b, subst)),
        ),
    }
}

fn subst_in_instr(instr: &mut Instr, subst: &HashMap<Ident, Exp>) {
    match instr {
        Instr::Load { exp, .. } => *exp = subst_exp(exp, subst),
        Instr::Store { exp1, exp2, .. } => {
            *exp1 = subst_exp(exp1, subst);
            *exp2 = subst_exp(exp2, subst);
        }
        Instr::Prune { exp, .. } => *exp = subst_exp(exp, subst),
        Instr::Let { exp, .. } => *exp = subst_exp(exp, subst),
    }
}

fn subst_in_terminator(term: &mut Terminator, subst: &HashMap<Ident, Exp>) {
    match term {
        Terminator::Ret(e) => *e = subst_exp(e, subst),
        Terminator::Throw(e) => *e = subst_exp(e, subst),
        Terminator::Jump(calls) => {
            for call in calls {
                call.ssa_args = call.ssa_args.iter().map(|a| subst_exp(a, subst)).collect();
            }
        }
        Terminator::If { bexp, then_, else_ } => {
            *bexp = subst_boolexp(bexp, subst);
            subst_in_terminator(then_, subst);
            subst_in_terminator(else_, subst);
        }
        Terminator::Unreachable => {}
    }
}

// ===========================================================================
// 4. out_of_ssa
// ===========================================================================

/// Convert SSA-form phi-parameters into explicit Store/Load instructions.
///
/// Each SSA parameter `(id, typ)` at a node becomes:
/// - A `Load` at the beginning of the target node
/// - A `Store` at the end of each jumping node
///
/// Mirrors OCaml's `TextualTransform.out_of_ssa`.
pub fn out_of_ssa(pdesc: &mut ProcDesc) {
    // Identify exception handler nodes (their SSA params are preserved)
    let handler_labels: HashSet<String> = pdesc
        .nodes
        .iter()
        .flat_map(|n| n.exn_succs.iter())
        .map(|name| name.value.clone())
        .collect();

    // Build lookup: label → ssa_parameters
    let ssa_params: HashMap<String, Vec<(Ident, Typ)>> = pdesc
        .nodes
        .iter()
        .map(|n| (n.label.value.clone(), n.ssa_parameters.clone()))
        .collect();

    for node in &mut pdesc.nodes {
        let is_handler = handler_labels.contains(&node.label.value);

        // Add prefix loads for SSA parameters (non-handler nodes only)
        if !is_handler && !node.ssa_parameters.is_empty() {
            let mut prefix_instrs = Vec::new();
            for (id, typ) in &node.ssa_parameters {
                let var_name = ssa_var_name(*id);
                prefix_instrs.push(Instr::Load {
                    id: *id,
                    exp: Exp::Lvar(var_name),
                    typ: Some(typ.clone()),
                    loc: Location::Unknown,
                });
            }
            prefix_instrs.append(&mut node.instrs);
            node.instrs = prefix_instrs;
            node.ssa_parameters.clear();
        }

        // Add suffix stores for jump SSA arguments
        let mut suffix_instrs = Vec::new();
        if let Terminator::Jump(calls) = &node.last {
            for call in calls {
                if let Some(params) = ssa_params.get(&call.label.value) {
                    for ((id, typ), arg) in params.iter().zip(call.ssa_args.iter()) {
                        // Don't generate stores for handler SSA params
                        if handler_labels.contains(&call.label.value) {
                            continue;
                        }
                        let var_name = ssa_var_name(*id);
                        suffix_instrs.push(Instr::Store {
                            exp1: Exp::Lvar(var_name),
                            typ: Some(typ.clone()),
                            exp2: arg.clone(),
                            loc: Location::Unknown,
                        });
                    }
                }
            }
        }
        node.instrs.append(&mut suffix_instrs);

        // Strip SSA args from jumps
        if let Terminator::Jump(calls) = &mut node.last {
            for call in calls {
                if !handler_labels.contains(&call.label.value) {
                    call.ssa_args.clear();
                }
            }
        }
    }

    // Add SSA locals to the procedure's local declarations
    for node_params in ssa_params.values() {
        for (id, typ) in node_params {
            let var_name = ssa_var_name(*id);
            let annot_typ = AnnotatedTyp {
                typ: typ.clone(),
                attributes: Vec::new(),
            };
            if !pdesc.locals.iter().any(|(n, _)| n.value == var_name.value) {
                pdesc.locals.push((var_name, annot_typ));
            }
        }
    }
}

/// Create the SSA variable name for an ident.
fn ssa_var_name(id: Ident) -> VarName {
    VarName::new(format!("__SSA{id}"), Location::Unknown)
}

// ===========================================================================
// Run all transformations
// ===========================================================================

/// Run all transformations.
///
/// Mirrors OCaml's pipeline order:
/// 1. fix_closure_app (disambiguate call vs apply, runs right after parse in OCaml)
/// 2. type_check (fill in typ: None, matching OCaml's TextualTypeVerification)
/// 3. remove_effects_in_subexprs (internally calls remove_if_terminators)
/// 4. let_propagation
/// 5. out_of_ssa
pub fn run(module: &mut Module, decls: &DeclEnv) {
    fix_closure_app(module, decls);
    crate::type_check::run(module, decls);
    remove_effects_in_subexprs(module);
    for decl in &mut module.decls {
        if let Decl::Proc(pdesc) = decl {
            let_propagation(pdesc);
            out_of_ssa(pdesc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decls::DeclEnv;
    use crate::parser::parse_module;

    #[test]
    fn test_fix_closure_app() {
        let src = r#".source_language = "hack"

define f(callback: *HackMixed) : void {
  #entry:
    n0 = callback(1, 2)
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        fix_closure_app(&mut module, &decls);

        match &module.decls[0] {
            Decl::Proc(pdesc) => match &pdesc.nodes[0].instrs[0] {
                Instr::Let { exp, .. } => {
                    assert!(
                        matches!(exp, Exp::Apply { .. }),
                        "expected Apply, got {exp:?}"
                    );
                }
                other => panic!("expected Let, got {other:?}"),
            },
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_no_fix_for_declared_proc() {
        let src = r#".source_language = "hack"

declare foo(int) : void

define f() : void {
  #entry:
    n0 = foo(1)
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        fix_closure_app(&mut module, &decls);

        match &module.decls[1] {
            Decl::Proc(pdesc) => match &pdesc.nodes[0].instrs[0] {
                Instr::Let { exp, .. } => {
                    assert!(
                        matches!(exp, Exp::Call { .. }),
                        "expected Call, got {exp:?}"
                    );
                }
                other => panic!("expected Let, got {other:?}"),
            },
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_fix_closure_app_local_var() {
        let src = r#".source_language = "hack"

define f() : void {
  local cb: *HackMixed
  #entry:
    n0 = cb(42)
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        fix_closure_app(&mut module, &decls);

        match &module.decls[0] {
            Decl::Proc(pdesc) => match &pdesc.nodes[0].instrs[0] {
                Instr::Let { exp, .. } => {
                    assert!(
                        matches!(exp, Exp::Apply { .. }),
                        "expected Apply, got {exp:?}"
                    );
                }
                other => panic!("expected Let, got {other:?}"),
            },
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_no_fix_for_class_method() {
        let src = r#".source_language = "hack"

declare A.method(int) : void

define f() : void {
  #entry:
    n0 = A.method(1)
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        fix_closure_app(&mut module, &decls);

        match &module.decls[1] {
            Decl::Proc(pdesc) => match &pdesc.nodes[0].instrs[0] {
                Instr::Let { exp, .. } => {
                    assert!(
                        matches!(exp, Exp::Call { .. }),
                        "expected Call, got {exp:?}"
                    );
                }
                other => panic!("expected Let, got {other:?}"),
            },
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_remove_effects_nested_calls() {
        let src = r#".source_language = "python"

declare g1(int) : int
declare g2(int) : int
declare m(int, int) : int

define f(x: int, y: int) : int {
  #entry:
    n0 : int = load &x
    n1 : int = load &y
    n3 = __sil_mult_int(g1(n0), g2(n1))
    ret n3
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        remove_effects_in_subexprs(&mut module);

        if let Decl::Proc(pdesc) = &module.decls[3] {
            // g1(n0) and g2(n1) should be hoisted into separate Let instructions
            // __sil_mult_int should stay inline (it's a SIL builtin)
            let entry = &pdesc.nodes[0];

            // Count effectful calls that were hoisted
            let let_count = entry
                .instrs
                .iter()
                .filter(|i| matches!(i, Instr::Let { .. }))
                .count();
            assert!(
                let_count >= 2,
                "g1() and g2() should be hoisted to separate Lets, got {let_count} Lets.\ninstrs: {:?}",
                entry.instrs
            );

            // The final Let (n3 = ...) should have __sil_mult_int with Var args (not nested calls)
            let last_let = entry
                .instrs
                .iter()
                .rev()
                .find(|i| matches!(i, Instr::Let { .. }));
            if let Some(Instr::Let { exp, .. }) = last_let {
                if let Exp::Call { proc, args, .. } = exp {
                    assert_eq!(proc.name.value, "__sil_mult_int");
                    // Both args should be Var (hoisted), not Call
                    for arg in args {
                        assert!(
                            matches!(arg, Exp::Var(_)),
                            "args to __sil_mult_int should be Var after flattening, got {arg:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_remove_effects_field_deref() {
        let src = r#".source_language = "python"

type cell = { value: int; next: *cell }

define next(l: *cell) : *cell {
  #entry:
    ret l->cell.next
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        remove_effects_in_subexprs(&mut module);

        // l->cell.next is Load{exp=Field{exp=Lvar(l), field=cell.next}}
        // The Load should be hoisted, resulting in a Load instruction + Var in ret
        if let Decl::Proc(pdesc) = &module.decls[1] {
            let entry = &pdesc.nodes[0];
            // Should have at least one Load instruction from the hoisted dereference
            let load_count = entry
                .instrs
                .iter()
                .filter(|i| matches!(i, Instr::Load { .. }))
                .count();
            assert!(
                load_count >= 1,
                "arrow dereference should produce at least 1 hoisted Load"
            );
        }
    }

    #[test]
    fn test_remove_effects_empty_function() {
        let src = r#".source_language = "python"

define empty() : void {
  #entry:
    ret null
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        remove_effects_in_subexprs(&mut module);

        if let Decl::Proc(pdesc) = &module.decls[0] {
            assert_eq!(
                pdesc.nodes[0].instrs.len(),
                0,
                "empty function should stay empty"
            );
        }
    }

    #[test]
    fn test_remove_if_terminator_simple() {
        let src = r#".source_language = "python"

define f(b: int) : int {
  #entry:
    n0 : int = load &b
    if n0 then jmp lab1 else jmp lab2
  #lab1:
    ret 1
  #lab2:
    ret 2
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            remove_if_terminators(pdesc);

            // The If terminator should be replaced with Jump to prune nodes
            assert!(
                matches!(&pdesc.nodes[0].last, Terminator::Jump(calls) if calls.len() == 2),
                "expected Jump with 2 targets, got {:?}",
                pdesc.nodes[0].last
            );

            // New prune nodes should have been created
            assert!(
                pdesc.nodes.len() > 3,
                "expected new prune nodes, got {} nodes",
                pdesc.nodes.len()
            );

            // Check that prune instructions were generated
            let prune_count: usize = pdesc
                .nodes
                .iter()
                .flat_map(|n| n.instrs.iter())
                .filter(|i| matches!(i, Instr::Prune { .. }))
                .count();
            assert!(prune_count >= 2, "expected at least 2 prune instructions");
        }
    }

    #[test]
    fn test_remove_if_terminator_compound() {
        let src = r#".source_language = "python"

define f(b1: int, b2: int) : int {
  #entry:
    n1 : int = load &b1
    n2 : int = load &b2
    if n1 && n2 then jmp lab1 else jmp lab2
  #lab1:
    ret 1
  #lab2:
    ret 2
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            remove_if_terminators(pdesc);

            // n1 && n2 → true branch has 1 disjunct with 2 conjuncts → 1 prune node
            // Not(n1 && n2) → Not(n1) || Not(n2) → 2 disjuncts → 2 prune nodes
            // Total: 3 new nodes
            let prune_nodes: Vec<_> = pdesc
                .nodes
                .iter()
                .filter(|n| n.instrs.iter().any(|i| matches!(i, Instr::Prune { .. })))
                .collect();
            assert_eq!(
                prune_nodes.len(),
                3,
                "expected 3 prune nodes (1 true + 2 false)"
            );
        }
    }

    #[test]
    fn test_let_propagation() {
        let src = r#".source_language = "python"

declare foo(int) : int

define f(x: int, y: int) : int {
  #entry:
    n0 : int = load &x
    n1 : int = load &y
    n3 = __sil_mult_int(n0, n1)
    n4 = __sil_minusa(n3, n0)
    n5 = foo(n4)
    ret n5
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[1] {
            let_propagation(pdesc);

            // n3 and n4 should be inlined into the foo() call
            // n3 = __sil_mult_int(n0, n1) → inlined
            // n4 = __sil_minusa(n3, n0) → inlined (n3 already substituted)
            // n5 = foo(__sil_minusa(__sil_mult_int(n0, n1), n0)) → kept (effectful)
            let let_count = pdesc.nodes[0]
                .instrs
                .iter()
                .filter(|i| matches!(i, Instr::Let { .. }))
                .count();
            assert_eq!(
                let_count, 1,
                "only the effectful foo() call should remain as Let"
            );
        }
    }

    #[test]
    fn test_out_of_ssa() {
        let src = r#".source_language = "python"

define f(x: int, y: int) : int {
  #entry:
    n0 : int = load &x
    n1 : int = load &y
    jmp lab(n0)
  #lab(n2: int):
    ret n2
}"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            out_of_ssa(pdesc);

            // lab's SSA parameter n2 should become a load from __SSA2
            let lab = pdesc.nodes.iter().find(|n| n.label.value == "lab").unwrap();
            assert!(
                lab.ssa_parameters.is_empty(),
                "SSA parameters should be cleared"
            );
            // First instruction should be a load from __SSA2
            assert!(
                matches!(&lab.instrs[0], Instr::Load { exp: Exp::Lvar(v), .. } if v.value.starts_with("__SSA")),
                "expected load from SSA var, got {:?}",
                lab.instrs[0]
            );

            // entry should have a store to __SSA2 before the jump
            let entry = pdesc
                .nodes
                .iter()
                .find(|n| n.label.value == "entry")
                .unwrap();
            let has_ssa_store = entry.instrs.iter().any(|i| {
                matches!(i, Instr::Store { exp1: Exp::Lvar(v), .. } if v.value.starts_with("__SSA"))
            });
            assert!(has_ssa_store, "entry should have a store to SSA var");

            // Jump should have SSA args cleared
            if let Terminator::Jump(calls) = &entry.last {
                assert!(
                    calls[0].ssa_args.is_empty(),
                    "SSA args should be cleared from jump"
                );
            }
        }
    }

    /// Regression: if-terminator with Load in condition + self-loop
    /// must not cause flatten_pdesc to loop forever.
    #[test]
    fn test_flatten_if_with_load_cond_does_not_hang() {
        let src = r#".source_language = "java"

define if_load_loop(x: int, y: float) : void {
  #entry:
    n0: int = load &x
    if n0 then ret null else jmp lab1
  #lab1:
    n1: float = load &y
    if n1 then ret null else jmp lab2
  #lab2:
    if n0 && n1 then ret null else jmp lab3
  #lab3:
    if [&y] then ret null else jmp lab2
}
"#;
        let mut module = parse_module(src, "test.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        // This must terminate — previously it looped forever
        run(&mut module, &decls);
    }
}
