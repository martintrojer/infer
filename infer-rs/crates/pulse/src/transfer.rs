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
use sil::instr::{Instr, InstrMetadata};
use sil::location::Location;
use sil::procdesc::Procdesc;
use sil::tenv::Tenv;
use sil::typ::TypeDesc;
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::attribute::Attribute;
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
    exec_instr_with_pdesc_and_tenv(pdesc, None, instr, state)
}

/// Execute a single SIL instruction with access to the enclosing procedure and
/// type environment.
pub fn exec_instr_with_pdesc_and_tenv(
    pdesc: Option<&Procdesc>,
    tenv: Option<&Tenv>,
    instr: &Instr,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    match instr {
        Instr::Load { id, e, loc, typ } => exec_load(pdesc, id, e, typ, loc, state),
        Instr::Store {
            e1, e2, loc, typ, ..
        } => exec_store(pdesc, e1, typ, e2, loc, state),
        Instr::Prune { exp, loc, .. } => exec_prune(pdesc, exp, loc, state),
        Instr::Call {
            ret: (ret_id, ret_typ),
            fun_exp,
            args,
            loc,
            ..
        } => exec_call(
            CallContext {
                pdesc,
                tenv,
                ret_id,
                ret_typ,
                fun_exp,
                args,
                loc,
            },
            state,
        ),
        Instr::Metadata(InstrMetadata::ExitScope(vars, _loc)) => {
            // Cross-ref: OCaml `Pulse.ml` handles `Metadata (ExitScope ...)`
            // by removing dead vars from the post stack while preserving
            // pre-rooted vars that must survive into summaries.
            let mut state = state;
            state.remove_vars(vars);
            vec![ExecutionDomain::ContinueProgram(state)]
        }
        Instr::Metadata(InstrMetadata::VariableLifetimeBegins {
            pvar,
            typ,
            loc,
            is_cpp_structured_binding,
        }) if !pvar.is_global() => {
            exec_variable_lifetime_begins(pvar, typ, loc, *is_cpp_structured_binding, state)
        }
        Instr::Metadata(_) => vec![ExecutionDomain::ContinueProgram(state)],
    }
}

fn exec_variable_lifetime_begins(
    pvar: &sil::pvar::Pvar,
    typ: &sil::typ::Typ,
    loc: &Location,
    is_cpp_structured_binding: bool,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Cross-ref: OCaml `Pulse.ml` routes this metadata to
    // `PulseOperations.realloc_pvar`, which rebinds the local to a fresh stack
    // slot and marks scalar/pointer locals uninitialized. Rust does not thread
    // Tenv through transfer yet, so we mirror the root local-slot behavior
    // here and keep the uninitialized marking to types we can model locally.
    let addr = AbstractValue::mk_fresh();
    let history = ValueHistory::assignment(loc.clone());
    state.post.stack.add_with_history(
        Var::ProgramVar(Box::new(pvar.clone())),
        ValueWithHistory::new(addr, history),
    );
    state.post.heap.register_address(addr);

    let should_mark_uninitialized = !is_cpp_structured_binding
        && matches!(
            &*typ.desc,
            sil::typ::TypeDesc::Tint(_)
                | sil::typ::TypeDesc::Tfloat(_)
                | sil::typ::TypeDesc::Tptr(..)
        );
    if should_mark_uninitialized {
        state.add_attr(addr, Attribute::Uninitialized);
    }

    vec![ExecutionDomain::ContinueProgram(state)]
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
            record_more_precise_formal_dynamic_type(pdesc, rhs_exp, typ, value.addr, &mut state);
            vec![ExecutionDomain::ContinueProgram(state)]
        }
        PulseResult::Recoverable(value, errors) => {
            operations::write_id_with_history(id, value.clone(), &mut state);
            if typ.is_int() {
                state.path_condition.and_is_int(value.addr);
            }
            record_more_precise_formal_dynamic_type(pdesc, rhs_exp, typ, value.addr, &mut state);
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

fn record_more_precise_formal_dynamic_type(
    pdesc: Option<&Procdesc>,
    rhs_exp: &Exp,
    load_typ: &sil::typ::Typ,
    value: crate::abstract_value::AbstractValue,
    state: &mut AbductiveDomain,
) {
    if state.get_dynamic_type(value).is_some() {
        return;
    }
    let (Some(pdesc), Exp::Lvar(pvar)) = (pdesc, rhs_exp) else {
        return;
    };
    if !matches!(
        &pvar.kind,
        sil::pvar::PvarKind::Local { proc_name, .. } if *proc_name == pdesc.proc_name
    ) {
        return;
    }
    let Some((_mangled, formal_typ, _annot)) = pdesc
        .formals
        .iter()
        .find(|(mangled, _typ, _annot)| mangled == &pvar.name)
    else {
        return;
    };
    if formal_typ == load_typ {
        return;
    }
    if let sil::typ::TypeDesc::Tptr(pointee, _) = formal_typ.desc.as_ref() {
        if matches!(pointee.desc.as_ref(), sil::typ::TypeDesc::Tstruct(_)) {
            state.add_dynamic_type_unsafe(value, (**pointee).clone());
        }
    }
}

/// Store: `*lhs_exp = rhs_exp`
fn exec_store(
    pdesc: Option<&Procdesc>,
    lhs_exp: &Exp,
    lhs_typ: &sil::typ::Typ,
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

    // Cross-ref: OCaml `Pulse.exec_instr_aux Store` evaluates the LHS with
    // mode=Write so that the outermost Lfield/Lindex base check abduces
    // `MustBeValid` only (no `MustBeInitialized`) on the formal whose
    // address we are about to overwrite. Using Read here — as Rust did
    // before — systematically over-attaches `MustBeInitialized` to every
    // formal that appears on the LHS of a store, e.g. `q->next = q` adding
    // it to `q.*`.
    let (lhs_addr, lhs_errors) = match operations::eval_with_history_mode(
        lhs_exp,
        operations::AccessMode::Write,
        loc,
        &mut state,
    ) {
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

    let stored_value_addr = rhs_val.addr;
    let lhs_addr_value = lhs_addr.addr;
    match operations::write_deref_with_history(lhs_addr, rhs_val, loc, &mut state) {
        PulseResult::Ok(()) => {
            preserve_canonical_pointee_after_store(
                lhs_typ,
                lhs_addr_value,
                stored_value_addr,
                &mut state,
            );
            vec![ExecutionDomain::ContinueProgram(state)]
        }
        PulseResult::FatalError(d, _) => vec![ExecutionDomain::AbortProgram {
            state: Box::new(state),
            diagnostic: Box::new(d),
        }],
        PulseResult::Recoverable((), errors) => {
            stopped_results_from_recoverable_errors(pdesc, state, errors)
        }
    }
}

fn preserve_canonical_pointee_after_store(
    lhs_typ: &sil::typ::Typ,
    lhs_addr: AbstractValue,
    stored_value: AbstractValue,
    state: &mut AbductiveDomain,
) {
    let TypeDesc::Tptr(pointee_typ, _) = lhs_typ.desc.as_ref() else {
        return;
    };
    if !(pointee_typ.is_int() || pointee_typ.is_pointer()) {
        return;
    }

    let stored_repr = state.path_condition.get_var_repr(stored_value);
    if state
        .post
        .heap
        .find_edge(lhs_addr, &crate::access::Access::Dereference)
        .is_some_and(|edge| edge == stored_repr)
    {
        return;
    }

    let Some(q) = state.path_condition.is_known_const(stored_value) else {
        return;
    };
    if !q.is_integer() {
        return;
    }
    let canonical = state.absval_of_int(*q.numer() / *q.denom());
    if canonical != stored_repr {
        state.write_heap(lhs_addr, crate::access::Access::Dereference, canonical);
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
    // Cross-ref: OCaml `Pulse.check_config_usage` runs over the prune
    // expression *before* the prune itself and walks `Exp.free_vars`
    // (logical idents only, not Lvars). It then resolves each ident through
    // `read_id` and abduces `UsedAsBranchCond` on the stack value via
    // `PulseAbductiveDomain.SafeAttributes.abduce_one`, which silently
    // drops the attribute when the address is not already materialised in
    // `pre.heap`. Mirror that here so we do not stamp `UsedAsBranchCond`
    // on derived intermediates (e.g. `&x` from `if (y == &x)`) or on
    // values that never reflect a caller-visible read.
    record_branch_cond_for_free_idents(pdesc, exp, loc, &mut state);
    if prune_expr(pdesc, exp, loc, &mut state) {
        log::trace!("  [prune] SAT: {exp}");
        vec![ExecutionDomain::ContinueProgram(state)]
    } else {
        log::debug!("  [prune] UNSAT (path killed): {exp}");
        vec![]
    }
}

/// Walk `exp` collecting free logical idents (mirroring OCaml's
/// `Exp.free_vars`) and abduce `UsedAsBranchCond` on the stack value behind
/// each ident. Pure traversal — no formula evaluation, no fresh allocations.
fn record_branch_cond_for_free_idents(
    pdesc: Option<&Procdesc>,
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) {
    let Some(pdesc) = pdesc else {
        return;
    };
    let mut idents = Vec::new();
    collect_free_idents(exp, &mut idents);
    for id in idents {
        let var = sil::var::Var::LogicalVar(id);
        let Some(addr) = state.post.stack.find(&var) else {
            continue;
        };
        let repr = state.path_condition.get_var_repr(addr);
        // Same gating as OCaml's abduce_one: only attach the attribute if
        // the address is already materialised in `pre.heap`.
        if state.pre.heap.get_edges(repr).is_none() {
            continue;
        }
        state.pre.attrs.add_one(
            repr,
            crate::attribute::Attribute::UsedAsBranchCond(pdesc.proc_name.clone(), loc.clone()),
        );
    }
}

/// Mirror OCaml's `Exp.free_vars` — collect only logical idents (`Exp::Var`),
/// not program-var addresses (`Exp::Lvar`) or constants.
fn collect_free_idents(exp: &Exp, out: &mut Vec<sil::ident::Ident>) {
    match exp {
        Exp::Var(id) => out.push(id.clone()),
        Exp::Cast(_, inner)
        | Exp::UnOp(_, inner, _)
        | Exp::Exn(inner)
        | Exp::Lfield(sil::exp::LfieldObjData { exp: inner, .. }, _, _) => {
            collect_free_idents(inner, out)
        }
        Exp::BinOp(_, lhs, rhs) | Exp::Lindex(lhs, rhs) => {
            collect_free_idents(lhs, out);
            collect_free_idents(rhs, out);
        }
        Exp::Closure(c) => {
            for (e, _) in &c.captured_vars {
                collect_free_idents(e, out);
            }
        }
        Exp::Const(_) | Exp::Lvar(_) | Exp::Sizeof(_) => {}
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

fn prune_eq_operands(
    _pdesc: Option<&Procdesc>,
    lhs: &Exp,
    rhs: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
    negated: bool,
) -> bool {
    let lhs = eval_operand_for_prune(lhs, loc, state);
    let rhs = eval_operand_for_prune(rhs, loc, state);
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

struct CallContext<'a> {
    pdesc: Option<&'a Procdesc>,
    tenv: Option<&'a Tenv>,
    ret_id: &'a sil::ident::Ident,
    ret_typ: &'a sil::typ::Typ,
    fun_exp: &'a Exp,
    args: &'a [(Exp, sil::typ::Typ)],
    loc: &'a Location,
}

/// Call: `ret_id = fun_exp(args)`
///
/// Dispatches to built-in models (malloc/free/etc.) via `models::dispatch`.
/// Unknown functions get a fresh return value.
fn exec_call(ctx: CallContext<'_>, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    let CallContext {
        pdesc,
        tenv,
        ret_id,
        ret_typ,
        fun_exp,
        args,
        loc,
    } = ctx;
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
            if let Some(results) = crate::models::dispatch(
                tenv,
                pdesc.map(|proc| &proc.proc_name),
                callee,
                ret_id,
                args,
                loc,
                state.clone(),
            ) {
                return results;
            }
        }
    }

    // Default: treat as unknown — havoc the return value and pointer args.
    log::debug!("  [call] unknown: {fun_exp}");
    let actuals_with_history: Vec<_> = args
        .iter()
        .map(|(arg_exp, _arg_typ)| operations::eval_or_fresh_with_history(arg_exp, loc, &mut state))
        .collect();
    let actual_vals: Vec<_> = actuals_with_history
        .iter()
        .map(|actual| actual.addr)
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
    // Cross-ref: OCaml `PulseCallOperations.add_returned_from_unknown`.
    // Keep the actual-value dependency on the fresh return so specialized
    // summaries preserve copy / provenance shape for unknown calls such as
    // `invoke(*f = add_more_bad)`.
    if !actual_vals.is_empty() {
        state.add_attr(
            ret_val,
            crate::attribute::Attribute::ReturnedFromUnknown(actual_vals.clone()),
        );
    }
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
        for ((arg_exp, arg_typ), actual) in args.iter().zip(actuals_with_history.iter()) {
            if should_havoc_unknown_call_arg(arg_typ) {
                is_pure = false;
                if should_materialize_unknown_call_pointer_actual(arg_typ) {
                    materialize_unknown_call_pointer_if_needed(actual, &mut state);
                }
                apply_unknown_call_pointer_actual_effect(arg_exp, actual.addr, loc, &mut state);
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

fn should_havoc_unknown_call_arg(arg_typ: &sil::typ::Typ) -> bool {
    arg_typ.is_pointer() || matches!(&*arg_typ.desc, sil::typ::TypeDesc::Tfun(_))
}

fn should_materialize_unknown_call_pointer_actual(arg_typ: &sil::typ::Typ) -> bool {
    !matches!(
        arg_typ.desc.as_ref(),
        sil::typ::TypeDesc::Tptr(pointee, _)
            if matches!(pointee.desc.as_ref(), sil::typ::TypeDesc::Tstruct(_))
    )
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
        .filter(|addr| {
            !state.path_condition.phi().is_marked_int(*addr) && !state.is_known_zero(*addr)
        })
        .collect();
    state.apply_unknown_effect(arg_val);
    state.add_attr(arg_val, crate::attribute::Attribute::UnknownEffect);
    state.mark_written_to_addrs_at(written_before_havoc, loc);
    operations::refresh_unknown_lvalue_root(arg_exp, arg_val, state);
}

fn materialize_unknown_call_pointer_if_needed(
    actual: &ValueWithHistory,
    state: &mut AbductiveDomain,
) {
    let actual_addr = state.path_condition.get_var_repr(actual.addr);
    if state.check_valid(actual_addr).is_err()
        || state
            .post
            .heap
            .find_edge(actual_addr, &crate::access::Access::Dereference)
            .is_some()
    {
        return;
    }

    // Cross-ref: OCaml unknown-call havoc can expose a fresh pointee cell even
    // when the caller only passed a pointer value (for example a loaded
    // callback actual in recursive specialization cases). Materialize that
    // pointee first so the later havoc rewrites the post edge but keeps the
    // pre/post reachable-cell shape.
    let target = state.read_heap_with_history(actual.clone(), crate::access::Access::Dereference);
    if state
        .pre
        .heap
        .find_edge(actual_addr, &crate::access::Access::Dereference)
        .is_none()
        && state.pre.heap.get_edges(actual_addr).is_some()
    {
        state.pre.heap.add_edge_with_history(
            actual_addr,
            crate::access::Access::Dereference,
            target.clone(),
        );
        state.pre.heap.register_address(target.addr);
    }
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
    use sil::instr::InstrMetadata;
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
    fn test_exit_scope_removes_dead_post_stack_vars() {
        let mut state = mk_state();
        let tmp = Ident::create_normal(IdentName::from_string("tmp"), 0);
        let local = Pvar::mk(Mangled::from_string("x"), Procname::c_from_string("test"));
        let tmp_addr = AbstractValue::mk_fresh();
        let local_addr = AbstractValue::mk_fresh();
        state.post.stack.add(Var::LogicalVar(tmp.clone()), tmp_addr);
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(local.clone())), local_addr);

        let instr = Instr::Metadata(InstrMetadata::ExitScope(
            vec![
                Var::LogicalVar(tmp.clone()),
                Var::ProgramVar(Box::new(local.clone())),
            ],
            Location::dummy(),
        ));
        let results = exec_instr(&instr, state);
        let state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("exit_scope should continue");

        assert!(
            state.post.stack.find(&Var::LogicalVar(tmp)).is_none(),
            "ExitScope should remove dead logical vars from the post stack"
        );
        assert!(
            state
                .post
                .stack
                .find(&Var::ProgramVar(Box::new(local)))
                .is_none(),
            "ExitScope should remove dead local vars from the post stack"
        );
    }

    #[test]
    fn test_exit_scope_keeps_pre_rooted_formals() {
        let pname = Procname::c_from_string("keep_formal");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];

        let state = AbductiveDomain::mk_initial(&pdesc);
        let formal = Pvar::mk(Mangled::from_string("x"), pname);
        let formal_var = Var::ProgramVar(Box::new(formal.clone()));
        let formal_addr = state
            .post
            .stack
            .find(&formal_var)
            .expect("formal should be present in post before exit_scope");

        let instr = Instr::Metadata(InstrMetadata::ExitScope(
            vec![formal_var.clone()],
            Location::dummy(),
        ));
        let results = exec_instr(&instr, state);
        let state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("exit_scope should continue");

        assert_eq!(
            state.post.stack.find(&formal_var),
            Some(formal_addr),
            "ExitScope should not drop pre-rooted formal vars needed for summaries"
        );
    }

    #[test]
    fn test_variable_lifetime_begins_rebinds_local_and_marks_scalar_uninitialized() {
        let mut state = mk_state();
        let local = Pvar::mk(Mangled::from_string("x"), Procname::c_from_string("test"));
        let old_addr = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(local.clone())), old_addr);

        let instr = Instr::Metadata(InstrMetadata::VariableLifetimeBegins {
            pvar: local.clone(),
            typ: Typ::int(sil::typ::IKind::IInt),
            loc: Location::dummy(),
            is_cpp_structured_binding: false,
        });
        let results = exec_instr(&instr, state);
        let state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("variable lifetime begins should continue");

        let var = Var::ProgramVar(Box::new(local));
        let addr = state
            .post
            .stack
            .find(&var)
            .expect("local should be rebound to a fresh slot");
        assert_ne!(
            addr, old_addr,
            "VariableLifetimeBegins should behave like realloc_pvar and replace the old slot"
        );
        assert!(
            state.check_initialized(addr).is_err(),
            "scalar locals declared by VariableLifetimeBegins should start uninitialized"
        );
    }

    #[test]
    fn test_variable_lifetime_begins_structured_binding_skips_uninitialized_mark() {
        let state = mk_state();
        let local = Pvar::mk(Mangled::from_string("x"), Procname::c_from_string("test"));

        let instr = Instr::Metadata(InstrMetadata::VariableLifetimeBegins {
            pvar: local.clone(),
            typ: Typ::int(sil::typ::IKind::IInt),
            loc: Location::dummy(),
            is_cpp_structured_binding: true,
        });
        let results = exec_instr(&instr, state);
        let state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("variable lifetime begins should continue");

        let addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(local)))
            .expect("structured binding local should be bound");
        assert!(
            state.check_initialized(addr).is_ok(),
            "structured bindings should skip the uninitialized mark like OCaml"
        );
    }

    #[test]
    fn test_load_directly_from_formal_records_formal_pointee_dynamic_type() {
        let pname = Procname::c_from_string("formal_load");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let precise_type = Typ::mk_struct(sil::typ::TypeName::HackClass(sil::typ::HackClassName(
            "A".to_string(),
        )));
        let load_type = Typ::mk_ptr(Typ::mk_struct(sil::typ::TypeName::HackClass(
            sil::typ::HackClassName("Base".to_string()),
        )));
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(precise_type.clone()),
            Default::default(),
        )];

        let state = AbductiveDomain::mk_initial(&pdesc);
        let id = Ident::create_normal(IdentName::from_string("n"), 0);
        let instr = Instr::Load {
            id: id.clone(),
            e: Exp::Lvar(Pvar::mk(Mangled::from_string("x"), pname)),
            typ: load_type,
            loc: Location::dummy(),
        };
        let state = exec_instr_with_pdesc(Some(&pdesc), &instr, state)
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("formal load should continue");
        let value = state
            .post
            .stack
            .find(&Var::LogicalVar(id))
            .expect("load result should be written");

        assert_eq!(state.get_dynamic_type(value), Some(&precise_type));
    }

    #[test]
    fn test_load_from_nonlocal_pvar_with_formal_name_does_not_record_dynamic_type() {
        let pname = Procname::c_from_string("formal_load");
        let other_pname = Procname::c_from_string("other_proc");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        let precise_type = Typ::mk_struct(sil::typ::TypeName::HackClass(sil::typ::HackClassName(
            "A".to_string(),
        )));
        let load_type = Typ::mk_ptr(Typ::mk_struct(sil::typ::TypeName::HackClass(
            sil::typ::HackClassName("Base".to_string()),
        )));
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(precise_type),
            Default::default(),
        )];

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let other_pvar = Pvar::mk(Mangled::from_string("x"), other_pname);
        let stack_addr = AbstractValue::mk_fresh();
        let value = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(other_pvar.clone())), stack_addr);
        state
            .post
            .heap
            .add_edge(stack_addr, Access::Dereference, value);

        let id = Ident::create_normal(IdentName::from_string("n"), 0);
        let instr = Instr::Load {
            id: id.clone(),
            e: Exp::Lvar(other_pvar),
            typ: load_type,
            loc: Location::dummy(),
        };
        let state = exec_instr_with_pdesc(Some(&pdesc), &instr, state)
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("nonlocal pvar load should continue");
        let value = state
            .post
            .stack
            .find(&Var::LogicalVar(id))
            .expect("load result should be written");

        assert!(state.get_dynamic_type(value).is_none());
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
    fn test_store_through_freed_formal_is_not_latent_invalid_access() {
        let pname = Procname::c_from_string("formal_uaf_store");
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
            crate::invalidation::Invalidation::CFree,
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
        );

        let instr = Instr::Store {
            e1: Box::new(Exp::Var(loaded)),
            typ: Typ::void(),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
            loc: Location::dummy(),
        };
        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);

        assert!(
            !matches!(results.as_slice(), [ExecutionDomain::LatentInvalidAccess { .. }]),
            "use-after-free on a direct formal should not be classified as LatentInvalidAccess: {results:?}"
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
        let _heap_target = state.read_heap(p_val, Access::Dereference);
        assert!(state.and_equal_const(p_val, 0).is_sat());
        assert!(
            state.check_valid(p_val).is_ok(),
            "local EqZero should no longer synthesize Invalid(ConstantDereference(0)); the invalid access is carried by sideband/summary reification"
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
        assert!(
            !results.iter().any(|r| matches!(r, ExecutionDomain::AbortProgram { .. })),
            "local EqZero should not abort immediately after synthesizing an Invalid(0) attr; summary export reifies the sideband instead"
        );
    }

    #[test]
    fn test_store_lvar_marks_written_cell_initialized_without_initializing_pointee() {
        let loc = Location::dummy();
        let pname = Procname::c_from_string("test");
        let pvar = Pvar::mk(Mangled::from_string("x"), pname.clone());
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());
        pdesc.formals = vec![(
            pvar.name.clone(),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar.clone())))
            .expect("formal should be bound");
        let rhs_id = Ident::create_normal(IdentName::from_string("rhs"), 0);
        let rhs_val = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(rhs_id.clone()), rhs_val);

        let instr = Instr::Store {
            e1: Box::new(Exp::Lvar(pvar)),
            typ: Typ::mk_ptr(Typ::void()),
            e2: Box::new(Exp::Var(rhs_id)),
            loc,
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let Some(ExecutionDomain::ContinueProgram(state)) = results.into_iter().next() else {
            panic!("store should continue");
        };
        let formal_attrs = state
            .post
            .attrs
            .get(&formal_addr)
            .expect("store should leave attrs on the written formal cell");
        assert!(
            formal_attrs.contains(&crate::attribute::Attribute::Initialized),
            "Store LHS Lvar should mark the written formal cell Initialized"
        );
        assert!(
            formal_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "Store LHS Lvar should mark the written formal cell WrittenTo"
        );
        assert!(
            !state
                .post
                .attrs
                .get(&rhs_val)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::Initialized)),
            "assigning a pointer value into the formal cell must not initialize the RHS/pointee"
        );
    }

    #[test]
    fn test_store_lfield_marks_field_cell_initialized_without_initializing_formal_base() {
        use sil::fieldname::Fieldname;
        use sil::qualified_cpp_name::QualifiedCppName;
        use sil::typ::TypeName;

        let loc = Location::dummy();
        let pname = Procname::c_from_string("test");
        let pvar = Pvar::mk(Mangled::from_string("q"), pname.clone());
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());
        pdesc.formals = vec![(
            pvar.name.clone(),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_value = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar.clone())))
            .expect("formal should be bound");
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("node")),
            "next",
        );

        let rhs_id = Ident::create_normal(IdentName::from_string("rhs"), 0);
        let rhs_val = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(rhs_id.clone()), rhs_val);
        let instr = Instr::Store {
            e1: Box::new(Exp::Lfield(
                sil::exp::LfieldObjData {
                    exp: Box::new(Exp::Lvar(pvar)),
                    is_implicit: false,
                },
                field.clone(),
                Typ::void(),
            )),
            typ: Typ::void(),
            e2: Box::new(Exp::Var(rhs_id)),
            loc,
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let Some(ExecutionDomain::ContinueProgram(state)) = results.into_iter().next() else {
            panic!("field store should continue");
        };
        let field_cell = state
            .post
            .heap
            .find_edge_with_history(formal_value, &Access::FieldAccess(field))
            .expect("evaluating the Lfield should materialize the field cell")
            .addr;
        let field_attrs = state
            .post
            .attrs
            .get(&field_cell)
            .expect("store should leave attrs on the written field cell");
        assert!(
            field_attrs.contains(&crate::attribute::Attribute::Initialized),
            "Store LHS Lfield should mark the field cell Initialized"
        );
        assert!(
            field_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "Store LHS Lfield should mark the field cell WrittenTo"
        );
        let base_attrs = state
            .post
            .attrs
            .get(&formal_value)
            .expect("OCaml Write-mode Lfield base checks initialize the formal pointer base");
        assert!(
            base_attrs.contains(&crate::attribute::Attribute::Initialized),
            "Lfield base check in Write mode should initialize the formal pointer base, matching OCaml `check_addr_access Write`"
        );
        assert!(
            !base_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "Lfield base check must not mark the formal pointer base WrittenTo; only the field cell is written"
        );
        let pre_base_attrs = state
            .pre
            .attrs
            .get(&formal_value)
            .expect("field access should abduce validity on the formal pointer base");
        assert!(
            pre_base_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::MustBeValid(_, _, _))),
            "field-store base should still abduce MustBeValid"
        );
        assert!(
            !pre_base_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::MustBeInitialized(_, _))),
            "field-store base should not abduce MustBeInitialized"
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
    fn test_unknown_call_struct_pointer_value_does_not_materialize_missing_pointee_before_havoc() {
        let pname = Procname::c_from_string("test");
        let node_typ = Typ::mk_struct(sil::typ::TypeName::CStruct(
            sil::qualified_cpp_name::QualifiedCppName::from_string("node_st"),
        ));
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("p"),
            Typ::mk_ptr(Typ::mk_ptr(node_typ.clone())),
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
        assert!(
            state.pre.heap.get_edges(p_val).is_some(),
            "loading the formal struct pointer should register the pointee root in pre"
        );
        assert!(
            state
                .pre
                .heap
                .find_edge(p_val, &Access::Dereference)
                .is_none(),
            "the struct pointee cell should start unmaterialized"
        );

        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        state.post.stack.add(Var::LogicalVar(arg_id.clone()), p_val);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 1);
        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("unknown"))),
            args: vec![(Exp::Var(arg_id), Typ::mk_ptr(node_typ))],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let continue_state = results.into_iter().find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        });
        let state = continue_state.expect("unknown call should continue");

        assert!(
            state
                .pre
                .heap
                .find_edge(p_val, &Access::Dereference)
                .is_none(),
            "unknown-call fallback should not synthesize an extra pre pointee for struct pointer values"
        );
        assert!(
            state
                .post
                .heap
                .find_edge(p_val, &Access::Dereference)
                .is_none(),
            "without a pre-existing field read, struct pointer values should not gain an extra post pointee"
        );
    }

    #[test]
    fn test_unknown_call_pointer_value_materializes_missing_pointee_before_havoc() {
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
        assert!(
            state.pre.heap.get_edges(p_val).is_some(),
            "loading the formal pointer should register the pointee root in pre"
        );
        assert!(
            state
                .pre
                .heap
                .find_edge(p_val, &Access::Dereference)
                .is_none(),
            "the pointee cell should start unmaterialized"
        );

        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        state.post.stack.add(Var::LogicalVar(arg_id.clone()), p_val);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 1);
        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("unknown"))),
            args: vec![(
                Exp::Var(arg_id),
                Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
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

        let pre_target = state
            .pre
            .heap
            .find_edge(p_val, &Access::Dereference)
            .expect("unknown-call pointer havoc should materialize a pre pointee");
        let post_target = state
            .post
            .heap
            .find_edge(p_val, &Access::Dereference)
            .expect("unknown-call pointer havoc should keep a post pointee");
        assert_ne!(
            state.path_condition.get_var_repr(pre_target),
            state.path_condition.get_var_repr(post_target),
            "havoc should replace the post pointee with a fresh value"
        );

        let pre_target_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(pre_target))
            .expect("the pre pointee should remain visible in post attrs");
        assert!(
            pre_target_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "the old pointee should record WrittenTo before the post edge is refreshed"
        );
    }

    #[test]
    fn test_unknown_call_function_value_materializes_missing_pointee_before_havoc() {
        let pname = Procname::c_from_string("test");
        let fun_typ = Typ::mk(sil::typ::TypeDesc::Tfun(None));
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("f"),
            Typ::mk_ptr(fun_typ.clone()),
            Default::default(),
        )];

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let f_pvar = Pvar::mk(Mangled::from_string("f"), pname);
        let f_root = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(f_pvar.clone())))
            .expect("f should be bound");
        let fun_val = state.read_heap(f_root, Access::Dereference);
        assert!(
            state.pre.heap.get_edges(fun_val).is_some(),
            "loading the formal function pointer should register the pointee root in pre"
        );
        assert!(
            state
                .pre
                .heap
                .find_edge(fun_val, &Access::Dereference)
                .is_none(),
            "the function-value pointee should start unmaterialized"
        );

        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        state
            .post
            .stack
            .add(Var::LogicalVar(arg_id.clone()), fun_val);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 1);
        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("unknown"))),
            args: vec![(Exp::Var(arg_id), fun_typ)],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let continue_state = results.into_iter().find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        });
        let state = continue_state.expect("unknown call should continue");

        let pre_target = state
            .pre
            .heap
            .find_edge(fun_val, &Access::Dereference)
            .expect("function-value unknown-call havoc should materialize a pre pointee");
        let post_target = state
            .post
            .heap
            .find_edge(fun_val, &Access::Dereference)
            .expect("function-value unknown-call havoc should keep a post pointee");
        assert_ne!(
            state.path_condition.get_var_repr(pre_target),
            state.path_condition.get_var_repr(post_target),
            "havoc should replace the post function-value pointee with a fresh value"
        );
    }

    #[test]
    fn test_unknown_call_return_records_returned_from_unknown_actuals() {
        let pname = Procname::c_from_string("test");
        let mut pdesc = Procdesc::new(
            pname.clone(),
            Typ::int(sil::typ::IKind::IInt),
            Location::dummy(),
        );
        pdesc.formals = vec![(
            Mangled::from_string("i"),
            Typ::int(sil::typ::IKind::IInt),
            Default::default(),
        )];

        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let i_pvar = Pvar::mk(Mangled::from_string("i"), pname);
        let i_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(i_pvar)))
            .expect("i should be bound");
        let i_val = state.read_heap(i_addr, Access::Dereference);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        state.post.stack.add(Var::LogicalVar(arg_id.clone()), i_val);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 1);
        let instr = Instr::Call {
            ret: (ret_id.clone(), Typ::int(sil::typ::IKind::IInt)),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("unknown"))),
            args: vec![(Exp::Var(arg_id), Typ::int(sil::typ::IKind::IInt))],
            loc: Location::dummy(),
            flags: CallFlags::default(),
        };

        let results = exec_instr_with_pdesc(Some(&pdesc), &instr, state);
        let continue_state = results.into_iter().find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        });
        let state = continue_state.expect("unknown call should continue");
        let ret_val = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return should be bound");
        let ret_attrs = state
            .post
            .attrs
            .get(&ret_val)
            .expect("return should keep post attrs");
        assert!(
            ret_attrs.iter().any(|attr| {
                matches!(
                    attr,
                    crate::attribute::Attribute::ReturnedFromUnknown(values)
                        if values == &vec![state.path_condition.get_var_repr(i_val)]
                )
            }),
            "unknown-call return should record ReturnedFromUnknown(actuals)"
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
        // Cross-ref: OCaml `PulseAbductiveDomain.SafeAttributes.abduce_one`
        // gates `UsedAsBranchCond` abduction on `BaseMemory.mem` over
        // `pre.heap`. Mirror a Load-then-Prune sequence by registering `p`
        // in pre.heap here.
        state.pre.heap.register_address(p);

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
