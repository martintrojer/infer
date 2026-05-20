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
    // Cross-ref: OCaml does not synthesize a brand-new invalidation merely
    // because the arithmetic knows `p == 0`; that caller-reifiable case is
    // surfaced later via `PotentialInvalidAccess{,Summary}`. The access-time
    // upgrade here is only for paths that already carry the lighter
    // `ComparedToNullInThisProcedure` marker from an earlier prune.
    if !state.is_known_zero(addr) {
        return;
    }

    let repr = state.path_condition.get_var_repr(addr);
    let should_materialize = matches!(
        state
            .post
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_invalid()),
        Some((Invalidation::ComparedToNullInThisProcedure(_), _))
    );

    if should_materialize {
        state.replace_invalid(
            addr,
            Invalidation::ConstantDereference(IntLit::zero()),
            history.append_event(HistoryEvent::Invalidated {
                invalidation: Invalidation::ConstantDereference(IntLit::zero()),
                location: loc.clone(),
            }),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessMode {
    /// Read access: abduce both `MustBeValid` and `MustBeInitialized`,
    /// then mark the address `Initialized` so repeated reads do not keep
    /// re-reporting it.
    Read,
    /// Write access: abduce `MustBeValid` only, then mark the address
    /// `Initialized` (the write itself initializes it).
    Write,
    /// Only abduce `MustBeValid` and check invalidation. No initialization
    /// side effect, no `MustBeInitialized` abduction. Mirrors OCaml's
    /// `PulseBasicInterface.AccessMode.NoAccess` used by `free`/`delete`,
    /// `memcpy` validity checks, and other models that need to assert
    /// pointer validity without claiming a read or write.
    NoAccess,
}

fn check_validity_and_record_access(
    addr: &ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
    mode: AccessMode,
) -> Result<(), Box<(Invalidation, ValueHistory)>> {
    // Cross-ref: OCaml `PulseOperations.check_addr_access` first abduces
    // `MustBeValid`, then checks invalidation, then applies the
    // mode-specific initialization side effect (Read => abduce
    // `MustBeInitialized` + initialize; Write => initialize; NoAccess =>
    // nothing).
    state.mark_must_be_valid_at(addr.addr, loc);
    materialize_known_zero_invalid(addr.addr, &addr.history, loc, state);
    let valid = state.check_valid(addr.addr);
    if valid.is_ok() {
        match mode {
            AccessMode::Read => {
                let _ = state.record_read_access_at(addr.addr, loc);
            }
            AccessMode::Write => state.record_write_access_at(addr.addr),
            AccessMode::NoAccess => {}
        }
    }
    valid
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

/// Evaluate a SIL expression to an abstract value and provenance, using
/// `Read` mode for the outermost Lfield/Lindex base check.
pub fn eval_with_history(
    exp: &Exp,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<ValueWithHistory, Diagnostic> {
    eval_with_history_mode(exp, AccessMode::Read, loc, state)
}

/// Evaluate a SIL expression to an abstract value and provenance, threading
/// an outer access `mode` into the immediate Lfield/Lindex base check.
///
/// Cross-ref: OCaml `PulseOperations.eval_to_value_origin` takes a `mode`
/// argument that decides whether the access on the *outermost* l-value
/// component abduces `MustBeInitialized` (Read), only initializes (Write),
/// or only abduces `MustBeValid` (NoAccess). Inner recursive Lfield/Lindex
/// bases always use `Read` because the inner pointer must be loaded.
pub(crate) fn eval_with_history_mode(
    exp: &Exp,
    mode: AccessMode,
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
            // Cross-ref: OCaml `PulseOperations.eval_to_value_origin` always
            // recurses on the inner Lfield/Lindex base with `Read` mode and
            // only threads the *outer* access mode into the FieldAccess
            // check on the immediate base. So the base check here picks up
            // the caller-supplied `mode` (defaulting to Read), not always
            // Read. This avoids spuriously abducing `MustBeInitialized` on a
            // formal whose only outermost use is a Store/free/etc.
            let base = match eval_with_history_mode(&data.exp, AccessMode::Read, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity and initialization of the base before field
            // access (`null.field` is a null deref, reading the field also
            // requires the base cell to be initialized for `Read` mode).
            if let Err(inv_info) = check_validity_and_record_access(&base, loc, state, mode) {
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
                        trace_access_location: None,
                        access_history: base.history.clone(),
                        invalidation_history,
                    }],
                );
            }
            let field_access = Access::FieldAccess(field.clone());
            PulseResult::Ok(state.read_heap_with_history(base, field_access))
        }
        Exp::Lindex(base_exp, index_exp) => {
            // Cross-ref: same Lfield reasoning — recurse on the inner base
            // with `Read`, but check the immediate base with the outer mode.
            let base = match eval_with_history_mode(base_exp, AccessMode::Read, loc, state) {
                PulseResult::Ok(v) => v,
                other => return other,
            };
            // Check validity and initialization of the base before array
            // access (`null[i]` is a null deref, reading the slot requires the
            // base cell to be initialized for `Read` mode).
            if let Err(inv_info) = check_validity_and_record_access(&base, loc, state, mode) {
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
                        trace_access_location: None,
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
            // Mirror the BinOp constant-canonicalization win: when the
            // unop result resolves to a known constant via the formula,
            // route through `const_cache` to reuse the existing
            // representative instead of returning the fresh `result`
            // that was just minted.
            let canonical = state.canonicalize_for_access(result);
            PulseResult::Ok(ValueWithHistory::new(canonical, inner_val.history))
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
            // Cross-ref: OCaml `PulseArithmetic.eval_binop` substitutes the
            // freshly minted `binop_addr` through the formula's new equations
            // (`incorporate_new_eqs_on_val`). The end result is that pure
            // constant arithmetic like `__sil_plusa_int(__sil_mult_int(3, 8),
            // 1)` collapses through `absval_of_int` / `const_cache` to one
            // shared value per constant instead of minting a fresh value for
            // every distinct address-arithmetic site. On encryption-style
            // byte loops with hundreds of constant array-index computations
            // per iteration, this dominates per-disjunct unique-value count.
            let canonical = state.canonicalize_for_access(result);
            let history = if lhs_val.history.is_epoch() {
                rhs_val.history
            } else if rhs_val.history.is_epoch() {
                lhs_val.history
            } else {
                lhs_val.history.merge_owned(&rhs_val.history)
            };
            PulseResult::Ok(ValueWithHistory::new(canonical, history))
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
                let v = state.absval_of_int(n);
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
            let v = i
                .to_i64()
                .map(|n| state.absval_of_int(n))
                .unwrap_or_else(AbstractValue::mk_fresh);
            // Cross-ref: OCaml `PulseOperations.eval_const` invalidates every
            // integer literal under `ConstantDereference`, while prune uses a
            // separate non-invalidating path. Keeping normal eval aligned with
            // OCaml is important for summary parity on stored constants such as
            // `1` and `2`, not just `0`.
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
        }
        Const::Cfun(pname) => {
            // Record the procedure name as a Closure attribute so that
            // __call_c_function_ptr can resolve direct Cfun constants as a
            // fallback. Summary export rewrites global/return function-pointer
            // surfaces to OCaml's dynamic-type + `0 < addr` encoding instead
            // of relying on exported Closure attrs.
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
    // Record the access preconditions and check the address before following
    // the dereference edge.
    if let Err(inv_info) = check_validity_and_record_access(&addr, loc, state, AccessMode::Read) {
        let (invalidation, invalidation_history) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr: addr.addr,
            invalidation,
            access_location: loc.clone(),
            trace_access_location: None,
            access_history: addr.history.clone(),
            invalidation_history,
        });
    }

    let target = state.read_heap_with_history(addr, Access::Dereference);
    PulseResult::Ok(target)
}

/// Check if accessing an address is valid (Read access).
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

/// Check if accessing an address is valid, using its current provenance
/// (Read access).
pub fn check_addr_access_with_history(
    addr: ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    if let Err(inv_info) = check_validity_and_record_access(&addr, loc, state, AccessMode::Read) {
        let (invalidation, invalidation_history) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr: addr.addr,
            invalidation,
            access_location: loc.clone(),
            trace_access_location: None,
            access_history: addr.history,
            invalidation_history,
        });
    }
    PulseResult::Ok(())
}

/// Check that an address is valid (`MustBeValid`) without claiming a read or
/// write. Mirrors OCaml's `check_addr_access path NoAccess`, used by C
/// models like `free`/`delete` and `memcpy`/`memmove` that must assert
/// pointer validity but do not actually load or store through the pointer
/// at the SIL level (the loads/stores happen inside the modelled call, on
/// addresses the model itself synthesizes).
pub fn check_addr_access_no_init(
    addr: AbstractValue,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    check_addr_access_no_init_with_history(
        ValueWithHistory::new(addr, ValueHistory::epoch()),
        loc,
        state,
    )
}

/// History-preserving variant of [`check_addr_access_no_init`].
pub fn check_addr_access_no_init_with_history(
    addr: ValueWithHistory,
    loc: &Location,
    state: &mut AbductiveDomain,
) -> PulseResult<(), Diagnostic> {
    if let Err(inv_info) = check_validity_and_record_access(&addr, loc, state, AccessMode::NoAccess)
    {
        let (invalidation, invalidation_history) = *inv_info;
        return PulseResult::fatal(Diagnostic::AccessToInvalidAddress {
            addr: addr.addr,
            invalidation,
            access_location: loc.clone(),
            trace_access_location: None,
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

    match check_validity_and_record_access(&ref_addr, loc, state, AccessMode::Write) {
        Ok(()) => {}
        Err(inv_info) => {
            let (invalidation, invalidation_history) = *inv_info;
            return PulseResult::FatalError(
                Diagnostic::AccessToInvalidAddress {
                    addr: ref_addr.addr,
                    invalidation,
                    access_location: loc.clone(),
                    trace_access_location: None,
                    access_history: ref_addr.history.clone(),
                    invalidation_history,
                },
                vec![],
            );
        }
    }
    // Cross-ref: OCaml `PulseOperations.write_deref` delegates to
    // `write_access`, which updates the post edge directly after the write
    // access check. It does not synthesize a pre-state dereference edge for
    // the "old" value being overwritten.
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
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::typ::Typ;
    use sil::var::Var;

    use super::*;
    use crate::attribute::Attribute;

    fn make_pdesc_with_formal(name: &str) -> (Procdesc, Pvar) {
        let pname = Procname::c_from_string("test");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(Mangled::from_string(name), Typ::void(), Default::default())];
        let pvar = Pvar::mk(Mangled::from_string(name), pname);
        (pdesc, pvar)
    }

    #[test]
    fn test_access_through_formula_zero_without_invalid_attr_stays_nonfatal() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let p = AbstractValue::mk_fresh();
        state.and_equal_const(p, 0);

        let result = check_addr_access(p, &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(())));
        assert!(
            state.check_valid(p).is_ok(),
            "formula-only zero should not fabricate an invalid attr at access time"
        );
        assert!(
            state
                .post
                .attrs
                .get(&p)
                .and_then(|attrs| attrs.get_invalid())
                .is_none(),
            "formula-only zero should stay latent until later summary/import handling"
        );
    }

    #[test]
    fn test_access_through_known_zero_upgrades_compared_to_null_to_null_deref() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let p = AbstractValue::mk_fresh();
        assert!(state.and_equal_const(p, 0).is_sat());
        state.invalidate(
            p,
            Invalidation::ComparedToNullInThisProcedure(loc.clone()),
            ValueHistory::invalidated(
                Invalidation::ComparedToNullInThisProcedure(loc.clone()),
                loc.clone(),
            ),
        );

        let result = check_addr_access(p, &loc, &mut state);
        assert!(matches!(
            result,
            PulseResult::FatalError(
                Diagnostic::AccessToInvalidAddress {
                    invalidation: Invalidation::ConstantDereference(value),
                    ..
                },
                _
            ) if value == IntLit::zero()
        ));
        let attrs = state
            .post
            .attrs
            .get(&p)
            .expect("known-zero access should keep attrs on the canonical address");
        assert!(matches!(
            attrs.get_invalid(),
            Some((Invalidation::ConstantDereference(value), _)) if *value == IntLit::zero()
        ));
    }

    #[test]
    fn test_eval_const_reuses_existing_integer_literal_value() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let first = match eval(&Exp::Const(Const::Cint(IntLit::zero())), &loc, &mut state) {
            PulseResult::Ok(v) => v,
            other => panic!("expected ok constant evaluation, got {other:?}"),
        };
        state.initialize(first);

        let second = match eval(&Exp::Const(Const::Cint(IntLit::zero())), &loc, &mut state) {
            PulseResult::Ok(v) => v,
            other => panic!("expected ok constant evaluation, got {other:?}"),
        };

        assert_eq!(
            first, second,
            "integer literals should reuse the existing formula representative"
        );
        let attrs = state
            .post
            .attrs
            .get(&first)
            .expect("reused literal should keep attrs on the shared address");
        assert!(
            attrs.contains(&Attribute::Initialized),
            "reused literal should preserve prior Initialized side effects"
        );
        assert!(
            matches!(
                attrs.get_invalid(),
                Some((
                    crate::invalidation::Invalidation::ConstantDereference(value),
                    _
                )) if *value == IntLit::zero()
            ),
            "reused literal should stay invalidated as the null constant"
        );
    }

    #[test]
    fn test_read_access_abduces_must_be_initialized_and_marks_initialized() {
        let loc = Location::dummy();
        let (pdesc, pvar) = make_pdesc_with_formal("p");
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar)))
            .expect("formal should be bound");

        let result = eval_deref_addr(formal_addr, &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(_)));

        let pre_attrs = state
            .pre
            .attrs
            .get(&formal_addr)
            .expect("formal read should abduce pre attrs");
        assert!(
            pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _))),
            "read access should keep MustBeValid in the precondition"
        );
        assert!(
            pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeInitialized(_, _))),
            "read access should keep MustBeInitialized in the precondition"
        );

        let post_attrs = state
            .post
            .attrs
            .get(&formal_addr)
            .expect("read access should leave post attrs");
        assert!(
            post_attrs.contains(&Attribute::Initialized),
            "successful reads should mark the accessed address initialized"
        );
    }

    #[test]
    fn test_write_access_marks_written_address_initialized() {
        let loc = Location::dummy();
        let (pdesc, pvar) = make_pdesc_with_formal("p");
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar)))
            .expect("formal should be bound");

        let result = write_deref(formal_addr, AbstractValue::mk_fresh(), &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(())));

        let post_attrs = state
            .post
            .attrs
            .get(&formal_addr)
            .expect("write access should leave post attrs");
        assert!(
            post_attrs.contains(&Attribute::Initialized),
            "writes should mark the written address initialized"
        );
        assert!(
            post_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::WrittenTo(_, _))),
            "writes should keep the WrittenTo summary marker"
        );
    }

    #[test]
    fn test_write_deref_does_not_abduce_old_pointee_into_pre() {
        let loc = Location::dummy();
        let (pdesc, pvar) = make_pdesc_with_formal("p");
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar)))
            .expect("formal should be bound");

        let formal_value = state.read_heap(formal_addr, Access::Dereference);
        let written = AbstractValue::mk_fresh();

        let result = write_deref(formal_value, written, &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(())));
        assert_eq!(
            state.pre.heap.find_edge(formal_addr, &Access::Dereference),
            Some(formal_value),
            "loading the formal should keep the formal-slot pre edge"
        );
        assert_eq!(
            state.pre.heap.find_edge(formal_value, &Access::Dereference),
            None,
            "writes should not synthesize a pre edge for the overwritten pointee"
        );
        assert_eq!(
            state
                .post
                .heap
                .find_edge(formal_value, &Access::Dereference),
            Some(written),
            "the post-state should record the new pointee value"
        );
    }

    #[test]
    fn test_check_addr_access_no_init_skips_must_be_initialized_abduction() {
        // Cross-ref: OCaml `PulseModelsImport.free_or_delete` calls
        // `check_addr_access path NoAccess`, which abduces `MustBeValid` on
        // the freed pointer but does *not* abduce `MustBeInitialized`. The
        // Rust equivalent is `check_addr_access_no_init` for the same
        // mode; this test pins the abduction shape so cluster A drift
        // (`MustBeInitialized` over-attached on every freed formal) cannot
        // creep back in via the model layer.
        let loc = Location::dummy();
        let (pdesc, pvar) = make_pdesc_with_formal("p");
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar)))
            .expect("formal should be bound");

        let result = check_addr_access_no_init(formal_addr, &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(())));

        let pre_attrs = state
            .pre
            .attrs
            .get(&formal_addr)
            .expect("NoAccess check should still abduce MustBeValid");
        assert!(
            pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _))),
            "NoAccess access should keep MustBeValid in the precondition"
        );
        assert!(
            !pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeInitialized(_, _))),
            "NoAccess access should NOT abduce MustBeInitialized in the precondition"
        );
        assert!(
            !state
                .post
                .attrs
                .get(&formal_addr)
                .map(|attrs| attrs.contains(&Attribute::Initialized))
                .unwrap_or(false),
            "NoAccess should not mark the address Initialized in the post"
        );
    }

    #[test]
    fn test_eval_lfield_base_in_write_mode_does_not_abduce_must_be_initialized() {
        // Cross-ref: OCaml `PulseOperations.eval_to_value_origin` for
        // `Lfield(base, field)` evaluated with mode=Write threads Write
        // through `eval_access_to_value_origin`, so the immediate base
        // check abduces `MustBeValid` only — not `MustBeInitialized`.
        // This is what makes `q->next = q` (a Store with LHS
        // `Lfield(Lvar q, next)`) abduce a clean `MustBeValid` on `q.*`
        // in OCaml, instead of `MustBeInitialized + MustBeValid` which is
        // what naive Read-mode evaluation would produce.
        use sil::fieldname::Fieldname;

        let loc = Location::dummy();
        let (pdesc, pvar) = make_pdesc_with_formal("q");
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        // `mk_initial` binds `q` on the stack to a fresh abstract value
        // representing the formal *value* (the pointer the caller passed in)
        // and pre-registers that value in `pre.heap` with empty edges, so
        // abducing attributes on it is allowed.
        let formal_value = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar.clone())))
            .expect("formal should be bound");

        // Build `Lfield(Lvar q, next)` and evaluate with Write mode.
        let lfield = Exp::Lfield(
            sil::exp::LfieldObjData {
                exp: Box::new(Exp::Lvar(pvar)),
                is_implicit: false,
            },
            Fieldname::make(
                sil::typ::TypeName::CStruct(
                    sil::qualified_cpp_name::QualifiedCppName::from_string("node"),
                ),
                "next",
            ),
            Typ::void(),
        );
        let result = eval_with_history_mode(&lfield, AccessMode::Write, &loc, &mut state);
        assert!(matches!(result, PulseResult::Ok(_)));

        let pre_attrs =
            state.pre.attrs.get(&formal_value).expect(
                "the field-access base check should abduce MustBeValid on the formal value",
            );
        assert!(
            pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _))),
            "Lfield base check in Write mode should still abduce MustBeValid"
        );
        assert!(
            !pre_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeInitialized(_, _))),
            "Lfield base check in Write mode should NOT abduce MustBeInitialized (cluster A regression guard)"
        );

        // Sanity check: the same call with mode=Read should produce both.
        let (pdesc2, pvar2) = make_pdesc_with_formal("q");
        let mut state2 = AbductiveDomain::mk_initial(&pdesc2);
        let formal_value2 = state2
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar2.clone())))
            .expect("formal should be bound");
        let lfield2 = Exp::Lfield(
            sil::exp::LfieldObjData {
                exp: Box::new(Exp::Lvar(pvar2)),
                is_implicit: false,
            },
            Fieldname::make(
                sil::typ::TypeName::CStruct(
                    sil::qualified_cpp_name::QualifiedCppName::from_string("node"),
                ),
                "next",
            ),
            Typ::void(),
        );
        let _ = eval_with_history_mode(&lfield2, AccessMode::Read, &loc, &mut state2);
        let pre_attrs2 = state2
            .pre
            .attrs
            .get(&formal_value2)
            .expect("the field-access base check should abduce on the formal value");
        assert!(
            pre_attrs2
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeInitialized(_, _))),
            "Lfield base check in Read mode SHOULD still abduce MustBeInitialized (no over-correction)"
        );
    }

    #[test]
    fn test_normal_eval_marks_nonzero_constants_invalid() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let value = match eval_with_history(
            &Exp::Const(Const::Cint(IntLit::of_int(1))),
            &loc,
            &mut state,
        ) {
            PulseResult::Ok(value) => value,
            other => panic!("expected constant eval to succeed, got {other:?}"),
        };

        let invalid = state
            .post
            .attrs
            .get(&value.addr)
            .and_then(|attrs| attrs.get_invalid())
            .map(|(inv, _)| inv.clone());
        assert_eq!(
            invalid,
            Some(Invalidation::ConstantDereference(IntLit::of_int(1))),
            "normal constant evaluation should keep OCaml's invalidation marker for non-zero ints"
        );
    }

    #[test]
    fn test_prune_eval_keeps_nonzero_constants_non_invalidating() {
        let loc = Location::dummy();
        let pdesc = Procdesc::new(Procname::c_from_string("test"), Typ::void(), loc.clone());
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let value = match eval_for_prune(
            &Exp::Const(Const::Cint(IntLit::of_int(1))),
            &loc,
            &mut state,
        ) {
            PulseResult::Ok(value) => value,
            other => panic!("expected prune eval to succeed, got {other:?}"),
        };

        assert!(
            state
                .post
                .attrs
                .get(&value)
                .and_then(|attrs| attrs.get_invalid())
                .is_none(),
            "prune-specific constant evaluation should stay non-invalidating"
        );
    }
}
