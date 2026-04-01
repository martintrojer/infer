// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse operations: eval, read, write, invalidate, allocate.
//!
//! Mirrors OCaml's `PulseOperations.ml` (simplified).
//!
//! These are the building blocks that the transfer functions use to
//! manipulate the abstract state.

use sil::const_val::Const;
use sil::exp::Exp;
use sil::int_lit::IntLit;
use sil::location::Location;
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Allocator;
use crate::diagnostic::Diagnostic;
use crate::formula::Operand;
use crate::invalidation::Invalidation;
use crate::pulse_result::PulseResult;
use crate::value_history::{HistoryEvent, ValueHistory, ValueWithHistory};

fn materialize_known_zero_invalid(
    addr: AbstractValue,
    history: &ValueHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) {
    // Cross-ref: OCaml eventually materializes a manifest abort summary for
    // paths such as `if (p) { ... } *p = 42` on the `p == 0` branch. Rust was
    // only catching the forward direction (must-be-valid, then later deduced
    // equal to 0). When the value was already known zero before the access, we
    // need to record the null invalidation at the access point as well.
    if state.check_valid(addr).is_ok() && state.is_known_zero(addr) {
        state.invalidate(
            addr,
            Invalidation::ConstantDereference(IntLit::zero()),
            history.append_event(HistoryEvent::Invalidated {
                invalidation: Invalidation::ConstantDereference(IntLit::zero()),
                location: loc.clone(),
            }),
        );
    }
}

/// Evaluate a SIL expression to an abstract value.
///
/// Mirrors OCaml's `PulseOperations.eval` (simplified).
pub fn eval(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    match eval_with_history(exp, loc, state) {
        PulseResult::Ok(value) => PulseResult::Ok(value.addr),
        PulseResult::Recoverable(value, errors) => PulseResult::Recoverable(value.addr, errors),
        PulseResult::FatalError(diag, errors) => PulseResult::FatalError(diag, errors),
    }
}

/// Evaluate a SIL expression to an abstract value and provenance.
pub fn eval_with_history(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<ValueWithHistory, Diagnostic> {
    match exp {
        Exp::Var(id) => {
            let var = Var::LogicalVar(id.clone());
            PulseResult::Ok(state.eval_var_with_history(&var))
        }
        Exp::Lvar(pvar) => {
            let var = Var::ProgramVar(Box::new(pvar.clone()));
            PulseResult::Ok(state.eval_var_with_history(&var))
        }
        Exp::Lfield(data, field, _typ) => {
            let base = match eval_with_history(&data.exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity of base before field access (null.field is a null deref)
            state.mark_must_be_valid(base.addr);
            materialize_known_zero_invalid(base.addr, &base.history, loc, state);
            if let Err(inv_info) = state.check_valid(base.addr) {
                let (invalidation, invalidation_history) = *inv_info;
                return PulseResult::Recoverable(
                    ValueWithHistory::new(
                        AbstractValue::mk_fresh(),
                        ValueHistory::assignment(loc.clone()),
                    ),
                    vec![Diagnostic::AccessToInvalidAddress {
                        addr: base.addr,
                        invalidation,
                        access_location: loc.clone(),
                        access_history: base.history.clone(),
                        invalidation_history,
                    }],
                );
            }
            let field_access = Access::FieldAccess(field.clone());
            PulseResult::Ok(state.read_heap_with_history(base, field_access))
        }
        Exp::Lindex(base_exp, index_exp) => {
            let base = match eval_with_history(base_exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity of base before array access (null[i] is a null deref)
            state.mark_must_be_valid(base.addr);
            materialize_known_zero_invalid(base.addr, &base.history, loc, state);
            if let Err(inv_info) = state.check_valid(base.addr) {
                let (invalidation, invalidation_history) = *inv_info;
                return PulseResult::Recoverable(
                    ValueWithHistory::new(
                        AbstractValue::mk_fresh(),
                        ValueHistory::assignment(loc.clone()),
                    ),
                    vec![Diagnostic::AccessToInvalidAddress {
                        addr: base.addr,
                        invalidation,
                        access_location: loc.clone(),
                        access_history: base.history.clone(),
                        invalidation_history,
                    }],
                );
            }
            let index = match eval_with_history(index_exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Canonicalize the index: if it's a known constant, use a
            // deterministic abstract value so that store &a[0] and load &a[0]
            // see the same heap edge.
            let canon_index = state.canonicalize_for_access(index.addr);
            let array_access = Access::ArrayAccess(sil::typ::Typ::void(), canon_index);
            PulseResult::Ok(state.read_heap_with_history(base, array_access))
        }
        Exp::Const(c) => eval_const(c, loc, state),
        Exp::UnOp(op, inner, _typ) => {
            let inner_val = match eval_with_history(inner, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let result = AbstractValue::mk_fresh();
            match op {
                sil::unop::Unop::LNot => {
                    // !x: if x is a known constant, fold to 0 or 1
                    if let Some(c) = state.get_const(inner_val.addr) {
                        let negated = if c == 0 { 1 } else { 0 };
                        let _ = state.and_equal_const(result, negated);
                    }
                }
                sil::unop::Unop::Neg => {
                    // Cross-ref: OCaml `PulseArithmetic.eval_unop` keeps
                    // unary minus connected to its operand in the formula.
                    // If we drop that symbolic relation here, interproc
                    // conditions imported through `-x` become disconnected
                    // from caller-visible inputs and latent arithmetic bugs
                    // look manifest.
                    if let Some(c) = state.get_const(inner_val.addr) {
                        let _ = state.and_equal_const(result, -c);
                    } else {
                        let _ = state.and_equal_linear(
                            result,
                            crate::formula::lin_arith::LinArith::of_var(inner_val.addr).neg(),
                        );
                    }
                }
                sil::unop::Unop::BNot => {
                    // ~x: if x is a known constant, fold to ~x
                    if let Some(c) = state.get_const(inner_val.addr) {
                        let _ = state.and_equal_const(result, !c);
                    }
                }
            }
            PulseResult::Ok(ValueWithHistory::new(result, inner_val.history))
        }
        Exp::BinOp(bop, lhs, rhs) => {
            let lhs_val = match eval_with_history(lhs, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let rhs_val = match eval_with_history(rhs, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let result = AbstractValue::mk_fresh();
            // Record the arithmetic relationship
            let _ = state.and_equal_binop(
                result,
                bop.clone(),
                &Operand::AbstractValue(lhs_val.addr),
                &Operand::AbstractValue(rhs_val.addr),
            );
            PulseResult::Ok(ValueWithHistory::new(
                result,
                lhs_val.history.merge(&rhs_val.history),
            ))
        }
        Exp::Cast(_, inner) => eval_with_history(inner, loc, state),
        Exp::Exn(inner) => eval_with_history(inner, loc, state),
        Exp::Sizeof(data) => {
            // Try nbytes first (set by some frontends), then compute from type.
            let size = data
                .nbytes
                .map(|n| n as i64)
                .or_else(|| data.typ.size_in_bytes());
            if let Some(n) = size {
                let v = AbstractValue::mk_fresh();
                state.and_equal_const(v, n);
                PulseResult::Ok(ValueWithHistory::new(
                    v,
                    ValueHistory::assignment(loc.clone()),
                ))
            } else {
                PulseResult::Ok(ValueWithHistory::new(
                    AbstractValue::mk_fresh(),
                    ValueHistory::assignment(loc.clone()),
                ))
            }
        }
        Exp::Closure(_) => PulseResult::Ok(ValueWithHistory::new(
            AbstractValue::mk_fresh(),
            ValueHistory::assignment(loc.clone()),
        )),
    }
}

/// Evaluate a constant to an abstract value and provenance.
fn eval_const(
    c: &Const,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<ValueWithHistory, Diagnostic> {
    match c {
        Const::Cint(i) => {
            let v = AbstractValue::mk_fresh();
            // Record the constant value in the formula
            if let Some(n) = i.to_i64() {
                state.and_equal_const(v, n);
            }
            // Mark as invalid only for values that look like pointers.
            // Cross-ref OCaml: PulseOperations.ml eval_const marks Cint
            // with ConstantDereference. In practice, only 0 (null) and
            // small constants cause real null derefs. Non-zero constants
            // used as integer values (e.g., set_ptr(buf, 1)) shouldn't
            // be marked invalid — they're not pointer dereferences.
            if i.is_zero() {
                let inv = Invalidation::ConstantDereference(i.clone());
                state.invalidate(
                    v,
                    inv.clone(),
                    ValueHistory::invalidated(inv.clone(), loc.clone()),
                );
                PulseResult::Ok(ValueWithHistory::new(
                    v,
                    ValueHistory::invalidated(inv, loc.clone()),
                ))
            } else {
                PulseResult::Ok(ValueWithHistory::new(
                    v,
                    ValueHistory::assignment(loc.clone()),
                ))
            }
        }
        Const::Cfun(pname) => {
            // Record the procedure name as a Closure attribute so that
            // __call_c_function_ptr can resolve it later.
            // Cross-ref: OCaml PulseOperations.ml eval_const records a
            // closure via record_closure for Cfun constants.
            let v = AbstractValue::mk_fresh();
            log::trace!("  [eval_const] Cfun({pname}) → {v} with Closure attr");
            state
                .post
                .attrs
                .add_one(v, crate::attribute::Attribute::Closure(pname.clone()));
            PulseResult::Ok(ValueWithHistory::new(
                v,
                ValueHistory::assignment(loc.clone()),
            ))
        }
        Const::Cstr(_) => PulseResult::Ok(ValueWithHistory::new(
            AbstractValue::mk_fresh(),
            ValueHistory::assignment(loc.clone()),
        )),
        Const::Cfloat(f) => {
            let v = AbstractValue::mk_fresh();
            // Convert float to rational for the linear solver.
            // E.g., 5.5 → 11/2, enabling 2x=5.5 → x=2.75 (non-integer).
            if let Some(q) = crate::formula::lin_arith::Q::approximate_float(f.0) {
                let lin = crate::formula::lin_arith::LinArith::of_q(q);
                let _ = state.and_equal_linear(v, lin);
            }
            PulseResult::Ok(ValueWithHistory::new(
                v,
                ValueHistory::assignment(loc.clone()),
            ))
        }
        Const::Cclass(_) => PulseResult::Ok(ValueWithHistory::new(
            AbstractValue::mk_fresh(),
            ValueHistory::assignment(loc.clone()),
        )),
    }
}

/// Unknown calls on `&slot` can overwrite the slot's value itself.
/// If the slot had not been read before the call, create a fresh post-state
/// dereference edge so later loads observe the unknown write without adding a
/// spurious pre-condition on the incoming value.
pub fn refresh_unknown_lvalue_root(exp: &Exp, addr: AbstractValue, state: &mut AbductiveDomain) {
    if matches!(exp, Exp::Lvar(_) | Exp::Lfield(..) | Exp::Lindex(..)) {
        state.ensure_deref_edge_if_missing(addr);
    }
}

/// Evaluate a dereference: `*exp`.
///
/// Evaluates the expression, then checks validity and follows the
/// Dereference edge.
pub fn eval_deref(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    match eval_deref_with_history(exp, loc, state) {
        PulseResult::Ok(value) => PulseResult::Ok(value.addr),
        PulseResult::Recoverable(value, errors) => PulseResult::Recoverable(value.addr, errors),
        PulseResult::FatalError(diag, errors) => PulseResult::FatalError(diag, errors),
    }
}

/// Evaluate a dereference and preserve the target provenance.
pub fn eval_deref_with_history(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<ValueWithHistory, Diagnostic> {
    let addr = match eval_with_history(exp, loc, state) {
        PulseResult::Ok(v) => v,
        other => return other,
    };
    eval_deref_addr_with_history(addr, loc, state)
}

/// Dereference an abstract address: check validity then follow the edge.
pub fn eval_deref_addr(
    addr: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    match eval_deref_addr_with_history(
        ValueWithHistory::new(addr, ValueHistory::epoch()),
        loc,
        state,
    ) {
        PulseResult::Ok(value) => PulseResult::Ok(value.addr),
        PulseResult::Recoverable(value, errors) => PulseResult::Recoverable(value.addr, errors),
        PulseResult::FatalError(diag, errors) => PulseResult::FatalError(diag, errors),
    }
}

/// Dereference an abstract address and preserve the target provenance.
pub fn eval_deref_addr_with_history(
    addr: ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<ValueWithHistory, Diagnostic> {
    // Record that this address must be valid (for interproc pre-condition checks).
    // Cross-ref: OCaml PulseOperations.ml check_addr_access sets MustBeValid.
    state.mark_must_be_valid(addr.addr);
    materialize_known_zero_invalid(addr.addr, &addr.history, loc, state);

    // THE null-deref / use-after-free check
    if let Err(inv_info) = state.check_valid(addr.addr) {
        let (invalidation, invalidation_history) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr: addr.addr,
            invalidation,
            access_location: loc.clone(),
            access_history: addr.history.clone(),
            invalidation_history,
        });
    }

    let target = state.read_heap_with_history(addr, Access::Dereference);
    PulseResult::Ok(target)
}

/// Check if accessing an address is valid.
pub fn check_addr_access(
    addr: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    check_addr_access_with_history(
        ValueWithHistory::new(addr, ValueHistory::epoch()),
        loc,
        state,
    )
}

/// Check if accessing an address is valid, using its current provenance.
pub fn check_addr_access_with_history(
    addr: ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    state.mark_must_be_valid(addr.addr);
    materialize_known_zero_invalid(addr.addr, &addr.history, loc, state);
    if let Err(inv_info) = state.check_valid(addr.addr) {
        let (invalidation, invalidation_history) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr: addr.addr,
            invalidation,
            access_location: loc.clone(),
            access_history: addr.history,
            invalidation_history,
        });
    }
    PulseResult::Ok(())
}

/// Write through a pointer: `*ref = obj`.
pub fn write_deref(
    ref_addr: AbstractValue,
    obj: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    write_deref_with_history(
        ValueWithHistory::new(ref_addr, ValueHistory::epoch()),
        ValueWithHistory::new(obj, ValueHistory::epoch()),
        loc,
        state,
    )
}

/// Write through a pointer, preserving the pointee provenance.
pub fn write_deref_with_history(
    ref_addr: ValueWithHistory,
    obj: ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    let record_write = |addr: AbstractValue, state: &mut AbductiveDomain| {
        let repr = state.path_condition.get_var_repr(addr);
        // Cross-ref: OCaml `PulseAbductiveDomain.set_post_cell` records a
        // `WrittenTo` attribute on each written address. Rust does not thread
        // PathContext timestamps yet, so keep a stable placeholder timestamp;
        // current summary classification only relies on the presence of the
        // write marker, not on its ordering.
        state.post.attrs.mark_written_to(repr, 0, loc.clone());
    };

    match check_addr_access_with_history(ref_addr.clone(), loc, state) {
        PulseResult::Ok(()) => {}
        PulseResult::FatalError(e, errs) => return PulseResult::FatalError(e, errs),
        PulseResult::Recoverable((), errs) => {
            state.write_heap_with_history(
                ref_addr.addr,
                Access::Dereference,
                ValueWithHistory::new(obj.addr, obj.history.append_assignment(loc.clone())),
            );
            record_write(ref_addr.addr, state);
            return PulseResult::Recoverable((), errs);
        }
    }
    // Abduce: record that the callee accesses this address for writing.
    // The pre-edge records what WAS at this address before the write
    // (a fresh "before" value), while the post-edge records the new value.
    // This distinction is critical for interproc: materialize_pre maps
    // the "before" value to the caller's existing value, and Step 2
    // overwrites it with the callee's new value.
    // Cross-ref: OCaml PulseOperations.ml write_deref calls write_access
    // which does biabduction (abduce on pre with old value).
    let pre_target = state.read_heap_with_history(ref_addr.clone(), Access::Dereference);
    // read_heap already added the pre-edge with the "before" value.
    // The pre_target is whatever was there (or a fresh value if nothing).
    let _ = pre_target; // used by read_heap to create pre-edge
    state.write_heap_with_history(
        ref_addr.addr,
        Access::Dereference,
        ValueWithHistory::new(obj.addr, obj.history.append_assignment(loc.clone())),
    );
    record_write(ref_addr.addr, state);
    PulseResult::Ok(())
}

/// Write the result of a Load into an identifier's stack slot.
pub fn write_id(id: &sil::ident::Ident, value: AbstractValue, state: &mut AbductiveDomain) {
    write_id_with_history(
        id,
        ValueWithHistory::new(value, ValueHistory::epoch()),
        state,
    );
}

/// Write the result of a Load into an identifier's stack slot with provenance.
pub fn write_id_with_history(
    id: &sil::ident::Ident,
    value: ValueWithHistory,
    state: &mut AbductiveDomain,
) {
    let var = Var::LogicalVar(id.clone());
    state.post.stack.add_with_history(var, value);
}

/// Mark an address as invalidated (freed, null, etc.).
pub fn invalidate(
    addr: AbstractValue,
    inv: Invalidation,
    loc: Location,
    state: &mut AbductiveDomain,
) {
    state.invalidate(addr, inv.clone(), ValueHistory::invalidated(inv, loc));
}

/// Evaluate a constant without marking it as Invalid.
///
/// Used by prune/comparison contexts where Const(0) is a comparison
/// operand, not a pointer dereference target. Without this, every
/// `prune ne(ptr, 0)` creates a fresh Invalid value for 0, which
/// interferes with formula unification during prune.
///
/// Cross-ref: OCaml also marks Cint as Invalid in eval_const, but
/// OCaml's prune works on already-evaluated values (not re-evaluating).
fn eval_const_no_invalidate(
    c: &Const,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    match c {
        Const::Cint(i) => {
            let v = AbstractValue::mk_fresh();
            if let Some(n) = i.to_i64() {
                state.and_equal_const(v, n);
            }
            // NO Invalid marking — this is for comparison contexts
            PulseResult::Ok(v)
        }
        // For non-integer constants, delegate to normal eval_const
        other => match eval_const(other, &Location::dummy(), state) {
            PulseResult::Ok(value) => PulseResult::Ok(value.addr),
            PulseResult::Recoverable(value, errors) => PulseResult::Recoverable(value.addr, errors),
            PulseResult::FatalError(diag, errors) => PulseResult::FatalError(diag, errors),
        },
    }
}

/// Evaluate an expression for comparison/prune contexts.
///
/// Like `eval` but doesn't mark Const(0) as Invalid. This prevents
/// false positives when constants appear as comparison operands in prune.
///
/// Cross-ref: OCaml's `PulseOperations.prune_binop` evaluates operands
/// via `eval path Read`, which creates Invalid on Cint. OCaml avoids
/// FPs because its prune works on pre-evaluated abstract values from
/// previous instructions, not re-evaluating expressions. Our prune
/// re-evaluates inline, so we need this non-invalidating variant.
pub fn eval_for_prune(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    match exp {
        Exp::Const(c) => eval_const_no_invalidate(c, state),
        Exp::Cast(_, inner) => eval_for_prune(inner, loc, state),
        _ => eval(exp, loc, state),
    }
}

/// Like eval_or_fresh but for prune contexts (no Invalid on constants).
pub fn eval_or_fresh_for_prune(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> AbstractValue {
    match eval_for_prune(exp, loc, state) {
        PulseResult::Ok(v) | PulseResult::Recoverable(v, _) => v,
        PulseResult::FatalError(_, _) => AbstractValue::mk_fresh(),
    }
}

/// Evaluate an expression, returning its abstract value and provenance.
/// On error (e.g. null deref during eval), returns a fresh value with a
/// best-effort assignment provenance at the current location.
pub fn eval_or_fresh_with_history(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> ValueWithHistory {
    match eval_with_history(exp, loc, state) {
        PulseResult::Ok(value) | PulseResult::Recoverable(value, _) => value,
        PulseResult::FatalError(_, _) => ValueWithHistory::new(
            AbstractValue::mk_fresh(),
            ValueHistory::assignment(loc.clone()),
        ),
    }
}

/// Evaluate an expression, returning its abstract value.
/// On error (e.g. null deref during eval), returns a fresh value.
pub fn eval_or_fresh(exp: &Exp, loc: &Location, state: &mut AbductiveDomain) -> AbstractValue {
    match eval(exp, loc, state) {
        PulseResult::Ok(v) | PulseResult::Recoverable(v, _) => v,
        PulseResult::FatalError(_, _) => AbstractValue::mk_fresh(),
    }
}

/// Mark an address as allocated.
pub fn allocate(
    addr: AbstractValue,
    allocator: Allocator,
    loc: Location,
    state: &mut AbductiveDomain,
) {
    state.allocate(addr, allocator, loc);
}

#[cfg(test)]
mod tests {
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::typ::Typ;

    use super::*;

    #[test]
    fn test_access_through_zero_records_null_invalidation() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let p = AbstractValue::mk_fresh();
        state.and_equal_const(p, 0);

        let result = check_addr_access(p, &loc, &mut state);
        assert!(matches!(
            result,
            PulseResult::FatalError(
                Diagnostic::AccessToInvalidAddress {
                    invalidation: Invalidation::ConstantDereference(_),
                    ..
                },
                _
            )
        ));
        assert!(
            state.check_valid(p).is_err(),
            "known-zero access should materialize a null invalidation for later reporting"
        );
    }
}
