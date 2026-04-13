// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse transfer functions: SIL instructions → state transitions.
//!
//! Mirrors OCaml's `Pulse.ml` exec_instr (simplified).
//!
//! Maps each SIL instruction to a list of possible resulting states.

use sil::const_val::Const;
use sil::exp::Exp;
use sil::instr::Instr;
use sil::location::Location;
use sil::procdesc::Procdesc;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::operations;
use crate::pulse_result::PulseResult;
use crate::value_history::{ValueHistory, ValueWithHistory};

/// Execute a single SIL instruction on the abstract state.
///
/// Returns a list of resulting execution domains. Most instructions
/// produce exactly one ContinueProgram; error-finding instructions
/// may produce an AbortProgram.
pub fn exec_instr(instr: &Instr, state: AbductiveDomain) -> Vec<ExecutionDomain> {
    exec_instr_with_pdesc(None, instr, state)
}

/// Execute a single SIL instruction with access to the enclosing procedure.
pub fn exec_instr_with_pdesc(
    pdesc: Option<&Procdesc>,
    instr: &Instr,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    match instr {
        Instr::Load { id, e, loc, typ } => exec_load(pdesc, id, e, typ, loc, state),
        Instr::Store { e1, e2, loc, .. } => exec_store(pdesc, e1, e2, loc, state),
        Instr::Prune { exp, loc, .. } => exec_prune(pdesc, exp, loc, state),
        Instr::Call {
            ret: (ret_id, ret_typ),
            fun_exp,
            args,
            loc,
            ..
        } => exec_call(ret_id, ret_typ, fun_exp, args, loc, state),
        Instr::Metadata(_) => vec![ExecutionDomain::ContinueProgram(state)],
    }
}

/// Load: `id = *rhs_exp`
///
/// Mirrors OCaml's `Pulse.ml exec_instr` for Load:
/// - L-value expressions (Lvar, Lfield, Lindex) are addresses that need
///   dereferencing to get the value stored at that address.
/// - R-value expressions (BinOp, UnOp, Const, Sizeof, etc.) produce values
///   directly — no dereference needed.
///
/// Cross-ref OCaml: Pulse.ml line ~1319 uses eval_deref for Lvar/Lfield/Lindex
/// and eval (without deref) for other expressions. Without this distinction,
/// BinOp results (comparisons) lose their formula constraints through the
/// unnecessary dereference edge, breaking path-sensitive pruning.
fn exec_load(
    pdesc: Option<&Procdesc>,
    id: &sil::ident::Ident,
    rhs_exp: &Exp,
    typ: &sil::typ::Typ,
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // L-value expressions and variable references need dereferencing;
    // R-value expressions (BinOp, UnOp, Const, Sizeof, etc.) don't.
    // Cross-ref OCaml: Pulse.ml distinguishes eval_deref (for addresses)
    // vs eval (for computed values) based on expression kind.
    let needs_deref = matches!(
        rhs_exp,
        Exp::Lvar(_) | Exp::Lfield(..) | Exp::Lindex(..) | Exp::Var(_)
    );

    let result = if needs_deref {
        operations::eval_deref_with_history(rhs_exp, loc, &mut state)
    } else {
        operations::eval_with_history(rhs_exp, loc, &mut state)
    };

    match result {
        PulseResult::Ok(value) => {
            operations::write_id_with_history(id, value.clone(), &mut state);
            // Mark integer-typed loads for integer reasoning.
            // Cross-ref: OCaml Pulse.ml and_is_int_if_integer_type.
            if typ.is_int() {
                state.path_condition.and_is_int(value.addr);
            }
            vec![ExecutionDomain::ContinueProgram(state)]
        }
        PulseResult::Recoverable(value, errors) => {
            operations::write_id_with_history(id, value.clone(), &mut state);
            if typ.is_int() {
                state.path_condition.and_is_int(value.addr);
            }
            stopped_results_from_recoverable_errors(pdesc, state, errors)
        }
        PulseResult::FatalError(diag, _) => {
            vec![ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(diag),
            }]
        }
    }
}

/// Store: `*lhs_exp = rhs_exp`
fn exec_store(
    pdesc: Option<&Procdesc>,
    lhs_exp: &Exp,
    rhs_exp: &Exp,
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let rhs_val = match operations::eval_with_history(rhs_exp, loc, &mut state) {
        PulseResult::Ok(v) => v,
        PulseResult::FatalError(d, _) => {
            return vec![ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(d),
            }];
        }
        PulseResult::Recoverable(v, _) => v,
    };

    let (lhs_addr, lhs_errors) = match operations::eval_with_history(lhs_exp, loc, &mut state) {
        PulseResult::Ok(v) => (v, vec![]),
        PulseResult::FatalError(d, _) => {
            return vec![ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(d),
            }];
        }
        PulseResult::Recoverable(v, errors) => (v, errors),
    };
    // Report errors from evaluating the LHS (e.g., null.field access)
    if !lhs_errors.is_empty() {
        return stopped_results_from_recoverable_errors(pdesc, state, lhs_errors);
    }

    if local_has_cleanup_attribute(pdesc, lhs_exp) {
        state.always_reachable(rhs_val.addr);
    }

    match operations::write_deref_with_history(lhs_addr, rhs_val, loc, &mut state) {
        PulseResult::Ok(()) => vec![ExecutionDomain::ContinueProgram(state)],
        PulseResult::FatalError(d, _) => vec![ExecutionDomain::AbortProgram {
            state: Box::new(state),
            diagnostic: Box::new(d),
        }],
        PulseResult::Recoverable((), errors) => {
            stopped_results_from_recoverable_errors(pdesc, state, errors)
        }
    }
}

fn stopped_results_from_recoverable_errors(
    pdesc: Option<&Procdesc>,
    state: AbductiveDomain,
    errors: Vec<Diagnostic>,
) -> Vec<ExecutionDomain> {
    let Some(diagnostic) = errors.into_iter().next() else {
        return vec![ExecutionDomain::ContinueProgram(state)];
    };

    let exec = if let Some(pdesc) = pdesc {
        match crate::summary::classify_abort_kind(pdesc, &state, &diagnostic) {
            crate::summary::PrePostKind::LatentInvalidAccess => {
                ExecutionDomain::LatentInvalidAccess {
                    state: Box::new(state),
                    diagnostic: Box::new(diagnostic),
                }
            }
            crate::summary::PrePostKind::LatentAbortProgram => {
                ExecutionDomain::LatentAbortProgram {
                    state: Box::new(state),
                    diagnostic: Box::new(diagnostic),
                }
            }
            _ => ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(diagnostic),
            },
        }
    } else {
        ExecutionDomain::AbortProgram {
            state: Box::new(state),
            diagnostic: Box::new(diagnostic),
        }
    };

    vec![exec]
}

fn local_has_cleanup_attribute(pdesc: Option<&Procdesc>, lhs_exp: &Exp) -> bool {
    let (Some(pdesc), Exp::Lvar(pvar)) = (pdesc, lhs_exp) else {
        return false;
    };

    pdesc
        .locals
        .iter()
        .any(|local| local.has_cleanup_attribute && pvar.is_local() && local.name == pvar.name)
}

/// Prune: add a path condition constraint.
///
/// Extracts the boolean meaning of `exp` and adds constraints to the formula.
/// For `if (p != NULL)`, the true branch gets `p ≠ 0` and the false branch gets `p = 0`.
/// If the constraint is unsatisfiable (e.g., pruning `2 < 3` on the false branch),
/// the path is killed (no ContinueProgram returned).
fn exec_prune(
    pdesc: Option<&Procdesc>,
    exp: &Exp,
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if prune_expr(pdesc, exp, loc, &mut state) {
        log::trace!("  [prune] SAT: {exp}");
        vec![ExecutionDomain::ContinueProgram(state)]
    } else {
        log::debug!("  [prune] UNSAT (path killed): {exp}");
        vec![]
    }
}

/// Recursively extract constraints from a prune expression.
/// Returns `true` if the constraint is satisfiable, `false` if the path is dead.
///
/// Cross-ref: OCaml's `PulseOperations.prune` dispatches `prune_binop` for
/// all BinOp operators, not just Eq/Ne. Handles comparison ops (Lt/Le/Gt/Ge)
/// directly to add atoms without going through eval+term_eq indirection.
fn prune_expr(
    pdesc: Option<&Procdesc>,
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> bool {
    prune_binop(pdesc, exp, loc, state, false)
}

/// Core prune handler for all binary/unary operators.
/// When `negated` is true, we're in the negated context (prune(!e)).
///
/// Cross-ref: OCaml's Pulse.ml `prune_binop` handles all comparison operators.
fn prune_binop(
    pdesc: Option<&Procdesc>,
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
    negated: bool,
) -> bool {
    use sil::binop::Binop;
    match exp {
        // Equality and disequality
        Exp::BinOp(Binop::Eq, lhs, rhs) => prune_eq_operands(pdesc, lhs, rhs, loc, state, negated),
        Exp::BinOp(Binop::Ne, lhs, rhs) => prune_eq_operands(pdesc, lhs, rhs, loc, state, !negated),
        // Comparison operators: add atoms directly
        Exp::BinOp(Binop::Lt, lhs, rhs) => {
            let op_lhs = eval_operand_for_prune(lhs, loc, state);
            let op_rhs = eval_operand_for_prune(rhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_lhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_rhs, loc, state);
            if negated {
                // !(x < y) → y ≤ x
                state.prune_less_equal(&op_rhs, &op_lhs).is_sat()
            } else {
                state.prune_less_than(&op_lhs, &op_rhs).is_sat()
            }
        }
        Exp::BinOp(Binop::Le, lhs, rhs) => {
            let op_lhs = eval_operand_for_prune(lhs, loc, state);
            let op_rhs = eval_operand_for_prune(rhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_lhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_rhs, loc, state);
            if negated {
                // !(x ≤ y) → y < x
                state.prune_less_than(&op_rhs, &op_lhs).is_sat()
            } else {
                state.prune_less_equal(&op_lhs, &op_rhs).is_sat()
            }
        }
        Exp::BinOp(Binop::Gt, lhs, rhs) => {
            // x > y ↔ y < x
            let op_lhs = eval_operand_for_prune(lhs, loc, state);
            let op_rhs = eval_operand_for_prune(rhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_lhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_rhs, loc, state);
            if negated {
                // !(x > y) → x ≤ y
                state.prune_less_equal(&op_lhs, &op_rhs).is_sat()
            } else {
                state.prune_less_than(&op_rhs, &op_lhs).is_sat()
            }
        }
        Exp::BinOp(Binop::Ge, lhs, rhs) => {
            // x ≥ y ↔ y ≤ x
            let op_lhs = eval_operand_for_prune(lhs, loc, state);
            let op_rhs = eval_operand_for_prune(rhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_lhs, loc, state);
            record_operand_used_as_branch_cond(pdesc, &op_rhs, loc, state);
            if negated {
                // !(x ≥ y) → y < x  → wait, !(x ≥ y) = y > x = x < y
                state.prune_less_than(&op_lhs, &op_rhs).is_sat()
            } else {
                state.prune_less_equal(&op_rhs, &op_lhs).is_sat()
            }
        }
        // Logical negation
        Exp::UnOp(sil::unop::Unop::LNot, inner, _) => {
            prune_binop(pdesc, inner, loc, state, !negated)
        }
        // Default: variable/expression — truthy (≠ 0) or falsy (= 0)
        _ => {
            let val = operations::eval_or_fresh_for_prune(exp, loc, state);
            record_used_as_branch_cond(pdesc, val, loc, state);
            state.prune_eq_const(val, 0, !negated).is_sat()
        }
    }
}

fn eval_operand_for_prune(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> crate::formula::Operand {
    match exp {
        Exp::Const(Const::Cint(i)) => i
            .to_i64()
            .map(crate::formula::Operand::ConstOperand)
            .unwrap_or_else(|| {
                crate::formula::Operand::AbstractValue(operations::eval_or_fresh_for_prune(
                    exp, loc, state,
                ))
            }),
        Exp::Cast(_, inner) => eval_operand_for_prune(inner, loc, state),
        _ => crate::formula::Operand::AbstractValue(operations::eval_or_fresh_for_prune(
            exp, loc, state,
        )),
    }
}

fn record_used_as_branch_cond(
    pdesc: Option<&Procdesc>,
    value: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) {
    let Some(pdesc) = pdesc else {
        return;
    };
    let value = state.path_condition.get_var_repr(value);
    state.pre.attrs.add_one(
        value,
        crate::attribute::Attribute::UsedAsBranchCond(pdesc.proc_name.clone(), loc.clone()),
    );
}

fn record_operand_used_as_branch_cond(
    pdesc: Option<&Procdesc>,
    operand: &crate::formula::Operand,
    loc: &Location,
    state: &mut AbductiveDomain,
) {
    if let crate::formula::Operand::AbstractValue(value) = operand {
        record_used_as_branch_cond(pdesc, *value, loc, state);
    }
}

fn prune_eq_operands(
    pdesc: Option<&Procdesc>,
    lhs: &Exp,
    rhs: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
    negated: bool,
) -> bool {
    let lhs = eval_operand_for_prune(lhs, loc, state);
    let rhs = eval_operand_for_prune(rhs, loc, state);
    record_operand_used_as_branch_cond(pdesc, &lhs, loc, state);
    record_operand_used_as_branch_cond(pdesc, &rhs, loc, state);
    match (lhs, rhs) {
        (
            crate::formula::Operand::AbstractValue(v1),
            crate::formula::Operand::AbstractValue(v2),
        ) => state.prune_eq(v1, v2, negated).is_sat(),
        (crate::formula::Operand::AbstractValue(v), crate::formula::Operand::ConstOperand(c))
        | (crate::formula::Operand::ConstOperand(c), crate::formula::Operand::AbstractValue(v)) => {
            if !negated && c == 0 {
                // Cross-ref: OCaml PulseOperations.prune records an explicit
                // ComparedToNull invalidation on equality-to-null branches.
                state.invalidate(
                    v,
                    crate::invalidation::Invalidation::ComparedToNullInThisProcedure(loc.clone()),
                    ValueHistory::invalidated(
                        crate::invalidation::Invalidation::ComparedToNullInThisProcedure(
                            loc.clone(),
                        ),
                        loc.clone(),
                    ),
                );
            }
            state.prune_eq_const(v, c, negated).is_sat()
        }
        (crate::formula::Operand::ConstOperand(c1), crate::formula::Operand::ConstOperand(c2)) => {
            if negated {
                c1 != c2
            } else {
                c1 == c2
            }
        }
    }
}

/// Call: `ret_id = fun_exp(args)`
///
/// Dispatches to built-in models (malloc/free/etc.) via `models::dispatch`.
/// Unknown functions get a fresh return value.
fn exec_call(
    ret_id: &sil::ident::Ident,
    ret_typ: &sil::typ::Typ,
    fun_exp: &Exp,
    args: &[(Exp, sil::typ::Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Try to dispatch to a built-in model
    if let Exp::Const(sil::const_val::Const::Cfun(callee)) = fun_exp {
        if crate::models::has_model(callee) {
            let model_actual_vals: Vec<_> = args
                .iter()
                .map(|(arg_exp, _arg_typ)| operations::eval_or_fresh(arg_exp, loc, &mut state))
                .collect();
            // Cross-ref: OCaml `Pulse.ml` initializes model arguments before
            // entering `PulseModels.dispatch`. This keeps caller-visible
            // reachable values, such as `*x` in `free(x)`, marked
            // `Initialized` in exported summaries.
            state.conservatively_initialize_args(model_actual_vals.iter().copied());
            if let Some(results) = crate::models::dispatch(callee, ret_id, args, loc, state.clone())
            {
                return results;
            }
        }
    }

    // Default: treat as unknown — havoc the return value and pointer args.
    log::debug!("  [call] unknown: {fun_exp}");
    let actual_vals: Vec<_> = args
        .iter()
        .map(|(arg_exp, _arg_typ)| operations::eval_or_fresh(arg_exp, loc, &mut state))
        .collect();
    // Cross-ref: OCaml `PulseCallOperations.call_aux_unknown` conservatively
    // initializes actual argument roots before entering unknown-call
    // semantics. This matters even for constant actuals such as
    // `__infer_skip(0)`: later summary canonicalization can expose the same
    // reused constant cell at the caller surface, where OCaml expects
    // `Initialized + Invalid(ConstantDereference(0))`.
    state.conservatively_initialize_args(actual_vals.iter().copied());
    let ret_val = AbstractValue::mk_fresh();
    operations::write_id_with_history(
        ret_id,
        ValueWithHistory::new(ret_val, ValueHistory::assignment(loc.clone())),
        &mut state,
    );
    mark_call_result_type(ret_val, ret_typ, &mut state);

    // Havoc pointer arguments for C/C++ unknown calls: unknown functions
    // may modify memory reachable from their arguments. For each actual,
    // evaluate it and replace all reachable heap edge targets with fresh
    // values. Only applies to C — Hack/Java/Python use ShouldOnlyHavocResources.
    // Cross-ref: OCaml PulseCallOperations.ml unknown_call + havoc_actual_if_ptr.
    let callee_name = match fun_exp {
        Exp::Const(sil::const_val::Const::Cfun(p)) if p.is_c() => Some(format!("{p}")),
        _ => None,
    };
    if let Some(callee_name) = callee_name {
        let mut is_pure = true;
        for ((arg_exp, arg_typ), arg_val) in args.iter().zip(actual_vals.iter().copied()) {
            if arg_typ.is_pointer() {
                is_pure = false;
                apply_unknown_call_pointer_actual_effect(arg_exp, arg_val, loc, &mut state);
            }
        }
        // Pure unknown calls should keep stable FunctionApplication results
        // when called with the same actuals.
        // Cross-ref: OCaml PulseCallOperations.ml unknown_call.
        if is_pure
            && state
                .path_condition
                .and_fn_app(ret_val, &callee_name, &actual_vals)
                .is_unsat()
        {
            return vec![];
        }
    }

    vec![ExecutionDomain::ContinueProgram(state)]
}

fn apply_unknown_call_pointer_actual_effect(
    arg_exp: &Exp,
    arg_val: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) {
    let reachable_before_havoc = state.reachable_from_post(arg_val);
    let written_before_havoc: Vec<_> = reachable_before_havoc
        .iter()
        .copied()
        .filter(|addr| !state.path_condition.phi().is_marked_int(*addr))
        .collect();
    state.apply_unknown_effect(arg_val);
    state.add_attr(arg_val, crate::attribute::Attribute::UnknownEffect);
    state.mark_written_to_addrs_at(written_before_havoc, loc);
    operations::refresh_unknown_lvalue_root(arg_exp, arg_val, state);
}

fn mark_call_result_type(
    ret_val: AbstractValue,
    ret_typ: &sil::typ::Typ,
    state: &mut AbductiveDomain,
) {
    // Cross-ref: OCaml Pulse.ml and_is_int_if_integer_type.
    if ret_typ.is_int() {
        state.path_condition.and_is_int(ret_val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use crate::diagnostic::Diagnostic;
    use crate::formula::Operand;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::ident::{Ident, IdentName};
    use sil::int_lit::IntLit;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::typ::Typ;
    use sil::var::Var;

    fn mk_state() -> AbductiveDomain {
        let pname = Procname::c_from_string("test");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        AbductiveDomain::mk_initial(&pdesc)
    }

    #[test]
    fn test_load_from_valid_address() {
        let mut state = mk_state();
        let p = crate::abstract_value::AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), p);

        // Store a value at *p
        let val = crate::abstract_value::AbstractValue::mk_fresh();
        state.write_heap(p, Access::Dereference, val);

        // Load from *p — should succeed
        let id = Ident::create_normal(IdentName::from_string("n"), 0);
        let instr = Instr::Load {
            id: id.clone(),
            e: Exp::Lvar(pvar),
            typ: Typ::void(),
            loc: Location::dummy(),
        };
        let results = exec_instr(&instr, state);
        assert!(
            results.iter().any(|r| r.is_continue()),
            "load from valid address should continue"
        );
    }

    #[test]
    fn test_load_from_null_detects_error() {
        let mut state = mk_state();
        let null_pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));

        // p = NULL (constant 0)
        let null_val = crate::abstract_value::AbstractValue::mk_fresh();
        state.invalidate(
            null_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(null_pvar.clone())), null_val);

        // Load *p — should detect null dereference
        let id = Ident::create_normal(IdentName::from_string("n"), 0);
        let load_exp = Exp::Lvar(null_pvar);
        let instr = Instr::Load {
            id,
            e: load_exp,
            typ: Typ::void(),
            loc: Location::dummy(),
        };
        let results = exec_instr(&instr, state);

        // Should have an AbortProgram with null deref diagnostic
        let has_abort = results.iter().any(|r| match r {
            ExecutionDomain::AbortProgram { diagnostic, .. } => {
                matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress { .. }
                )
            }
            _ => false,
        });
        assert!(has_abort, "loading from null should produce AbortProgram");
    }

    #[test]
    fn test_store_to_null_detects_error() {
        let mut state = mk_state();
        let null_pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));

        let null_val = crate::abstract_value::AbstractValue::mk_fresh();
        state.invalidate(
            null_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(null_pvar.clone())), null_val);

        // *p = 42 — should detect null dereference
        let instr = Instr::Store {
            e1: Box::new(Exp::Lvar(null_pvar)),
            typ: Typ::void(),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
            loc: Location::dummy(),
        };
        let results = exec_instr(&instr, state);
        let has_abort = results.iter().any(|r| match r {
            ExecutionDomain::AbortProgram { diagnostic, .. } => {
                matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress { .. }
                )
            }
            _ => false,
        });
        assert!(has_abort, "storing to null should produce AbortProgram");
    }

    #[test]
    fn test_store_through_null_formal_stops_as_latent_without_continue() {
        let pname = Procname::c_from_string("formal_store");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal = Pvar::mk(Mangled::from_string("x"), pname);
        let formal_var = Var::ProgramVar(Box::new(formal.clone()));
        let formal_addr = state.post.stack.find(&formal_var).unwrap();
        let formal_val = state.read_heap(formal_addr, Access::Dereference);
        let loaded = Ident::create_normal(IdentName::from_string("n"), 0);
        state
            .post
            .stack
            .add(Var::LogicalVar(loaded.clone()), formal_val);
        state.invalidate(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        let instr = Instr::Store {
            e1: Box::new(Exp::Var(loaded)),
            typ: Typ::void(),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
            loc: Location::dummy(),
        };
        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);

        assert!(
            !results.iter().any(|r| r.is_continue()),
            "recoverable formal null stores should stop the path instead of continuing"
        );
        assert!(
            matches!(
                results.as_slice(),
                [ExecutionDomain::AbortProgram { diagnostic, .. }]
                    | [ExecutionDomain::LatentInvalidAccess { diagnostic, .. }]
                    | [ExecutionDomain::LatentAbortProgram { diagnostic, .. }]
                    if matches!(diagnostic.as_ref(), Diagnostic::AccessToInvalidAddress { .. })
            ),
            "expected a single stopped invalid-access state, got {results:?}"
        );
    }

    #[test]
    fn test_store_to_formula_known_zero_detects_error() {
        let mut state = mk_state();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        let stack_addr = crate::abstract_value::AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), stack_addr);
        let p_val = state.read_heap(stack_addr, Access::Dereference);
        assert!(state.and_equal_const(p_val, 0).is_sat());
        assert!(
            state.check_valid(p_val).is_ok(),
            "formula-only null should not require a preexisting Invalid attr"
        );

        let id = Ident::create_normal(IdentName::from_string("n"), 0);
        state.post.stack.add(Var::LogicalVar(id.clone()), p_val);
        let instr = Instr::Store {
            e1: Box::new(Exp::Var(id)),
            typ: Typ::void(),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
            loc: Location::dummy(),
        };
        let results = exec_instr(&instr, state);
        let has_abort = results.iter().any(|r| match r {
            ExecutionDomain::AbortProgram { diagnostic, .. } => {
                matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress { .. }
                )
            }
            _ => false,
        });
        assert!(
            has_abort,
            "storing through an address that is provably null should abort"
        );
    }

    #[test]
    fn test_store_to_cleanup_local_marks_rhs_always_reachable() {
        let pname = Procname::c_from_string("test");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let cleanup_pvar = Pvar::mk(Mangled::from_string("x"), pname.clone());
        pdesc.locals.push(sil::procdesc::VarData {
            name: cleanup_pvar.name.clone(),
            typ: Typ::int(sil::typ::IKind::IInt),
            modify_in_block: false,
            is_constexpr: false,
            is_declared_unused: false,
            is_structured_binding: false,
            has_cleanup_attribute: true,
        });

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let allocated = crate::abstract_value::AbstractValue::mk_fresh();
        state.allocate(
            allocated,
            crate::attribute::Allocator::CMalloc,
            Location::dummy(),
        );

        let instr = Instr::Store {
            e1: Box::new(Exp::Lvar(cleanup_pvar)),
            typ: Typ::void(),
            e2: Box::new(Exp::Var(Ident::create_normal(
                IdentName::from_string("rhs"),
                0,
            ))),
            loc: Location::dummy(),
        };
        state.post.stack.add(
            Var::LogicalVar(Ident::create_normal(IdentName::from_string("rhs"), 0)),
            allocated,
        );

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);

        let has_always_reachable = results.iter().any(|result| match result {
            ExecutionDomain::ContinueProgram(state) => state
                .post
                .attrs
                .get(&allocated)
                .is_some_and(|attrs| attrs.is_always_reachable()),
            _ => false,
        });
        assert!(
            has_always_reachable,
            "storing into a cleanup local should keep the RHS always reachable"
        );
    }

    #[test]
    fn test_call_produces_fresh_return() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("foo");
        let instr = Instr::Call {
            ret: (ret_id.clone(), Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(callee)),
            args: vec![],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&instr, state);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_continue());
    }

    #[test]
    fn test_unknown_call_conservatively_initializes_constant_actuals() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("__infer_skip");
        let instr = Instr::Call {
            ret: (ret_id, Typ::int(sil::typ::IKind::IInt)),
            fun_exp: Exp::Const(Const::Cfun(callee)),
            args: vec![(
                Exp::Const(Const::Cint(IntLit::zero())),
                Typ::int(sil::typ::IKind::IInt),
            )],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&instr, state);

        let continue_state = results.into_iter().find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        });
        let state = continue_state.expect("unknown call should continue");

        let has_initialized_zero_actual = state.post.attrs.iter().any(|(_addr, attrs)| {
            attrs.contains(&crate::attribute::Attribute::Initialized)
                && attrs.iter().any(|attr| {
                    matches!(
                        attr,
                        crate::attribute::Attribute::Invalid(
                            crate::invalidation::Invalidation::ConstantDereference(value),
                            _
                        ) if *value == IntLit::zero()
                    )
                })
        });
        assert!(
            has_initialized_zero_actual,
            "unknown-call argument handling should conservatively initialize constant actuals"
        );
    }

    #[test]
    fn test_unknown_call_pointer_actual_records_unknown_effect_and_written_to() {
        let pname = Procname::c_from_string("test");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("p"),
            Typ::mk_ptr(Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt))),
            Default::default(),
        )];

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let p_pvar = Pvar::mk(Mangled::from_string("p"), pname);
        let p_root = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar.clone())))
            .expect("p should be bound");
        let p_val = state.read_heap(p_root, Access::Dereference);
        let mid_ptr = state.read_heap(p_val, Access::Dereference);
        let leaf_int = state.read_heap(mid_ptr, Access::Dereference);
        state.path_condition.and_is_int(leaf_int);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        state.post.stack.add(Var::LogicalVar(arg_id.clone()), p_val);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 1);
        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("unknown"))),
            args: vec![(
                Exp::Var(arg_id),
                Typ::mk_ptr(Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt))),
            )],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let continue_state = results.into_iter().find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        });
        let state = continue_state.expect("unknown call should continue");

        let actual_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(p_val))
            .expect("pointer actual should keep post attrs");
        assert!(
            actual_attrs.contains(&crate::attribute::Attribute::UnknownEffect),
            "pointer actual roots should record UnknownEffect for caller import"
        );
        assert!(
            actual_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "pointer actual roots should record WrittenTo"
        );

        let mid_ptr_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(mid_ptr))
            .expect("reachable pointer should keep post attrs");
        assert!(
            mid_ptr_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "unknown calls should record WrittenTo on reachable pointer values"
        );

        let leaf_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(leaf_int))
            .expect("reachable integer leaf should stay materialized");
        assert!(
            !leaf_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "known integer leaves should not be marked WrittenTo by unknown-call fallback"
        );
    }

    #[test]
    fn test_prune_eq_zero_constrains() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("p"), 0);
        state.post.stack.add(Var::LogicalVar(id.clone()), p);

        // prune(p == 0) → p is known zero
        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Eq,
                Box::new(Exp::Var(id)),
                Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
            ),
            loc: Location::dummy(),
            is_then_branch: true,
            if_kind: sil::instr::IfKind::If,
        };
        let results = exec_instr(&instr, state);
        assert_eq!(results.len(), 1);
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            assert!(
                s.is_known_zero(p),
                "after prune(p == 0), p should be known zero"
            );
        } else {
            panic!("expected ContinueProgram");
        }
    }

    #[test]
    fn test_prune_eq_zero_records_used_as_branch_cond() {
        let pname = Procname::c_from_string("branch_cond");
        let pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let p = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("p"), 0);
        state.post.stack.add(Var::LogicalVar(id.clone()), p);

        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Eq,
                Box::new(Exp::Var(id)),
                Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
            ),
            loc: Location::dummy(),
            is_then_branch: true,
            if_kind: sil::instr::IfKind::If,
        };
        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);

        assert_eq!(results.len(), 1);
        if let ExecutionDomain::ContinueProgram(state) = &results[0] {
            assert!(
                state
                    .pre
                    .attrs
                    .get(&p)
                    .is_some_and(|attrs| attrs.iter().any(|attr| matches!(
                        attr,
                        crate::attribute::Attribute::UsedAsBranchCond(proc, _)
                            if proc == &pname
                    ))),
                "pruning on a value should record it as a branch condition"
            );
        } else {
            panic!("expected ContinueProgram");
        }
    }

    #[test]
    fn test_prune_ne_zero_constrains() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("p"), 0);
        state.post.stack.add(Var::LogicalVar(id.clone()), p);

        // prune(p != 0) → p is NOT known zero
        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Ne,
                Box::new(Exp::Var(id)),
                Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
            ),
            loc: Location::dummy(),
            is_then_branch: true,
            if_kind: sil::instr::IfKind::If,
        };
        let results = exec_instr(&instr, state);
        assert_eq!(results.len(), 1);
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            assert!(
                !s.is_known_zero(p),
                "after prune(p != 0), p should not be known zero"
            );
        } else {
            panic!("expected ContinueProgram");
        }
    }

    #[test]
    fn test_prune_eq_with_constant_preserves_constant_condition() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("p"), 0);
        state.post.stack.add(Var::LogicalVar(id.clone()), p);

        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Eq,
                Box::new(Exp::Var(id)),
                Box::new(Exp::Const(Const::Cint(IntLit::of_int(4)))),
            ),
            loc: Location::dummy(),
            is_then_branch: true,
            if_kind: sil::instr::IfKind::If,
        };
        let results = exec_instr(&instr, state);

        assert_eq!(results.len(), 1);
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            assert_eq!(
                s.path_condition.conditions().get(&crate::formula::atom::Atom::Equal(
                    crate::formula::term::Term::Var(p),
                    crate::formula::term::Term::Const(4),
                )),
                Some(&0),
                "prune conditions should preserve literal constants instead of collapsing to Var=Var"
            );
        } else {
            panic!("expected ContinueProgram");
        }
    }

    #[test]
    fn test_prune_eq_zero_records_compared_to_null_invalidation() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("p"), 0);
        let loc = Location::dummy();
        state.post.stack.add(Var::LogicalVar(id.clone()), p);

        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Eq,
                Box::new(Exp::Var(id)),
                Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
            ),
            loc: loc.clone(),
            is_then_branch: true,
            if_kind: sil::instr::IfKind::If,
        };
        let results = exec_instr(&instr, state);

        assert_eq!(results.len(), 1);
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            let Some(attrs) = s.post.attrs.get(&p) else {
                panic!("expected compared-to-null invalidation on pruned value");
            };
            assert!(
                matches!(
                    attrs.get_invalid(),
                    Some((
                        crate::invalidation::Invalidation::ComparedToNullInThisProcedure(found),
                        _
                    )) if *found == loc
                ),
                "prune(p == 0) should record a ComparedToNull invalidation"
            );
        } else {
            panic!("expected ContinueProgram");
        }
    }

    #[test]
    fn test_malloc_allocates() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("malloc");
        let instr = Instr::Call {
            ret: (ret_id.clone(), Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(callee)),
            args: vec![],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&instr, state);
        assert_eq!(results.len(), 2, "malloc returns ok + null disjuncts");
        assert!(results.iter().all(|r| r.is_continue()));

        // The return value should be bound in both disjuncts
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            let var = Var::LogicalVar(ret_id);
            assert!(
                s.post.stack.find(&var).is_some(),
                "malloc should bind return id"
            );
        }
    }

    #[test]
    fn test_free_invalidates() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let ptr = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr);
        // Allocate first so it's valid
        state.allocate(ptr, crate::attribute::Allocator::CMalloc, Location::dummy());

        // free(p)
        let callee = Procname::c_from_string("free");
        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(callee)),
            args: vec![(Exp::Lvar(pvar), Typ::void())],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&instr, state);
        assert!(results.iter().any(|r| r.is_continue()));

        // After free, some disjunct should have ptr invalidated
        let has_freed = results.iter().any(|r| {
            if let ExecutionDomain::ContinueProgram(s) = r {
                s.check_valid(ptr).is_err()
            } else {
                false
            }
        });
        assert!(
            has_freed,
            "some disjunct should have freed pointer as invalid"
        );
    }

    #[test]
    fn test_known_model_conservatively_initializes_actual_reachable_values() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let ptr = AbstractValue::mk_fresh();
        let loaded = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr);
        state.write_heap(ptr, Access::Dereference, loaded);
        state.allocate(ptr, crate::attribute::Allocator::CMalloc, Location::dummy());
        let _ = state.and_not_equal(&Operand::AbstractValue(ptr), &Operand::ConstOperand(0));

        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("free"))),
            args: vec![(Exp::Lvar(pvar), Typ::void())],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&instr, state);
        let state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("free should keep a valid non-null path");

        assert!(
            state
                .post
                .attrs
                .get(&loaded)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::Initialized)),
            "known-model calls should conservatively initialize caller-visible values reachable from actuals"
        );
    }

    #[test]
    fn test_use_after_free_detected() {
        let mut state = mk_state();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));

        // Step 1: p = malloc()
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let malloc_instr = Instr::Call {
            ret: (n0.clone(), Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("malloc"))),
            args: vec![],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&malloc_instr, state);
        state = match results.into_iter().next().unwrap() {
            ExecutionDomain::ContinueProgram(s) => s,
            _ => panic!("malloc should continue"),
        };

        // Bind p = n0
        let ptr_val = state.post.stack.find(&Var::LogicalVar(n0.clone())).unwrap();
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr_val);

        // Step 2: free(p)
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let free_instr = Instr::Call {
            ret: (n1, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("free"))),
            args: vec![(Exp::Lvar(pvar.clone()), Typ::void())],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };
        let results = exec_instr(&free_instr, state);
        state = match results.into_iter().find(|r| r.is_continue()).unwrap() {
            ExecutionDomain::ContinueProgram(s) => s,
            _ => panic!("free should continue"),
        };

        // Step 3: *p = 42 (use after free!)
        let store_instr = Instr::Store {
            e1: Box::new(Exp::Lvar(pvar)),
            typ: Typ::void(),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
            loc: Location::dummy(),
        };
        let results = exec_instr(&store_instr, state);

        let has_abort = results.iter().any(|r| match r {
            ExecutionDomain::AbortProgram { diagnostic, .. } => {
                matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress { .. }
                )
            }
            _ => false,
        });
        assert!(has_abort, "use after free should produce AbortProgram");
    }
}
