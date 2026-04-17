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
use sil::int_lit::IntLit;
use sil::location::Location;
use sil::procdesc::Procdesc;
use sil::pvar::Pvar;
use sil::specialization::HeapPath;
use sil::typ::Typ;

use crate::abductive::{AbductiveDomain, ImportedFormulaEffect};
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::operations;
use crate::summary::PrePost;
use crate::value_history::{HistoryEvent, ValueHistory};

#[derive(Debug, Default)]
pub(crate) struct ApplySummaryOutcome {
    pub(crate) results: Vec<ExecutionDomain>,
    pub(crate) alias_specialization: Option<Vec<Vec<HeapPath>>>,
}

enum TranslateFormulaResult {
    Sat,
    Unsat,
    PotentialInvalidAccess(Box<Diagnostic>),
}

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
    caller_state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    apply_summary_with_aliasing(caller_pdesc, pre_post, ret_id, actuals, loc, caller_state).results
}

pub(crate) fn apply_summary_with_aliasing(
    caller_pdesc: &sil::procdesc::Procdesc,
    pre_post: &PrePost,
    ret_id: &Ident,
    actuals: &[(Exp, Typ)],
    loc: &Location,
    mut caller_state: AbductiveDomain,
) -> ApplySummaryOutcome {
    // Step 1: Build the callee→caller substitution from formals→actuals
    let mut subst: HashMap<AbstractValue, AbstractValue> = HashMap::new();
    let mut callee_heap_paths: HashMap<AbstractValue, Option<HeapPath>> = HashMap::new();
    let mut value_actual_formal_stack_addrs = std::collections::HashSet::new();
    let mut formal_histories: std::collections::BTreeMap<Pvar, ValueHistory> =
        std::collections::BTreeMap::new();

    for (i, (formal_pvar, formal_addr)) in pre_post.formals.iter().enumerate() {
        if let Some((actual_exp, _typ)) = actuals.get(i) {
            let actual_val =
                operations::eval_or_fresh_with_history(actual_exp, loc, &mut caller_state);
            subst.insert(*formal_addr, actual_val.addr);
            if !matches!(actual_exp, Exp::Lvar(_) | Exp::Lfield(..) | Exp::Lindex(..)) {
                value_actual_formal_stack_addrs.insert(*formal_addr);
            }
            formal_histories.insert(formal_pvar.clone(), actual_val.history);
            callee_heap_paths.entry(*formal_addr).or_insert(None);
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
        let subst_snapshot: Vec<_> = pre_post
            .formals
            .iter()
            .filter_map(|(formal_pvar, formal_stack_addr)| {
                subst
                    .get(formal_stack_addr)
                    .copied()
                    .map(|actual_val| (formal_pvar, *formal_stack_addr, actual_val))
            })
            .collect();
        for (formal_pvar, formal_stack_addr, actual_val) in &subst_snapshot {
            if let Some(edges) = pre_heap.get_edges(*formal_stack_addr) {
                for (access, target) in edges.iter() {
                    if matches!(access, Access::Dereference) {
                        subst.entry(*target).or_insert(*actual_val);
                        callee_heap_paths
                            .entry(*target)
                            .or_insert_with(|| Some(pvar_heap_path(formal_pvar)));
                    }
                }
            }
        }
    }

    // Step 1b: Map callee globals to caller globals before pre-materialization.
    // Cross-ref: OCaml PulseInterproc.ml materialize_pre_for_globals.
    extend_subst_with_callee_globals(
        pre_post,
        &mut subst,
        &mut callee_heap_paths,
        &mut caller_state,
    );

    // Step 1c: Translate constant array indices into caller-space values.
    // Cross-ref: OCaml PulseInterproc.ml materialize_pre_from_array_indices.
    extend_subst_with_callee_array_indices(
        pre_post,
        &mut subst,
        &mut callee_heap_paths,
        &mut caller_state,
    );

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
        MaterializePreContext {
            formal_stack_addrs: &formal_stack_addrs,
            value_actual_formal_stack_addrs: &value_actual_formal_stack_addrs,
            subst: &mut subst,
            callee_heap_paths: &mut callee_heap_paths,
            caller_state: &mut caller_state,
            loc,
        },
    ) {
        PreMaterializeResult::PreConditionViolation(diag) => Some(*diag),
        PreMaterializeResult::AliasingContradiction {
            caller_addr,
            callee_addr,
            other_callee_addr,
            alias_groups,
        } => {
            // Cross-ref: OCaml `PulseInterproc.apply_summary` turns
            // `AliasingWithAllAliases` into an inapplicable pre/post so the
            // caller can rely on alias specialization instead of forcing the
            // unspecialized summary through.
            log::debug!(
                "[apply_summary] rejected due to aliasing contradiction: caller={caller_addr} callee={callee_addr} other={other_callee_addr}"
            );
            return ApplySummaryOutcome {
                results: vec![],
                alias_specialization: alias_groups,
            };
        }
        PreMaterializeResult::Ok => None,
    };

    import_callee_pre_attributes(
        &pre_post.pre,
        &formal_stack_addrs,
        &mut subst,
        &mut caller_state,
    );

    // Snapshot caller-owned allocated roots after pre-materialization but
    // before the callee post is applied. Cross-ref: OCaml imports callee
    // arithmetic before `apply_post`, so imported `EqZero` only treats
    // addresses that already existed in the caller as contradictions or
    // potential invalid accesses.
    let (stack_allocated_before_call, heap_allocated_before_call) =
        caller_state.snapshot_allocated_before_call();

    // Step 2: Apply the callee's post heap to the caller.
    //
    // This must handle strong updates, not just writes. If an access exists in
    // the callee pre but disappears from the callee post, we must delete the
    // corresponding caller edge. Cross-ref: OCaml PulseInterproc.ml
    // `delete_edges_in_callee_pre_from_caller` + `record_post_cell`.
    let mut processed_pre_cells = HashSet::new();
    for (callee_addr, pre_edges) in pre_post.pre.heap.iter() {
        // Cross-ref: OCaml materializes actuals from the dereferenced formal
        // value, not by replaying the callee formal stack cell onto the
        // caller. Re-applying that bookkeeping cell after Step 1a would turn
        // by-value actuals into bogus self-edges such as `v -*-> v`.
        if value_actual_formal_stack_addrs.contains(callee_addr) {
            continue;
        }
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
        if value_actual_formal_stack_addrs.contains(callee_addr) {
            continue;
        }
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

    // Step 3: Resolve the return value into the substitution before importing
    // the formula, so constraints on the return value map to caller space.
    if let Some(ret_addr) = &pre_post.result {
        let caller_ret = resolve_mut(&mut subst, *ret_addr);
        let ret_history = pre_post
            .post
            .history_of_value(*ret_addr)
            .map(|history| history.map_formals(&formal_histories))
            .unwrap_or_else(|| ValueHistory::assignment(loc.clone()));
        operations::write_id_with_history(
            ret_id,
            crate::value_history::ValueWithHistory::new(caller_ret, ret_history),
            &mut caller_state,
        );
    } else {
        let fresh = AbstractValue::mk_fresh();
        operations::write_id_with_history(
            ret_id,
            crate::value_history::ValueWithHistory::new(
                fresh,
                ValueHistory::assignment(loc.clone()),
            ),
            &mut caller_state,
        );
    }

    // Step 4: Translate callee's formula constraints to the caller.
    // We keep Rust's existing heap-then-formula sequencing, but preserve
    // OCaml's allocation distinction by checking imported `EqZero` against the
    // caller's pre-call allocation snapshot rather than the already-updated
    // post heap.
    log::debug!(
        "[apply_summary] translate_formula for {:?} pre_post",
        pre_post.kind
    );
    match translate_formula(
        &pre_post.post.path_condition,
        &pre_post.post,
        &mut subst,
        &mut caller_state,
        stack_allocated_before_call,
        heap_allocated_before_call,
        loc,
    ) {
        TranslateFormulaResult::Sat => {
            log::debug!("[apply_summary] → accepted (Sat)");
        }
        TranslateFormulaResult::Unsat => {
            log::debug!("[apply_summary] → rejected (Unsat)");
            return ApplySummaryOutcome::default();
        }
        TranslateFormulaResult::PotentialInvalidAccess(diag) => {
            mark_diagnostic_addr_must_be_valid(&mut caller_state, &diag);
            log::debug!("[apply_summary] → imported potential invalid access: {diag}");
            return if latent_invalid_access_is_manifest(caller_pdesc, &diag, &caller_state) {
                let manifest_diag = reify_invalid_access_diagnostic(*diag, &caller_state);
                ApplySummaryOutcome {
                    results: vec![ExecutionDomain::AbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(manifest_diag),
                    }],
                    alias_specialization: None,
                }
            } else {
                ApplySummaryOutcome {
                    results: vec![ExecutionDomain::LatentInvalidAccess {
                        state: Box::new(caller_state),
                        diagnostic: diag,
                    }],
                    alias_specialization: None,
                }
            };
        }
    }

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
            ApplySummaryOutcome {
                results: vec![ExecutionDomain::AbortProgram {
                    state: Box::new(caller_state),
                    diagnostic: Box::new(diag),
                }],
                alias_specialization: None,
            }
        } else {
            ApplySummaryOutcome {
                results: vec![ExecutionDomain::LatentAbortProgram {
                    state: Box::new(caller_state),
                    diagnostic: Box::new(diag),
                }],
                alias_specialization: None,
            }
        };
    }

    let stopped_summary = (!matches!(pre_post.kind, crate::summary::PrePostKind::ContinueProgram))
        .then(|| crate::summary::summarize_stopped_state(caller_pdesc, &caller_state));

    if let Some(summary) = stopped_summary.as_ref() {
        if let Some(diag) = summary.potential_invalid_access.as_ref() {
            let mut caller_summary_state = summary.state.clone();
            let diag = rebase_diagnostic_to_state(diag.clone(), &caller_summary_state);
            mark_diagnostic_addr_must_be_valid(&mut caller_summary_state, &diag);
            return if latent_invalid_access_is_manifest(caller_pdesc, &diag, &caller_summary_state)
            {
                let manifest_diag = reify_invalid_access_diagnostic(diag, &caller_summary_state);
                ApplySummaryOutcome {
                    results: vec![ExecutionDomain::AbortProgram {
                        state: Box::new(caller_summary_state),
                        diagnostic: Box::new(manifest_diag),
                    }],
                    alias_specialization: None,
                }
            } else {
                ApplySummaryOutcome {
                    results: vec![ExecutionDomain::LatentInvalidAccess {
                        state: Box::new(caller_summary_state),
                        diagnostic: Box::new(diag),
                    }],
                    alias_specialization: None,
                }
            };
        }
    }

    let caller_state = stopped_summary
        .map(|summary| summary.state)
        .unwrap_or(caller_state);

    // Return the same execution domain kind as the callee's pre_post.
    // Cross-ref: OCaml PulseCallOperations.ml apply_callee dispatches
    // on the callee's execution state to determine the caller's state.
    let results = match pre_post.kind {
        crate::summary::PrePostKind::ExitProgram => {
            vec![ExecutionDomain::ExitProgram(caller_state)]
        }
        crate::summary::PrePostKind::ContinueProgram => {
            vec![ExecutionDomain::ContinueProgram(caller_state)]
        }
        crate::summary::PrePostKind::AbortProgram => {
            // A manifest AbortProgram in the callee is already published on
            // the callee's own summary. Applying that same abort as a caller
            // AbortProgram republishes the local callee issue on every caller
            // (`angelism.c: skip_function_with_no_spec_ok` style duplication).
            //
            // Specialized summaries are handled separately:
            // `PulseSummary::add_specialized_summary` merges their diagnostics
            // onto the owning summary and strips manifest abort diagnostics
            // from the cached specialized pre/posts before they can reach
            // callers.
            //
            // So for ordinary callee-local manifest aborts, stop this caller
            // path without producing a caller-side execution state.
            vec![]
        }
        crate::summary::PrePostKind::LatentAbortProgram => {
            // Cross-ref: OCaml PulseCallOperations.ml re-checks a latent issue
            // in the caller's summarized state before deciding whether it is
            // still latent or has become manifest here.
            if let Some(diag) = &pre_post.diagnostic {
                let diag = rebase_diagnostic_to_state(
                    translate_diagnostic(diag, &mut subst, &caller_state, &formal_histories, loc),
                    &caller_state,
                );
                if crate::summary::abort_is_manifest(caller_pdesc, &caller_state) {
                    vec![ExecutionDomain::AbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(diag),
                    }]
                } else {
                    vec![ExecutionDomain::LatentAbortProgram {
                        state: Box::new(caller_state),
                        diagnostic: Box::new(diag),
                    }]
                }
            } else {
                vec![]
            }
        }
        crate::summary::PrePostKind::LatentInvalidAccess => {
            if let Some(diag) = pre_post.diagnostic.clone().or_else(|| {
                crate::summary::latent_invalid_access_diagnostic_from_exported_pre_post(pre_post)
            }) {
                let mut caller_state = caller_state;
                let diag = rebase_diagnostic_to_state(
                    translate_diagnostic(&diag, &mut subst, &caller_state, &formal_histories, loc),
                    &caller_state,
                );
                mark_diagnostic_addr_must_be_valid(&mut caller_state, &diag);
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
    };

    ApplySummaryOutcome {
        results,
        alias_specialization: None,
    }
}

fn translate_diagnostic(
    diagnostic: &Diagnostic,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &AbductiveDomain,
    formal_histories: &std::collections::BTreeMap<Pvar, ValueHistory>,
    loc: &Location,
) -> Diagnostic {
    match diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location: _,
            access_history,
            invalidation_history,
        } => {
            let caller_addr = caller_state
                .path_condition
                .get_var_repr(resolve_mut(subst, *addr));
            Diagnostic::AccessToInvalidAddress {
                addr: caller_addr,
                invalidation: invalidation.clone(),
                access_location: loc.clone(),
                access_history: access_history.map_formals(formal_histories),
                invalidation_history: invalidation_history.map_formals(formal_histories),
            }
        }
        _ => diagnostic.clone(),
    }
}

fn rebase_diagnostic_to_state(diagnostic: Diagnostic, state: &AbductiveDomain) -> Diagnostic {
    match diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location,
            access_history,
            invalidation_history,
        } => Diagnostic::AccessToInvalidAddress {
            addr: state.path_condition.get_var_repr(addr),
            invalidation,
            access_location,
            access_history,
            invalidation_history,
        },
        _ => diagnostic,
    }
}

fn latent_invalid_access_is_manifest(
    caller_pdesc: &Procdesc,
    diagnostic: &Diagnostic,
    caller_state: &AbductiveDomain,
) -> bool {
    matches!(
        crate::summary::classify_abort_kind(
            caller_pdesc,
            caller_state,
            &reify_invalid_access_diagnostic(diagnostic.clone(), caller_state),
        ),
        crate::summary::PrePostKind::AbortProgram
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
            access_history,
            ..
        } => match caller_state.check_valid(addr) {
            Err(inv_info) => Diagnostic::AccessToInvalidAddress {
                addr,
                invalidation: inv_info.0,
                access_location,
                access_history,
                invalidation_history: inv_info.1,
            },
            Ok(()) => Diagnostic::AccessToInvalidAddress {
                addr,
                invalidation: crate::invalidation::Invalidation::ConstantDereference(
                    sil::int_lit::IntLit::zero(),
                ),
                access_location,
                access_history,
                invalidation_history: ValueHistory::invalidated(
                    crate::invalidation::Invalidation::ConstantDereference(
                        sil::int_lit::IntLit::zero(),
                    ),
                    Location::dummy(),
                ),
            },
        },
        _ => diagnostic,
    }
}

fn mark_diagnostic_addr_must_be_valid(caller_state: &mut AbductiveDomain, diagnostic: &Diagnostic) {
    if let Diagnostic::AccessToInvalidAddress { addr, .. } = diagnostic {
        caller_state.mark_must_be_valid(*addr);
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
    /// Caller aliasing collapses callee heap roots that must stay disjoint.
    /// The unspecialized pre/post is therefore inapplicable.
    AliasingContradiction {
        caller_addr: AbstractValue,
        callee_addr: AbstractValue,
        other_callee_addr: AbstractValue,
        alias_groups: Option<Vec<Vec<HeapPath>>>,
    },
}

struct MaterializePreContext<'a> {
    formal_stack_addrs: &'a std::collections::HashSet<AbstractValue>,
    value_actual_formal_stack_addrs: &'a std::collections::HashSet<AbstractValue>,
    subst: &'a mut HashMap<AbstractValue, AbstractValue>,
    callee_heap_paths: &'a mut HashMap<AbstractValue, Option<HeapPath>>,
    caller_state: &'a mut AbductiveDomain,
    loc: &'a Location,
}

#[derive(Clone, Copy, Debug)]
struct AliasConflict {
    caller_addr: AbstractValue,
    callee_addr: AbstractValue,
    other_callee_addr: AbstractValue,
}

#[derive(Default)]
struct CallerAliasRoots {
    // Callee roots that have their own heap cell in the summary pre.
    pre_backed: HashMap<AbstractValue, Option<HeapPath>>,
    // Callee roots that have their own heap cell in the summary post.
    post_backed: HashMap<AbstractValue, Option<HeapPath>>,
}

struct AliasingCheck<'a> {
    callee_pre: &'a crate::base_domain::BaseDomain,
    callee_post: &'a AbductiveDomain,
    formal_stack_addrs: &'a std::collections::HashSet<AbstractValue>,
}

fn callee_heap_contains(heap: &crate::base_memory::BaseMemory, addr: AbstractValue) -> bool {
    heap.get_edges(addr).is_some()
}

fn pvar_heap_path(pvar: &Pvar) -> HeapPath {
    HeapPath::Dereference(Box::new(HeapPath::Pvar(pvar.clone())))
}

fn extend_heap_path(parent: Option<&HeapPath>, access: &Access) -> Option<HeapPath> {
    let parent = parent?.clone();
    match access {
        Access::FieldAccess(field) => Some(HeapPath::FieldAccess(field.clone(), Box::new(parent))),
        Access::Dereference => Some(HeapPath::Dereference(Box::new(parent))),
        Access::ArrayAccess(_, _) => None,
    }
}

fn heap_path_sort_key(path: &HeapPath) -> String {
    format!("{path}")
}

fn canonicalize_alias_groups(mut groups: Vec<Vec<HeapPath>>) -> Vec<Vec<HeapPath>> {
    for group in &mut groups {
        group.sort_by_key(heap_path_sort_key);
        group.dedup_by(|left, right| heap_path_sort_key(left) == heap_path_sort_key(right));
    }
    groups.retain(|group| group.len() > 1);
    groups.sort_by_key(|group| {
        group
            .iter()
            .map(heap_path_sort_key)
            .collect::<Vec<_>>()
            .join(" = ")
    });
    groups.dedup_by(|left, right| {
        left.iter().map(heap_path_sort_key).collect::<Vec<_>>()
            == right.iter().map(heap_path_sort_key).collect::<Vec<_>>()
    });
    groups
}

fn record_conflicting_paths(
    seen: &HashMap<AbstractValue, Option<HeapPath>>,
    caller_repr: AbstractValue,
    callee_addr: AbstractValue,
    current_path: Option<&HeapPath>,
    alias_groups: &mut HashMap<AbstractValue, HashSet<HeapPath>>,
    first_conflict: &mut Option<AliasConflict>,
) -> Result<(), AliasConflict> {
    for (&other_callee_addr, other_path) in seen {
        if other_callee_addr == callee_addr {
            continue;
        }
        let conflict = AliasConflict {
            caller_addr: caller_repr,
            callee_addr,
            other_callee_addr,
        };
        first_conflict.get_or_insert(conflict);
        match (current_path, other_path.as_ref()) {
            (Some(current_path), Some(other_path)) => {
                let group = alias_groups.entry(caller_repr).or_default();
                group.insert(current_path.clone());
                group.insert(other_path.clone());
            }
            _ => return Err(conflict),
        }
    }
    Ok(())
}

/// Detect when multiple distinct callee heap roots collapse onto the same
/// caller representative.
///
/// Cross-ref: OCaml `PulseInterproc.visit` records alias groups in
/// `call_state.aliases` and `apply_summary` rejects the unspecialized summary
/// with `AliasingWithAllAliases`. The Rust port keeps the check smaller: once
/// two distinct callee addresses that both own heap structure in the same
/// summary phase (pre or post) map to one caller representative, we reject the
/// unspecialized pre/post and let higher-level specialization logic handle the
/// aliased call.
///
/// The tracking is keyed by the caller representative rather than traversal
/// order so the result stays deterministic even when the initial substitution
/// is explored in a different order.
fn find_aliasing_contradiction(
    aliasing: &AliasingCheck<'_>,
    caller_alias_roots: &mut HashMap<AbstractValue, CallerAliasRoots>,
    callee_heap_paths: &HashMap<AbstractValue, Option<HeapPath>>,
    alias_groups: &mut HashMap<AbstractValue, HashSet<HeapPath>>,
    first_conflict: &mut Option<AliasConflict>,
    caller_repr: AbstractValue,
    callee_addr: AbstractValue,
) -> Result<(), AliasConflict> {
    if aliasing.formal_stack_addrs.contains(&callee_addr) {
        return Ok(());
    }

    let in_pre = callee_heap_contains(&aliasing.callee_pre.heap, callee_addr);
    let in_post = callee_heap_contains(&aliasing.callee_post.post.heap, callee_addr);
    if !in_pre && !in_post {
        // OCaml only raises the alias contradiction when the aliased callee
        // values are meaningful heap roots in the summary; otherwise equality
        // can be handled as normal caller-side aliasing.
        return Ok(());
    }

    let seen = caller_alias_roots.entry(caller_repr).or_default();
    let current_path = callee_heap_paths
        .get(&callee_addr)
        .and_then(|path| path.as_ref());

    if in_pre {
        record_conflicting_paths(
            &seen.pre_backed,
            caller_repr,
            callee_addr,
            current_path,
            alias_groups,
            first_conflict,
        )?;
        seen.pre_backed
            .entry(callee_addr)
            .or_insert_with(|| current_path.cloned());
    }

    if in_post {
        record_conflicting_paths(
            &seen.post_backed,
            caller_repr,
            callee_addr,
            current_path,
            alias_groups,
            first_conflict,
        )?;
        seen.post_backed
            .entry(callee_addr)
            .or_insert_with(|| current_path.cloned());
    }

    Ok(())
}

fn materialize_pre(
    callee_pre: &crate::base_domain::BaseDomain,
    callee_post: &AbductiveDomain,
    ctx: MaterializePreContext<'_>,
) -> PreMaterializeResult {
    let MaterializePreContext {
        formal_stack_addrs,
        value_actual_formal_stack_addrs,
        subst,
        callee_heap_paths,
        caller_state,
        loc,
    } = ctx;
    let mut visited = std::collections::HashSet::new();
    let mut caller_alias_roots: HashMap<AbstractValue, CallerAliasRoots> = HashMap::new();
    let mut alias_groups: HashMap<AbstractValue, HashSet<HeapPath>> = HashMap::new();
    let mut first_alias_conflict = None;
    let aliasing = AliasingCheck {
        callee_pre,
        callee_post,
        formal_stack_addrs,
    };
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
        let caller_repr = caller_state.get_var_repr(caller_addr);
        if let Err(conflict) = find_aliasing_contradiction(
            &aliasing,
            &mut caller_alias_roots,
            callee_heap_paths,
            &mut alias_groups,
            &mut first_alias_conflict,
            caller_repr,
            callee_addr,
        ) {
            return PreMaterializeResult::AliasingContradiction {
                caller_addr: conflict.caller_addr,
                callee_addr: conflict.callee_addr,
                other_callee_addr: conflict.other_callee_addr,
                alias_groups: None,
            };
        }

        let callee_pre_attrs = callee_pre.attrs.get(&callee_addr);
        // Cross-ref: OCaml `PulseInterproc.materialize_pre` must honor
        // `MustBeValid` obligations on leaf pre values as well as values with
        // outgoing pre-edges. The old Rust behavior only checked values with
        // non-empty pre cells, which a synthetic write-time read happened to
        // mask before we removed that incorrect edge creation.
        let is_formal_stack = formal_stack_addrs.contains(&callee_addr);
        let needs_must_be_valid = callee_pre_attrs
            .and_then(|attrs| attrs.get_must_be_valid())
            .is_some()
            || callee_post.is_must_be_valid(callee_addr);
        if !is_formal_stack && needs_must_be_valid {
            if let Err(inv_info) = caller_state.check_valid(caller_addr) {
                log::debug!(
                    "    [materialize_pre] PRE-VIOLATION: callee={callee_addr} caller={caller_addr}"
                );
                if first_error.is_none() {
                    let access_history = caller_state
                        .history_of_value(caller_addr)
                        .unwrap_or_else(|| ValueHistory::assignment(loc.clone()));
                    first_error = Some(Box::new(
                        crate::diagnostic::Diagnostic::AccessToInvalidAddress {
                            addr: caller_addr,
                            invalidation: inv_info.0,
                            access_location: loc.clone(),
                            access_history,
                            invalidation_history: inv_info.1,
                        },
                    ));
                }
                // Skip exploring edges from this invalid address
                // (OCaml line 621-623)
                continue;
            }
            caller_state.mark_must_be_valid_at(caller_addr, loc);
        }

        if let Some(edges) = callee_pre.heap.get_edges(callee_addr) {
            if callee_pre_attrs
                .and_then(|attrs| attrs.get_must_be_initialized())
                .is_some()
                && !is_formal_stack
            {
                let _ = caller_state.record_read_access_at(caller_addr, loc);
            }

            for (access, callee_target) in edges.iter() {
                let caller_access = translate_access(subst, access, caller_state);
                let caller_target = if value_actual_formal_stack_addrs.contains(&callee_addr) {
                    // Cross-ref: OCaml `materialize_pre_from_actual` starts
                    // from the dereferenced formal value for non-struct/value
                    // actuals; it does not replay the callee formal stack
                    // bookkeeping cell itself onto the caller.
                    if let Some(existing) = caller_state
                        .post
                        .heap
                        .find_edge(caller_addr, &caller_access)
                    {
                        existing
                    } else {
                        resolve_mut(subst, *callee_target)
                    }
                } else {
                    // Cross-ref: OCaml `PulseInterproc.materialize_pre_from_address`
                    // uses `Memory.eval_edge`, which abduces missing pre-edges
                    // into the caller state while traversing the callee pre.
                    // A bare fresh substitution target is not enough here:
                    // callers must remember the imported pre-cell so later
                    // summary export can distinguish the old pointee from the
                    // refreshed post pointee in recursive/value-actual cases
                    // such as `invoke_itself_bad(f, i - 1)`.
                    caller_state.read_heap(caller_addr, caller_access)
                };

                subst.entry(*callee_target).or_insert(caller_target);
                let parent_path = callee_heap_paths
                    .get(&callee_addr)
                    .and_then(|path| path.as_ref());
                let child_path = extend_heap_path(parent_path, access);
                callee_heap_paths
                    .entry(*callee_target)
                    .or_insert(child_path);

                if !visited.contains(callee_target) {
                    worklist.push(*callee_target);
                }
            }
        }
    }

    if !alias_groups.is_empty() {
        let alias_groups = canonicalize_alias_groups(
            alias_groups
                .into_values()
                .map(|group| group.into_iter().collect())
                .collect(),
        );
        if !alias_groups.is_empty() {
            let conflict = first_alias_conflict.expect("alias groups should have a first conflict");
            return PreMaterializeResult::AliasingContradiction {
                caller_addr: conflict.caller_addr,
                callee_addr: conflict.callee_addr,
                other_callee_addr: conflict.other_callee_addr,
                alias_groups: Some(alias_groups),
            };
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
    callee_heap_paths: &mut HashMap<AbstractValue, Option<HeapPath>>,
    caller_state: &mut AbductiveDomain,
) {
    let mut map_globals = |stack: &crate::base_stack::BaseStack| {
        for (var, addr) in stack.iter() {
            if !var.is_global() {
                continue;
            }
            let caller_addr = caller_state.eval_var(var);
            subst.entry(*addr).or_insert(caller_addr);
            if let Some(pvar) = var.get_pvar() {
                callee_heap_paths
                    .entry(*addr)
                    .or_insert_with(|| Some(pvar_heap_path(pvar)));
            }
        }
    };

    map_globals(&pre_post.pre.stack);
    map_globals(&pre_post.post.post.stack);
}

fn extend_subst_with_callee_array_indices(
    pre_post: &PrePost,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    callee_heap_paths: &mut HashMap<AbstractValue, Option<HeapPath>>,
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
                callee_heap_paths.entry(*callee_idx).or_insert(None);
            }
        }
    };

    map_indices(&pre_post.pre.heap);
    map_indices(&pre_post.post.post.heap);
}

fn import_callee_pre_attributes(
    callee_pre: &crate::base_domain::BaseDomain,
    formal_stack_addrs: &std::collections::HashSet<AbstractValue>,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
) {
    for (callee_addr, attrs) in callee_pre.attrs.iter() {
        if formal_stack_addrs.contains(callee_addr) {
            continue;
        }

        let Some(caller_addr) = subst.get(callee_addr).copied() else {
            continue;
        };
        let caller_addr = caller_state.get_var_repr(caller_addr);

        // Cross-ref: OCaml `AddressAttributes.abduce_one` only keeps imported
        // pre attrs on addresses that already belong to the caller pre-state.
        if caller_state.pre.heap.get_edges(caller_addr).is_none() {
            continue;
        }

        for attr in attrs.iter() {
            caller_state
                .pre
                .attrs
                .add_one(caller_addr, translate_attribute(subst, attr));
        }
    }
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
    callee_post: &AbductiveDomain,
    subst: &mut HashMap<AbstractValue, AbstractValue>,
    caller_state: &mut AbductiveDomain,
    mut stack_allocated_before_call: std::collections::HashSet<AbstractValue>,
    mut heap_allocated_before_call: std::collections::HashSet<AbstractValue>,
    loc: &Location,
) -> TranslateFormulaResult {
    fn imported_potential_invalid_access_diagnostic(
        addr: AbstractValue,
        loc: &Location,
        caller_state: &AbductiveDomain,
    ) -> Diagnostic {
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let access_history = caller_state
            .history_of_value(addr)
            .unwrap_or_else(|| ValueHistory::assignment(loc.clone()));
        let invalidation_history = access_history.append_event(HistoryEvent::Invalidated {
            invalidation: invalidation.clone(),
            location: loc.clone(),
        });
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_location: loc.clone(),
            access_history,
            invalidation_history,
        }
    }

    fn apply_imported_formula_result(
        caller_state: &mut AbductiveDomain,
        imported_must_be_valid: &mut std::collections::HashSet<AbstractValue>,
        stack_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
        heap_allocated_before_call: &mut std::collections::HashSet<AbstractValue>,
        result: crate::sat_unsat::SatUnsat<Vec<crate::formula::NewEq>>,
        loc: &Location,
    ) -> TranslateFormulaResult {
        match caller_state.apply_formula_result_for_summary_import(
            result,
            imported_must_be_valid,
            stack_allocated_before_call,
            heap_allocated_before_call,
        ) {
            crate::sat_unsat::SatUnsat::Unsat => TranslateFormulaResult::Unsat,
            crate::sat_unsat::SatUnsat::Sat(ImportedFormulaEffect::Sat) => {
                TranslateFormulaResult::Sat
            }
            crate::sat_unsat::SatUnsat::Sat(ImportedFormulaEffect::PotentialInvalidAccess(
                addr,
            )) => TranslateFormulaResult::PotentialInvalidAccess(Box::new(
                imported_potential_invalid_access_diagnostic(addr, loc, caller_state),
            )),
        }
    }

    let phi = &callee_formula.phi();
    let mut ensure_formula_var = |callee_v: AbstractValue| {
        subst
            .entry(callee_v)
            .or_insert_with(|| callee_v.mk_fresh_same_kind());
    };
    for &callee_v in &callee_post.must_be_valid {
        ensure_formula_var(callee_v);
    }

    // Cross-ref: OCaml `PulseFormula.and_callee_formula` uses the same
    // substitution builder for both remembered conditions and the rest of the
    // callee phi. Any previously unseen callee value gets a fresh caller value
    // of the same kind instead of being resolved away eagerly.
    for atom in callee_formula.conditions().keys() {
        for v in atom.all_vars() {
            ensure_formula_var(v);
        }
    }
    for (&callee_v, lin) in &phi.linear_eqs {
        ensure_formula_var(callee_v);
        for dep in lin.vars.keys() {
            ensure_formula_var(*dep);
        }
    }
    for atom in &phi.atoms {
        for v in atom.all_vars() {
            ensure_formula_var(v);
        }
    }
    for &callee_v in &phi.is_int_vars {
        ensure_formula_var(callee_v);
    }
    for (key, ret) in phi.iter_fn_app_eqs() {
        ensure_formula_var(*ret);
        for actual in &key.actuals {
            let crate::formula::phi::FnAppActual::Var(v) = actual else {
                continue;
            };
            ensure_formula_var(*v);
        }
    }

    let callee_stack_addrs: std::collections::HashSet<_> = callee_post
        .pre
        .stack
        .iter()
        .chain(callee_post.post.stack.iter())
        .map(|(_, addr)| callee_post.path_condition.get_var_repr(*addr))
        .collect();

    let mut imported_must_be_valid: std::collections::HashSet<_> = callee_post
        .must_be_valid
        .iter()
        .copied()
        .map(|callee_v| callee_post.path_condition.get_var_repr(callee_v))
        // Callee stack slots (including formal parameter cells) are local
        // bookkeeping, not caller-space addresses. Propagating their
        // MustBeValid obligations to actual values turns scalar facts such as
        // `x == 0` into bogus imported invalid-access reports at call sites.
        .filter(|callee_v| !callee_stack_addrs.contains(callee_v))
        .filter_map(|callee_v| subst.get(&callee_v).copied())
        .map(|caller_v| caller_state.path_condition.get_var_repr(caller_v))
        .collect();

    // Cross-ref: OCaml `PulseFormula.and_callee_formula` conjoins remembered
    // conditions before importing the rest of the callee phi so caller-visible
    // guards do not get trivialized by freshly imported equalities.
    for (atom, depth) in callee_formula.conditions() {
        let translated = atom.translate(|v| *subst.get(&v).expect("formula subst"));
        log::debug!("    condition[{depth}]: {atom} → {translated}");
        let result = caller_state
            .path_condition
            .and_condition_direct(translated, depth + 1);
        match apply_imported_formula_result(
            caller_state,
            &mut imported_must_be_valid,
            &mut stack_allocated_before_call,
            &mut heap_allocated_before_call,
            result,
            loc,
        ) {
            TranslateFormulaResult::Sat => {}
            TranslateFormulaResult::Unsat => {
                log::debug!("    → UNSAT!");
                return TranslateFormulaResult::Unsat;
            }
            TranslateFormulaResult::PotentialInvalidAccess(diag) => {
                return TranslateFormulaResult::PotentialInvalidAccess(diag);
            }
        }
    }

    // Translate linear equations: for each callee_v = lin_expr,
    // translate all variables in the linear expression to caller space
    for (&callee_v, lin) in &phi.linear_eqs {
        let caller_v = *subst.get(&callee_v).expect("formula subst");
        // Check if it's a constant
        if let Some(q) = lin.get_as_const() {
            let c = *q.numer() / *q.denom();
            let result = caller_state.path_condition.and_equal_const(caller_v, c);
            match apply_imported_formula_result(
                caller_state,
                &mut imported_must_be_valid,
                &mut stack_allocated_before_call,
                &mut heap_allocated_before_call,
                result,
                loc,
            ) {
                TranslateFormulaResult::Sat => {}
                TranslateFormulaResult::Unsat => return TranslateFormulaResult::Unsat,
                TranslateFormulaResult::PotentialInvalidAccess(diag) => {
                    return TranslateFormulaResult::PotentialInvalidAccess(diag);
                }
            }
            continue;
        }
        // Check if it's a single variable
        if let Some(callee_other) = lin.get_as_var() {
            let caller_other = *subst.get(&callee_other).expect("formula subst");
            let result = caller_state
                .path_condition
                .and_equal_vars(caller_v, caller_other);
            match apply_imported_formula_result(
                caller_state,
                &mut imported_must_be_valid,
                &mut stack_allocated_before_call,
                &mut heap_allocated_before_call,
                result,
                loc,
            ) {
                TranslateFormulaResult::Sat => {}
                TranslateFormulaResult::Unsat => return TranslateFormulaResult::Unsat,
                TranslateFormulaResult::PotentialInvalidAccess(diag) => {
                    return TranslateFormulaResult::PotentialInvalidAccess(diag);
                }
            }
            continue;
        }
        // For more complex linear expressions, translate if all vars are in subst
        let all_vars_mapped = lin.vars.keys().all(|v| subst.contains_key(v));
        if all_vars_mapped {
            let translated = lin.translate(|v| *subst.get(&v).expect("formula subst"));
            let result = caller_state
                .path_condition
                .and_equal_linear(caller_v, translated);
            match apply_imported_formula_result(
                caller_state,
                &mut imported_must_be_valid,
                &mut stack_allocated_before_call,
                &mut heap_allocated_before_call,
                result,
                loc,
            ) {
                TranslateFormulaResult::Sat => {}
                TranslateFormulaResult::Unsat => return TranslateFormulaResult::Unsat,
                TranslateFormulaResult::PotentialInvalidAccess(diag) => {
                    return TranslateFormulaResult::PotentialInvalidAccess(diag);
                }
            }
        }
    }

    log::debug!(
        "  [translate_formula] atoms={}, conditions={}, subst_size={}, extended_subst_size={}",
        phi.atoms.len(),
        callee_formula.conditions().len(),
        subst.len(),
        subst.len()
    );
    for atom in &phi.atoms {
        let all_mapped = atom.all_vars().iter().all(|v| subst.contains_key(v));
        if !all_mapped {
            continue;
        }
        let translated = atom.translate(|v| subst.get(&v).copied().unwrap_or(v));
        log::debug!("    atom: {atom} → {translated}");
        let result = caller_state.path_condition.and_atom_direct(translated);
        match apply_imported_formula_result(
            caller_state,
            &mut imported_must_be_valid,
            &mut stack_allocated_before_call,
            &mut heap_allocated_before_call,
            result,
            loc,
        ) {
            TranslateFormulaResult::Sat => {}
            TranslateFormulaResult::Unsat => {
                log::debug!("    → UNSAT!");
                return TranslateFormulaResult::Unsat;
            }
            TranslateFormulaResult::PotentialInvalidAccess(diag) => {
                return TranslateFormulaResult::PotentialInvalidAccess(diag);
            }
        }
    }

    // Translate remembered pure-function applications so imported conditions
    // on their results stay connected to caller-visible actuals.
    // Cross-ref: OCaml PulseFormula.and_callee_formula folds substitutions
    // through the whole formula, including function-application terms.
    for (key, ret) in phi.iter_fn_app_eqs() {
        let Some(&caller_ret) = subst.get(ret) else {
            continue;
        };
        let mut caller_actuals = Vec::with_capacity(key.actuals.len());
        let mut all_mapped = true;
        for actual in &key.actuals {
            match actual {
                crate::formula::phi::FnAppActual::Const(c) => {
                    let fresh = crate::abstract_value::AbstractValue::mk_fresh();
                    if caller_state.and_equal_const(fresh, *c).is_unsat() {
                        return TranslateFormulaResult::Unsat;
                    }
                    caller_actuals.push(fresh);
                }
                crate::formula::phi::FnAppActual::Var(v) => {
                    let Some(&caller_actual) = subst.get(v) else {
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
            return TranslateFormulaResult::Unsat;
        }
    }

    // Cross-ref: OCaml `PulseFormula.and_callee_formula` also imports `IsInt`
    // facts, not just conditions / equalities / function applications. The
    // caller needs those facts for integer-return recursive summaries such as
    // `specialization.c:add_more_bad`.
    for &callee_v in &phi.is_int_vars {
        let Some(&caller_v) = subst.get(&callee_v) else {
            continue;
        };
        caller_state.path_condition.and_is_int(caller_v);
    }
    TranslateFormulaResult::Sat
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
    use crate::value_history::ValueHistory;
    use sil::fieldname::Fieldname;
    use sil::ident::IdentName;
    use sil::int_lit::IntLit;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::var::Var;

    fn invalidation_history(invalidation: &crate::invalidation::Invalidation) -> ValueHistory {
        ValueHistory::invalidated(invalidation.clone(), Location::dummy())
    }

    fn dummy_invalid_access_diagnostic(
        addr: AbstractValue,
        invalidation: crate::invalidation::Invalidation,
    ) -> Diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation: invalidation.clone(),
            access_location: Location::dummy(),
            access_history: ValueHistory::assignment(Location::dummy()),
            invalidation_history: invalidation_history(&invalidation),
        }
    }

    fn add_local_load(pdesc: &mut Procdesc, pvar: Pvar, loc: Location) {
        let load_node = pdesc.add_node(
            sil::procdesc::NodeKind::StmtNode(sil::procdesc::StmtNodeKind::MethodBody),
            vec![sil::instr::Instr::Load {
                id: Ident::create_none(),
                e: Exp::Lvar(pvar),
                typ: Typ::void(),
                loc: loc.clone(),
            }],
            loc,
        );
        pdesc.set_succs(0, vec![load_node]);
        pdesc.set_succs(load_node, vec![1]);
    }

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
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
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
    fn test_apply_summary_imports_is_int_for_result_value() {
        let callee_pname = Procname::c_from_string("callee");
        let callee_pdesc = Procdesc::new(
            callee_pname,
            Typ::int(sil::typ::IKind::IInt),
            Location::dummy(),
        );
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let ret_val = AbstractValue::mk_fresh();
        callee_state.path_condition.and_is_int(ret_val);

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![],
            result: Some(ret_val),
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(
            caller_pname,
            Typ::int(sil::typ::IKind::IInt),
            Location::dummy(),
        );
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

        let Some(ExecutionDomain::ContinueProgram(state)) =
            results.iter().find(|r| r.is_continue())
        else {
            panic!("expected a continuing result");
        };
        let ret_addr = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return value should be bound");
        assert!(
            state.path_condition.phi().is_marked_int(ret_addr),
            "callee is_int facts should be imported onto the caller result value"
        );
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
    fn test_apply_summary_rejects_aliasing_disjoint_formals() {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![
            (
                Mangled::from_string("p"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
            (
                Mangled::from_string("q"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
        ];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let p_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname.clone());
        let q_pvar = Pvar::mk(Mangled::from_string("q"), callee_pname);
        let p_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar.clone())))
            .unwrap();
        let q_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(q_pvar.clone())))
            .unwrap();
        let p_val = callee_state.read_heap(p_addr, Access::Dereference);
        let q_val = callee_state.read_heap(q_addr, Access::Dereference);
        callee_state.write_heap(p_val, Access::Dereference, AbstractValue::mk_fresh());
        callee_state.write_heap(q_val, Access::Dereference, AbstractValue::mk_fresh());

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(p_pvar, p_addr), (q_pvar, q_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let shared_actual = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let shared_addr = AbstractValue::mk_fresh();
        caller_state.post.stack.add(
            Var::ProgramVar(Box::new(shared_actual.clone())),
            shared_addr,
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[
                (Exp::Lvar(shared_actual.clone()), Typ::mk_ptr(Typ::void())),
                (Exp::Lvar(shared_actual), Typ::mk_ptr(Typ::void())),
            ],
            &Location::dummy(),
            caller_state,
        );

        assert!(
            results.is_empty(),
            "unspecialized summary should be rejected when aliased actuals collapse disjoint callee formals"
        );
    }

    #[test]
    fn test_apply_summary_reports_heap_path_alias_specialization() {
        let node_struct = sil::typ::TypeName::CStruct(QualifiedCppName::from_string("node"));
        let next_field = Fieldname::make(node_struct, "next");

        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(
            Mangled::from_string("p"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let p_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname);
        let p_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar.clone())))
            .unwrap();
        let p_val = callee_state.read_heap(p_addr, Access::Dereference);
        // Cross-ref: OCaml records aliasing while materializing the callee
        // precondition. Build the cycle through PRE edges, not only POST
        // writes, so the Rust contradiction follows the same path.
        let next_val = callee_state.read_heap(p_val, Access::FieldAccess(next_field.clone()));
        callee_state.read_heap(next_val, Access::Dereference);

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(p_pvar.clone(), p_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let x_addr = AbstractValue::mk_fresh();
        caller_state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(x_pvar.clone())), x_addr);
        caller_state
            .post
            .heap
            .add_edge(x_addr, Access::FieldAccess(next_field.clone()), x_addr);

        let outcome = apply_summary_with_aliasing(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[(Exp::Lvar(x_pvar), Typ::mk_ptr(Typ::void()))],
            &Location::dummy(),
            caller_state,
        );

        assert!(
            outcome.results.is_empty(),
            "unspecialized summary should be rejected, got {:?}",
            outcome.results
        );
        let alias_groups = outcome
            .alias_specialization
            .expect("supported heap-path alias contradiction should request specialization");
        let alias_groups: Vec<Vec<String>> = alias_groups
            .into_iter()
            .map(|group| group.into_iter().map(|path| format!("{path}")).collect())
            .collect();
        assert_eq!(
            alias_groups,
            vec![vec![
                format!("{}", pvar_heap_path(&p_pvar)),
                format!(
                    "{}",
                    HeapPath::FieldAccess(next_field.clone(), Box::new(pvar_heap_path(&p_pvar)))
                )
            ]]
        );
    }

    #[test]
    fn test_apply_summary_allows_aliased_actuals_when_extra_formal_is_unused() {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![
            (
                Mangled::from_string("p"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
            (
                Mangled::from_string("q"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
        ];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let p_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname.clone());
        let q_pvar = Pvar::mk(Mangled::from_string("q"), callee_pname);
        let p_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar.clone())))
            .unwrap();
        let q_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(q_pvar.clone())))
            .unwrap();
        let p_val = callee_state.read_heap(p_addr, Access::Dereference);
        let written = AbstractValue::mk_fresh();
        callee_state.and_equal_const(written, 42);
        callee_state.write_heap(p_val, Access::Dereference, written);

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(p_pvar, p_addr), (q_pvar, q_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let shared_actual = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let shared_addr = AbstractValue::mk_fresh();
        caller_state.post.stack.add(
            Var::ProgramVar(Box::new(shared_actual.clone())),
            shared_addr,
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[
                (Exp::Lvar(shared_actual.clone()), Typ::mk_ptr(Typ::void())),
                (Exp::Lvar(shared_actual), Typ::mk_ptr(Typ::void())),
            ],
            &Location::dummy(),
            caller_state,
        );

        if let Some(ExecutionDomain::ContinueProgram(s)) = results.iter().find(|r| r.is_continue())
        {
            assert!(
                s.post
                    .heap
                    .find_edge(shared_addr, &Access::Dereference)
                    .is_some(),
                "unused aliased extra formals should not make the summary inapplicable"
            );
        } else {
            panic!("summary should still apply when only one aliased formal is heap-backed");
        }
    }

    #[test]
    fn test_apply_summary_does_not_replay_formal_stack_cell_onto_value_actual() {
        let callee_pname = Procname::c_from_string("id");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("i"), Typ::void(), Default::default())];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let formal_pvar = Pvar::mk(Mangled::from_string("i"), callee_pname);
        let formal_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val = callee_state.read_heap(formal_addr, Access::Dereference);
        let result_addr = AbstractValue::mk_fresh();
        callee_state
            .post
            .heap
            .add_edge(result_addr, Access::Dereference, formal_val);
        callee_state.initialize(result_addr);
        callee_state
            .post
            .attrs
            .mark_written_to(result_addr, 0, Location::dummy());

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: Some(result_addr),
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar)))
            .unwrap();
        let caller_formal_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        let actual_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id_with_history(
            &actual_id,
            crate::value_history::ValueWithHistory::new(
                caller_formal_val,
                ValueHistory::assignment(Location::dummy()),
            ),
            &mut caller_state,
        );
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &[(Exp::Var(actual_id), Typ::void())],
            &Location::dummy(),
            caller_state,
        );

        let ExecutionDomain::ContinueProgram(state) = &results[0] else {
            panic!("expected continue result, got {results:?}");
        };
        assert!(
            state
                .post
                .heap
                .find_edge(caller_formal_val, &Access::Dereference)
                .is_none(),
            "callee formal bookkeeping should not become a caller self-edge"
        );
        let ret_addr = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return id should be written");
        assert_eq!(
            state.post.heap.find_edge(ret_addr, &Access::Dereference),
            Some(caller_formal_val),
            "return cell should point to the caller actual value"
        );
    }

    #[test]
    fn test_apply_summary_materializes_missing_nested_pre_edge_for_value_actual() {
        let callee_pname = Procname::c_from_string("invoke_itself_bad");
        let fun_ptr_typ = Typ::mk_ptr(Typ::mk(sil::typ::TypeDesc::Tfun(None)));
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("f"), fun_ptr_typ, Default::default())];
        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);

        let formal_pvar = Pvar::mk(Mangled::from_string("f"), callee_pname);
        let formal_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val = callee_state.read_heap(formal_addr, Access::Dereference);
        let old_pointee = callee_state.read_heap(formal_val, Access::Dereference);
        let new_pointee = AbstractValue::mk_fresh();
        callee_state.write_heap(formal_val, Access::Dereference, new_pointee);
        callee_state.initialize(formal_val);
        callee_state.initialize(old_pointee);
        callee_state
            .post
            .attrs
            .mark_written_to(old_pointee, 0, Location::dummy());

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(
            Mangled::from_string("cb"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("cb"), caller_pname);
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar)))
            .unwrap();
        let caller_fun_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        let actual_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id_with_history(
            &actual_id,
            crate::value_history::ValueWithHistory::new(
                caller_fun_val,
                ValueHistory::assignment(Location::dummy()),
            ),
            &mut caller_state,
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[(Exp::Var(actual_id), Typ::void())],
            &Location::dummy(),
            caller_state,
        );

        let ExecutionDomain::ContinueProgram(state) = &results[0] else {
            panic!("expected continue result, got {results:?}");
        };
        let pre_target = state
            .pre
            .heap
            .find_edge(caller_fun_val, &Access::Dereference)
            .expect("summary import should abduce the missing pre-edge on the value actual");
        let post_target = state
            .post
            .heap
            .find_edge(caller_fun_val, &Access::Dereference)
            .expect("summary import should keep the refreshed post-edge");
        assert_ne!(
            pre_target, post_target,
            "callee pre/post pointees should stay distinct after summary import"
        );
        let post_target_attrs = state
            .post
            .attrs
            .get(&pre_target)
            .expect("old pointee should remain visible in post attrs");
        assert!(
            post_target_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::WrittenTo(_, _))),
            "summary import should preserve caller-visible WrittenTo on the old pointee"
        );
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
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
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
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
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
    fn test_apply_summary_drops_plain_abort_program_at_callers() {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let diagnostic = dummy_invalid_access_diagnostic(
            AbstractValue::of_raw(1),
            crate::invalidation::Invalidation::CFree,
        );
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

        assert!(
            results.is_empty(),
            "plain callee-local AbortProgram paths should stop the caller path without republishing the callee diagnostic"
        );
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

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );
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
        callee_state.mark_must_be_valid_at(formal_val, &Location::dummy());
        assert!(
            callee_state.pre.attrs.get(&formal_val).is_some(),
            "callee pre attrs should include the pointee validity requirement"
        );
        if invalidate_formal {
            callee_state.invalidate(
                formal_val,
                crate::invalidation::Invalidation::CFree,
                ValueHistory::invalidated(
                    crate::invalidation::Invalidation::CFree,
                    Location::dummy(),
                ),
            );
        }

        let diagnostic =
            dummy_invalid_access_diagnostic(formal_val, crate::invalidation::Invalidation::CFree);
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

    fn mk_latent_null_invalid_access_pre_post() -> (PrePost, Diagnostic) {
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
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        callee_state.mark_must_be_valid(formal_val);
        callee_state.invalidate(
            formal_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(formal_val, invalidation);
        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::LatentInvalidAccess,
            diagnostic: Some(diagnostic.clone()),
        };

        (pre_post, diagnostic)
    }

    #[test]
    fn test_apply_summary_keeps_latent_abort_when_imported_condition_depends_on_caller() {
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

        // Cross-ref: OCaml `PulseFormula.and_callee_formula` imports remembered
        // conditions before the rest of the callee phi. The imported `*p == 4`
        // guard is therefore still caller-controlled here and must remain
        // latent until some caller proves `x == 4`.
        let [ExecutionDomain::LatentAbortProgram {
            diagnostic: found, ..
        }] = results.as_slice()
        else {
            panic!("expected caller-dependent imported condition to keep latent abort, got {results:?}");
        };
        assert_eq!(
            found.get_issue_type_id(),
            diagnostic.get_issue_type_id(),
            "latent abort kind should stay stable after caller-space translation"
        );
        assert_eq!(
            found.get_location(),
            &Location::dummy(),
            "dummy callsites should keep the translated latent abort at the dummy location"
        );
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

        let [ExecutionDomain::AbortProgram {
            diagnostic: found, ..
        }] = results.as_slice()
        else {
            panic!("expected latent abort to reify at the entry point, got {results:?}");
        };
        assert_eq!(
            found.get_issue_type_id(),
            diagnostic.get_issue_type_id(),
            "entry-point reification should preserve the invalid-access kind"
        );
        assert_eq!(
            found.get_location(),
            &Location::dummy(),
            "dummy entry-point callsites should keep the translated abort at the dummy location"
        );
    }

    #[test]
    fn test_apply_summary_translates_latent_abort_location_to_callsite() {
        let (pre_post, _diagnostic) = mk_latent_abort_pre_post();

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
        let callsite = Location {
            line: 99,
            col: 3,
            ..Location::dummy()
        };
        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &ret_id,
            &actuals,
            &callsite,
            caller_state,
        );

        let [ExecutionDomain::AbortProgram { diagnostic, .. }] = results.as_slice() else {
            panic!("expected a manifest abort translated to the callsite, got {results:?}");
        };
        match diagnostic.as_ref() {
            Diagnostic::AccessToInvalidAddress {
                access_location, ..
            } => {
                assert_eq!(
                    access_location, &callsite,
                    "latent abort diagnostics should be translated through the caller callsite"
                );
                assert_eq!(diagnostic.get_location(), &callsite);
            }
            other => panic!("expected invalid-access diagnostic, got {other:?}"),
        }
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
    fn test_apply_summary_imported_eq_zero_becomes_latent_invalid_access() {
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
        let _heap_target = callee_state.read_heap(formal_val, Access::Dereference);
        callee_state.mark_must_be_valid(formal_val);
        assert!(callee_state
            .path_condition
            .and_equal_const(formal_val, 0)
            .is_sat());

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar.clone())))
            .unwrap();
        let caller_formal_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        let _caller_heap_target = caller_state.read_heap(caller_formal_val, Access::Dereference);
        let actual_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id_with_history(
            &actual_id,
            crate::value_history::ValueWithHistory::new(
                caller_formal_val,
                ValueHistory::assignment(Location::dummy()),
            ),
            &mut caller_state,
        );
        let actuals = vec![(Exp::Var(actual_id), Typ::void())];

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &actuals,
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentInvalidAccess { state, diagnostic }]
                if matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress {
                        invalidation: crate::invalidation::Invalidation::ConstantDereference(_),
                        ..
                    }
                ) && matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress { addr, .. }
                        if state.check_valid(*addr).is_ok()
                )
        ));
    }

    #[test]
    fn test_apply_summary_reconstructs_exported_latent_invalid_access_without_diagnostic() {
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(Mangled::from_string("p"), Typ::void(), Default::default())];
        let access_loc = Location {
            line: 11,
            col: 7,
            ..Location::dummy()
        };
        let formal_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname.clone());
        add_local_load(&mut callee_pdesc, formal_pvar.clone(), access_loc.clone());

        let mut callee_state = AbductiveDomain::mk_initial(&callee_pdesc);
        let formal_addr = callee_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val = callee_state.read_heap(formal_addr, Access::Dereference);
        let _heap_target = callee_state.read_heap(formal_val, Access::Dereference);
        callee_state.mark_must_be_valid_at(formal_val, &access_loc);
        assert!(callee_state.and_equal_const(formal_val, 0).is_sat());

        let summary = crate::summary::PulseSummary::of_proc(
            &callee_pdesc,
            &[ExecutionDomain::ContinueProgram(callee_state)],
            vec![],
            false,
        );
        let pre_post = summary
            .pre_posts
            .iter()
            .find(|pp| pp.kind == crate::summary::PrePostKind::LatentInvalidAccess)
            .cloned()
            .expect("expected a latent invalid-access summary pre/post");
        assert!(
            pre_post.diagnostic.is_none(),
            "exported latent invalid-access summaries should omit the diagnostic payload"
        );

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar.clone())))
            .unwrap();
        let caller_formal_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        let _caller_heap_target = caller_state.read_heap(caller_formal_val, Access::Dereference);
        let actual_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id_with_history(
            &actual_id,
            crate::value_history::ValueWithHistory::new(
                caller_formal_val,
                ValueHistory::assignment(Location::dummy()),
            ),
            &mut caller_state,
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[(Exp::Var(actual_id), Typ::void())],
            &Location::dummy(),
            caller_state,
        );

        assert!(matches!(
            results.as_slice(),
            [ExecutionDomain::LatentInvalidAccess { diagnostic, .. }]
                if matches!(
                    diagnostic.as_ref(),
                    Diagnostic::AccessToInvalidAddress {
                        invalidation: crate::invalidation::Invalidation::ConstantDereference(_),
                        ..
                    }
                )
        ));
    }

    #[test]
    fn test_apply_summary_imports_pre_attrs_without_pre_edges() {
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
        callee_state.mark_must_be_valid_at(formal_val, &Location::dummy());
        assert!(
            callee_state.pre.attrs.get(&formal_val).is_some(),
            "callee pre attrs should include the pointee validity requirement"
        );

        let pre_post = PrePost {
            pre: callee_state.pre.clone(),
            post: callee_state,
            formals: vec![(formal_pvar, formal_addr)],
            result: None,
            kind: crate::summary::PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let caller_pname = Procname::c_from_string("caller");
        let mut caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        caller_pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let caller_formal_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let caller_formal_addr = caller_state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(caller_formal_pvar)))
            .unwrap();
        let caller_formal_val = caller_state.read_heap(caller_formal_addr, Access::Dereference);
        let actual_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id_with_history(
            &actual_id,
            crate::value_history::ValueWithHistory::new(
                caller_formal_val,
                ValueHistory::assignment(Location::dummy()),
            ),
            &mut caller_state,
        );

        let results = apply_summary(
            &caller_pdesc,
            &pre_post,
            &Ident::create_none(),
            &[(Exp::Var(actual_id), Typ::void())],
            &Location::dummy(),
            caller_state,
        );

        let ExecutionDomain::ContinueProgram(state) = &results[0] else {
            panic!("expected continue result, got {results:?}");
        };
        let imported_attrs = state
            .pre
            .attrs
            .get(&caller_formal_val)
            .expect("caller pointee should keep imported pre attrs");
        assert!(
            imported_attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _))),
            "callee pre attrs without outgoing pre edges should still import into caller pre"
        );
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
    fn test_apply_summary_keeps_direct_formal_null_invalid_access_latent() {
        let (pre_post, diagnostic) = mk_latent_null_invalid_access_pre_post();

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
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
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

    fn mk_leaf_latent_precondition_pre_post() -> PrePost {
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
        callee_state.mark_must_be_valid_at(p_val, &Location::dummy());
        assert!(
            callee_state
                .pre
                .attrs
                .get(&p_val)
                .is_some_and(|attrs| attrs.get_must_be_valid().is_some()),
            "leaf pre values should carry MustBeValid without needing outgoing pre edges"
        );
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
    fn test_apply_summary_keeps_latent_precondition_violation_when_flag_depends_on_caller() {
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

        assert!(
            matches!(
                results.as_slice(),
                [ExecutionDomain::LatentAbortProgram { .. }]
            ),
            "expected caller-dependent precondition violation to stay latent, got {results:?}"
        );
    }

    #[test]
    fn test_apply_summary_keeps_leaf_precondition_violation_latent_when_flag_depends_on_caller() {
        let pre_post = mk_leaf_latent_precondition_pre_post();

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

        assert!(
            matches!(
                results.as_slice(),
                [ExecutionDomain::LatentAbortProgram { .. }]
            ),
            "expected caller-dependent leaf precondition violation to stay latent, got {results:?}"
        );
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
