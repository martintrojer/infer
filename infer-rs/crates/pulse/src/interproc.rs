// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Interprocedural summary application for Pulse.
//!
//! Mirrors OCaml's `PulseInterproc.ml` (simplified).
//!
//! When a call to a known procedure with a summary is encountered,
//! `apply_summary` maps the callee's effects (heap writes, invalidations,
//! constraints) into the caller's abstract state.

use std::collections::{HashMap, HashSet};

use sil::exp::Exp;
use sil::ident::Ident;
use sil::location::Location;
use sil::procdesc::Procdesc;
use sil::typ::Typ;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::operations;
use crate::summary::PrePost;

/// Apply a callee's summary to the caller's abstract state.
///
/// Creates a substitution from callee abstract values to caller abstract
/// values, then applies the callee's heap effects, invalidations, and
/// path conditions to the caller's state.
///
/// Returns a list of resulting execution domains (may include errors
/// from the callee's summary).
pub fn apply_summary(
    caller_pdesc: &sil::procdesc::Procdesc,
    pre_post: &PrePost,
    ret_id: &Ident,
    actuals: &[(Exp, Typ)],
    loc: &Location,
    mut caller_state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Step 1: Build the callee→caller substitution from formals→actuals
    let mut subst: HashMap<AbstractValue, AbstractValue> = HashMap::new();

    for (i, (_formal_pvar, formal_addr)) in pre_post.formals.iter().enumerate() {
        if let Some((actual_exp, _typ)) = actuals.get(i) {
            let actual_val = operations::eval_or_fresh(actual_exp, loc, &mut caller_state);
            subst.insert(*formal_addr, actual_val);
        }
    }

    // Step 1a: Map each formal's loaded value (one deref from stack address)
    // to the actual value. This captures the C semantics where the formal's
    // loaded value IS the parameter value passed by the caller.
    //
    // Without this, pre-materialization follows the caller's existing Deref
    // edge and maps the formal value to what the caller's pointer POINTS TO
    // (too deep by one level), causing write-through-pointer effects to go
    // to the wrong address.
    //
    // Cross-ref: In OCaml this is handled implicitly through how eval_var
    // and the pre-materialization interact. Our explicit mapping ensures
    // formal values correctly correspond to actual values.
    {
        let pre_heap = &pre_post.pre.heap;
        let subst_snapshot: Vec<_> = subst.iter().map(|(k, v)| (*k, *v)).collect();
        for (formal_stack_addr, actual_val) in &subst_snapshot {
            if let Some(edges) = pre_heap.get_edges(*formal_stack_addr) {
                for (access, target) in edges.iter() {
                    if matches!(access, Access::Dereference) {
                        subst.entry(*target).or_insert(*actual_val);
                    }
                }
            }
        }
    }

    // Step 1b: Map callee globals to caller globals before pre-materialization.
    // Cross-ref: OCaml PulseInterproc.ml materialize_pre_for_globals.
    extend_subst_with_callee_globals(pre_post, &mut subst, &mut caller_state);

    // Step 1c: Translate constant array indices into caller-space values.
    // Cross-ref: OCaml PulseInterproc.ml materialize_pre_from_array_indices.
    extend_subst_with_callee_array_indices(pre_post, &mut subst, &mut caller_state);

    // Step 1d: Materialize pre-condition — walk the callee's pre-state heap
    // and match against the caller's heap. Check that callee's pre-conditions
    // are satisfied (addresses it reads are valid in the caller).
    // Matches OCaml's `materialize_pre` in PulseInterproc.ml.
    // Only the original formal→actual entries are "formal stack addresses"
    // (always valid). Step 1a additions are loaded values that may need checking.
    let formal_stack_addrs: std::collections::HashSet<AbstractValue> =
        pre_post.formals.iter().map(|(_, addr)| *addr).collect();

    let pre_error = match materialize_pre(
        &pre_post.pre,
        &pre_post.post,
        &formal_stack_addrs,
        &mut subst,
        &mut caller_state,
        loc,
    ) {
        PreMaterializeResult::PreConditionViolation(diag) => Some(*diag),
        PreMaterializeResult::Ok => None,
    };

    // Step 2: Apply the callee's post heap to the caller.
    //
    // This must handle strong updates, not just writes. If an access exists in
    // the callee pre but disappears from the callee post, we must delete the
    // corresponding caller edge. Cross-ref: OCaml PulseInterproc.ml
    // `delete_edges_in_callee_pre_from_caller` + `record_post_cell`.
    let mut processed_pre_cells = HashSet::new();
    for (callee_addr, pre_edges) in pre_post.pre.heap.iter() {
        let Some(&caller_addr) = subst.get(callee_addr) else {
            continue;
        };
        apply_post_cell(
            caller_addr,
            Some(pre_edges),
            pre_post.post.post.heap.get_edges(*callee_addr),
            &mut subst,
            &mut caller_state,
        );
        processed_pre_cells.insert(*callee_addr);
    }

    for (callee_addr, post_edges) in pre_post.post.post.heap.iter() {
        if processed_pre_cells.contains(callee_addr) {
            continue;
        }
        let caller_addr = resolve_mut(&mut subst, *callee_addr);
        apply_post_cell(
            caller_addr,
            None,
            Some(post_edges),
            &mut subst,
            &mut caller_state,
        );
    }

    // Step 3: Resolve the return value into the substitution BEFORE
    // translating the formula, so constraints on the return value (e.g.,
    // "return value >= 0") are properly mapped to the caller.
    if let Some(ret_addr) = &pre_post.result {
        let caller_ret = resolve_mut(&mut subst, *ret_addr);
        operations::write_id(ret_id, caller_ret, &mut caller_state);
    } else {
        let fresh = AbstractValue::mk_fresh();
        operations::write_id(ret_id, fresh, &mut caller_state);
    }

    // Step 4: Translate callee's formula constraints to caller.
    // If the callee's constraints contradict the caller's state
    // (e.g., callee assumes x < 128 but caller has x = 1000),
    // this pre_post is inapplicable → skip it (return empty).
    // Cross-ref: OCaml PulseInterproc.ml apply_post returns Unsat
    // when the callee's formula contradicts the caller.
    log::debug!(
        "[apply_summary] translate_formula for {:?} pre_post",
        pre_post.kind
    );
    if !translate_formula(&pre_post.post.path_condition, &subst, &mut caller_state) {
        log::debug!("[apply_summary] → rejected (Unsat)");
        return vec![]; // pre_post inapplicable to this caller state
    }
    log::debug!("[apply_summary] → accepted (Sat)");

    // Step 5: Apply callee's attributes to caller (invalidations + allocations).
    // This MUST happen after formula translation (step 4) because
    // translate_formula calls and_equal/and_equal_const which can merge
    // union-find classes, changing get_var_repr results. If attrs are
    // applied before formula translation, Allocated and later CFree (from
    // the free model) would be stored under different representatives,
    // causing false positive memory leaks.
    // Cross-ref: OCaml PulseInterproc.ml apply_post applies attrs after
    // incorporating the callee's path condition.
    let callee_attrs = &pre_post.post.post.attrs;
    for (callee_addr, attrs) in callee_attrs.iter() {
        let caller_addr = resolve_mut(&mut subst, *callee_addr);
        for attr in attrs.iter() {
            caller_state
                .post
                .attrs
                .add_one(caller_addr, translate_attribute(&mut subst, attr));
        }
    }

    // If there was a pre-condition violation, report it and abort this path.
    // The callee dereferences a formal that's invalid in the caller.
    // Matches OCaml's first_error → check_all_valid → error flow.
    if let Some(ref diag) = pre_error {
        log::debug!("[apply_summary] pre_error present: {diag}");
    }
    if let Some(diag) = pre_error {
        return if crate::summary::abort_is_manifest(caller_pdesc, &caller_state) {
            vec![ExecutionDomain::AbortProgram {
                state: Box::new(caller_state),
                diagnostic: Box::new(diag),
            }]
        } else {
            vec![ExecutionDomain::LatentAbortProgram {
                state: Box::new(caller_state),
                diagnostic: Box::new(diag),
            }]
        };
    }

    // Return the same execution domain kind as the callee's pre_post.
    // Cross-ref: OCaml PulseCallOperations.ml apply_callee dispatches
    // on the callee's execution state to determine the caller's state.
    match pre_post.kind {
        crate::summary::PrePostKind::ExitProgram => {
            vec![ExecutionDomain::ExitProgram(caller_state)]
        }
        crate::summary::PrePostKind::ContinueProgram => {
            vec![ExecutionDomain::ContinueProgram(caller_state)]
        }
        crate::summary::PrePostKind::AbortProgram => {
            // Cross-ref: OCaml PulseCallOperations.ml apply_callee preserves
            // AbortProgram when applying a callee summary instead of dropping
            // it. This is particularly important for on-demand specialized
            // summaries: their diagnostics are not published elsewhere.
            if let Some(diag) = &pre_post.diagnostic {
                vec![ExecutionDomain::AbortProgram {
                    state: Box::new(caller_state),
                    diagnostic: Box::new(diag.clone()),
                }]
            } else {
                vec![]
            }
        }
        crate::summary::PrePostKind::LatentAbortProgram => {
            // Cross-ref: OCaml PulseCallOperations.ml re-checks a latent issue
            // in the caller's summarized state before deciding whether it is
            // still latent or has become manifest here.
            if let Some(diag) = &pre_post.diagnostic {
                if crate::summary::abort_is_manifest(caller_pdesc, &caller_state) {
                    vec![ExecutionDomain::AbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(diag.clone()),
                    }]
                } else {
                    vec![ExecutionDomain::LatentAbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(diag.clone()),
                    }]
                }
            } else {
                vec![]
            }
        }
        crate::summary::PrePostKind::LatentInvalidAccess => {
            if let Some(diag) = &pre_post.diagnostic {
                let diag = translate_diagnostic(diag, &mut subst, &caller_state);
                if latent_invalid_access_is_manifest(caller_pdesc, &diag, &caller_state) {
                    let manifest_diag = reify_invalid_access_diagnostic(diag, &caller_state);
                    vec![ExecutionDomain::AbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(manifest_diag),
                    }]
                } else {
                    vec![ExecutionDomain::LatentInvalidAccess {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(diag),
                    }]
                }
            } else {
                vec![]
            }
        }
    }
}

fn translate_diagnostic(
    diagnostic: &Diagnostic,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &AbductiveDomain,
) -> Diagnostic {
    match diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location,
            invalidation_location,
        } => {
            let caller_addr = caller_state
                .path_condition
                .get_var_repr(resolve_mut(subst, *addr));
            Diagnostic::AccessToInvalidAddress {
                addr: caller_addr,
                invalidation: invalidation.clone(),
                access_location: access_location.clone(),
                invalidation_location: invalidation_location.clone(),
            }
        }
        _ => diagnostic.clone(),
    }
}

fn latent_invalid_access_is_manifest(
    caller_pdesc: &Procdesc,
    diagnostic: &Diagnostic,
    caller_state: &AbductiveDomain,
) -> bool {
    matches!(
        diagnostic,
        Diagnostic::AccessToInvalidAddress { addr, .. }
            if caller_state.check_valid(*addr).is_err()
                && crate::summary::abort_is_manifest(caller_pdesc, caller_state)
    )
}

fn reify_invalid_access_diagnostic(
    diagnostic: Diagnostic,
    caller_state: &AbductiveDomain,
) -> Diagnostic {
    match diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            access_location,
            ..
        } => match caller_state.check_valid(addr) {
            Err(inv_info) => Diagnostic::AccessToInvalidAddress {
                addr,
                invalidation: inv_info.0,
                access_location,
                invalidation_location: inv_info.1,
            },
            Ok(()) => Diagnostic::AccessToInvalidAddress {
                addr,
                invalidation: crate::invalidation::Invalidation::ConstantDereference(
                    sil::int_lit::IntLit::zero(),
                ),
                access_location,
                invalidation_location: Location::dummy(),
            },
        },
        _ => diagnostic,
    }
}

/// Walk the callee's pre-state heap and match it against the caller's heap.
///
/// For each edge `callee_addr --access--> callee_target` in the pre-state:
/// 1. Resolve `callee_addr` to `caller_addr` via the substitution
/// 2. If the caller has a matching edge `caller_addr --access--> caller_target`,
///    record `callee_target → caller_target` in the substitution
/// 3. If not, create a fresh caller value (the caller didn't have this edge)
///
/// This is the core of biabduction: it connects callee's internal abstract
/// values to the caller's existing heap structure, enabling null attributes
/// and other properties to flow through multi-level call chains.
///
/// Mirrors OCaml's `materialize_pre` in PulseInterproc.ml.
/// Result of pre-materialization.
enum PreMaterializeResult {
    /// Pre-materialization succeeded — callee's pre-conditions are met.
    Ok,
    /// Pre-condition violation — the callee dereferences a formal
    /// that is invalid (null/freed) in the caller's state.
    PreConditionViolation(Box<crate::diagnostic::Diagnostic>),
}

fn materialize_pre(
    callee_pre: &crate::base_domain::BaseDomain,
    callee_post: &AbductiveDomain,
    formal_stack_addrs: &std::collections::HashSet<AbstractValue>,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
    loc: &Location,
) -> PreMaterializeResult {
    let mut visited = std::collections::HashSet::new();
    let mut worklist: Vec<AbstractValue> = subst.keys().copied().collect();
    // Record first error but continue walking the pre (matching OCaml's
    // first_error field in call_state). OCaml (PulseInterproc.ml:601-624):
    // check_valid on every address with pre edges. On invalid: record
    // first_error, skip that address's edges, continue with the rest.
    let mut first_error: Option<Box<crate::diagnostic::Diagnostic>> = None;

    while let Some(callee_addr) = worklist.pop() {
        if !visited.insert(callee_addr) {
            continue;
        }

        let caller_addr = resolve_mut(subst, callee_addr);

        if let Some(edges) = callee_pre.heap.get_edges(callee_addr) {
            // Only check validity for addresses the callee actually
            // dereferences (marked must_be_valid). Just having pre-edges
            // (from loading the formal) doesn't mean the address must be valid.
            // Cross-ref: OCaml PulseInterproc.ml check_all_valid only checks
            // addresses with MustBeValid attribute (line 1218).
            // Skip must_be_valid check for formal stack addresses — they
            // map to actual VALUES in the subst, not stack addresses, so
            // check_valid would incorrectly test the value instead of the
            // (always-valid) stack slot. Only check derived addresses.
            let is_formal_stack = formal_stack_addrs.contains(&callee_addr);
            if !edges.is_empty() && !is_formal_stack && callee_post.is_must_be_valid(callee_addr) {
                if let Err(inv_info) = caller_state.check_valid(caller_addr) {
                    log::debug!("    [materialize_pre] PRE-VIOLATION: callee={callee_addr} caller={caller_addr}");
                    if first_error.is_none() {
                        first_error = Some(Box::new(
                            crate::diagnostic::Diagnostic::AccessToInvalidAddress {
                                addr: caller_addr,
                                invalidation: inv_info.0,
                                access_location: loc.clone(),
                                invalidation_location: inv_info.1,
                            },
                        ));
                    }
                    // Skip exploring edges from this invalid address
                    // (OCaml line 621-623)
                    continue;
                }
            }

            for (access, callee_target) in edges.iter() {
                let caller_access = translate_access(subst, access, caller_state);
                let caller_target = if let Some(existing) = caller_state
                    .post
                    .heap
                    .find_edge(caller_addr, &caller_access)
                {
                    existing
                } else {
                    resolve_mut(subst, *callee_target)
                };

                subst.entry(*callee_target).or_insert(caller_target);

                if !visited.contains(callee_target) {
                    worklist.push(*callee_target);
                }
            }
        }
    }

    match first_error {
        Some(diag) => PreMaterializeResult::PreConditionViolation(diag),
        None => PreMaterializeResult::Ok,
    }
}

fn extend_subst_with_callee_globals(
    pre_post: &PrePost,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
) {
    let mut map_globals = |stack: &crate::base_stack::BaseStack| {
        for (var, addr) in stack.iter() {
            if !var.is_global() {
                continue;
            }
            let caller_addr = caller_state.eval_var(var);
            subst.entry(*addr).or_insert(caller_addr);
        }
    };

    map_globals(&pre_post.pre.stack);
    map_globals(&pre_post.post.post.stack);
}

fn extend_subst_with_callee_array_indices(
    pre_post: &PrePost,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
) {
    let mut map_indices = |heap: &crate::base_memory::BaseMemory| {
        for (_addr, edges) in heap.iter() {
            for (access, _target) in edges.iter() {
                let Access::ArrayAccess(_, callee_idx) = access else {
                    continue;
                };
                if subst.contains_key(callee_idx) {
                    continue;
                }
                let Some(c) = pre_post.post.get_const(*callee_idx) else {
                    continue;
                };
                let caller_idx = AbstractValue::mk_fresh();
                let _ = caller_state.and_equal_const(caller_idx, c);
                let caller_idx = caller_state.canonicalize_for_access(caller_idx);
                subst.entry(*callee_idx).or_insert(caller_idx);
            }
        }
    };

    map_indices(&pre_post.pre.heap);
    map_indices(&pre_post.post.post.heap);
}

fn apply_post_cell(
    caller_addr: AbstractValue,
    pre_edges_opt: Option<&crate::base_memory::Edges>,
    post_edges_opt: Option<&crate::base_memory::Edges>,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
) {
    let mut caller_edges = caller_state
        .post
        .heap
        .get_edges(caller_addr)
        .cloned()
        .unwrap_or_default();

    if let Some(pre_edges) = pre_edges_opt {
        for (access, _) in pre_edges.iter() {
            let caller_access = translate_access(subst, access, caller_state);
            caller_edges.remove(&caller_access);
        }
    }

    if let Some(post_edges) = post_edges_opt {
        for (access, callee_target) in post_edges.iter() {
            let caller_target = resolve_mut(subst, *callee_target);
            let caller_access = translate_access(subst, access, caller_state);
            caller_edges.add(caller_access, caller_target);
        }
    }

    caller_state.post.heap.set_edges(caller_addr, caller_edges);
}

/// Resolve a callee abstract value to a caller abstract value.
///
/// If the value is in the substitution, return the caller value.
/// Otherwise, create a fresh value, remember the mapping, and return it.
fn resolve_mut(
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    callee_val: AbstractValue,
) -> AbstractValue {
    *subst
        .entry(callee_val)
        .or_insert_with(AbstractValue::mk_fresh)
}

/// Translate the callee's formula constraints into the caller's state.
///
/// Returns false if unsatisfiable (callee constraints contradict caller).
///
/// Translates linear equations, atoms, and known constants from the
/// callee's formula space into the caller's abstract value space.
fn translate_formula(
    callee_formula: &crate::formula::Formula,
    subst: &HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
) -> bool {
    let phi = &callee_formula.phi();

    // Translate linear equations: for each callee_v = lin_expr,
    // translate all variables in the linear expression to caller space
    for (&callee_v, lin) in &phi.linear_eqs {
        let Some(&caller_v) = subst.get(&callee_v) else {
            continue;
        };
        // Check if it's a constant
        if let Some(q) = lin.get_as_const() {
            let c = *q.numer() / *q.denom();
            if caller_state.and_equal_const(caller_v, c).is_unsat() {
                return false;
            }
            continue;
        }
        // Check if it's a single variable
        if let Some(callee_other) = lin.get_as_var() {
            if let Some(&caller_other) = subst.get(&callee_other) {
                if caller_state.and_equal(caller_v, caller_other).is_unsat() {
                    return false;
                }
            }
            continue;
        }
        // For more complex linear expressions, translate if all vars are in subst
        let all_vars_mapped = lin.vars.keys().all(|v| subst.contains_key(v));
        if all_vars_mapped {
            let translated = lin.translate(|v| subst.get(&v).copied().unwrap_or(v));
            if caller_state
                .and_equal_linear(caller_v, translated)
                .is_unsat()
            {
                return false;
            }
        }
    }

    // Build extended subst: add callee constants not in formal→actual subst.
    // This handles atoms like `x >= 0` where `0` is a callee-local abstract
    // value not in the subst. Without this, atoms with callee constants
    // are either skipped or translated with dangling callee values.
    // Cross-ref: OCaml PulseInterproc.ml translates formula with full
    // variable resolution.
    let mut extended_subst = subst.clone();
    for atom in &phi.atoms {
        for v in atom.all_vars() {
            extended_subst.entry(v).or_insert_with(|| {
                if let Some(q) = phi.get_known_const(v) {
                    if q.is_integer() {
                        let c = *q.numer() / *q.denom();
                        let fresh = crate::abstract_value::AbstractValue::mk_fresh();
                        caller_state.and_equal_const(fresh, c);
                        return fresh;
                    }
                }
                v // unmapped, keep as-is (will be filtered by all_mapped check)
            });
        }
    }
    for atom in callee_formula.conditions().keys() {
        for v in atom.all_vars() {
            extended_subst.entry(v).or_insert_with(|| {
                if let Some(q) = phi.get_known_const(v) {
                    if q.is_integer() {
                        let c = *q.numer() / *q.denom();
                        let fresh = crate::abstract_value::AbstractValue::mk_fresh();
                        caller_state.and_equal_const(fresh, c);
                        return fresh;
                    }
                }
                v // unmapped, keep as-is (will be filtered by all_mapped check)
            });
        }
    }
    // Translate atoms using extended subst
    log::debug!(
        "  [translate_formula] atoms={}, conditions={}, subst_size={}, extended_subst_size={}",
        phi.atoms.len(),
        callee_formula.conditions().len(),
        subst.len(),
        extended_subst.len()
    );
    for atom in &phi.atoms {
        let all_mapped = atom
            .all_vars()
            .iter()
            .all(|v| extended_subst.contains_key(v));
        if !all_mapped {
            continue;
        }
        let translated = atom.translate(|v| extended_subst.get(&v).copied().unwrap_or(v));
        log::debug!("    atom: {atom} → {translated}");
        let sat = caller_state.and_atom_direct(translated);
        if sat.is_unsat() {
            log::debug!("    → UNSAT!");
            return false;
        }
    }

    // Translate remembered pure-function applications so imported conditions
    // on their results stay connected to caller-visible actuals.
    // Cross-ref: OCaml PulseFormula.and_callee_formula folds substitutions
    // through the whole formula, including function-application terms.
    for (key, ret) in phi.iter_fn_app_eqs() {
        let Some(&caller_ret) = extended_subst.get(ret) else {
            continue;
        };
        let mut caller_actuals = Vec::with_capacity(key.actuals.len());
        let mut all_mapped = true;
        for actual in &key.actuals {
            match actual {
                crate::formula::phi::FnAppActual::Const(c) => {
                    let fresh = crate::abstract_value::AbstractValue::mk_fresh();
                    if caller_state.and_equal_const(fresh, *c).is_unsat() {
                        return false;
                    }
                    caller_actuals.push(fresh);
                }
                crate::formula::phi::FnAppActual::Var(v) => {
                    let Some(&caller_actual) = extended_subst.get(v) else {
                        all_mapped = false;
                        break;
                    };
                    caller_actuals.push(caller_actual);
                }
            }
        }
        if !all_mapped {
            continue;
        }
        if caller_state
            .path_condition
            .and_fn_app(caller_ret, &key.callee, &caller_actuals)
            .is_unsat()
        {
            log::debug!("    fn_app: {}({:?}) → UNSAT!", key.callee, key.actuals);
            return false;
        }
    }

    for (atom, depth) in callee_formula.conditions() {
        let all_mapped = atom
            .all_vars()
            .iter()
            .all(|v| extended_subst.contains_key(v));
        if !all_mapped {
            continue;
        }
        let translated = atom.translate(|v| extended_subst.get(&v).copied().unwrap_or(v));
        log::debug!("    condition[{depth}]: {atom} → {translated}");
        let sat = caller_state.and_condition_direct(translated, depth + 1);
        if sat.is_unsat() {
            log::debug!("    → UNSAT!");
            return false;
        }
    }
    true
}

/// Translate a callee Access to a caller Access (substituting array indices).
fn translate_access(
    subst: &HashMap<AbstractValue, AbstractValue>,
    access: &Access,
    caller_state: &mut AbductiveDomain,
) -> Access {
    match access {
        Access::ArrayAccess(typ, idx) => {
            let caller_idx = subst.get(idx).copied().unwrap_or(*idx);
            let caller_idx = caller_state.canonicalize_for_access(caller_idx);
            Access::ArrayAccess(typ.clone(), caller_idx)
        }
        other => other.clone(),
    }
}

fn translate_attribute(
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    attr: &Attribute,
) -> Attribute {
    match attr {
        Attribute::ReturnedFromUnknown(values) => {
            Attribute::ReturnedFromUnknown(values.iter().map(|v| resolve_mut(subst, *v)).collect())
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute::Attribute;
    use sil::ident::IdentName;
    use sil::int_lit::IntLit;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::var::Var;

    fn mk_callee_summary_null_return() -> (Procdesc, PrePost) {
        // Simulate: int* callee() { return NULL; }
        let pname = Procname::c_from_string("callee");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        // Return value is null (= 0)
        let ret_val = AbstractValue::mk_fresh();
        state.and_equal_const(ret_val, 0);
        state.invalidate(
            ret_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            Location::dummy(),
        );

        let pre = state.pre.clone();
        let pre_post = PrePost {
            pre,
            post: state,
            formals: vec![],
            result: Some(ret_val),
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        (pdesc, pre_post)
    }

    #[test]
    fn test_apply_summary_null_return() {
        let (_, pre_post) = mk_callee_summary_null_return();

        // Caller state
        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname, Typ::void(), Location::dummy());
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);

        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &[],
            &Location::dummy(),
            caller_state,
        );

        // Should have at least a ContinueProgram
        assert!(results.iter().any(|r| r.is_continue()));

        // The return value should be bound and the address should be invalid
        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            let ret_var = Var::LogicalVar(ret_id);
            let ret_addr = s.post.stack.find(&ret_var);
            assert!(ret_addr.is_some(), "return value should be bound");
        }
    }

    #[test]
    fn test_apply_summary_propagates_return_closure_attr() {
        let callee_pname = Procname::c_from_string("return_funptr");
        let callee_pdesc = Procdesc::new(callee_pname, Typ::void(), Location::dummy());
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let ret_val = AbstractValue::mk_fresh();
        callee_state.post.attrs.add_one(
            ret_val,
            Attribute::Closure(Procname::c_from_string("assign_NULL")),
        );

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![],
            result: Some(ret_val),
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname, Typ::void(), Location::dummy());
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &[],
            &Location::dummy(),
            caller_state,
        );

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            let ret_var = Var::LogicalVar(ret_id);
            let ret_addr = s
                .post
                .stack
                .find(&ret_var)
                .expect("return value should be bound");
            assert!(s
                .get_closure_proc_name(ret_addr)
                .is_some_and(|pname| *pname == Procname::c_from_string("assign_NULL")));
        } else {
            panic!("expected a continuing result");
        }
    }

    #[test]
    fn test_apply_summary_with_actuals() {
        // Simulate: void callee(int *p) { *p = 42; }
        let pname = Procname::c_from_string("callee");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        // Formal p's address
        let p_pvar = Pvar::mk(Mangled::from_string("p"), pname);
        let p_var = Var::ProgramVar(Box::new(p_pvar.clone()));
        let p_addr = state.post.stack.find(&p_var).unwrap();

        // *p = 42: write through p's dereference
        let val_42 = AbstractValue::mk_fresh();
        state.and_equal_const(val_42, 42);
        state.write_heap(p_addr, Access::Dereference, val_42);

        let pre = state.pre.clone();
        let pre_post = PrePost {
            pre,
            post: state,
            formals: vec![(p_pvar, p_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        // Caller: call callee(&x)
        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let x_addr = AbstractValue::mk_fresh();
        caller_state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(x_pvar.clone())), x_addr);

        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(Exp::Lvar(x_pvar), Typ::void())];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(results.iter().any(|r| r.is_continue()));

        // After applying the summary, x should have a dereference edge
        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            assert!(
                s.post
                    .heap
                    .find_edge(x_addr, &Access::Dereference)
                    .is_some(),
                "callee's write through p should materialize in caller's heap"
            );
        }
    }

    #[test]
    fn test_apply_summary_propagates_global_stack_effects() {
        let callee_pname = Procname::c_from_string("__infer_globals_initializer_fp");
        let callee_pdesc = Procdesc::new(callee_pname, Typ::void(), Location::dummy());
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let global = Pvar::mk_global(Mangled::from_string("fp"));
        let global_var = Var::ProgramVar(Box::new(global.clone()));
        let global_addr = callee_state.eval_var(&global_var);
        let closure_val = AbstractValue::mk_fresh();
        callee_state
            .post
            .heap
            .add_edge(global_addr, Access::Dereference, closure_val);
        callee_state.post.attrs.add_one(
            closure_val,
            Attribute::Closure(Procname::c_from_string("assign_NULL")),
        );

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname, Typ::void(), Location::dummy());
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[],
            &Location::dummy(),
            caller_state,
        );

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            let caller_global_addr = s
                .post
                .stack
                .find(&global_var)
                .expect("caller global should be bound");
            let closure_addr = s
                .post
                .heap
                .find_edge(caller_global_addr, &Access::Dereference)
                .expect("initializer should populate the caller global");
            assert!(s
                .get_closure_proc_name(closure_addr)
                .is_some_and(|pname| *pname == Procname::c_from_string("assign_NULL")));
        } else {
            panic!("expected a continuing result");
        }
    }

    #[test]
    fn test_apply_summary_materialize_pre_translates_array_indices() {
        let callee_pname = Procname::c_from_string("free_slot");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(
            Mangled::from_string("array"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let array_pvar = Pvar::mk(Mangled::from_string("array"), callee_pname);
        let array_var = Var::ProgramVar(Box::new(array_pvar.clone()));
        let array_addr = callee_state.post.stack.find(&array_var).unwrap();
        let array_val = callee_state.read_heap(array_addr, Access::Dereference);
        let callee_idx = AbstractValue::mk_fresh();
        let _ = callee_state.and_equal_const(callee_idx, 42);
        let callee_idx = callee_state.canonicalize_for_access(callee_idx);
        let slot_val =
            callee_state.read_heap(array_val, Access::ArrayAccess(Typ::void(), callee_idx));
        callee_state.invalidate(
            slot_val,
            crate::invalidation::Invalidation::CFree,
            Location::dummy(),
        );

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(array_pvar.clone(), array_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let actual = Pvar::mk(Mangled::from_string("local_array"), caller_pname);
        let actual_var = Var::ProgramVar(Box::new(actual.clone()));
        let actual_addr = caller_state.eval_var(&actual_var);
        let caller_idx = AbstractValue::mk_fresh();
        let _ = caller_state.and_equal_const(caller_idx, 42);
        let caller_idx = caller_state.canonicalize_for_access(caller_idx);
        let allocated = AbstractValue::mk_fresh();
        caller_state.post.heap.add_edge(
            actual_addr,
            Access::ArrayAccess(Typ::void(), caller_idx),
            allocated,
        );
        caller_state.allocate(
            allocated,
            crate::attribute::Allocator::CMalloc,
            Location::dummy(),
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[(Exp::Lvar(actual), Typ::mk_ptr(Typ::void()))],
            &Location::dummy(),
            caller_state,
        );

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            let attrs = s
                .post
                .attrs
                .get(&allocated)
                .expect("allocated value should keep its attrs");
            let invalidated: Vec<_> = s
                .post
                .attrs
                .iter()
                .filter_map(|(addr, attrs)| {
                    attrs
                        .get_invalid()
                        .map(|(inv, _)| (*addr, format!("{inv:?}")))
                })
                .collect();
            assert!(
                attrs
                    .get_invalid()
                    .is_some_and(|(inv, _)| matches!(inv, crate::invalidation::Invalidation::CFree)),
                "translated array access should invalidate the caller's allocated element; allocated={allocated:?} invalid={invalidated:?}"
            );
        } else {
            panic!("expected a continuing result");
        }
    }

    #[test]
    fn test_apply_summary_canonicalizes_constant_array_indices_in_post() {
        let writer_pname = Procname::c_from_string("store_slot");
        let mut writer_pdesc = Procdesc::new(writer_pname.clone(), Typ::void(), Location::dummy());
        writer_pdesc.formals = vec![
            (
                Mangled::from_string("array"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
            (Mangled::from_string("idx"), Typ::void(), Default::default()),
        ];
        let mut writer_state = AbductiveDomain::mk_initial(&writer_pdesc);

        let writer_array_pvar = Pvar::mk(Mangled::from_string("array"), writer_pname.clone());
        let writer_idx_pvar = Pvar::mk(Mangled::from_string("idx"), writer_pname);
        let writer_array_addr = writer_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(writer_array_pvar.clone())))
            .unwrap();
        let writer_idx_addr = writer_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(writer_idx_pvar.clone())))
            .unwrap();
        let writer_array_val = writer_state.read_heap(writer_array_addr, Access::Dereference);
        let writer_idx_val = writer_state.read_heap(writer_idx_addr, Access::Dereference);
        let allocated = AbstractValue::mk_fresh();
        writer_state.post.heap.add_edge(
            writer_array_val,
            Access::ArrayAccess(Typ::void(), writer_idx_val),
            allocated,
        );
        writer_state.allocate(
            allocated,
            crate::attribute::Allocator::CMalloc,
            Location::dummy(),
        );

        let writer_summary = PrePost {
            pre: writer_state.pre.clone(),
            post: writer_state,
            formals: vec![
                (writer_array_pvar.clone(), writer_array_addr),
                (writer_idx_pvar.clone(), writer_idx_addr),
            ],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let free_pname = Procname::c_from_string("free_slot");
        let mut free_pdesc = Procdesc::new(free_pname.clone(), Typ::void(), Location::dummy());
        free_pdesc.formals = vec![
            (
                Mangled::from_string("array"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
            (Mangled::from_string("idx"), Typ::void(), Default::default()),
        ];
        let mut free_state = AbductiveDomain::mk_initial(&free_pdesc);

        let free_array_pvar = Pvar::mk(Mangled::from_string("array"), free_pname.clone());
        let free_idx_pvar = Pvar::mk(Mangled::from_string("idx"), free_pname);
        let free_array_addr = free_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(free_array_pvar.clone())))
            .unwrap();
        let free_idx_addr = free_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(free_idx_pvar.clone())))
            .unwrap();
        let free_array_val = free_state.read_heap(free_array_addr, Access::Dereference);
        let free_idx_val = free_state.read_heap(free_idx_addr, Access::Dereference);
        let free_slot_val = free_state.read_heap(
            free_array_val,
            Access::ArrayAccess(Typ::void(), free_idx_val),
        );
        free_state.invalidate(
            free_slot_val,
            crate::invalidation::Invalidation::CFree,
            Location::dummy(),
        );

        let free_summary = PrePost {
            pre: free_state.pre.clone(),
            post: free_state,
            formals: vec![
                (free_array_pvar, free_array_addr),
                (free_idx_pvar, free_idx_addr),
            ],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let caller_array = Pvar::mk(Mangled::from_string("array"), caller_pname);
        let caller_array_var = Var::ProgramVar(Box::new(caller_array.clone()));
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let actuals = vec![
            (Exp::Lvar(caller_array.clone()), Typ::mk_ptr(Typ::void())),
            (
                Exp::Const(sil::const_val::Const::Cint(IntLit::of_int(42))),
                Typ::void(),
            ),
        ];

        let mut after_write = match apply_summary(
            &caller_pdesc,
            &writer_summary,
            &Ident::create_none(),
            &actuals,
            &Location::dummy(),
            caller_state,
        )
        .into_iter()
        .find_map(|result| match result {
            ExecutionDomain::ContinueProgram(state) => Some(state),
            _ => None,
        }) {
            Some(state) => state,
            None => panic!("writer summary should continue"),
        };

        let caller_array_addr = after_write
            .post
            .stack
            .find(&caller_array_var)
            .expect("caller array should be bound");
        let lookup_idx = AbstractValue::mk_fresh();
        let _ = after_write.and_equal_const(lookup_idx, 42);
        let lookup_idx = after_write.canonicalize_for_access(lookup_idx);
        let caller_allocated = after_write
            .post
            .heap
            .find_edge(
                caller_array_addr,
                &Access::ArrayAccess(Typ::void(), lookup_idx),
            )
            .expect("writer summary should store under the canonical caller index");

        let results = apply_summary(
            &caller_pdesc,
            &free_summary,
            &Ident::create_none(),
            &actuals,
            &Location::dummy(),
            after_write,
        );

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            let caller_allocated = s.get_var_repr(caller_allocated);
            let attrs = s
                .post
                .attrs
                .get(&caller_allocated)
                .expect("allocated value should keep its attrs");
            assert!(
                attrs
                    .get_invalid()
                    .is_some_and(|(inv, _)| matches!(inv, crate::invalidation::Invalidation::CFree)),
                "free summary should invalidate the array element stored under the same constant index"
            );
        } else {
            panic!("free summary should continue");
        }
    }

    #[test]
    fn test_apply_summary_removes_caller_edges_missing_from_callee_post() {
        // Simulate: void clear_out(int **out) { *out = <deleted>; }
        // The callee pre reads `*out`, but the post no longer has that edge.
        // Applying the summary should delete the corresponding caller edge.
        let pname = Procname::c_from_string("clear_out");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(Mangled::from_string("out"), Typ::void(), Default::default())];
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let out_pvar = Pvar::mk(Mangled::from_string("out"), pname);
        let out_var = Var::ProgramVar(Box::new(out_pvar.clone()));
        let out_stack_addr = state.post.stack.find(&out_var).unwrap();
        let out_value = state.read_heap(out_stack_addr, Access::Dereference);
        let _old_target = state.read_heap(out_value, Access::Dereference);

        let pre = state.pre.clone();
        state.post.heap.remove(out_value);

        let pre_post = PrePost {
            pre,
            post: state,
            formals: vec![(out_pvar, out_stack_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let x_addr = AbstractValue::mk_fresh();
        let caller_old_target = AbstractValue::mk_fresh();
        caller_state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(x_pvar.clone())), x_addr);
        caller_state.write_heap(x_addr, Access::Dereference, caller_old_target);

        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(Exp::Lvar(x_pvar), Typ::void())];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(results.iter().any(|r| r.is_continue()));

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            assert_eq!(
                s.post.heap.find_edge(x_addr, &Access::Dereference),
                None,
                "callee post should delete caller edges that disappeared from the callee post"
            );
        }
    }

    #[test]
    fn test_apply_summary_propagates_abort_program_diagnostic() {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let diagnostic = crate::diagnostic::Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::of_raw(1),
            invalidation: crate::invalidation::Invalidation::CFree,
            access_location: Location::dummy(),
            invalidation_location: Location::dummy(),
        };
        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state.clone(),
            formals: vec![(
                Pvar::mk(Mangled::from_string("p"), callee_pname),
                callee_state
                    .post
                    .stack
                    .find(&Var::ProgramVar(Box::new(Pvar::mk(
                        Mangled::from_string("p"),
                        Procname::c_from_string("callee"),
                    ))))
                    .unwrap(),
            )],
            result: None,
            kind: crate::summary::PrePostKind::AbortProgram,
            diagnostic: Some(diagnostic.clone()),
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let arg = AbstractValue::mk_fresh();
        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        caller_state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(x_pvar.clone())), arg);

        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(Exp::Lvar(x_pvar), Typ::void())];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::AbortProgram { diagnostic: found, .. }]
                if found.as_ref() == &diagnostic
        ));
    }

    fn mk_latent_abort_pre_post() -> (PrePost, crate::diagnostic::Diagnostic) {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let formal_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname);
        let formal_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val = callee_state.read_heap(formal_addr, Access::Dereference);
        let _ = callee_state.path_condition.and_condition_direct(
            crate::formula::atom::Atom::Equal(
                crate::formula::term::Term::Var(formal_val),
                crate::formula::term::Term::Const(4),
            ),
            1,
        );

        let diagnostic = crate::diagnostic::Diagnostic::AccessToInvalidAddress {
            addr: formal_val,
            invalidation: crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            access_location: Location::dummy(),
            invalidation_location: Location::dummy(),
        };
        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::LatentAbortProgram,
            diagnostic: Some(diagnostic.clone()),
        };

        (pre_post, diagnostic)
    }

    fn mk_latent_invalid_access_pre_post(invalidate_formal: bool) -> (PrePost, Diagnostic, Pvar) {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let formal_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname);
        let formal_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val = callee_state.read_heap(formal_addr, Access::Dereference);
        callee_state.mark_must_be_valid(formal_val);
        if invalidate_formal {
            callee_state.invalidate(
                formal_val,
                crate::invalidation::Invalidation::CFree,
                Location::dummy(),
            );
        }

        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: formal_val,
            invalidation: crate::invalidation::Invalidation::CFree,
            access_location: Location::dummy(),
            invalidation_location: Location::dummy(),
        };
        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar.clone(), formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::LatentInvalidAccess,
            diagnostic: Some(diagnostic.clone()),
        };

        (pre_post, diagnostic, formal_pvar)
    }

    #[test]
    fn test_apply_summary_keeps_still_latent_abort_as_latent() {
        let (pre_post, diagnostic) = mk_latent_abort_pre_post();

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(
            Exp::Lvar(Pvar::mk(Mangled::from_string("x"), caller_pname)),
            Typ::void(),
        )];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentAbortProgram { diagnostic: found, .. }]
                if found.as_ref() == &diagnostic
        ));
    }

    #[test]
    fn test_apply_summary_reifies_latent_abort_at_entry_point() {
        let (pre_post, diagnostic) = mk_latent_abort_pre_post();

        let caller_pname = Procname::c_from_string("main");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(
            Mangled::from_string("argc"),
            Typ::void(),
            Default::default(),
        )];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(
            Exp::Lvar(Pvar::mk(Mangled::from_string("argc"), caller_pname)),
            Typ::void(),
        )];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::AbortProgram { diagnostic: found, .. }]
                if found.as_ref() == &diagnostic
        ));
    }

    #[test]
    fn test_apply_summary_keeps_still_latent_invalid_access_as_latent() {
        let (pre_post, diagnostic, _formal_pvar) = mk_latent_invalid_access_pre_post(false);

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(
            Exp::Lvar(Pvar::mk(Mangled::from_string("x"), caller_pname)),
            Typ::void(),
        )];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentInvalidAccess { diagnostic: found, .. }]
                if found.as_ref().get_issue_type_id() == diagnostic.get_issue_type_id()
        ));
    }

    #[test]
    fn test_apply_summary_reifies_latent_invalid_access_when_caller_addr_is_invalid() {
        let (pre_post, _diagnostic, _formal_pvar) = mk_latent_invalid_access_pre_post(true);

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(
            Exp::Lvar(Pvar::mk(Mangled::from_string("x"), caller_pname)),
            Typ::void(),
        )];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::AbortProgram { diagnostic: found, .. }]
                if found.get_issue_type_id() == diagnostics::issue_type::IssueTypeId::UseAfterFree
        ));
    }

    #[test]
    fn test_apply_summary_keeps_invalid_access_latent_when_caller_condition_is_imported() {
        let (pre_post, diagnostic, _formal_pvar) = mk_latent_invalid_access_pre_post(true);

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname.clone());
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar.clone())))
            .unwrap();
        let caller_formal_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        caller_state.invalidate(
            caller_formal_val,
            crate::invalidation::Invalidation::CFree,
            Location::dummy(),
        );
        let _ = caller_state.path_condition.and_condition_direct(
            crate::formula::atom::Atom::Equal(
                crate::formula::term::Term::Var(caller_formal_val),
                crate::formula::term::Term::Const(4),
            ),
            1,
        );

        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![(Exp::Lvar(caller_formal_pvar), Typ::void())];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentInvalidAccess { diagnostic: found, .. }]
                if found.as_ref().get_issue_type_id() == diagnostic.get_issue_type_id()
        ));
    }

    fn mk_latent_precondition_pre_post() -> PrePost {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![
            (
                Mangled::from_string("flag"),
                Typ::void(),
                Default::default(),
            ),
            (Mangled::from_string("p"), Typ::void(), Default::default()),
        ];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let flag_pvar = Pvar::mk(Mangled::from_string("flag"), callee_pname.clone());
        let flag_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(flag_pvar.clone())))
            .unwrap();
        let flag_val = callee_state.read_heap(flag_addr, Access::Dereference);
        let p_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname);
        let p_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar.clone())))
            .unwrap();
        let p_val = callee_state.read_heap(p_addr, Access::Dereference);
        let p_target = AbstractValue::mk_fresh();
        callee_state
            .pre
            .heap
            .add_edge(p_val, Access::Dereference, p_target);
        callee_state.pre.heap.register_address(p_target);
        callee_state
            .must_be_valid
            .insert(callee_state.path_condition.get_var_repr(p_val));
        let _ = callee_state.path_condition.and_condition_direct(
            crate::formula::atom::Atom::Equal(
                crate::formula::term::Term::Var(flag_val),
                crate::formula::term::Term::Const(4),
            ),
            1,
        );

        PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(flag_pvar, flag_addr), (p_pvar, p_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        }
    }

    #[test]
    fn test_apply_summary_keeps_latent_precondition_violation_as_latent() {
        let pre_post = mk_latent_precondition_pre_post();

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![
            (
                Exp::Lvar(Pvar::mk(Mangled::from_string("x"), caller_pname)),
                Typ::void(),
            ),
            (
                Exp::Const(sil::const_val::Const::Cint(IntLit::zero())),
                Typ::void(),
            ),
        ];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentAbortProgram { .. }]
        ));
    }

    #[test]
    fn test_apply_summary_reifies_latent_precondition_violation_at_entry_point() {
        let pre_post = mk_latent_precondition_pre_post();

        let caller_pname = Procname::c_from_string("main");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(
            Mangled::from_string("argc"),
            Typ::void(),
            Default::default(),
        )];
        let caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let actuals = vec![
            (
                Exp::Lvar(Pvar::mk(Mangled::from_string("argc"), caller_pname)),
                Typ::void(),
            ),
            (
                Exp::Const(sil::const_val::Const::Cint(IntLit::zero())),
                Typ::void(),
            ),
        ];
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::AbortProgram { .. }]
        ));
    }
}
