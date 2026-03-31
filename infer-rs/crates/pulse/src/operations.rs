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

fn materialize_known_zero_invalid(
    addr: AbstractValue,
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
            loc.clone(),
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
    match exp {
        Exp::Var(id) => {
            let var = Var::LogicalVar(id.clone());
            let addr = state.eval_var(&var);
            PulseResult::Ok(addr)
        }
        Exp::Lvar(pvar) => {
            let var = Var::ProgramVar(Box::new(pvar.clone()));
            let addr = state.eval_var(&var);
            PulseResult::Ok(addr)
        }
        Exp::Lfield(data, field, _typ) => {
            let base = match eval(&data.exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity of base before field access (null.field is a null deref)
            state.mark_must_be_valid(base);
            materialize_known_zero_invalid(base, loc, state);
            if let Err(inv_info) = state.check_valid(base) {
                let (invalidation, inv_loc) = *inv_info;
                return PulseResult::Recoverable(
                    AbstractValue::mk_fresh(),
                    vec![Diagnostic::AccessToInvalidAddress {
                        addr: base,
                        invalidation,
                        access_location: loc.clone(),
                        invalidation_location: inv_loc,
                    }],
                );
            }
            let field_access = Access::FieldAccess(field.clone());
            let target = state.read_heap(base, field_access);
            PulseResult::Ok(target)
        }
        Exp::Lindex(base_exp, index_exp) => {
            let base = match eval(base_exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity of base before array access (null[i] is a null deref)
            state.mark_must_be_valid(base);
            materialize_known_zero_invalid(base, loc, state);
            if let Err(inv_info) = state.check_valid(base) {
                let (invalidation, inv_loc) = *inv_info;
                return PulseResult::Recoverable(
                    AbstractValue::mk_fresh(),
                    vec![Diagnostic::AccessToInvalidAddress {
                        addr: base,
                        invalidation,
                        access_location: loc.clone(),
                        invalidation_location: inv_loc,
                    }],
                );
            }
            let index = match eval(index_exp, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Canonicalize the index: if it's a known constant, use a
            // deterministic abstract value so that store &a[0] and load &a[0]
            // see the same heap edge.
            let canon_index = state.canonicalize_for_access(index);
            let array_access = Access::ArrayAccess(sil::typ::Typ::void(), canon_index);
            let target = state.read_heap(base, array_access);
            PulseResult::Ok(target)
        }
        Exp::Const(c) => eval_const(c, loc, state),
        Exp::UnOp(op, inner, _typ) => {
            let inner_val = match eval(inner, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let result = AbstractValue::mk_fresh();
            match op {
                sil::unop::Unop::LNot => {
                    // !x: if x is a known constant, fold to 0 or 1
                    if let Some(c) = state.get_const(inner_val) {
                        let negated = if c == 0 { 1 } else { 0 };
                        let _ = state.and_equal_const(result, negated);
                    }
                }
                sil::unop::Unop::Neg => {
                    // -x: if x is a known constant, fold to -x
                    if let Some(c) = state.get_const(inner_val) {
                        let _ = state.and_equal_const(result, -c);
                    }
                }
                sil::unop::Unop::BNot => {
                    // ~x: if x is a known constant, fold to ~x
                    if let Some(c) = state.get_const(inner_val) {
                        let _ = state.and_equal_const(result, !c);
                    }
                }
            }
            PulseResult::Ok(result)
        }
        Exp::BinOp(bop, lhs, rhs) => {
            let lhs_val = match eval(lhs, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let rhs_val = match eval(rhs, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            let result = AbstractValue::mk_fresh();
            // Record the arithmetic relationship
            let _ = state.and_equal_binop(
                result,
                bop.clone(),
                &Operand::AbstractValue(lhs_val),
                &Operand::AbstractValue(rhs_val),
            );
            PulseResult::Ok(result)
        }
        Exp::Cast(_, inner) => eval(inner, loc, state),
        Exp::Exn(inner) => eval(inner, loc, state),
        Exp::Sizeof(data) => {
            // Try nbytes first (set by some frontends), then compute from type.
            let size = data
                .nbytes
                .map(|n| n as i64)
                .or_else(|| data.typ.size_in_bytes());
            if let Some(n) = size {
                let v = AbstractValue::mk_fresh();
                state.and_equal_const(v, n);
                PulseResult::Ok(v)
            } else {
                PulseResult::Ok(AbstractValue::mk_fresh())
            }
        }
        Exp::Closure(_) => PulseResult::Ok(AbstractValue::mk_fresh()),
    }
}

/// Evaluate a constant to an abstract value.
fn eval_const(
    c: &Const,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
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
                state.invalidate(v, inv, loc.clone());
            }
            PulseResult::Ok(v)
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
            PulseResult::Ok(v)
        }
        Const::Cstr(_) => PulseResult::Ok(AbstractValue::mk_fresh()),
        Const::Cfloat(f) => {
            let v = AbstractValue::mk_fresh();
            // Convert float to rational for the linear solver.
            // E.g., 5.5 → 11/2, enabling 2x=5.5 → x=2.75 (non-integer).
            if let Some(q) = crate::formula::lin_arith::Q::approximate_float(f.0) {
                let lin = crate::formula::lin_arith::LinArith::of_q(q);
                let _ = state.and_equal_linear(v, lin);
            }
            PulseResult::Ok(v)
        }
        Const::Cclass(_) => PulseResult::Ok(AbstractValue::mk_fresh()),
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
    let addr = match eval(exp, loc, state) {
        PulseResult::Ok(v) => v,
        other => return other,
    };
    eval_deref_addr(addr, loc, state)
}

/// Dereference an abstract address: check validity then follow the edge.
pub fn eval_deref_addr(
    addr: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<AbstractValue, Diagnostic> {
    // Record that this address must be valid (for interproc pre-condition checks).
    // Cross-ref: OCaml PulseOperations.ml check_addr_access sets MustBeValid.
    state.mark_must_be_valid(addr);
    materialize_known_zero_invalid(addr, loc, state);

    // THE null-deref / use-after-free check
    if let Err(inv_info) = state.check_valid(addr) {
        let (invalidation, inv_loc) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location: loc.clone(),
            invalidation_location: inv_loc,
        });
    }

    let target = state.read_heap(addr, Access::Dereference);
    PulseResult::Ok(target)
}

/// Check if accessing an address is valid.
pub fn check_addr_access(
    addr: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    state.mark_must_be_valid(addr);
    materialize_known_zero_invalid(addr, loc, state);
    if let Err(inv_info) = state.check_valid(addr) {
        let (invalidation, inv_loc) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location: loc.clone(),
            invalidation_location: inv_loc,
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
    match check_addr_access(ref_addr, loc, state) {
        PulseResult::Ok(()) => {}
        PulseResult::FatalError(e, errs) => return PulseResult::FatalError(e, errs),
        PulseResult::Recoverable((), errs) => {
            state.write_heap(ref_addr, Access::Dereference, obj);
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
    let pre_target = state.read_heap(ref_addr, Access::Dereference);
    // read_heap already added the pre-edge with the "before" value.
    // The pre_target is whatever was there (or a fresh value if nothing).
    let _ = pre_target; // used by read_heap to create pre-edge
    state.write_heap(ref_addr, Access::Dereference, obj);
    PulseResult::Ok(())
}

/// Write the result of a Load into an identifier's stack slot.
pub fn write_id(id: &sil::ident::Ident, value: AbstractValue, state: &mut AbductiveDomain) {
    let var = Var::LogicalVar(id.clone());
    state.post.stack.add(var, value);
}

/// Mark an address as invalidated (freed, null, etc.).
pub fn invalidate(
    addr: AbstractValue,
    inv: Invalidation,
    loc: Location,
    state: &mut AbductiveDomain,
) {
    state.invalidate(addr, inv, loc);
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
        other => eval_const(other, &Location::dummy(), state),
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
