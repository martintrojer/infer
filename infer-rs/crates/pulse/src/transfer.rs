// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse transfer functions: SIL instructions → state transitions.
//!
//! Mirrors OCaml's `Pulse.ml` exec_instr (simplified).
//!
//! Maps each SIL instruction to a list of possible resulting states
//! (the list has >1 element when an error is found — one AbortProgram
//! and one ContinueProgram for the "maybe it's ok" case).

use sil::exp::Exp;
use sil::instr::Instr;
use sil::location::Location;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::execution_domain::ExecutionDomain;
use crate::operations;
use crate::pulse_result::PulseResult;

/// Execute a single SIL instruction on the abstract state.
///
/// Returns a list of resulting execution domains. Most instructions
/// produce exactly one ContinueProgram; error-finding instructions
/// may produce an AbortProgram.
pub fn exec_instr(instr: &Instr, state: AbductiveDomain) -> Vec<ExecutionDomain> {
    match instr {
        Instr::Load { id, e, loc, typ } => exec_load(id, e, typ, loc, state),
        Instr::Store { e1, e2, loc, .. } => exec_store(e1, e2, loc, state),
        Instr::Prune { exp, loc, .. } => exec_prune(exp, loc, state),
        Instr::Call {
            ret: (ret_id, _),
            fun_exp,
            args,
            loc,
            ..
        } => exec_call(ret_id, fun_exp, args, loc, state),
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
        operations::eval_deref(rhs_exp, loc, &mut state)
    } else {
        operations::eval(rhs_exp, loc, &mut state)
    };

    match result {
        PulseResult::Ok(value) => {
            operations::write_id(id, value, &mut state);
            // Mark integer-typed loads for integer reasoning.
            // Cross-ref: OCaml Pulse.ml and_is_int_if_integer_type.
            if typ.is_int() {
                state.path_condition.and_is_int(value);
            }
            vec![ExecutionDomain::ContinueProgram(state)]
        }
        PulseResult::Recoverable(value, errors) => {
            operations::write_id(id, value, &mut state);
            let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
            for diag in errors {
                results.push(ExecutionDomain::AbortProgram {
                    state: Box::new(state.clone()),
                    diagnostic: Box::new(diag),
                });
            }
            results
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
    lhs_exp: &Exp,
    rhs_exp: &Exp,
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let rhs_val = match operations::eval(rhs_exp, loc, &mut state) {
        PulseResult::Ok(v) => v,
        PulseResult::FatalError(d, _) => {
            return vec![ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(d),
            }];
        }
        PulseResult::Recoverable(v, _) => v,
    };

    let (lhs_addr, lhs_errors) = match operations::eval(lhs_exp, loc, &mut state) {
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
        let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
        for diag in lhs_errors {
            results.push(ExecutionDomain::AbortProgram {
                state: Box::new(state.clone()),
                diagnostic: Box::new(diag),
            });
        }
        return results;
    }

    match operations::write_deref(lhs_addr, rhs_val, loc, &mut state) {
        PulseResult::Ok(()) => vec![ExecutionDomain::ContinueProgram(state)],
        PulseResult::FatalError(d, _) => vec![ExecutionDomain::AbortProgram {
            state: Box::new(state),
            diagnostic: Box::new(d),
        }],
        PulseResult::Recoverable((), errors) => {
            let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
            for diag in errors {
                results.push(ExecutionDomain::AbortProgram {
                    state: Box::new(state.clone()),
                    diagnostic: Box::new(diag),
                });
            }
            results
        }
    }
}

/// Prune: add a path condition constraint.
///
/// Extracts the boolean meaning of `exp` and adds constraints to the formula.
/// For `if (p != NULL)`, the true branch gets `p ≠ 0` and the false branch gets `p = 0`.
/// If the constraint is unsatisfiable (e.g., pruning `2 < 3` on the false branch),
/// the path is killed (no ContinueProgram returned).
fn exec_prune(exp: &Exp, loc: &Location, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    if prune_expr(exp, loc, &mut state) {
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
fn prune_expr(exp: &Exp, loc: &Location, state: &mut AbductiveDomain) -> bool {
    prune_binop(exp, loc, state, false)
}

/// Core prune handler for all binary/unary operators.
/// When `negated` is true, we're in the negated context (prune(!e)).
///
/// Cross-ref: OCaml's Pulse.ml `prune_binop` handles all comparison operators.
fn prune_binop(exp: &Exp, loc: &Location, state: &mut AbductiveDomain, negated: bool) -> bool {
    use sil::binop::Binop;
    match exp {
        // Equality and disequality
        Exp::BinOp(Binop::Eq, lhs, rhs) => {
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            state.prune_eq(lhs_val, rhs_val, negated).is_sat()
        }
        Exp::BinOp(Binop::Ne, lhs, rhs) => {
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            state.prune_eq(lhs_val, rhs_val, !negated).is_sat()
        }
        // Comparison operators: add atoms directly
        Exp::BinOp(Binop::Lt, lhs, rhs) => {
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            let op_lhs = crate::formula::Operand::AbstractValue(lhs_val);
            let op_rhs = crate::formula::Operand::AbstractValue(rhs_val);
            if negated {
                // !(x < y) → y ≤ x
                state
                    .path_condition
                    .and_less_equal(&op_rhs, &op_lhs)
                    .is_sat()
            } else {
                state
                    .path_condition
                    .and_less_than(&op_lhs, &op_rhs)
                    .is_sat()
            }
        }
        Exp::BinOp(Binop::Le, lhs, rhs) => {
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            let op_lhs = crate::formula::Operand::AbstractValue(lhs_val);
            let op_rhs = crate::formula::Operand::AbstractValue(rhs_val);
            if negated {
                // !(x ≤ y) → y < x
                state
                    .path_condition
                    .and_less_than(&op_rhs, &op_lhs)
                    .is_sat()
            } else {
                state
                    .path_condition
                    .and_less_equal(&op_lhs, &op_rhs)
                    .is_sat()
            }
        }
        Exp::BinOp(Binop::Gt, lhs, rhs) => {
            // x > y ↔ y < x
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            let op_lhs = crate::formula::Operand::AbstractValue(lhs_val);
            let op_rhs = crate::formula::Operand::AbstractValue(rhs_val);
            if negated {
                // !(x > y) → x ≤ y
                state
                    .path_condition
                    .and_less_equal(&op_lhs, &op_rhs)
                    .is_sat()
            } else {
                state
                    .path_condition
                    .and_less_than(&op_rhs, &op_lhs)
                    .is_sat()
            }
        }
        Exp::BinOp(Binop::Ge, lhs, rhs) => {
            // x ≥ y ↔ y ≤ x
            let lhs_val = operations::eval_or_fresh_for_prune(lhs, loc, state);
            let rhs_val = operations::eval_or_fresh_for_prune(rhs, loc, state);
            let op_lhs = crate::formula::Operand::AbstractValue(lhs_val);
            let op_rhs = crate::formula::Operand::AbstractValue(rhs_val);
            if negated {
                // !(x ≥ y) → y < x  → wait, !(x ≥ y) = y > x = x < y
                state
                    .path_condition
                    .and_less_than(&op_lhs, &op_rhs)
                    .is_sat()
            } else {
                state
                    .path_condition
                    .and_less_equal(&op_rhs, &op_lhs)
                    .is_sat()
            }
        }
        // Logical negation
        Exp::UnOp(sil::unop::Unop::LNot, inner, _) => prune_binop(inner, loc, state, !negated),
        // Default: variable/expression — truthy (≠ 0) or falsy (= 0)
        _ => {
            let val = operations::eval_or_fresh_for_prune(exp, loc, state);
            state.prune_eq_const(val, 0, !negated).is_sat()
        }
    }
}

/// Call: `ret_id = fun_exp(args)`
///
/// Dispatches to built-in models (malloc/free/etc.) via `models::dispatch`.
/// Unknown functions get a fresh return value.
fn exec_call(
    ret_id: &sil::ident::Ident,
    fun_exp: &Exp,
    args: &[(Exp, sil::typ::Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Try to dispatch to a built-in model
    if let Exp::Const(sil::const_val::Const::Cfun(callee)) = fun_exp {
        if let Some(results) = crate::models::dispatch(callee, ret_id, args, loc, state.clone()) {
            return results;
        }
    }

    // Default: treat as unknown — havoc the return value and pointer args.
    log::debug!("  [call] unknown: {fun_exp}");
    let ret_val = AbstractValue::mk_fresh();
    operations::write_id(ret_id, ret_val, &mut state);

    // Havoc pointer arguments for C/C++ unknown calls: unknown functions
    // may modify memory reachable from their arguments. For each actual,
    // evaluate it and replace all reachable heap edge targets with fresh
    // values. Only applies to C — Hack/Java/Python use ShouldOnlyHavocResources.
    // Cross-ref: OCaml PulseCallOperations.ml unknown_call + havoc_actual_if_ptr.
    let should_havoc = matches!(fun_exp, Exp::Const(sil::const_val::Const::Cfun(p)) if p.is_c());
    if should_havoc {
        for (arg_exp, _arg_typ) in args {
            let arg_val = operations::eval_or_fresh(arg_exp, loc, &mut state);
            state.apply_unknown_effect(arg_val);
        }
    }

    vec![ExecutionDomain::ContinueProgram(state)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use crate::diagnostic::Diagnostic;
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
            Location::dummy(),
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
            Location::dummy(),
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
    fn test_prune_eq_zero_constrains() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), p);

        // prune(p == 0) → p is known zero
        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Eq,
                Box::new(Exp::Lvar(pvar)),
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
    fn test_prune_ne_zero_constrains() {
        let mut state = mk_state();
        let p = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), p);

        // prune(p != 0) → p is NOT known zero
        let instr = Instr::Prune {
            exp: Exp::BinOp(
                sil::binop::Binop::Ne,
                Box::new(Exp::Lvar(pvar)),
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
