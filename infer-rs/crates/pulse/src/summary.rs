// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse summary: captures the result of analyzing a procedure.
//!
//! Mirrors OCaml's `PulseSummary.ml`.
//!
//! A summary records what the procedure does: its post-state (heap effects,
//! invalidations, path conditions) and any diagnostics found. Summaries
//! are applied at call sites to propagate effects interprocedurally.

use sil::int_lit::IntLit;
use sil::procdesc::Procdesc;
use sil::pvar::Pvar;
use sil::specialization::{HeapPath, PulseSpecialization};
use sil::var::Var;
use std::collections::{HashMap, HashSet};

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::formula::Operand;
use crate::value_history::HistoryEvent;

/// The summary of a Pulse analysis on a single procedure.
///
/// Captures the post-state and diagnostics. For interprocedural analysis,
/// callers use `formals_map` to connect their actuals to the summary's
/// formal parameter addresses.
#[derive(Clone, Debug)]
pub struct PulseSummary {
    /// The main (non-specialized) post-states at procedure exit.
    /// Matches OCaml's `PulseSummary.main.pre_post_list`.
    pub pre_posts: Vec<PrePost>,
    /// True when analysis had to drop some disjuncts while computing this
    /// summary (for example because of the disjunct bound or widening limit).
    /// Cross-ref: OCaml `PulseNonDisjunctiveDomain.Summary.has_dropped_disjuncts`.
    pub has_dropped_disjuncts: bool,
    /// Specialized summaries, each paired with the specialization used.
    /// Matches OCaml's `PulseSummary.specialized`.
    pub specialized: Vec<(PulseSpecialization, SpecializedSummary)>,
    /// Diagnostics found during analysis.
    pub diagnostics: Vec<Diagnostic>,
    /// True if the procedure never returns (all paths end in ExitProgram).
    /// Callers should transition to ExitProgram when calling this procedure.
    pub is_noreturn: bool,
    /// Heap paths that need dynamic type info for better precision.
    /// Populated when `__call_c_function_ptr` can't resolve a function pointer.
    /// Callers use this to decide whether to request specialization.
    /// Cross-ref: OCaml `Summary.heap_paths_that_need_dynamic_type_specialization`.
    pub needs_specialization: HashMap<HeapPath, AbstractValue>,
    /// True if the procedure had an empty body (extern declaration stub).
    /// Callers should havoc pointer-typed formals for unknown call handling.
    pub is_empty_body: bool,
    /// Types of formal parameters. Used to determine which arguments to
    /// havoc for unknown/empty-body calls (only pointer types get havoced).
    pub formal_types: Vec<sil::typ::Typ>,
}

#[derive(Clone, Debug)]
pub struct SpecializedSummary {
    pub pre_posts: Vec<PrePost>,
    /// Specialized latent aborts stay latent in the cached summary, like
    /// OCaml's `LatentAbortProgram {latent_issue; ...}`. Keep the issue
    /// sideband here so callers can still reify it when applying the summary.
    pub latent_abort_diagnostics: Vec<Option<Diagnostic>>,
    pub has_dropped_disjuncts: bool,
}

/// A single pre/post pair from the analysis.
///
/// Captures the final abstract state and the mapping from formal
/// parameter variables to their abstract addresses. This mapping
/// is needed to apply the summary at call sites.
/// Whether a PrePost represents a continuing or exiting execution path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrePostKind {
    ContinueProgram,
    ExitProgram,
    /// Error path: callee detected a manifest (definite) error.
    /// Kept in summaries so callers see all possible execution paths.
    /// Cross-ref: OCaml keeps AbortProgram in pre_post_list.
    AbortProgram,
    /// Latent error: the error depends on caller-provided values.
    /// Callers re-evaluate whether the error manifests in their context.
    /// Cross-ref: OCaml PulseExecutionDomain.ml LatentAbortProgram.
    LatentAbortProgram,
    /// Latent invalid access: the callee touched an address derived from the
    /// caller, but only callers can decide whether that address is invalid in
    /// their own context.
    ///
    /// Cross-ref: OCaml PulseExecutionDomain.ml LatentInvalidAccess.
    LatentInvalidAccess,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrePost {
    /// The pre-condition: what the procedure read from its inputs.
    /// Used during biabduction to match callee expectations against caller state.
    pub pre: crate::base_domain::BaseDomain,
    /// The final abstract state (post-condition).
    pub post: AbductiveDomain,
    /// Map from formal parameter Pvar to the abstract value assigned
    /// to it at procedure entry. Used to connect caller actuals to
    /// callee formals during summary application.
    pub formals: Vec<(Pvar, AbstractValue)>,
    /// The abstract value representing the return value, if the procedure
    /// returned a value (stored via `ret` in Call instructions).
    pub result: Option<AbstractValue>,
    /// Whether this path continued, exited, or aborted.
    pub kind: PrePostKind,
    /// Diagnostic for AbortProgram paths. None for Continue/Exit.
    pub diagnostic: Option<Diagnostic>,
}

pub(crate) struct StoppedStateSummary {
    pub(crate) state: AbductiveDomain,
    pub(crate) potential_invalid_access: Option<Diagnostic>,
}

struct NormalizedSummaryInfo {
    leaks: Vec<Diagnostic>,
    summary_eq_zero_must_be_valid: std::collections::HashSet<AbstractValue>,
}

struct PotentialInvalidAccessSummaryCandidate {
    diagnostic: Diagnostic,
    recovered_from_summary_eq_zero: bool,
}

fn build_pre_post(
    pdesc: &Procdesc,
    astate: AbductiveDomain,
    kind: PrePostKind,
    diagnostic: Option<Diagnostic>,
) -> PrePost {
    let formals: Vec<(Pvar, AbstractValue)> = pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar.clone()));
            astate.post.stack.find(&var).map(|addr| (pvar, addr))
        })
        .collect();

    let result = find_return_value(&astate, pdesc);
    let pre = astate.pre.clone();

    PrePost {
        pre,
        post: astate,
        formals,
        result,
        kind,
        diagnostic,
    }
}

pub(crate) fn abort_is_manifest(pdesc: &Procdesc, astate: &AbductiveDomain) -> bool {
    let mut pp = build_pre_post(pdesc, astate.clone(), PrePostKind::AbortProgram, None);
    let _ = pp.normalize();
    pre_post_is_manifest(pdesc, &pp)
}

pub(crate) fn classify_abort_kind(
    pdesc: &Procdesc,
    astate: &AbductiveDomain,
    diagnostic: &Diagnostic,
) -> PrePostKind {
    let mut pp = build_pre_post(
        pdesc,
        astate.clone(),
        PrePostKind::AbortProgram,
        Some(diagnostic.clone()),
    );
    let _ = pp.normalize();
    classify_non_exit_abort_pre_post(pdesc, &mut pp);
    pp.kind
}

impl PrePost {
    /// Canonicalize the exported state to the current formula representatives
    /// before summary filtering.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` first calls
    /// `canonicalize`, then restores formals and discards unreachable state.
    fn canonicalize_for_summary(&mut self) {
        self.post.canonicalize_with_current_path_condition();
        for (_formal, addr) in &mut self.formals {
            *addr = self.post.path_condition.get_var_repr(*addr);
        }
        if let Some(result) = &mut self.result {
            *result = self.post.path_condition.get_var_repr(*result);
        }
    }

    /// Restore formal/global/return variable views in the post-state before
    /// summary filtering.
    ///
    /// Mirrors OCaml `restore_formals_for_summary`: when a procedure mutates a
    /// formal variable locally (for example, advancing a loop cursor), callers
    /// should still see the original formal view from the pre-state rather than
    /// the callee's final local cursor position.
    fn restore_formals_for_summary(&mut self) {
        use std::collections::HashSet;

        let mut filtered_post_stack = crate::base_stack::BaseStack::empty();
        for (var, addr) in self.post.post.stack.iter() {
            if var.is_global() || var.is_return() {
                filtered_post_stack.add(var.clone(), *addr);
            }
        }
        self.post.post.stack = filtered_post_stack;

        let pre_bindings: Vec<_> = self
            .pre
            .stack
            .iter()
            .map(|(var, addr)| (var.clone(), *addr))
            .collect();

        for (var, addr) in pre_bindings {
            self.post.post.stack.add(var.clone(), addr);
            self.restore_pre_var_value(
                addr,
                var.is_global() || var.is_return(),
                &mut HashSet::new(),
            );
        }
    }

    fn restore_pre_var_value(
        &mut self,
        addr: AbstractValue,
        is_value_visible_outside: bool,
        visited: &mut std::collections::HashSet<AbstractValue>,
    ) {
        if !visited.insert(addr) {
            return;
        }

        let Some(pre_edges) = self.pre.heap.get_edges(addr).cloned() else {
            if !is_value_visible_outside {
                self.post.post.heap.remove(addr);
            }
            return;
        };

        let post_has_edge =
            |post: &crate::base_memory::BaseMemory, src: AbstractValue, access: &Access| {
                post.find_edge(src, access).is_some()
            };

        for (access, target) in pre_edges.iter() {
            match access {
                Access::Dereference => {
                    if is_value_visible_outside && post_has_edge(&self.post.post.heap, addr, access)
                    {
                        continue;
                    }
                    self.post.post.heap.add_edge(addr, access.clone(), *target);
                }
                Access::FieldAccess(_) | Access::ArrayAccess(_, _) => {
                    self.post.post.heap.add_edge(addr, access.clone(), *target);
                    self.restore_pre_var_value(*target, is_value_visible_outside, visited);
                }
            }
        }
    }

    fn collect_reachable_from_seeds(
        &self,
        seeds: impl IntoIterator<Item = AbstractValue>,
        include_pre_heap: bool,
        include_post_heap: bool,
    ) -> std::collections::HashSet<AbstractValue> {
        let mut reachable = std::collections::HashSet::new();
        let mut worklist: Vec<_> = seeds.into_iter().collect();

        while let Some(addr) = worklist.pop() {
            if !reachable.insert(addr) {
                continue;
            }

            if include_pre_heap {
                if let Some(edges) = self.pre.heap.get_edges(addr) {
                    for (_, target) in edges.iter() {
                        worklist.push(*target);
                    }
                }
            }

            if include_post_heap {
                if let Some(edges) = self.post.post.heap.get_edges(addr) {
                    for (_, target) in edges.iter() {
                        worklist.push(*target);
                    }
                }
            }
        }

        reachable
    }

    fn collect_reachable_array_indices(
        &self,
        heap_reachable: &std::collections::HashSet<AbstractValue>,
    ) -> std::collections::HashSet<AbstractValue> {
        let mut indices = std::collections::HashSet::new();

        let mut collect = |heap: &crate::base_memory::BaseMemory| {
            for (addr, edges) in heap.iter() {
                if !heap_reachable.contains(addr) {
                    continue;
                }
                for (access, target) in edges.iter() {
                    if !heap_reachable.contains(target) {
                        continue;
                    }
                    if let Access::ArrayAccess(_, idx) = access {
                        indices.insert(self.post.path_condition.get_var_repr(*idx));
                    }
                }
            }
        };

        collect(&self.pre.heap);
        collect(&self.post.post.heap);
        indices
    }

    fn collect_always_reachable_from_post_attrs(&self) -> std::collections::HashSet<AbstractValue> {
        self.post
            .post
            .attrs
            .iter()
            .filter_map(|(addr, attrs)| attrs.is_always_reachable().then_some(*addr))
            .collect()
    }

    /// Cross-ref: OCaml normal evaluation records every integer literal as
    /// `Invalid(ConstantDereference k)`. When a caller-visible summary value is
    /// only known equal to a constant through phi (for example after
    /// specialization or recursive summary application), recreate the same
    /// exported attr on the final summary surface.
    fn materialize_visible_constant_invalidations(
        &mut self,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        let mut materialized = Vec::new();

        for addr in reachable {
            let repr = self.post.path_condition.get_var_repr(*addr);
            let Some(q) = self.post.path_condition.is_known_const(repr) else {
                continue;
            };
            if !q.is_integer() {
                continue;
            }

            let constant = *q.numer() / *q.denom();
            if constant == 0 {
                continue;
            }

            if self
                .post
                .post
                .attrs
                .get(&repr)
                .is_some_and(|attrs| attrs.get_invalid().is_some())
            {
                continue;
            }

            let invalidation =
                crate::invalidation::Invalidation::ConstantDereference(IntLit::of_int(constant));
            let history = self.post.history_of_value(repr).unwrap_or_default();
            let location = history
                .last_location()
                .cloned()
                .unwrap_or_else(sil::location::Location::dummy);
            let history = history.append_event(HistoryEvent::Invalidated {
                invalidation: invalidation.clone(),
                location,
            });
            materialized.push((
                repr,
                crate::attribute::Attribute::Invalid(invalidation, history),
            ));
        }

        for (addr, attr) in materialized {
            self.post.post.attrs.add_one(addr, attr);
        }
    }

    /// Normalize the summary by discarding unreachable state.
    ///
    /// Matches OCaml's `discard_unreachable_ ~for_summary:true` which trims
    /// dead heap cells and address attributes from exported summaries, then
    /// simplifies the path condition to live values only.
    fn normalize(&mut self) -> Vec<Diagnostic> {
        self.normalize_with_summary_info().leaks
    }

    fn normalize_with_summary_info(&mut self) -> NormalizedSummaryInfo {
        use std::collections::HashSet;

        self.canonicalize_for_summary();

        // OCaml checks leaks from the pre-filter state, before restoring and
        // trimming the post stack for summary creation.
        let locally_reachable = self.collect_reachable_from_seeds(
            self.post.post.stack.iter().map(|(_, addr)| *addr),
            false,
            true,
        );

        self.restore_formals_for_summary();

        // Cross-ref: OCaml exported summaries for scalar/value formals keep the
        // formal stack cells and their dereference edges, but they do not keep
        // read-side `Initialized` markers on those local stack roots
        // themselves. Callers care about the pointee/return-visible effects,
        // not that the callee read its own local formal slot.
        let hidden_stack_roots: HashSet<_> = self
            .post
            .post
            .stack
            .iter()
            .filter(|(var, _)| !var.is_global() && !var.is_return())
            .map(|(_, addr)| *addr)
            .collect();
        let mut empty_attr_roots = Vec::new();
        for addr in &hidden_stack_roots {
            if let Some(attrs) = self.post.post.attrs.get_mut(addr) {
                attrs.remove(&crate::attribute::Attribute::Initialized);
                if attrs.is_empty() {
                    empty_attr_roots.push(*addr);
                }
            }
        }
        for addr in empty_attr_roots {
            self.post.post.attrs.remove_addr(&addr);
        }

        // The caller-visible summary surface is rooted in the visible stack:
        // restored pre bindings, globals, and the return slot. After
        // restore_formals_for_summary() there are no arbitrary post locals
        // left, so the remaining post stack bindings should stay reachable.
        let mut summary_roots: Vec<AbstractValue> =
            self.pre.stack.iter().map(|(_, addr)| *addr).collect();
        summary_roots.extend(self.post.post.stack.iter().map(|(_, addr)| *addr));
        summary_roots.extend(self.formals.iter().map(|(_, addr)| *addr));
        if let Some(rv) = self.result {
            summary_roots.push(rv);
        }

        let mut reachable = self.collect_reachable_from_seeds(summary_roots, true, true);
        let always_reachable = self.collect_reachable_from_seeds(
            self.collect_always_reachable_from_post_attrs(),
            false,
            true,
        );
        reachable.extend(always_reachable);

        let post_canonical_reachable: HashSet<_> = reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        let mut post_heap_reachable = reachable.clone();
        post_heap_reachable.extend(post_canonical_reachable.iter().copied());
        let pre_reachable = self.collect_reachable_from_seeds(
            self.pre.stack.iter().map(|(_, addr)| *addr),
            true,
            false,
        );
        let pre_canonical_reachable: HashSet<_> = pre_reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        let mut pre_heap_reachable = pre_reachable.clone();
        pre_heap_reachable.extend(pre_canonical_reachable.iter().copied());
        let mut formula_seeds = post_canonical_reachable.clone();
        formula_seeds.extend(self.collect_reachable_array_indices(&post_heap_reachable));
        let formula_reachable = expand_formula_reachable(&self.post.path_condition, &formula_seeds);
        let mut precondition_vocabulary = pre_reachable.clone();
        precondition_vocabulary.extend(expand_formula_reachable(
            &self.post.path_condition,
            &pre_reachable,
        ));

        let leaks = self.check_memory_leaks(&formula_reachable, &locally_reachable);

        // Cross-ref: OCaml `discard_unreachable_ ~for_summary:true` keeps the
        // exported precondition stricter than the summarized post. Post-only
        // values can stay in the post, but they must not leak into `pre`.
        self.pre.heap.retain_reachable(&pre_heap_reachable);
        self.pre.attrs.retain_reachable(&pre_canonical_reachable);
        self.pre.attrs.retain_for_pre_summary();
        self.post.post.heap.retain_reachable(&post_heap_reachable);
        self.post
            .post
            .attrs
            .retain_reachable(&post_canonical_reachable);
        self.post.post.attrs.retain_for_post_summary();
        self.post
            .must_be_valid
            .retain(|addr| post_canonical_reachable.contains(addr));
        self.post
            .need_dynamic_type_specialization
            .retain(|addr| post_canonical_reachable.contains(addr));

        let summary_eq_zero_must_be_valid = self
            .post
            .must_be_valid
            .iter()
            .copied()
            .filter(|addr| {
                self.post.path_condition.is_known_zero_for_summary(
                    *addr,
                    &precondition_vocabulary,
                    &formula_reachable,
                )
            })
            .collect();

        // Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` calls
        // `PulseFormula.simplify ~precondition_vocabulary ~keep`. The key
        // effect here is that exported conditions keep caller-visible vars in
        // their original shape while dead callee-local alias vars are
        // rewritten through phi and dropped if they become tautological.
        self.post
            .path_condition
            .simplify_for_summary(&precondition_vocabulary, &formula_reachable);
        self.materialize_visible_constant_invalidations(&post_canonical_reachable);

        NormalizedSummaryInfo {
            leaks,
            summary_eq_zero_must_be_valid,
        }
    }

    /// Check for memory leaks among locally-reachable but summary-unreachable addresses.
    ///
    /// An address is a leak if:
    /// 1. It has an Allocated attribute (was malloc'd/new'd)
    /// 2. It IS reachable from local variables (was used in this procedure)
    /// 3. It is NOT reachable from the summary (formals, return value)
    /// 4. It is NOT freed/invalidated (no matching CFree/CppDelete)
    ///
    /// Cross-ref: OCaml PulseAbductiveDomain.ml check_memory_leaks +
    /// PulseAttribute.ml get_allocated_not_freed.
    fn check_memory_leaks(
        &self,
        summary_reachable: &std::collections::HashSet<AbstractValue>,
        locally_reachable: &std::collections::HashSet<AbstractValue>,
    ) -> Vec<Diagnostic> {
        let mut leaks = Vec::new();
        let canonical_locally_reachable: std::collections::HashSet<_> = locally_reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        for (addr, attrs) in self.post.post.attrs.iter() {
            let addr = self.post.path_condition.get_var_repr(*addr);
            if !canonical_locally_reachable.contains(&addr) {
                continue;
            }
            if summary_reachable.contains(&addr) {
                continue;
            }
            let Some((allocator, alloc_loc)) = attrs.get_allocated() else {
                continue;
            };
            // Must not be freed/invalidated with a matching invalidation
            if let Some((inv, _)) = attrs.get_invalid() {
                if alloc_free_match(allocator, inv) {
                    continue;
                }
            }
            if self.reaches_live_via_pointer_arithmetic(addr, summary_reachable) {
                continue;
            }
            leaks.push(Diagnostic::MemoryLeak {
                addr,
                allocator: allocator.clone(),
                allocation_location: alloc_loc.clone(),
            });
        }
        leaks
    }

    fn reaches_live_via_pointer_arithmetic(
        &self,
        root: AbstractValue,
        live_addresses: &std::collections::HashSet<AbstractValue>,
    ) -> bool {
        if live_addresses.contains(&root) {
            return true;
        }

        let mut visited = std::collections::HashSet::new();
        let mut worklist = vec![root];

        while let Some(addr) = worklist.pop() {
            let addr = self.post.path_condition.get_var_repr(addr);
            if !visited.insert(addr) {
                continue;
            }
            let Some(edges) = self.post.post.heap.get_edges(addr) else {
                continue;
            };
            for (access, target) in edges.iter() {
                match access {
                    Access::FieldAccess(_) | Access::ArrayAccess(_, _) => {
                        let target = self.post.path_condition.get_var_repr(*target);
                        if live_addresses.contains(&target) {
                            return true;
                        }
                        worklist.push(target);
                    }
                    Access::Dereference => {}
                }
            }
        }

        false
    }
}

impl PulseSummary {
    /// Create a summary with no interprocedural information (diagnostics only).
    pub fn intra_only(diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            pre_posts: Vec::new(),
            has_dropped_disjuncts: false,
            specialized: Vec::new(),
            diagnostics,
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: Vec::new(),
        }
    }

    /// Compute a summary from the execution results of a procedure.
    ///
    /// Extracts the formal→address mapping and the final post-state from
    /// the list of execution domain results.
    pub fn of_proc(
        pdesc: &Procdesc,
        exec_states: &[ExecutionDomain],
        diagnostics: Vec<Diagnostic>,
        is_noreturn: bool,
    ) -> Self {
        Self::of_proc_with_metadata(pdesc, exec_states, diagnostics, is_noreturn, false)
    }

    pub fn of_proc_with_metadata(
        pdesc: &Procdesc,
        exec_states: &[ExecutionDomain],
        diagnostics: Vec<Diagnostic>,
        is_noreturn: bool,
        has_dropped_disjuncts: bool,
    ) -> Self {
        // Build a PrePost for each execution path (ContinueProgram, ExitProgram,
        // AbortProgram). Matches OCaml's `pre_post_list` which keeps ALL paths
        // including error paths. AbortProgram disjuncts stay in the summary so
        // callers see all possible execution outcomes.
        // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common keeps
        // AbortProgram/LatentAbortProgram in the pre_post_list.
        let mut diagnostics = diagnostics;
        let existing_latent_invalid_access_keys: std::collections::HashSet<_> = exec_states
            .iter()
            .filter_map(|state| match state {
                ExecutionDomain::LatentInvalidAccess { diagnostic, .. } => {
                    Some(diagnostic.dedup_key())
                }
                _ => None,
            })
            .collect();
        let mut pre_posts = Vec::new();
        for state in exec_states.iter().filter(|s| {
            matches!(
                s,
                ExecutionDomain::ContinueProgram(_)
                    | ExecutionDomain::ExitProgram(_)
                    | ExecutionDomain::AbortProgram { .. }
                    | ExecutionDomain::LatentAbortProgram { .. }
                    | ExecutionDomain::LatentInvalidAccess { .. }
            )
        }) {
            let mut drop_exported_latent_invalid_access_diagnostic = false;
            let (initial_kind, abort_diag) = match state {
                ExecutionDomain::ExitProgram(_) => (PrePostKind::ExitProgram, None),
                ExecutionDomain::AbortProgram { diagnostic, .. } => {
                    // Temporarily mark as AbortProgram; will reclassify below
                    (PrePostKind::AbortProgram, Some(diagnostic.as_ref().clone()))
                }
                ExecutionDomain::LatentAbortProgram { diagnostic, .. } => (
                    PrePostKind::LatentAbortProgram,
                    Some(diagnostic.as_ref().clone()),
                ),
                ExecutionDomain::LatentInvalidAccess { diagnostic, .. } => (
                    PrePostKind::LatentInvalidAccess,
                    Some(diagnostic.as_ref().clone()),
                ),
                _ => (PrePostKind::ContinueProgram, None),
            };
            let mut pp =
                build_pre_post(pdesc, state.get_astate().clone(), initial_kind, abort_diag);
            let info = pp.normalize_with_summary_info();
            let leak_diags = info.leaks;
            let potential_invalid_access = if pp.kind == PrePostKind::ContinueProgram {
                potential_invalid_access_from_normalized_continue_pre_post(
                    pdesc,
                    &pp,
                    &info.summary_eq_zero_must_be_valid,
                )
            } else {
                None
            };
            let mut extra_continue_latent_invalid_access = None;
            if let Some(candidate) = potential_invalid_access {
                let recovered_eq_zero_compared_to_null = matches!(
                    &candidate.diagnostic,
                    Diagnostic::AccessToInvalidAddress { addr, .. }
                        if post_addr_was_compared_to_null(&pp, *addr)
                );
                if candidate.recovered_from_summary_eq_zero {
                    if abort_state_has_caller_sensitive_field_write(pdesc, &pp) {
                        let mut latent_pp = pp.clone();
                        latent_pp.kind = PrePostKind::LatentInvalidAccess;
                        latent_pp.diagnostic = Some(candidate.diagnostic);
                        if normalize_direct_formal_latent_invalid_access_shape(
                            pdesc,
                            &mut latent_pp,
                        ) {
                            latent_pp.diagnostic = None;
                            extra_continue_latent_invalid_access = Some(latent_pp);
                        }
                    } else if !recovered_eq_zero_compared_to_null {
                        pp.kind = PrePostKind::LatentInvalidAccess;
                        pp.diagnostic = Some(candidate.diagnostic);
                        drop_exported_latent_invalid_access_diagnostic = true;
                    }
                } else {
                    pp.kind = PrePostKind::LatentInvalidAccess;
                    pp.diagnostic = Some(candidate.diagnostic);
                    drop_exported_latent_invalid_access_diagnostic = true;
                }
            }
            if pp.kind == PrePostKind::LatentInvalidAccess
                && !normalize_direct_formal_latent_invalid_access_shape(pdesc, &mut pp)
            {
                continue;
            }
            // Only report leaks from ordinary ContinueProgram paths — latent /
            // error paths (ExitProgram/AbortProgram/Latent*) typically
            // produce spurious leaks.
            // Cross-ref: OCaml PulseReport.ml summary_of_error_post ignores
            // leaks on stopped states, including PotentialInvalidAccessSummary.
            if pp.kind == PrePostKind::ContinueProgram {
                diagnostics.extend(leak_diags);
            }
            // OCaml error states already carry summarized abort states because
            // `PulseReport.summary_of_error_post` runs `Summary.of_post`
            // before wrapping them in `AbortProgram` / `LatentAbortProgram`.
            // Rust keeps plain abort states until this final summary pass, so
            // recover any surviving caller-controlled `must_be_valid`
            // obligations here before the final abort classification hides
            // them behind the manifest error.
            let recovered_invalid_accesses = recovered_invalid_access_pre_posts_from_abort_state(
                pdesc,
                &pp,
                &existing_latent_invalid_access_keys,
            );
            let suppress_original_abort = !recovered_invalid_accesses.is_empty();
            let export_local_latent_abort_twin = pp.kind == PrePostKind::AbortProgram
                && !pre_post_is_manifest(pdesc, &pp)
                && abort_should_keep_local_manifest_twin(pdesc, &pp);

            // Classify AbortProgram as manifest or latent.
            // Manifest errors: publish the diagnostic now.
            // Latent errors: keep the disjunct in the summary but do NOT
            // publish a manifest diagnostic at this procedure.
            // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common
            // reports only after latent-vs-manifest classification.
            if pp.kind == PrePostKind::AbortProgram {
                let direct_formal_constant_deref = !proc_is_entry_point(pdesc)
                    && pre_post_has_direct_formal_constant_deref(pdesc, &mut pp);
                if direct_formal_constant_deref
                    && !abort_invalid_access_is_imported_from_call(pdesc, &pp)
                {
                    pp.kind = PrePostKind::LatentInvalidAccess;
                }
                // Reclassify as latent if the error depends on caller inputs.
                // Latent pre_posts propagate to callers for re-evaluation.
                else if !pre_post_is_manifest(pdesc, &pp)
                    && !abort_should_keep_local_manifest_twin(pdesc, &pp)
                {
                    pp.kind = PrePostKind::LatentAbortProgram;
                } else if !suppress_original_abort {
                    if let Some(diag) = &pp.diagnostic {
                        diagnostics.push(diag.clone());
                    }
                }
            }

            if drop_exported_latent_invalid_access_diagnostic {
                // Cross-ref: OCaml `PotentialInvalidAccessSummary` serializes
                // latent invalid-access pre/posts without embedding a concrete
                // diagnostic payload. Callers reconstruct the diagnostic from
                // the latent summary state when reifying it.
                pp.diagnostic = None;
            }

            if !suppress_original_abort {
                let latent_abort_twin = export_local_latent_abort_twin.then(|| {
                    let mut twin = pp.clone();
                    twin.kind = PrePostKind::LatentAbortProgram;
                    twin
                });
                pre_posts.push(pp);
                if let Some(twin) = latent_abort_twin {
                    pre_posts.push(twin);
                }
                if let Some(latent_pp) = extra_continue_latent_invalid_access {
                    pre_posts.push(latent_pp);
                }
            }
            for recovered in &recovered_invalid_accesses {
                if recovered.kind == PrePostKind::AbortProgram {
                    if let Some(diag) = &recovered.diagnostic {
                        diagnostics.push(diag.clone());
                    }
                }
            }
            pre_posts.extend(recovered_invalid_accesses);
        }

        let latent_invalid_access_specificity = |pre_post: &PrePost| {
            (
                pre_post.post.path_condition.conditions().len(),
                usize::from(!pre_post_is_manifest(pdesc, pre_post)),
                usize::from(pre_post.diagnostic.is_some()),
            )
        };
        let mut keyed_pre_posts = Vec::with_capacity(pre_posts.len());
        for pre_post in pre_posts.drain(..) {
            let Some(key) = latent_invalid_access_report_key(&pre_post) else {
                keyed_pre_posts.push(pre_post);
                continue;
            };

            let Some(existing_idx) = keyed_pre_posts.iter().position(|existing| {
                latent_invalid_access_report_key(existing).as_deref() == Some(key.as_str())
            }) else {
                keyed_pre_posts.push(pre_post);
                continue;
            };

            if latent_invalid_access_specificity(&pre_post)
                > latent_invalid_access_specificity(&keyed_pre_posts[existing_idx])
            {
                keyed_pre_posts[existing_idx] = pre_post;
            }
        }
        pre_posts = keyed_pre_posts;

        // Cross-ref: OCaml reports manifest diagnostics only after the final
        // latent-vs-manifest classification in `PulseSummary.exec_summary_of_post_common`.
        // If Rust still carries a manifest diagnostic for an issue whose final
        // summary surface is latent-only, drop that stale manifest publication.
        // Keep the manifest diagnostic when the final summary intentionally
        // exports both variants via an `AbortProgram` twin.
        let latent_keys: std::collections::HashSet<_> = pre_posts
            .iter()
            .filter(|pre_post| {
                matches!(
                    pre_post.kind,
                    PrePostKind::LatentAbortProgram | PrePostKind::LatentInvalidAccess
                )
            })
            .filter_map(|pre_post| pre_post.diagnostic.as_ref())
            .map(Diagnostic::dedup_key)
            .collect();
        let manifest_abort_keys: std::collections::HashSet<_> = pre_posts
            .iter()
            .filter(|pre_post| pre_post.kind == PrePostKind::AbortProgram)
            .filter_map(|pre_post| pre_post.diagnostic.as_ref())
            .map(Diagnostic::dedup_key)
            .collect();
        diagnostics.retain(|diag| {
            let key = diag.dedup_key();
            !latent_keys.contains(&key) || manifest_abort_keys.contains(&key)
        });

        // Deduplicate leak diagnostics: multiple disjuncts (e.g., malloc
        // null/non-null) can report the same leak from the same allocation.
        {
            let mut seen = std::collections::HashSet::new();
            diagnostics.retain(|d| seen.insert(d.dedup_key()));
        }

        // Compute heap paths that need dynamic type specialization.
        // Walk the pre-state heap from stack vars to find paths leading
        // to addresses in need_dynamic_type_specialization.
        // Cross-ref: OCaml PulseAbductiveDomain.Summary.heap_paths_that_need_dynamic_type_specialization.
        let needs_specialization = compute_specialization_heap_paths(&pre_posts);

        let is_empty_body = pdesc.is_empty_body();
        let formal_types = pdesc
            .formals
            .iter()
            .map(|(_, typ, _)| typ.clone())
            .collect();

        Self {
            pre_posts,
            has_dropped_disjuncts,
            specialized: Vec::new(),
            diagnostics,
            is_noreturn,
            needs_specialization,
            is_empty_body,
            formal_types,
        }
    }

    /// Add a specialized summary for a given specialization.
    pub fn add_specialized_summary(
        &mut self,
        spec: PulseSpecialization,
        mut summary: PulseSummary,
    ) {
        // Specialized callee diagnostics should be reported on the callee
        // itself. Keep them on the owning summary, and strip manifest abort
        // diagnostics from the cached specialized pre/posts so callers do not
        // report the same issue again for each call context.
        let latent_abort_diagnostics: Vec<_> = summary
            .pre_posts
            .iter_mut()
            .map(|pre_post| {
                if pre_post.kind == PrePostKind::LatentAbortProgram {
                    pre_post.diagnostic.take()
                } else {
                    None
                }
            })
            .collect();
        let latent_keys: std::collections::HashSet<_> = summary
            .pre_posts
            .iter()
            .filter(|pre_post| {
                matches!(
                    pre_post.kind,
                    PrePostKind::LatentAbortProgram | PrePostKind::LatentInvalidAccess
                )
            })
            .filter_map(|pre_post| pre_post.diagnostic.as_ref())
            .chain(latent_abort_diagnostics.iter().filter_map(Option::as_ref))
            .map(Diagnostic::dedup_key)
            .collect();
        let mut seen: std::collections::HashSet<_> =
            self.diagnostics.iter().map(Diagnostic::dedup_key).collect();
        for diag in summary.diagnostics.drain(..) {
            let key = diag.dedup_key();
            if !latent_keys.contains(&key) && seen.insert(key) {
                self.diagnostics.push(diag);
            }
        }
        for pre_post in &mut summary.pre_posts {
            if pre_post.kind == PrePostKind::AbortProgram {
                pre_post.diagnostic = None;
            }
        }
        self.specialized.push((
            spec,
            SpecializedSummary {
                pre_posts: summary.pre_posts,
                latent_abort_diagnostics,
                has_dropped_disjuncts: summary.has_dropped_disjuncts,
            },
        ));
    }

    /// Look up a specialized summary.
    pub fn get_specialized(&self, spec: &PulseSpecialization) -> Option<&Vec<PrePost>> {
        self.get_specialized_data(spec).map(|data| &data.pre_posts)
    }

    pub fn get_specialized_data(&self, spec: &PulseSpecialization) -> Option<&SpecializedSummary> {
        self.specialized
            .iter()
            .find(|(s, _)| s == spec)
            .map(|(_, data)| data)
    }

    /// Check if the specialization limit has been reached.
    pub fn is_specialization_limit_reached(&self) -> bool {
        self.specialized.len() >= 5 // matches Config.pulse_specialization_limit default
    }
}

/// Cross-ref: OCaml `PulseCallOperations.apply_callee` runs
/// `AbductiveDomain.Summary.of_post` on the caller post-call state before it
/// decides which stopped execution variant to keep. That summary pass can
/// surface `PotentialInvalidAccessSummary`, which must take precedence over a
/// raw propagated `LatentAbortProgram`.
pub(crate) fn summarize_stopped_state(
    pdesc: &Procdesc,
    astate: &AbductiveDomain,
) -> StoppedStateSummary {
    let mut pp = build_pre_post(pdesc, astate.clone(), PrePostKind::ContinueProgram, None);
    let _ = pp.normalize();
    let potential_invalid_access =
        potential_invalid_access_from_normalized_stopped_pre_post(pdesc, &pp)
            .map(|candidate| candidate.diagnostic);
    StoppedStateSummary {
        state: pp.post,
        potential_invalid_access,
    }
}

pub(crate) fn recovered_invalid_accesses_from_continue_state(
    pdesc: &Procdesc,
    astate: &AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let mut pp = build_pre_post(pdesc, astate.clone(), PrePostKind::ContinueProgram, None);
    if !pp.normalize().is_empty() {
        return Vec::new();
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, &pp);
    let direct_formal_values = direct_formal_value_addrs(pdesc, &pp);
    latent_invalid_access_diagnostics_from_normalized_pre_post(pdesc, &pp, None)
        .into_iter()
        .filter_map(|(addr, diagnostic)| {
            let mut recovered_state = pp.post.clone();
            if recovered_state.and_equal_const(addr, 0).is_sat() {
                let mut recovered = PrePost {
                    pre: pp.pre.clone(),
                    post: recovered_state,
                    formals: pp.formals.clone(),
                    result: pp.result,
                    kind: if direct_formal_values.contains(&addr) {
                        PrePostKind::LatentInvalidAccess
                    } else {
                        PrePostKind::AbortProgram
                    },
                    diagnostic: Some(diagnostic),
                };
                if recovered.kind != PrePostKind::LatentInvalidAccess
                    || !caller_controlled.contains(&addr)
                {
                    classify_recovered_invalid_access_pre_post(pdesc, &mut recovered);
                }
                let diagnostic = Box::new(recovered.diagnostic.take()?);
                Some(match recovered.kind {
                    PrePostKind::AbortProgram => ExecutionDomain::AbortProgram {
                        state: Box::new(recovered.post),
                        diagnostic,
                    },
                    PrePostKind::LatentInvalidAccess => ExecutionDomain::LatentInvalidAccess {
                        state: Box::new(recovered.post),
                        diagnostic,
                    },
                    _ => return None,
                })
            } else {
                None
            }
        })
        .collect()
}

fn recovered_invalid_access_pre_posts_from_abort_state(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    existing_latent_invalid_access_keys: &std::collections::HashSet<String>,
) -> Vec<PrePost> {
    // Cross-ref: OCaml only exports `LatentInvalidAccess` when summary
    // creation has preserved a caller-reifiable `must_be_valid` obligation
    // (`PotentialInvalidAccessSummary` in `PulseSummary.ml` /
    // `PulseReport.ml`). A generic latent pre-heap is not enough by itself:
    // non-manifest local aborts such as
    // `latent.c:traverse_and_crash_if_equal_to_root` should stay
    // `LatentAbortProgram`, not sprout extra latent invalid-access twins.
    if pre_post.kind != PrePostKind::AbortProgram
        || (!abort_state_has_caller_sensitive_field_write(pdesc, pre_post)
            && !abort_invalid_access_is_imported_from_call(pdesc, pre_post))
    {
        return Vec::new();
    }

    // Only recover extra caller-reifiable invalid accesses when the original
    // stopped state would still export as a manifest abort. If the abort
    // itself stays latent after summary classification, OCaml keeps the
    // summary as latent-abort-only instead of synthesizing extra latent
    // invalid-access twins (for example in the cycle-wrapper latent cases).
    let mut classified_abort = pre_post.clone();
    classify_non_exit_abort_pre_post(pdesc, &mut classified_abort);
    if classified_abort.kind != PrePostKind::AbortProgram {
        return Vec::new();
    }
    let Some(Diagnostic::AccessToInvalidAddress { invalidation, .. }) =
        pre_post.diagnostic.as_ref()
    else {
        return Vec::new();
    };
    if !invalidation.is_null_deref() {
        return Vec::new();
    }

    // When a latent callee abort has already reified at the caller callsite
    // and no caller-side path condition survives, keep that manifest abort.
    // Synthesizing an extra latent invalid-access twin here suppresses the
    // real caller report in the simplified one-node cycle shape.
    if pre_post
        .diagnostic
        .as_ref()
        .is_some_and(|diag| proc_has_call_at_location(pdesc, diag.get_location()))
        && pre_post.post.path_condition.conditions().is_empty()
    {
        return Vec::new();
    }

    let excluded_addr = diagnostic_addr_repr(pre_post);
    let mut recovered_pre_posts = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (addr, diagnostic) in
        latent_invalid_access_diagnostics_from_normalized_pre_post(pdesc, pre_post, excluded_addr)
    {
        let diagnostic_key = diagnostic.dedup_key();
        if existing_latent_invalid_access_keys.contains(&diagnostic_key)
            || !seen.insert(diagnostic_key)
        {
            continue;
        }

        let mut latent_state = pre_post.post.clone();
        if latent_state.and_equal_const(addr, 0).is_unsat() {
            continue;
        }

        let mut recovered = PrePost {
            pre: pre_post.pre.clone(),
            post: latent_state,
            formals: pre_post.formals.clone(),
            result: pre_post.result,
            kind: PrePostKind::AbortProgram,
            diagnostic: Some(diagnostic),
        };
        classify_recovered_invalid_access_pre_post(pdesc, &mut recovered);

        let key = pre_post
            .pre
            .attrs
            .get(&addr)
            .and_then(|attrs| attrs.get_must_be_valid())
            .map(|(ts, loc, _reason)| (ts, loc.clone(), addr))
            .unwrap_or_else(|| {
                let location = recovered
                    .diagnostic
                    .as_ref()
                    .map(Diagnostic::get_location)
                    .cloned()
                    .unwrap_or_else(sil::location::Location::dummy);
                (u64::MAX, location, addr)
            });

        recovered_pre_posts.push((key, recovered));
    }

    recovered_pre_posts.sort_by(|(lhs, _), (rhs, _)| lhs.cmp(rhs));

    let unique_locations: std::collections::HashSet<_> = recovered_pre_posts
        .iter()
        .map(|((_, location, _), _)| location.clone())
        .collect();

    if unique_locations.len() == 1 {
        return recovered_pre_posts
            .into_iter()
            .map(|(_key, recovered)| recovered)
            .collect();
    }

    recovered_pre_posts
        .into_iter()
        .next()
        .map(|(_key, recovered)| vec![recovered])
        .unwrap_or_default()
}

/// Cross-ref: OCaml `PulseAbductiveDomain.Summary.of_post` can turn a normal
/// exit state into `PotentialInvalidAccessSummary` when summary simplification
/// discovers that a caller-controlled `must_be_valid` address is equal to
/// zero. `PulseSummary.exec_summary_of_post_common` then exports that as a
/// single `LatentInvalidAccess`, not as an additional ContinueProgram.
fn potential_invalid_access_from_normalized_continue_pre_post(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    summary_eq_zero_must_be_valid: &std::collections::HashSet<AbstractValue>,
) -> Option<PotentialInvalidAccessSummaryCandidate> {
    if pre_post.kind != PrePostKind::ContinueProgram {
        return None;
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    let formal_stack_addrs = formal_stack_addrs(pdesc, pre_post);
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
    let mut candidates: Vec<_> = pre_post.post.must_be_valid.iter().copied().collect();
    candidates.sort();

    let mut best: Option<(
        (sil::location::Location, u64, AbstractValue),
        PotentialInvalidAccessSummaryCandidate,
    )> = None;
    let mut seen = std::collections::HashSet::new();

    for addr in candidates {
        let repr = pre_post.post.path_condition.get_var_repr(addr);
        if !seen.insert(repr) {
            continue;
        }
        let recovered_from_summary_eq_zero = summary_eq_zero_must_be_valid.contains(&repr);
        let known_zero = pre_post.post.path_condition.is_known_zero(repr);
        if !recovered_from_summary_eq_zero && !known_zero {
            continue;
        }
        // Cross-ref: OCaml does not turn a plain direct-formal dereference
        // with no actual zero proof into `PotentialInvalidAccessSummary`.
        // `formal_load_then_exit` should stay a pure ContinueProgram; the
        // summary-space zero recovery is for caller-visible aliases/fields
        // whose zero proof only emerges after simplification, not bare formals.
        if recovered_from_summary_eq_zero && !known_zero && direct_formal_values.contains(&repr) {
            continue;
        }
        if formal_stack_addrs.contains(&repr) || !deref_value_targets.contains(&repr) {
            continue;
        }

        let invalid_attr = pre_post
            .post
            .post
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_invalid());
        if invalid_attr.is_some_and(|(inv, _history)| !inv.is_null_deref()) {
            continue;
        }

        if post_addr_was_compared_to_null(pre_post, repr) {
            continue;
        }
        if addr_was_used_as_branch_cond(pre_post, repr) {
            continue;
        }

        let access_history = pre_post.post.history_of_value(repr).unwrap_or_default();
        if access_history
            .first_invalidation()
            .is_some_and(|(inv, _loc)| !inv.is_null_deref())
        {
            continue;
        }
        if latent_invalid_access_is_imported_from_call(pdesc, pre_post, repr, &access_history) {
            continue;
        }
        if !caller_controlled.contains(&repr) && !access_history.contains_formal_origin() {
            continue;
        }

        let Some((timestamp, location)) = pre_post
            .pre
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_must_be_valid())
            .map(|(ts, loc, _reason)| (ts, loc.clone()))
            .or_else(|| {
                access_history
                    .last_location()
                    .cloned()
                    .map(|loc| (u64::MAX, loc))
            })
        else {
            continue;
        };

        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let invalidation_history = access_history.append_event(HistoryEvent::Invalidated {
            invalidation: invalidation.clone(),
            location: location.clone(),
        });
        let candidate = PotentialInvalidAccessSummaryCandidate {
            diagnostic: Diagnostic::AccessToInvalidAddress {
                addr: repr,
                invalidation,
                access_location: location.clone(),
                access_history,
                invalidation_history,
            },
            recovered_from_summary_eq_zero,
        };
        let key = (location, timestamp, repr);
        match &best {
            Some((best_key, _)) if &key >= best_key => {}
            _ => best = Some((key, candidate)),
        }
    }

    best.map(|(_key, candidate)| candidate)
}

fn potential_invalid_access_from_normalized_stopped_pre_post(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> Option<PotentialInvalidAccessSummaryCandidate> {
    potential_invalid_access_from_normalized_continue_pre_post(
        pdesc,
        pre_post,
        &std::collections::HashSet::new(),
    )
}

fn drop_selected_null_invalidation(pre_post: &mut PrePost, addr: AbstractValue) {
    let repr = pre_post.post.path_condition.get_var_repr(addr);
    let Some(attrs) = pre_post.post.post.attrs.get_mut(&repr) else {
        return;
    };

    let invalids_to_remove: Vec<_> = attrs
        .iter()
        .filter(|attr| {
            matches!(
                attr,
                crate::attribute::Attribute::Invalid(inv, _history) if inv.is_null_deref()
            )
        })
        .cloned()
        .collect();
    for attr in invalids_to_remove {
        attrs.remove(&attr);
    }
    if attrs.is_empty() {
        pre_post.post.post.attrs.remove_addr(&repr);
    }
}

fn normalize_direct_formal_latent_invalid_access_shape(
    pdesc: &Procdesc,
    pre_post: &mut PrePost,
) -> bool {
    let Some(addr) = diagnostic_addr_repr(pre_post) else {
        return true;
    };

    if !require_earlier_direct_formals_nonzero_for_potential_invalid_access(pdesc, pre_post, addr) {
        return false;
    }
    prune_later_direct_formal_artifacts_for_potential_invalid_access(pdesc, pre_post, addr);
    drop_selected_null_invalidation(pre_post, addr);
    true
}

/// Cross-ref: OCaml publishes `PotentialInvalidAccessSummary` obligations in
/// source order on direct-formal reads. If we export a later direct-formal
/// access as latent, every earlier direct-formal read must already have
/// succeeded, so record those reads as non-null guards on the summary path.
fn require_earlier_direct_formals_nonzero_for_potential_invalid_access(
    pdesc: &Procdesc,
    pre_post: &mut PrePost,
    selected_addr: AbstractValue,
) -> bool {
    let selected_repr = pre_post.post.path_condition.get_var_repr(selected_addr);
    let direct_formal_ordering = direct_formal_value_must_be_valid_ordering(pdesc, pre_post);
    let Some(selected_order) = direct_formal_ordering.get(&selected_repr).cloned() else {
        return true;
    };

    let mut earlier_formals: Vec<_> = direct_formal_ordering
        .into_iter()
        .filter_map(|(addr, order)| {
            (addr != selected_repr && order < selected_order).then_some(addr)
        })
        .collect();
    earlier_formals.sort();

    for addr in earlier_formals {
        if pre_post
            .post
            .path_condition
            .prune_less_than(&Operand::ConstOperand(0), &Operand::AbstractValue(addr))
            .is_unsat()
        {
            return false;
        }
    }

    true
}

fn prune_later_direct_formal_artifacts_for_potential_invalid_access(
    pdesc: &Procdesc,
    pre_post: &mut PrePost,
    selected_addr: AbstractValue,
) {
    let selected_repr = pre_post.post.path_condition.get_var_repr(selected_addr);
    let Some(selected_order) = pre_post.diagnostic.as_ref().and_then(|_| {
        direct_formal_value_must_be_valid_ordering(pdesc, pre_post)
            .get(&selected_repr)
            .cloned()
    }) else {
        return;
    };
    let direct_formal_ordering = direct_formal_value_must_be_valid_ordering(pdesc, pre_post);

    let later_formal_roots: Vec<_> = direct_formal_ordering
        .into_iter()
        .filter_map(|(addr, order)| {
            (addr != selected_repr && order > selected_order).then_some(addr)
        })
        .collect();
    if later_formal_roots.is_empty() {
        return;
    }

    let later_reachable: std::collections::HashSet<_> = pre_post
        .collect_reachable_from_seeds(later_formal_roots, true, true)
        .into_iter()
        .map(|addr| pre_post.post.path_condition.get_var_repr(addr))
        .collect();
    if later_reachable.is_empty() {
        return;
    }

    pre_post
        .post
        .path_condition
        .forget_non_type_constraints_involving(&later_reachable);

    for addr in &later_reachable {
        pre_post.post.post.attrs.remove_addr(addr);
    }

    let mut summary_roots: Vec<AbstractValue> =
        pre_post.pre.stack.iter().map(|(_, addr)| *addr).collect();
    summary_roots.extend(pre_post.post.post.stack.iter().map(|(_, addr)| *addr));
    summary_roots.extend(pre_post.formals.iter().map(|(_, addr)| *addr));
    if let Some(result) = pre_post.result {
        summary_roots.push(result);
    }

    let post_reachable: std::collections::HashSet<_> = pre_post
        .collect_reachable_from_seeds(summary_roots, true, true)
        .into_iter()
        .filter(|addr| !later_reachable.contains(&pre_post.post.path_condition.get_var_repr(*addr)))
        .collect();
    let mut formula_reachable: std::collections::HashSet<_> = post_reachable
        .iter()
        .map(|addr| pre_post.post.path_condition.get_var_repr(*addr))
        .collect();
    formula_reachable.extend(expand_formula_reachable(
        &pre_post.post.path_condition,
        &formula_reachable,
    ));
    // Cross-ref: OCaml still keeps `IsInt` facts on restored later-formal
    // values even when pruning later direct-formal success guards from an
    // earlier latent invalid-access summary. Keep those typed values alive for
    // summary simplification after erasing the non-type constraints above.
    formula_reachable.extend(
        later_reachable
            .iter()
            .copied()
            .filter(|addr| pre_post.post.path_condition.phi().is_marked_int(*addr)),
    );

    let pre_reachable = pre_post.collect_reachable_from_seeds(
        pre_post.pre.stack.iter().map(|(_, addr)| *addr),
        true,
        false,
    );
    let mut precondition_vocabulary: std::collections::HashSet<_> = pre_reachable
        .iter()
        .map(|addr| pre_post.post.path_condition.get_var_repr(*addr))
        .collect();
    precondition_vocabulary.extend(expand_formula_reachable(
        &pre_post.post.path_condition,
        &precondition_vocabulary,
    ));

    pre_post
        .post
        .path_condition
        .simplify_for_summary(&precondition_vocabulary, &formula_reachable);
}

fn classify_recovered_invalid_access_pre_post(pdesc: &Procdesc, pre_post: &mut PrePost) {
    let stays_latent = (!proc_is_entry_point(pdesc)
        && pre_post_has_direct_formal_constant_deref(pdesc, pre_post))
        || !pre_post_is_manifest(pdesc, pre_post);
    pre_post.kind = if stays_latent {
        PrePostKind::LatentInvalidAccess
    } else {
        PrePostKind::AbortProgram
    };
}

fn diagnostic_addr_repr(pre_post: &PrePost) -> Option<AbstractValue> {
    pre_post.diagnostic.as_ref().and_then(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { addr, .. } => {
            Some(pre_post.post.path_condition.get_var_repr(*addr))
        }
        _ => None,
    })
}

fn latent_invalid_access_is_imported_from_call(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    addr: AbstractValue,
    access_history: &crate::value_history::ValueHistory,
) -> bool {
    let repr = pre_post.post.path_condition.get_var_repr(addr);
    pre_post
        .pre
        .attrs
        .get(&repr)
        .and_then(|attrs| attrs.get_must_be_valid())
        .is_some_and(|(_timestamp, location, _reason)| {
            !proc_has_local_access_at_location(pdesc, location)
                || proc_has_call_at_location(pdesc, location)
                || access_history.has_call_at_location_before_invalidation(location)
        })
}

/// Cross-ref: OCaml summary-space traversals operate on normalized canon
/// values (`PulseAbductiveDomain.Summary.pre_heap_has_assumptions` explicitly
/// assumes this). Rust does not eagerly rewrite every heap cell to the current
/// representative, so build a canonical view on demand for summary
/// classification instead of repeatedly walking raw alias-heavy heap cells.
#[derive(Default)]
struct CanonicalHeapGraph {
    non_field_outgoing: HashMap<AbstractValue, HashSet<AbstractValue>>,
    field_outgoing: HashMap<AbstractValue, HashSet<AbstractValue>>,
    addrs_with_field_edge: HashSet<AbstractValue>,
}

impl CanonicalHeapGraph {
    fn from_heap(
        heap: &crate::base_memory::BaseMemory,
        repr_of: impl Fn(AbstractValue) -> AbstractValue,
    ) -> Self {
        let mut graph = Self::default();
        for (src, edges) in heap.iter() {
            let src = repr_of(*src);
            for (access, target) in edges.iter() {
                let target = repr_of(*target);
                match access {
                    Access::FieldAccess(_) => {
                        graph.addrs_with_field_edge.insert(src);
                        graph.field_outgoing.entry(src).or_default().insert(target);
                    }
                    Access::Dereference => {
                        graph
                            .non_field_outgoing
                            .entry(src)
                            .or_default()
                            .insert(target);
                    }
                    Access::ArrayAccess(_, _) => {
                        graph
                            .non_field_outgoing
                            .entry(src)
                            .or_default()
                            .insert(target);
                    }
                }
            }
        }
        graph
    }

    fn reachable_from(
        &self,
        seeds: impl IntoIterator<Item = AbstractValue>,
    ) -> HashSet<AbstractValue> {
        let mut reachable = HashSet::new();
        let mut worklist: Vec<_> = seeds.into_iter().collect();

        while let Some(addr) = worklist.pop() {
            if !reachable.insert(addr) {
                continue;
            }

            if let Some(targets) = self.non_field_outgoing.get(&addr) {
                worklist.extend(targets.iter().copied());
            }
            if let Some(targets) = self.field_outgoing.get(&addr) {
                worklist.extend(targets.iter().copied());
            }
        }

        reachable
    }

    fn reachable_via_field_from(
        &self,
        seeds: impl IntoIterator<Item = AbstractValue>,
    ) -> HashSet<AbstractValue> {
        let mut reachable = HashSet::new();
        let mut visited = HashSet::new();
        let mut worklist: Vec<_> = seeds.into_iter().map(|addr| (addr, false)).collect();

        while let Some((addr, seen_field)) = worklist.pop() {
            if !visited.insert((addr, seen_field)) {
                continue;
            }

            if seen_field {
                reachable.insert(addr);
            }

            if let Some(targets) = self.non_field_outgoing.get(&addr) {
                worklist.extend(targets.iter().copied().map(|target| (target, seen_field)));
            }
            if let Some(targets) = self.field_outgoing.get(&addr) {
                worklist.extend(targets.iter().copied().map(|target| (target, true)));
            }
        }

        reachable
    }

    fn has_field_edge(&self, addr: AbstractValue) -> bool {
        self.addrs_with_field_edge.contains(&addr)
    }
}

fn canonical_heap_graph(
    heap: &crate::base_memory::BaseMemory,
    pre_post: &PrePost,
) -> CanonicalHeapGraph {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);
    CanonicalHeapGraph::from_heap(heap, repr_of)
}

fn formal_pre_root_reprs(pdesc: &Procdesc, pre_post: &PrePost) -> Vec<AbstractValue> {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);
    pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar));
            pre_post.pre.stack.find(&var).map(repr_of)
        })
        .collect()
}

fn summary_formal_root_reprs(pre_post: &PrePost) -> Vec<AbstractValue> {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);
    pre_post
        .formals
        .iter()
        .map(|(_formal, addr)| repr_of(*addr))
        .collect()
}

fn abort_state_has_caller_sensitive_field_write(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let pre_graph = canonical_heap_graph(&pre_post.pre.heap, pre_post);
    let post_graph = canonical_heap_graph(&pre_post.post.post.heap, pre_post);
    let formal_roots = formal_pre_root_reprs(pdesc, pre_post);
    let reachable_via_field = pre_graph.reachable_via_field_from(formal_roots.iter().copied());
    pre_graph
        .reachable_from(formal_roots)
        .into_iter()
        .any(|addr| {
            post_addr_has_written_to(pre_post, addr)
                && (reachable_via_field.contains(&addr)
                    || pre_graph.has_field_edge(addr)
                    || post_graph.has_field_edge(addr))
        })
}

pub(crate) fn exported_latent_invalid_access_is_reportable(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> bool {
    if pre_post.kind != PrePostKind::LatentInvalidAccess {
        return false;
    }

    let Some(Diagnostic::AccessToInvalidAddress {
        addr,
        access_history,
        ..
    }) = latent_invalid_access_diagnostic_from_exported_pre_post(pre_post)
    else {
        return pre_post.diagnostic.is_some();
    };

    !latent_invalid_access_is_imported_from_call(pdesc, pre_post, addr, &access_history)
}

fn proc_has_call_at_location(pdesc: &Procdesc, location: &sil::location::Location) -> bool {
    pdesc.nodes.iter().any(|node| {
        node.instrs.iter().any(|instr| {
            matches!(
                instr,
                sil::instr::Instr::Call { loc, .. } if loc == location
            )
        })
    })
}

fn proc_has_local_access_at_location(pdesc: &Procdesc, location: &sil::location::Location) -> bool {
    pdesc.nodes.iter().any(|node| {
        node.instrs.iter().any(|instr| {
            matches!(
                instr,
                sil::instr::Instr::Load { loc, .. }
                    | sil::instr::Instr::Store { loc, .. }
                    | sil::instr::Instr::Prune { loc, .. }
                    if loc == location
            )
        })
    })
}

/// Check if an error is manifest (not dependent on caller-provided values).
///
/// An error is manifest if every recorded prune condition is either:
/// - local to the current procedure (depth 0),
/// - ground, or
/// - a benign non-null / disequality constraint on allocated or must-be-valid
///   addresses.
///
/// Cross-ref: OCaml PulseArithmetic.ml / PulseFormula.is_manifest. The key
/// detail is that only conditions imported from callees (depth > 0) can make
/// an issue latent; direct tests in the current procedure do not.
fn is_manifest(pre_post: &PrePost) -> bool {
    pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .all(|(atom, depth)| {
            *depth == 0
                || atom_is_ground(atom)
                || atom_is_benign_manifest_constraint(pre_post, atom)
        })
        && !pre_heap_has_assumptions(pre_post)
}

/// Cross-ref: OCaml `PulseAbductiveDomain.Summary.pre_heap_has_assumptions`.
///
/// A summary pre-heap can itself encode caller-sensitive assumptions even when
/// the path condition looks manifest. Two important cases from OCaml are:
/// 1. a restricted (non-negative) symbolic value appears in the pre-heap, and
/// 2. the same pre value is reachable through multiple heap paths.
///
/// Either case means the callee summary depends on caller memory shape or
/// arithmetic facts, so the issue should stay latent.
fn pre_heap_has_assumptions(pre_post: &PrePost) -> bool {
    let mut seen = std::collections::HashSet::new();

    for (_src, edges) in pre_post.pre.heap.iter() {
        for (_access, target) in edges.iter() {
            // OCaml summaries are normalized before this check. Rust summaries
            // are not guaranteed to have all heap values rewritten eagerly, so
            // use the current formula representative as the summary-space key.
            let repr = pre_post.post.path_condition.get_var_repr(*target);
            if repr.is_restricted() || !seen.insert(repr) {
                return true;
            }
        }
    }

    false
}

fn pre_post_is_manifest(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    proc_is_entry_point(pdesc) || is_manifest(pre_post)
}

/// Cross-ref: OCaml `PulseSummary.exec_summary_of_post_common` turns
/// `PotentialInvalidAccessSummary` ContinueProgram states into latent invalid
/// accesses. Rust keeps a reduced form of that logic here: if
/// caller-controlled `must_be_valid` obligations survive summary
/// normalization without a concrete invalidation, preserve them as latent
/// invalid-access pre/posts for caller reification.
fn latent_invalid_access_diagnostics_from_normalized_pre_post(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    excluded_addr: Option<AbstractValue>,
) -> Vec<(AbstractValue, Diagnostic)> {
    if !matches!(
        pre_post.kind,
        PrePostKind::ContinueProgram | PrePostKind::AbortProgram
    ) {
        return Vec::new();
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    let formal_stack_addrs = formal_stack_addrs(pdesc, pre_post);
    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
    let mut candidates: Vec<_> = pre_post.post.must_be_valid.iter().copied().collect();
    candidates.sort();

    let mut diagnostics = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for addr in candidates {
        let repr = pre_post.post.path_condition.get_var_repr(addr);
        if !seen.insert(repr) {
            continue;
        }
        if excluded_addr == Some(repr) {
            continue;
        }
        if formal_stack_addrs.contains(&repr) || !deref_value_targets.contains(&repr) {
            continue;
        }
        if pre_post
            .post
            .post
            .attrs
            .get(&repr)
            .is_some_and(|attrs| attrs.get_invalid().is_some())
        {
            continue;
        }
        if post_addr_was_compared_to_null(pre_post, repr) {
            continue;
        }
        let access_history = pre_post.post.history_of_value(repr).unwrap_or_default();
        if latent_invalid_access_is_imported_from_call(pdesc, pre_post, repr, &access_history) {
            continue;
        }
        if !caller_controlled.contains(&repr) && !access_history.contains_formal_origin() {
            continue;
        }

        let Some(location) = pre_post
            .pre
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_must_be_valid())
            .map(|(_ts, loc, _reason)| loc.clone())
            .or_else(|| access_history.last_location().cloned())
        else {
            continue;
        };

        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let invalidation_history = access_history.append_event(HistoryEvent::Invalidated {
            invalidation: invalidation.clone(),
            location: location.clone(),
        });
        diagnostics.push((
            repr,
            Diagnostic::AccessToInvalidAddress {
                addr: repr,
                invalidation,
                access_location: location.clone(),
                access_history,
                invalidation_history,
            },
        ));
    }

    diagnostics
}

fn classify_non_exit_abort_pre_post(pdesc: &Procdesc, pre_post: &mut PrePost) {
    if pre_post.kind != PrePostKind::AbortProgram {
        return;
    }

    let direct_formal_constant_deref =
        !proc_is_entry_point(pdesc) && pre_post_has_direct_formal_constant_deref(pdesc, pre_post);

    if direct_formal_constant_deref {
        pre_post.kind = PrePostKind::LatentInvalidAccess;
        return;
    }

    let is_manifest = pre_post_is_manifest(pdesc, pre_post);
    let keep_local_manifest_twin =
        !is_manifest && abort_should_keep_local_manifest_twin(pdesc, pre_post);

    if !is_manifest && !keep_local_manifest_twin {
        pre_post.kind = PrePostKind::LatentAbortProgram;
    }
}

/// Cross-ref: OCaml `PotentialInvalidAccessSummary` uses unresolved
/// `must_be_valid` addresses in the summarized state, not just the formal
/// itself. The caller-controlled portion is the value graph already reachable
/// from formals in the pre-heap. This includes shapes such as `q->next`,
/// `q->next->next`, etc., while excluding post-written field values that did
/// not exist in the pre-state.
fn pre_heap_values_reachable_from_formals(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    let pre_graph = canonical_heap_graph(&pre_post.pre.heap, pre_post);
    pre_graph.reachable_from(formal_pre_root_reprs(pdesc, pre_post))
}

fn pre_heap_values_reachable_from_summary_formals(
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    let pre_graph = canonical_heap_graph(&pre_post.pre.heap, pre_post);
    pre_graph.reachable_from(summary_formal_root_reprs(pre_post))
}

fn formal_stack_addrs(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar));
            pre_post
                .pre
                .stack
                .find(&var)
                .map(|addr| pre_post.post.path_condition.get_var_repr(addr))
        })
        .collect()
}

fn summary_formal_stack_addrs(pre_post: &PrePost) -> std::collections::HashSet<AbstractValue> {
    pre_post
        .formals
        .iter()
        .map(|(_formal, addr)| pre_post.post.path_condition.get_var_repr(*addr))
        .collect()
}

fn direct_formal_value_addrs(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar));
            pre_post
                .pre
                .stack
                .find(&var)
                .and_then(|addr| pre_post.pre.heap.find_edge(addr, &Access::Dereference))
                .map(|value| pre_post.post.path_condition.get_var_repr(value))
        })
        .collect()
}

fn direct_formal_value_must_be_valid_ordering(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> HashMap<AbstractValue, (u64, sil::location::Location)> {
    pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar));
            let addr = pre_post.pre.stack.find(&var)?;
            let value = pre_post.pre.heap.find_edge(addr, &Access::Dereference)?;
            let repr = pre_post.post.path_condition.get_var_repr(value);
            let (timestamp, loc, _reason) = pre_post.pre.attrs.get(&repr)?.get_must_be_valid()?;
            Some((repr, (timestamp, loc.clone())))
        })
        .collect()
}

fn pre_heap_deref_value_targets(pre_post: &PrePost) -> std::collections::HashSet<AbstractValue> {
    pre_post
        .pre
        .heap
        .iter()
        .flat_map(|(_addr, edges)| {
            edges.iter().filter_map(|(access, target)| {
                matches!(access, Access::Dereference)
                    .then_some(pre_post.post.path_condition.get_var_repr(*target))
            })
        })
        .collect()
}

fn post_addr_was_compared_to_null(pre_post: &PrePost, addr: AbstractValue) -> bool {
    pre_post
        .post
        .post
        .attrs
        .get(&addr)
        .and_then(|attrs| attrs.get_invalid())
        .is_some_and(|(inv, _history)| {
            matches!(
                inv,
                crate::invalidation::Invalidation::ComparedToNullInThisProcedure(_)
            )
        })
}

pub(crate) fn latent_invalid_access_diagnostic_from_exported_pre_post(
    pre_post: &PrePost,
) -> Option<Diagnostic> {
    if pre_post.kind != PrePostKind::LatentInvalidAccess {
        return pre_post.diagnostic.clone();
    }
    if let Some(diag) = &pre_post.diagnostic {
        return Some(diag.clone());
    }

    latent_invalid_access_diagnostic_from_summary_state(pre_post)
}

pub(crate) fn latent_invalid_access_diagnostic_from_summary_state(
    pre_post: &PrePost,
) -> Option<Diagnostic> {
    if pre_post.kind != PrePostKind::LatentInvalidAccess {
        return pre_post.diagnostic.clone();
    }

    let preferred_addr = pre_post.diagnostic.as_ref().and_then(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { addr, .. } => {
            Some(pre_post.post.path_condition.get_var_repr(*addr))
        }
        _ => None,
    });
    let caller_controlled = pre_heap_values_reachable_from_summary_formals(pre_post);
    let formal_stack_addrs = summary_formal_stack_addrs(pre_post);
    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
    let mut candidates: Vec<_> = pre_post.post.must_be_valid.iter().copied().collect();
    candidates.sort();

    let mut best: Option<((sil::location::Location, u64, AbstractValue), Diagnostic)> = None;
    let mut seen = std::collections::HashSet::new();
    for addr in candidates {
        let repr = pre_post.post.path_condition.get_var_repr(addr);
        if !seen.insert(repr) {
            continue;
        }
        if preferred_addr.is_some() && preferred_addr != Some(repr) {
            continue;
        }
        if formal_stack_addrs.contains(&repr) || !deref_value_targets.contains(&repr) {
            continue;
        }
        if pre_post
            .post
            .post
            .attrs
            .get(&repr)
            .is_some_and(|attrs| attrs.get_invalid().is_some())
        {
            continue;
        }
        if post_addr_was_compared_to_null(pre_post, repr) {
            continue;
        }

        let access_history = pre_post.post.history_of_value(repr).unwrap_or_default();
        if !caller_controlled.contains(&repr) && !access_history.contains_formal_origin() {
            continue;
        }

        let Some((timestamp, location)) = pre_post
            .pre
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_must_be_valid())
            .map(|(ts, loc, _reason)| (ts, loc.clone()))
            .or_else(|| {
                access_history
                    .last_location()
                    .cloned()
                    .map(|loc| (u64::MAX, loc))
            })
        else {
            continue;
        };

        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let invalidation_history = access_history.append_event(HistoryEvent::Invalidated {
            invalidation: invalidation.clone(),
            location: location.clone(),
        });
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: repr,
            invalidation,
            access_location: location.clone(),
            access_history,
            invalidation_history,
        };
        if preferred_addr == Some(repr) {
            return Some(diagnostic);
        }
        let key = (location, timestamp, repr);
        match &best {
            Some((best_key, _)) if &key >= best_key => {}
            _ => best = Some((key, diagnostic)),
        }
    }

    if preferred_addr.is_some() {
        return None;
    }
    best.map(|((_location, _timestamp, _addr), diagnostic)| diagnostic)
}

pub(crate) fn latent_invalid_access_report_key(pre_post: &PrePost) -> Option<String> {
    let diagnostic = latent_invalid_access_diagnostic_from_summary_state(pre_post)?;
    let issue_type = diagnostic.get_issue_type_id();
    let Diagnostic::AccessToInvalidAddress {
        addr,
        access_location,
        ..
    } = diagnostic
    else {
        return None;
    };
    let target = pre_post.post.path_condition.get_var_repr(addr);
    let path_key = latent_invalid_access_heap_path(pre_post, target)
        .map(|path| format!("{path}"))
        .unwrap_or_else(|| format!("{target}"));
    Some(format!(
        "{}|{}|{}",
        issue_type.id(),
        access_location,
        path_key
    ))
}

fn latent_invalid_access_heap_path(pre_post: &PrePost, target: AbstractValue) -> Option<HeapPath> {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);
    let mut best: Option<HeapPath> = None;

    for (formal, stack_addr) in &pre_post.formals {
        let root = HeapPath::Pvar(formal.clone());
        find_heap_path_to_target(
            pre_post,
            *stack_addr,
            root,
            target,
            &repr_of,
            &mut std::collections::HashSet::new(),
            &mut best,
        );
    }

    best
}

fn find_heap_path_to_target(
    pre_post: &PrePost,
    addr: AbstractValue,
    path: HeapPath,
    target: AbstractValue,
    repr_of: &impl Fn(AbstractValue) -> AbstractValue,
    visited: &mut std::collections::HashSet<AbstractValue>,
    best: &mut Option<HeapPath>,
) {
    let repr = repr_of(addr);
    if !visited.insert(repr) {
        return;
    }

    if repr == target {
        match best {
            Some(current) if format!("{current}") <= format!("{path}") => {}
            _ => *best = Some(path.clone()),
        }
        return;
    }

    let Some(edges) = pre_post.pre.heap.get_edges(addr) else {
        return;
    };
    for (access, next_addr) in edges.iter() {
        let next_path = match access {
            Access::Dereference => HeapPath::Dereference(Box::new(path.clone())),
            Access::FieldAccess(field) => {
                HeapPath::FieldAccess(field.clone(), Box::new(path.clone()))
            }
            Access::ArrayAccess(_, _) => continue,
        };
        find_heap_path_to_target(
            pre_post, *next_addr, next_path, target, repr_of, visited, best,
        );
    }
}

fn pre_post_has_direct_formal_constant_deref(pdesc: &Procdesc, pre_post: &mut PrePost) -> bool {
    let Some((diag_addr, access_history_has_formal_origin)) =
        pre_post.diagnostic.as_ref().and_then(|diag| match diag {
            Diagnostic::AccessToInvalidAddress {
                addr,
                invalidation,
                access_history,
                ..
            } if invalidation.is_null_deref()
                || matches!(
                    invalidation,
                    crate::invalidation::Invalidation::ComparedToNullInThisProcedure(_)
                ) =>
            {
                Some((
                    pre_post.post.path_condition.get_var_repr(*addr),
                    access_history.contains_formal_origin(),
                ))
            }
            _ => None,
        })
    else {
        return false;
    };

    if let Some(Diagnostic::AccessToInvalidAddress { addr, .. }) = pre_post.diagnostic.as_mut() {
        *addr = diag_addr;
    }

    if !pre_post.post.must_be_valid.contains(&diag_addr) {
        return false;
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    let direct_formal = direct_formal_value_addrs(pdesc, pre_post).contains(&diag_addr);
    let caller_owned = caller_controlled.contains(&diag_addr) || access_history_has_formal_origin;
    if pre_post.diagnostic.as_ref().is_some_and(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { access_history, .. } => {
            access_history.first_call_before_invalidation().is_some()
        }
        _ => false,
    }) && abort_state_has_caller_sensitive_field_write(pdesc, pre_post)
        && pre_post.post.path_condition.conditions().is_empty()
    {
        return false;
    }
    if pre_post_has_post_written_byref_invalid_access(pdesc, pre_post, diag_addr) {
        return !pre_post_diag_addr_has_non_null_invalidation(pre_post);
    }

    if pre_post_has_local_zero_condition(pre_post, diag_addr) {
        return direct_formal && !addr_was_used_as_branch_cond(pre_post, diag_addr);
    }

    caller_owned
        && !pre_post_has_locally_written_direct_formal(pdesc, pre_post, diag_addr)
        && !pre_post_diag_addr_has_non_null_invalidation(pre_post)
}

fn abort_has_local_invalid_access(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let Some((diag_addr, access_history_has_formal_origin)) =
        pre_post.diagnostic.as_ref().and_then(|diag| match diag {
            Diagnostic::AccessToInvalidAddress {
                addr,
                access_history,
                ..
            } => Some((
                pre_post.post.path_condition.get_var_repr(*addr),
                access_history.contains_formal_origin(),
            )),
            _ => None,
        })
    else {
        return false;
    };

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    !caller_controlled.contains(&diag_addr) && !access_history_has_formal_origin
}

/// Cross-ref: OCaml goes through `PulseLatentIssue.should_report` and
/// `PulseArithmetic.is_manifest`, and that manifestness check already rejects
/// summaries with `pre_heap_has_assumptions`.
///
/// Rust therefore keeps a local manifest twin only for narrower shapes that
/// stay caller-sensitive for reasons other than a generic latent pre-heap:
/// caller-sensitive field writes in the current proc, or imported call-side
/// must-be-valid obligations. Plain latent pre-heap assumptions should keep
/// the local crash latent, which is the OCaml behavior for
/// `latent.c:traverse_and_crash_if_equal_to_root`.
fn abort_should_keep_local_manifest_twin(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let Some(Diagnostic::AccessToInvalidAddress { invalidation, .. }) =
        pre_post.diagnostic.as_ref()
    else {
        return false;
    };
    let is_null_like = invalidation.is_null_deref()
        || matches!(
            invalidation,
            crate::invalidation::Invalidation::ComparedToNullInThisProcedure(_)
        );
    if !is_null_like {
        return false;
    }

    let has_local_invalid_access = abort_has_local_invalid_access(pdesc, pre_post);
    if !has_local_invalid_access {
        return false;
    }

    let has_caller_sensitive_field_write =
        abort_state_has_caller_sensitive_field_write(pdesc, pre_post);
    if has_caller_sensitive_field_write {
        return true;
    }

    abort_invalid_access_is_imported_from_call(pdesc, pre_post)
}

fn abort_invalid_access_is_imported_from_call(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let Some((diag_addr, access_history)) =
        pre_post.diagnostic.as_ref().and_then(|diag| match diag {
            Diagnostic::AccessToInvalidAddress {
                addr,
                access_history,
                ..
            } => Some((
                pre_post.post.path_condition.get_var_repr(*addr),
                access_history.clone(),
            )),
            _ => None,
        })
    else {
        return false;
    };

    latent_invalid_access_is_imported_from_call(pdesc, pre_post, diag_addr, &access_history)
}

/// OCaml still keeps locally-proven direct-formal null dereferences manifest.
/// `create_null_path2_bad_FN` and
/// `malloc_then_call_create_null_path_then_deref_unconditionally_bad_FN`
/// both export `AbortProgram` summaries because the callee itself established
/// the `p == 0` path with a depth-0 branch condition before dereferencing `p`.
///
/// The direct-formal latent rule should only approximate true
/// `PotentialInvalidAccessSummary`-style caller obligations, not ordinary
/// callee-local proofs. A plain local `p == 0` formula equality is not enough:
/// `latent.c:deref_then_free_then_deref_bad` reaches `x == 0` through the
/// `free(NULL)` split and still stays latent in OCaml. The surviving parity
/// signal for the manifest cases is that the value was recorded as
/// `UsedAsBranchCond` by a real prune in the current procedure.
fn pre_post_has_local_zero_condition(pre_post: &PrePost, diag_addr: AbstractValue) -> bool {
    let repr = pre_post.post.path_condition.get_var_repr(diag_addr);
    pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .any(|(atom, depth)| {
            *depth == 0
                && matches!(
                    atom,
                    crate::formula::atom::Atom::Equal(
                        crate::formula::term::Term::Var(v),
                        crate::formula::term::Term::Const(0)
                    ) | crate::formula::atom::Atom::Equal(
                        crate::formula::term::Term::Const(0),
                        crate::formula::term::Term::Var(v)
                    ) if *v == repr
                )
        })
}

fn addr_was_used_as_branch_cond(pre_post: &PrePost, addr: AbstractValue) -> bool {
    let repr = pre_post.post.path_condition.get_var_repr(addr);
    [&pre_post.pre.attrs, &pre_post.post.post.attrs]
        .into_iter()
        .filter_map(|attrs| attrs.get(&repr))
        .any(|attrs| {
            attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::UsedAsBranchCond(_, _)))
        })
}

fn proc_is_entry_point(pdesc: &Procdesc) -> bool {
    pdesc.proc_name.get_method_name() == "main"
}

fn post_addr_has_written_to(pre_post: &PrePost, addr: AbstractValue) -> bool {
    let repr = pre_post.post.path_condition.get_var_repr(addr);
    pre_post.post.post.attrs.get(&repr).is_some_and(|attrs| {
        attrs
            .iter()
            .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _)))
    })
}

/// Cross-ref: OCaml keeps direct-formal null dereferences latent when they
/// still reflect untouched caller-owned inputs, but summaries like
/// `test_syntactic_specialization_bad` and `test_assign_NULL_callback_bad`
/// remain manifest because the procedure locally wrote its own formal slot
/// through a by-ref call chain before dereferencing it.
///
/// Writing through the pointee behind the formal (`*x = ...`) is different:
/// that mutates caller-owned memory, but it does not rewrite the formal slot
/// `x` itself. OCaml still keeps `latent.c:deref_then_free_then_deref_bad`
/// latent on the null-deref side.
fn pre_post_has_locally_written_direct_formal(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    diag_addr: AbstractValue,
) -> bool {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);

    for (mangled, _typ, _annot) in &pdesc.formals {
        let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));

        for stack in [&pre_post.pre.stack, &pre_post.post.post.stack] {
            let Some(stack_addr) = stack.find(&var) else {
                continue;
            };
            let stack_addr = repr_of(stack_addr);
            let loaded_value = pre_post
                .post
                .post
                .heap
                .find_edge(stack_addr, &Access::Dereference)
                .map(repr_of);

            if stack_addr != diag_addr && loaded_value != Some(diag_addr) {
                continue;
            }

            if post_addr_has_written_to(pre_post, stack_addr) {
                return true;
            }
        }
    }

    false
}

/// Cross-ref: OCaml keeps fresh post-written by-ref cells caller-controlled,
/// while ordinary caller-owned heap contents remain regular aborts and are
/// classified by manifestness.
fn formal_is_true_by_ref(typ: &sil::typ::Typ) -> bool {
    typ.strip_ptr().is_some_and(sil::typ::Typ::is_pointer)
}

fn collect_deref_only_reachable(
    heap: &crate::base_memory::BaseMemory,
    repr_of: impl Fn(AbstractValue) -> AbstractValue,
    seeds: impl IntoIterator<Item = AbstractValue>,
) -> std::collections::HashSet<AbstractValue> {
    let mut reachable = std::collections::HashSet::new();
    let mut worklist: Vec<_> = seeds.into_iter().collect();

    while let Some(addr) = worklist.pop() {
        let addr = repr_of(addr);
        if !reachable.insert(addr) {
            continue;
        }

        let Some(edges) = heap.get_edges(addr) else {
            continue;
        };
        for (access, target) in edges.iter() {
            if matches!(access, Access::Dereference) {
                worklist.push(*target);
            }
        }
    }

    reachable
}

fn pre_post_has_post_written_byref_invalid_access(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    diag_addr: AbstractValue,
) -> bool {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);

    for (mangled, typ, _annot) in &pdesc.formals {
        if !formal_is_true_by_ref(typ) {
            continue;
        }

        let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let mut seeds = Vec::new();
        if let Some(addr) = pre_post.pre.stack.find(&var) {
            seeds.push(addr);
        }
        if let Some(addr) = pre_post.post.post.stack.find(&var) {
            seeds.push(addr);
        }
        if seeds.is_empty() {
            continue;
        }

        let pre_reachable =
            collect_deref_only_reachable(&pre_post.pre.heap, repr_of, seeds.iter().copied());
        let post_reachable =
            collect_deref_only_reachable(&pre_post.post.post.heap, repr_of, seeds.into_iter());
        if post_reachable.contains(&diag_addr) && !pre_reachable.contains(&diag_addr) {
            return true;
        }
    }

    false
}

fn pre_post_diag_addr_has_non_null_invalidation(pre_post: &PrePost) -> bool {
    let Some(diag_addr) = pre_post.diagnostic.as_ref().and_then(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { addr, .. } => {
            Some(pre_post.post.path_condition.get_var_repr(*addr))
        }
        _ => None,
    }) else {
        return false;
    };

    pre_post
        .post
        .post
        .attrs
        .get(&diag_addr)
        .and_then(|attrs| attrs.get_invalid())
        .is_some_and(|(inv, _)| !inv.is_null_deref())
}

fn atom_is_ground(atom: &crate::formula::atom::Atom) -> bool {
    atom.all_vars().is_empty()
}

fn atom_is_benign_manifest_constraint(
    pre_post: &PrePost,
    atom: &crate::formula::atom::Atom,
) -> bool {
    use crate::formula::atom::Atom;
    use crate::formula::term::Term;

    let is_allocatedish = |v: AbstractValue| {
        let repr = pre_post.post.path_condition.get_var_repr(v);
        pre_post
                .post
                .post
                .attrs
                .get(&repr)
                .is_some_and(|attrs| attrs.get_allocated().is_some())
                // Cross-ref: OCaml `PulseArithmetic.is_manifest` treats
                // must-be-valid values like allocated ones for benign `x != 0`
                // / `0 < x` guards, even if the summarized state later
                // invalidates the same address.
                || pre_post.post.must_be_valid.contains(&repr)
    };

    let var_neq_zero = match atom {
        Atom::NotEqual(Term::Var(v), Term::Const(0))
        | Atom::NotEqual(Term::Const(0), Term::Var(v))
        | Atom::LessThan(Term::Const(0), Term::Var(v))
        | Atom::LessThan(Term::Var(v), Term::Const(0)) => Some(*v),
        _ => None,
    };
    if let Some(v) = var_neq_zero {
        return is_allocatedish(v);
    }

    match atom {
        Atom::NotEqual(Term::Var(x), Term::Var(y)) => is_allocatedish(*x) && is_allocatedish(*y),
        _ => false,
    }
}

fn expand_formula_reachable(
    formula: &crate::formula::Formula,
    seed_reachable: &std::collections::HashSet<AbstractValue>,
) -> std::collections::HashSet<AbstractValue> {
    let phi = formula.phi();
    let mut reachable = seed_reachable.clone();
    let mut worklist: Vec<_> = seed_reachable.iter().copied().collect();

    while let Some(v) = worklist.pop() {
        let repr = phi.get_repr(v);
        if let Some(lin) = phi.linear_eqs.get(&repr) {
            for dep in lin.vars.keys() {
                let dep_repr = phi.get_repr(*dep);
                if reachable.insert(dep_repr) {
                    worklist.push(dep_repr);
                }
            }
        }

        for (&lhs, lin) in &phi.linear_eqs {
            let lhs_repr = phi.get_repr(lhs);
            if lhs_repr != repr
                && lin.vars.keys().any(|dep| phi.get_repr(*dep) == repr)
                && reachable.insert(lhs_repr)
            {
                worklist.push(lhs_repr);
            }
        }

        // Cross-ref: OCaml PulseFormula.DeadVariables.build_var_graph keeps
        // function-application results connected to their actual arguments.
        // Without this, imported conditions on pure-call results can be
        // dropped during summary normalization even when the actuals are
        // caller-visible formals, which makes latent caller-dependent errors
        // look manifest.
        for (key, ret) in phi.iter_fn_app_eqs() {
            let ret_repr = phi.get_repr(*ret);
            let mut connected = ret_repr == repr;
            let mut actual_reprs = Vec::new();
            for actual in &key.actuals {
                let crate::formula::phi::FnAppActual::Var(actual) = actual else {
                    continue;
                };
                let actual_repr = phi.get_repr(*actual);
                connected |= actual_repr == repr;
                actual_reprs.push(actual_repr);
            }
            if !connected {
                continue;
            }
            if reachable.insert(ret_repr) {
                worklist.push(ret_repr);
            }
            for actual_repr in actual_reprs {
                if reachable.insert(actual_repr) {
                    worklist.push(actual_repr);
                }
            }
        }
    }

    reachable
}

/// Check if an allocator and invalidation are a matching pair (alloc then free).
///
/// Cross-ref: OCaml PulseAttribute.ml alloc_free_match.
fn alloc_free_match(
    allocator: &crate::attribute::Allocator,
    invalidation: &crate::invalidation::Invalidation,
) -> bool {
    use crate::attribute::Allocator;
    use crate::invalidation::Invalidation;
    matches!(
        (allocator, invalidation),
        (
            Allocator::CMalloc | Allocator::CRealloc,
            Invalidation::CFree
        ) | (
            Allocator::CustomMalloc(_) | Allocator::CustomRealloc(_),
            Invalidation::CFree
        ) | (Allocator::CppNew, Invalidation::CppDelete)
            | (Allocator::CppNewArray, Invalidation::CppDeleteArray)
    )
}

/// Compute heap paths leading to addresses that need dynamic type specialization.
///
/// Walks the pre-state heap from stack variables, following Dereference and
/// FieldAccess edges, to find paths leading to addresses in the
/// `need_dynamic_type_specialization` set. Returns a map from HeapPath to
/// the abstract value at that path.
///
/// Cross-ref: OCaml `PulseAbductiveDomain.Summary.heap_paths_that_need_dynamic_type_specialization`.
fn compute_specialization_heap_paths(pre_posts: &[PrePost]) -> HashMap<HeapPath, AbstractValue> {
    let mut result = HashMap::new();

    for pp in pre_posts {
        let needed = &pp.post.need_dynamic_type_specialization;
        if needed.is_empty() {
            continue;
        }

        // Walk the pre-state heap from stack variables
        for (var, stack_addr) in pp.pre.stack.iter() {
            let pvar = match var {
                Var::ProgramVar(pv) => (**pv).clone(),
                _ => continue,
            };
            let root = HeapPath::Pvar(pvar);
            walk_heap_for_specialization(
                *stack_addr,
                root,
                needed,
                &pp.pre,
                &mut result,
                &mut std::collections::HashSet::new(),
            );
        }
    }

    result
}

/// Recursively walk heap edges to find paths to addresses needing specialization.
fn walk_heap_for_specialization(
    addr: AbstractValue,
    path: HeapPath,
    needed: &std::collections::HashSet<AbstractValue>,
    domain: &crate::base_domain::BaseDomain,
    result: &mut HashMap<HeapPath, AbstractValue>,
    visited: &mut std::collections::HashSet<AbstractValue>,
) {
    if !visited.insert(addr) {
        return;
    }

    if needed.contains(&addr) {
        result.insert(path.clone(), addr);
    }

    if let Some(edges) = domain.heap.get_edges(addr) {
        for (access, target) in edges.iter() {
            let next_path = match access {
                Access::Dereference => HeapPath::Dereference(Box::new(path.clone())),
                Access::FieldAccess(field) => {
                    HeapPath::FieldAccess(field.clone(), Box::new(path.clone()))
                }
                Access::ArrayAccess(_, _) => continue, // OCaml skips array accesses
            };
            walk_heap_for_specialization(*target, next_path, needed, domain, result, visited);
        }
    }
}

/// Try to find the return value in the abstract state.
///
/// Looks for the SIL return variable (`__return`) or falls back to finding
/// the last logical variable written by a Load or Call instruction.
/// Only applies to non-void procedures — void procedures have no return value,
/// and the fallback heuristic would incorrectly pick up malloc/call results,
/// making them summary-reachable and hiding leaks.
fn find_return_value(astate: &AbductiveDomain, pdesc: &Procdesc) -> Option<AbstractValue> {
    // Void procedures never return a value. Without this check, the fallback
    // heuristic picks up the last Call result (e.g., malloc) as a "return value",
    // making it summary-reachable and preventing leak detection.
    if pdesc.ret_type.is_void() {
        return None;
    }

    // Check for __return pvar (set by Ret → Store conversion in to_sil).
    // The return value is stored via `Store { __return <- val }`, which means
    // the actual value is behind a Dereference edge from __return's address.
    let ret_pvar = Pvar::mk(
        sil::mangled::Mangled::from_string("__return"),
        pdesc.proc_name.clone(),
    );
    let ret_var = Var::ProgramVar(Box::new(ret_pvar));
    if let Some(addr) = astate.post.stack.find(&ret_var) {
        // Follow the dereference edge to get the actual return value
        if let Some(val) = astate
            .post
            .heap
            .find_edge(addr, &crate::access::Access::Dereference)
        {
            return Some(val);
        }
        return Some(addr);
    }

    // Fallback: find the last Load/Call ret_id in the procedure and look it up
    // in the stack. This handles the common pattern of "n0 = <expr>; ret n0".
    let mut last_id = None;
    for node in &pdesc.nodes {
        for instr in &node.instrs {
            match instr {
                sil::instr::Instr::Load { id, .. } => last_id = Some(id.clone()),
                sil::instr::Instr::Call { ret: (id, _), .. } => last_id = Some(id.clone()),
                _ => {}
            }
        }
    }

    if let Some(id) = last_id {
        let var = Var::LogicalVar(id);
        return astate.post.stack.find(&var);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute::Allocator;
    use crate::checker;
    use crate::formula::atom::Atom;
    use crate::formula::lin_arith::LinArith;
    use crate::formula::term::Term;
    use crate::value_history::ValueHistory;
    use ondemand::checker::InterChecker;
    use sil::fieldname::Fieldname;
    use sil::ident::{Ident, IdentName};
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::typ::Typ;
    use sil::typ::TypeName;
    use sil::var::Var;
    use test_harness::textual_utils;

    struct TestPulseInterChecker;

    impl InterChecker for TestPulseInterChecker {
        type Summary = PulseSummary;

        fn id(&self) -> &str {
            "pulse"
        }

        fn analyze(
            &self,
            pdesc: &Procdesc,
            ctx: &ondemand::checker::AnalysisContext<Self::Summary>,
        ) -> Self::Summary {
            let callee_summaries: std::collections::HashMap<_, _> =
                ctx.summaries.to_vec().into_iter().collect();
            checker::analyze_with_summaries(pdesc, &callee_summaries)
        }
    }

    fn make_pdesc_with_formals(formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|name| (Mangled::from_string(*name), Typ::void(), Default::default()))
            .collect();
        pdesc
    }

    fn add_local_load(pdesc: &mut Procdesc, pvar: Pvar, loc: Location) {
        let load_node = pdesc.add_node(
            sil::procdesc::NodeKind::StmtNode(sil::procdesc::StmtNodeKind::MethodBody),
            vec![sil::instr::Instr::Load {
                id: Ident::create_none(),
                e: sil::exp::Exp::Lvar(pvar),
                typ: Typ::void(),
                loc: loc.clone(),
            }],
            loc,
        );
        pdesc.set_succs(0, vec![load_node]);
        pdesc.set_succs(load_node, vec![1]);
    }

    fn retain_named_procs(tm: &mut textual_utils::TestModule, proc_names: &[&str]) {
        let keep: std::collections::HashSet<_> = proc_names.iter().copied().collect();
        tm.cfg
            .proc_descs
            .retain(|pname, _| keep.contains(format!("{pname}").as_str()));
    }

    fn make_abort_pre_post_with_formal(name: &str) -> (Procdesc, PrePost, AbstractValue) {
        let pdesc = make_pdesc_with_formals(&[name]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string(name), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);

        let pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::AbortProgram,
            diagnostic: None,
        };

        (pdesc, pre_post, formal_val)
    }

    fn make_named_pdesc_with_formals(name: &str, formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string(name);
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|formal| {
                (
                    Mangled::from_string(*formal),
                    Typ::void(),
                    Default::default(),
                )
            })
            .collect();
        pdesc
    }

    fn dummy_invalid_access_diagnostic(
        addr: AbstractValue,
        invalidation: crate::invalidation::Invalidation,
    ) -> Diagnostic {
        dummy_invalid_access_diagnostic_at(addr, invalidation, Location::dummy())
    }

    fn dummy_invalid_access_diagnostic_at(
        addr: AbstractValue,
        invalidation: crate::invalidation::Invalidation,
        access_location: Location,
    ) -> Diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation: invalidation.clone(),
            access_location: access_location.clone(),
            access_history: ValueHistory::assignment(access_location.clone()),
            invalidation_history: ValueHistory::invalidated(invalidation.clone(), access_location),
        }
    }

    fn make_continue_pre_post_with_two_direct_formals() -> (
        Procdesc,
        PrePost,
        AbstractValue,
        AbstractValue,
        Location,
        Location,
    ) {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_var = Var::ProgramVar(Box::new(x_pvar.clone()));
        let x_formal_addr = astate.post.stack.find(&x_var).unwrap();
        let x_val = astate.read_heap(x_formal_addr, Access::Dereference);

        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let y_var = Var::ProgramVar(Box::new(y_pvar.clone()));
        let y_formal_addr = astate.post.stack.find(&y_var).unwrap();
        let y_val = astate.read_heap(y_formal_addr, Access::Dereference);

        let x_loc = Location {
            line: 79,
            col: 3,
            ..Location::dummy()
        };
        let y_loc = Location {
            line: 80,
            col: 3,
            ..Location::dummy()
        };
        astate.mark_must_be_valid_at(x_val, &x_loc);
        astate.mark_must_be_valid_at(y_val, &y_loc);

        (
            pdesc,
            PrePost {
                pre: astate.pre.clone(),
                post: astate,
                formals: vec![(x_pvar, x_formal_addr), (y_pvar, y_formal_addr)],
                result: None,
                kind: PrePostKind::ContinueProgram,
                diagnostic: None,
            },
            x_val,
            y_val,
            x_loc,
            y_loc,
        )
    }

    #[test]
    fn test_pre_heap_reachable_from_formals_follows_canonical_alias_edges() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let next = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("list")),
            "next",
        );
        let alias1 = AbstractValue::mk_fresh();
        let alias2 = AbstractValue::mk_fresh();
        let target = AbstractValue::mk_fresh();
        astate
            .pre
            .heap
            .add_edge(formal_val, Access::FieldAccess(next), alias1);
        astate
            .pre
            .heap
            .add_edge(alias2, Access::Dereference, target);
        assert!(
            astate.and_equal(alias1, alias2).is_sat(),
            "test setup should exercise aliased pre-heap roots"
        );

        let pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        assert!(
            pre_heap_values_reachable_from_formals(&pdesc, &pre_post)
                .contains(&pre_post.post.path_condition.get_var_repr(target)),
            "canonical summary-space reachability should merge alias-owned edges"
        );
    }

    #[test]
    fn test_summary_captures_formals() {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let initial = AbductiveDomain::mk_initial(&pdesc);
        let states = vec![ExecutionDomain::ContinueProgram(initial)];

        let summary = PulseSummary::of_proc(&pdesc, &states, vec![], false);
        assert_eq!(summary.pre_posts.len(), 1);

        assert_eq!(summary.pre_posts[0].formals.len(), 2);
        assert_eq!(format!("{}", summary.pre_posts[0].formals[0].0.name), "x");
        assert_eq!(format!("{}", summary.pre_posts[0].formals[1].0.name), "y");
    }

    #[test]
    fn test_summary_intra_only() {
        let summary = PulseSummary::intra_only(vec![]);
        assert!(summary.pre_posts.is_empty());
        assert!(summary.diagnostics.is_empty());
    }

    #[test]
    fn test_add_specialized_summary_merges_diagnostics_but_hides_abort_from_callers() {
        let pdesc = make_pdesc_with_formals(&[]);
        let state = AbductiveDomain::mk_initial(&pdesc);
        let diagnostic = dummy_invalid_access_diagnostic(
            AbstractValue::of_raw(1),
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );
        let specialized = PulseSummary {
            pre_posts: vec![PrePost {
                pre: state.pre.clone(),
                post: state.clone(),
                formals: vec![],
                result: None,
                kind: PrePostKind::AbortProgram,
                diagnostic: Some(diagnostic.clone()),
            }],
            has_dropped_disjuncts: false,
            specialized: vec![],
            diagnostics: vec![diagnostic.clone()],
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: vec![],
        };

        let mut summary = PulseSummary::intra_only(vec![]);
        let spec = PulseSpecialization::bottom();
        summary.add_specialized_summary(spec.clone(), specialized);

        assert_eq!(summary.diagnostics, vec![diagnostic]);
        let stored = summary
            .get_specialized(&spec)
            .expect("specialized summary should be stored");
        assert_eq!(stored.len(), 1);
        assert!(stored[0].diagnostic.is_none());
    }

    #[test]
    fn test_summary_keeps_all_disjuncts() {
        let pdesc = make_pdesc_with_formals(&[]);
        let initial = AbductiveDomain::mk_initial(&pdesc);
        let exit_state = AbductiveDomain::mk_initial(&pdesc);

        let states = vec![
            ExecutionDomain::ContinueProgram(initial),
            ExecutionDomain::ExitProgram(exit_state),
        ];

        let summary = PulseSummary::of_proc(&pdesc, &states, vec![], false);
        assert_eq!(summary.pre_posts.len(), 2, "should keep both disjuncts");
    }

    #[test]
    fn test_normalize_restores_original_formal_view() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_stack_addr = astate.post.stack.find(&var).unwrap();
        let original_value = astate.read_heap(formal_stack_addr, Access::Dereference);
        let advanced_value = AbstractValue::mk_fresh();
        astate.write_heap(formal_stack_addr, Access::Dereference, advanced_value);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_stack_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post
                .post
                .heap
                .find_edge(formal_stack_addr, &Access::Dereference),
            Some(original_value),
            "summary normalization should restore the pre-state formal view"
        );
    }

    #[test]
    fn test_normalize_drops_local_only_heap_but_still_reports_leak() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let local_root = AbstractValue::mk_fresh();
        let local_value = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));

        astate.post.stack.add(local_var, local_root);
        astate.write_heap(local_root, Access::Dereference, local_value);
        astate.allocate(local_value, Allocator::CMalloc, Location::dummy());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let leaks = pp.normalize();

        assert!(
            leaks
                .iter()
                .any(|diag| matches!(diag, Diagnostic::MemoryLeak { .. })),
            "leak reporting should happen before dead summary state is trimmed"
        );
        assert!(
            pp.post.post.heap.get_edges(local_root).is_none(),
            "local-only heap cells should not survive summary normalization"
        );
        assert!(
            pp.post.post.attrs.get(&local_value).is_none(),
            "local-only attrs should be removed from the exported summary"
        );
    }

    #[test]
    fn test_normalize_drops_unreachable_formula_constraints() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let local_value = AbstractValue::mk_fresh();
        let _ = astate.path_condition.and_equal_const(local_value, 7);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert!(
            !pp.post
                .path_condition
                .phi()
                .linear_eqs
                .contains_key(&local_value),
            "constraints on dead local-only values should be dropped from summaries"
        );
    }

    #[test]
    fn test_normalize_keeps_formula_for_reachable_array_index_constants() {
        let pdesc = make_pdesc_with_formals(&["array"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let array_pvar = Pvar::mk(Mangled::from_string("array"), pdesc.proc_name.clone());
        let array_var = Var::ProgramVar(Box::new(array_pvar.clone()));
        let array_stack_addr = astate.post.stack.find(&array_var).unwrap();
        let array_val = astate.read_heap(array_stack_addr, Access::Dereference);
        let index = AbstractValue::mk_fresh();
        let _ = astate.and_equal_const(index, 42);
        let allocated = AbstractValue::mk_fresh();
        astate.write_heap(
            array_val,
            Access::ArrayAccess(Typ::void(), index),
            allocated,
        );
        astate.allocate(allocated, Allocator::CMalloc, Location::dummy());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(array_pvar, array_stack_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post.get_const(index),
            Some(42),
            "array index constants on retained heap accesses should survive summary normalization"
        );
    }

    #[test]
    fn test_normalize_suppresses_leak_reachable_via_field_access() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let local_root = AbstractValue::mk_fresh();
        let returned_field = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));
        let field = sil::fieldname::Fieldname::make(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "fat_ptr",
            )),
            "data",
        );

        astate.post.stack.add(local_var, local_root);
        astate.allocate(local_root, Allocator::CMalloc, Location::dummy());
        astate.write_heap(local_root, Access::FieldAccess(field), returned_field);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(returned_field),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let leaks = pp.normalize();

        assert!(
            leaks.iter()
                .all(|diag| !matches!(diag, Diagnostic::MemoryLeak { .. })),
            "an allocated root should not leak if a returned field can still reach it via pointer arithmetic"
        );
    }

    #[test]
    fn test_normalize_suppresses_leak_for_always_reachable_address() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let local_root = AbstractValue::mk_fresh();
        let local_value = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));

        astate.post.stack.add(local_var, local_root);
        astate.write_heap(local_root, Access::Dereference, local_value);
        astate.allocate(local_value, Allocator::CMalloc, Location::dummy());
        astate.always_reachable(local_value);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let leaks = pp.normalize();

        assert!(
            leaks
                .iter()
                .all(|diag| !matches!(diag, Diagnostic::MemoryLeak { .. })),
            "AlwaysReachable addresses should be excluded from leak reporting"
        );
    }

    #[test]
    fn test_normalize_drops_pre_attrs_for_post_only_values() {
        let pdesc = make_pdesc_with_formals(&["p"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("p"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let post_only = AbstractValue::mk_fresh();

        astate.write_heap(formal_val, Access::Dereference, post_only);
        astate.pre.attrs.add_one(
            post_only,
            crate::attribute::Attribute::UsedAsBranchCond(
                pdesc.proc_name.clone(),
                Location::dummy(),
            ),
        );

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert!(
            pp.pre.attrs.get(&post_only).is_none(),
            "post-only values should not survive in the exported precondition"
        );
    }

    #[test]
    fn test_normalize_drops_initialized_on_formal_stack_root() {
        let pdesc = make_pdesc_with_formals(&["p"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("p"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);

        astate.initialize(formal_addr);
        astate.initialize(formal_val);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert!(
            pp.post.post.attrs.get(&formal_addr).is_none(),
            "formal stack roots should not keep exported Initialized attrs"
        );
        assert!(
            pp.post
                .post
                .attrs
                .get(&formal_val)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::Initialized)),
            "the caller-visible pointee value should keep Initialized"
        );
    }

    #[test]
    fn test_normalize_canonicalizes_return_root_to_formula_repr() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let stale_result = AbstractValue::of_raw(10);
        let canonical_result = AbstractValue::of_raw(2);

        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, stale_result);
        astate.post.attrs.initialize(stale_result);

        let result = astate.path_condition.and_equal(
            &crate::formula::Operand::AbstractValue(stale_result),
            &crate::formula::Operand::AbstractValue(canonical_result),
        );
        assert!(result.is_sat());
        assert_eq!(
            astate.post.heap.find_edge(return_addr, &Access::Dereference),
            Some(stale_result),
            "this fixture should keep the stale heap root until summary normalization canonicalizes it"
        );

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(stale_result),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post.post.heap.find_edge(return_addr, &Access::Dereference),
            Some(canonical_result),
            "summary normalization should rewrite return-visible heap roots to the formula representative"
        );
        assert_eq!(
            pp.result,
            Some(canonical_result),
            "summary metadata should stay aligned with the canonicalized state"
        );
        assert!(
            pp.post
                .post
                .attrs
                .get(&canonical_result)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::Initialized)),
            "caller-visible attrs should follow the canonicalized return value"
        );
        assert!(
            pp.post.post.attrs.get(&stale_result).is_none(),
            "stale pre-canonicalization roots should not survive summary export"
        );
    }

    #[test]
    fn test_normalize_drops_compared_to_null_invalid_from_post_summary() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let result = AbstractValue::of_raw(2);

        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, result);
        astate.initialize(return_addr);
        astate.initialize(result);
        astate.invalidate(
            result,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` keeps the first
        // same-rank `Invalid _` payload unless the attr is `OptionalEmpty`.
        astate.invalidate(
            result,
            crate::invalidation::Invalidation::ComparedToNullInThisProcedure(Location::dummy()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ComparedToNullInThisProcedure(Location::dummy()),
                Location::dummy(),
            ),
        );

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(result),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        let attrs = pp
            .post
            .post
            .attrs
            .get(&result)
            .expect("return-visible attrs should survive normalization");
        assert!(
            attrs.contains(&crate::attribute::Attribute::Initialized),
            "post-summary filtering should keep caller-visible Initialized attrs"
        );
        assert!(
            attrs.iter().any(|attr| matches!(
                attr,
                crate::attribute::Attribute::Invalid(
                    crate::invalidation::Invalidation::ConstantDereference(value),
                    _
                ) if *value == IntLit::zero()
            )),
            "post-summary filtering should keep real invalidations"
        );
        assert!(
            !attrs.iter().any(|attr| matches!(
                attr,
                crate::attribute::Attribute::Invalid(
                    crate::invalidation::Invalidation::ComparedToNullInThisProcedure(_),
                    _
                )
            )),
            "post-summary filtering should drop ComparedToNull post attrs like OCaml"
        );
    }

    #[test]
    fn test_normalize_materializes_nonzero_constant_invalid_for_visible_value() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let result = AbstractValue::of_raw(2);

        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, result);
        astate.initialize(return_addr);
        astate.initialize(result);
        assert!(astate.path_condition.and_equal_const(result, 1).is_sat());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(result),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        let attrs = pp
            .post
            .post
            .attrs
            .get(&result)
            .expect("visible summary value should keep exported attrs");
        assert!(
            attrs.iter().any(|attr| matches!(
                attr,
                crate::attribute::Attribute::Invalid(
                    crate::invalidation::Invalidation::ConstantDereference(value),
                    _
                ) if *value == IntLit::one()
            )),
            "summary export should recreate OCaml's constant invalidation surface for visible non-zero values"
        );
    }

    #[test]
    fn test_normalize_does_not_materialize_zero_constant_invalid_for_visible_value() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let result = AbstractValue::of_raw(2);

        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, result);
        astate.initialize(return_addr);
        astate.initialize(result);
        assert!(astate.path_condition.and_equal_const(result, 0).is_sat());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(result),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let _ = pp.normalize();

        let attrs = pp
            .post
            .post
            .attrs
            .get(&result)
            .expect("visible summary value should keep exported attrs");
        assert!(
            !attrs.iter().any(|attr| matches!(
                attr,
                crate::attribute::Attribute::Invalid(
                    crate::invalidation::Invalidation::ConstantDereference(_),
                    _
                )
            )),
            "summary export should leave zero-specific invalidation handling to the existing null-deref paths"
        );
    }

    #[test]
    fn test_is_manifest_ignores_local_prune_on_formal_value() {
        let (_pdesc, mut pre_post, formal_val) = make_abort_pre_post_with_formal("x");
        let _ = pre_post
            .post
            .path_condition
            .prune_eq_const(formal_val, 4, false);

        assert!(
            is_manifest(&pre_post),
            "direct tests in the current procedure should not make the issue latent"
        );
    }

    #[test]
    fn test_is_manifest_detects_imported_condition_on_formal_dependent_value() {
        let (_pdesc, mut pre_post, formal_val) = make_abort_pre_post_with_formal("x");
        let derived = AbstractValue::mk_fresh();
        let _ = pre_post.post.path_condition.and_equal_linear(
            derived,
            LinArith::of_var(formal_val).add(&LinArith::of_int(1)),
        );
        let _ = pre_post
            .post
            .path_condition
            .and_condition_direct(Atom::LessThan(Term::Var(derived), Term::Const(0)), 1);

        assert!(
            !is_manifest(&pre_post),
            "callee-imported conditions on formal-derived values should be latent"
        );
    }

    #[test]
    fn test_is_manifest_ignores_unconstrained_formal_arithmetic() {
        let (_pdesc, mut pre_post, formal_val) = make_abort_pre_post_with_formal("x");
        let derived = AbstractValue::mk_fresh();
        let _ = pre_post.post.path_condition.and_equal_linear(
            derived,
            LinArith::of_var(formal_val).add(&LinArith::of_int(1)),
        );

        assert!(
            is_manifest(&pre_post),
            "pure arithmetic definitions without path constraints should stay manifest"
        );
    }

    #[test]
    fn test_is_manifest_ignores_positive_constraint_on_must_be_valid_formal() {
        let (_pdesc, mut pre_post, formal_val) = make_abort_pre_post_with_formal("x");
        pre_post
            .post
            .must_be_valid
            .insert(pre_post.post.path_condition.get_var_repr(formal_val));
        let _ = pre_post
            .post
            .path_condition
            .and_condition_direct(Atom::LessThan(Term::Const(0), Term::Var(formal_val)), 2);

        assert!(
            is_manifest(&pre_post),
            "nonnull imported constraints on must-be-valid values should stay manifest"
        );
    }

    #[test]
    fn test_is_manifest_detects_shared_pre_heap_value_as_latent() {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let x_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(x_pvar.clone())))
            .unwrap();
        let y_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(y_pvar.clone())))
            .unwrap();
        let x_val = astate.read_heap(x_addr, Access::Dereference);
        let y_val = astate.read_heap(y_addr, Access::Dereference);
        let shared = AbstractValue::mk_fresh();
        astate.pre.heap.add_edge(x_val, Access::Dereference, shared);
        astate.pre.heap.add_edge(y_val, Access::Dereference, shared);
        astate.pre.heap.register_address(shared);

        let pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(x_pvar, x_addr), (y_pvar, y_addr)],
            result: None,
            kind: PrePostKind::AbortProgram,
            diagnostic: None,
        };

        assert!(
            !is_manifest(&pre_post),
            "shared values reachable through multiple pre-heap paths should keep the issue latent"
        );
    }

    #[test]
    fn test_is_manifest_detects_restricted_pre_heap_value_as_latent() {
        let (_pdesc, mut pre_post, formal_val) = make_abort_pre_post_with_formal("x");
        let restricted = AbstractValue::mk_fresh_restricted();
        pre_post
            .pre
            .heap
            .add_edge(formal_val, Access::Dereference, restricted);
        pre_post.pre.heap.register_address(restricted);

        assert!(
            !is_manifest(&pre_post),
            "restricted values in the pre heap should keep the issue latent"
        );
    }

    #[test]
    fn test_of_proc_suppresses_latent_abort_diagnostic() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _ = astate
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(formal_val), Term::Const(4)), 1);

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "latent aborts should stay in the summary but not be published as manifest diagnostics"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::LatentAbortProgram
        ));
    }

    #[test]
    fn test_of_proc_keeps_fn_app_dependent_abort_latent() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let fn_ret = AbstractValue::mk_fresh();
        assert!(astate
            .path_condition
            .and_fn_app(fn_ret, "unknown", &[formal_val])
            .is_sat());
        let _ = astate
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(fn_ret), Term::Const(999)), 1);

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "imported conditions on pure-call results derived from formals should stay latent"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::LatentAbortProgram
        ));
    }

    #[test]
    fn test_of_proc_preserves_explicit_latent_abort() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _ = astate
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(formal_val), Term::Const(4)), 2);

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::LatentAbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic),
            }],
            vec![],
            false,
        );

        assert!(summary.diagnostics.is_empty());
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::LatentAbortProgram
        ));
    }

    #[test]
    fn test_of_proc_reports_direct_formal_invalid_access_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        astate.mark_must_be_valid(formal_val);
        astate.invalidate(
            formal_val,
            crate::invalidation::Invalidation::CFree,
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
        );

        let diagnostic =
            dummy_invalid_access_diagnostic(formal_val, crate::invalidation::Invalidation::CFree);

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert_eq!(
            summary.diagnostics,
            vec![diagnostic],
            "locally invalidating a direct formal should stay manifest, matching OCaml summary reporting"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_classify_abort_kind_reports_direct_formal_invalid_access_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        astate.mark_must_be_valid(formal_val);
        astate.invalidate(
            formal_val,
            crate::invalidation::Invalidation::CFree,
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
        );

        let diagnostic =
            dummy_invalid_access_diagnostic(formal_val, crate::invalidation::Invalidation::CFree);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_classify_abort_kind_keeps_direct_formal_null_deref_latent() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.mark_must_be_valid(formal_val);
        astate.invalidate(
            formal_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(formal_val, invalidation);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::LatentInvalidAccess
        ));
    }

    #[test]
    fn test_classify_abort_kind_reports_direct_formal_null_manifest_when_locally_proven_zero() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.mark_must_be_valid(formal_val);
        let condition = crate::formula::atom::Atom::Equal(
            crate::formula::term::Term::Var(formal_val),
            crate::formula::term::Term::Const(0),
        );
        assert!(astate
            .path_condition
            .and_condition_direct(condition, 0)
            .is_sat());
        astate.pre.attrs.add_one(
            formal_val,
            crate::attribute::Attribute::UsedAsBranchCond(
                pdesc.proc_name.clone(),
                Location::dummy(),
            ),
        );
        astate.invalidate(
            formal_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );
        // Cross-ref: OCaml `PulseAttribute.Attributes.Set.add` keeps the first
        // same-rank `Invalid _` payload unless the attr is `OptionalEmpty`.
        astate.invalidate(
            formal_val,
            crate::invalidation::Invalidation::ComparedToNullInThisProcedure(Location::dummy()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ComparedToNullInThisProcedure(Location::dummy()),
                Location::dummy(),
            ),
        );

        let diagnostic = dummy_invalid_access_diagnostic(formal_val, invalidation);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_classify_abort_kind_reports_locally_written_direct_formal_null_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.mark_must_be_valid(formal_val);
        astate
            .post
            .attrs
            .mark_written_to(formal_addr, 1, Location::dummy());
        astate.invalidate(
            formal_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(formal_val, invalidation);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_classify_abort_kind_keeps_write_through_pointee_null_deref_latent() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.mark_must_be_valid(formal_val);
        astate
            .post
            .attrs
            .mark_written_to(formal_val, 1, Location::dummy());
        astate.invalidate(
            formal_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(formal_val, invalidation);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::LatentInvalidAccess
        ));
    }

    #[test]
    fn test_classify_abort_kind_keeps_pre_heap_reachable_field_null_deref_latent() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("list")),
            "next",
        );
        let next_slot = astate.read_heap(formal_val, Access::FieldAccess(field));
        let next_val = astate.read_heap(next_slot, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.mark_must_be_valid(next_val);
        astate.invalidate(
            next_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(next_val, invalidation);

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::LatentInvalidAccess
        ));
    }

    #[test]
    fn test_classify_abort_kind_keeps_freed_caller_visible_null_deref_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let cfree = crate::invalidation::Invalidation::CFree;
        astate.mark_must_be_valid(formal_val);
        astate.invalidate(
            formal_val,
            cfree.clone(),
            ValueHistory::invalidated(cfree, Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_classify_abort_kind_keeps_callee_written_field_null_deref_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("list")),
            "next",
        );
        let old_field_val = astate.read_heap(formal_val, Access::FieldAccess(field.clone()));
        let local_null = AbstractValue::mk_fresh();
        astate.write_heap(formal_val, Access::FieldAccess(field), local_null);
        astate.mark_must_be_valid(local_null);
        astate.invalidate(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        assert_ne!(
            astate.path_condition.get_var_repr(old_field_val),
            astate.path_condition.get_var_repr(local_null),
            "store_bad-style regression should exercise a post-written field value, not the old pre value"
        );

        let diagnostic = dummy_invalid_access_diagnostic(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_of_proc_keeps_post_written_invalid_access_latent_through_repr() {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(Typ::mk_ptr(Typ::void())),
            Default::default(),
        )];
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);

        // Store a caller-visible value into the slot behind the formal, then
        // force its formula representative to be a different abstract value.
        let alias = AbstractValue::mk_fresh();
        let slot_val = AbstractValue::mk_fresh();
        astate.write_heap(formal_val, Access::Dereference, slot_val);
        astate.mark_must_be_valid(slot_val);
        assert!(
            astate.and_equal(slot_val, alias).is_sat(),
            "equal caller-visible values should stay satisfiable"
        );
        astate.invalidate(
            slot_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        let diagnostic = dummy_invalid_access_diagnostic(
            slot_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic),
            }],
            vec![],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "newly written caller-visible cells behind by-ref formals should stay latent even after repr canonicalization"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::LatentInvalidAccess
        ));
    }

    #[test]
    fn test_of_proc_drops_exported_diagnostic_for_continue_derived_latent_invalid_access() {
        let mut pdesc = make_pdesc_with_formals(&["x"]);
        let access_loc = Location {
            line: 87,
            col: 5,
            ..Location::dummy()
        };
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        add_local_load(&mut pdesc, pvar.clone(), access_loc.clone());

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _target = astate.read_heap(formal_val, Access::Dereference);
        astate.mark_must_be_valid_at(formal_val, &access_loc);
        assert!(astate.and_equal_const(formal_val, 0).is_sat());

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        let latent = summary
            .pre_posts
            .iter()
            .find(|pp| pp.kind == PrePostKind::LatentInvalidAccess)
            .expect("expected a latent invalid-access pre/post");
        assert!(
            latent.diagnostic.is_none(),
            "continue-derived latent invalid-access summaries should not export a concrete diagnostic"
        );
        assert!(
            latent_invalid_access_diagnostic_from_exported_pre_post(latent).is_some(),
            "callers should still be able to reconstruct the latent invalid-access diagnostic"
        );
    }

    #[test]
    fn test_of_proc_reports_callee_written_field_null_deref_manifest() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("list")),
            "next",
        );
        let _old_field_val = astate.read_heap(formal_val, Access::FieldAccess(field.clone()));
        let local_null = AbstractValue::mk_fresh();
        astate.write_heap(formal_val, Access::FieldAccess(field), local_null);
        astate.mark_must_be_valid(local_null);
        astate.invalidate(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        let diagnostic = dummy_invalid_access_diagnostic(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert_eq!(
            summary.diagnostics,
            vec![diagnostic],
            "callee-written nulls in normal caller-owned fields should stay manifest, matching store_bad-style OCaml behavior"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_of_proc_keeps_local_invalid_access_latent_when_pre_heap_is_non_manifest() {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let x_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(x_pvar)))
            .unwrap();
        let y_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(y_pvar)))
            .unwrap();
        let x_val = astate.read_heap(x_addr, Access::Dereference);
        let y_val = astate.read_heap(y_addr, Access::Dereference);
        let shared = AbstractValue::mk_fresh();
        astate.pre.heap.add_edge(x_val, Access::Dereference, shared);
        astate.pre.heap.add_edge(y_val, Access::Dereference, shared);
        astate.pre.heap.register_address(shared);

        let local_root = AbstractValue::mk_fresh();
        let local_null = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));
        astate.post.stack.add(local_var, local_root);
        astate.write_heap(local_root, Access::Dereference, local_null);
        astate.mark_must_be_valid(local_null);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.invalidate(
            local_null,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(local_null, invalidation);
        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "local invalid accesses should stay latent when the only caller-sensitive signal is a latent pre heap assumption"
        );
        assert!(
            summary.pre_posts.iter().all(|pp| {
                pp.kind != PrePostKind::AbortProgram || pp.diagnostic.as_ref() != Some(&diagnostic)
            }),
            "no manifest abort summary should be exported for the local invalid access"
        );
        assert!(
            summary.pre_posts.iter().any(|pp| {
                pp.kind == PrePostKind::LatentAbortProgram && pp.diagnostic.as_ref() == Some(&diagnostic)
            }),
            "expected the local invalid access to remain latent under non-manifest pre-heap assumptions"
        );
    }

    #[test]
    fn test_of_proc_drops_stale_manifest_diag_when_final_summary_is_latent_only() {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let x_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(x_pvar)))
            .unwrap();
        let y_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(y_pvar)))
            .unwrap();
        let x_val = astate.read_heap(x_addr, Access::Dereference);
        let y_val = astate.read_heap(y_addr, Access::Dereference);
        let shared = AbstractValue::mk_fresh();
        astate.pre.heap.add_edge(x_val, Access::Dereference, shared);
        astate.pre.heap.add_edge(y_val, Access::Dereference, shared);
        astate.pre.heap.register_address(shared);

        let local_root = AbstractValue::mk_fresh();
        let local_null = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));
        astate.post.stack.add(local_var, local_root);
        astate.write_heap(local_root, Access::Dereference, local_null);
        astate.mark_must_be_valid(local_null);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.invalidate(
            local_null,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(local_null, invalidation);
        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![diagnostic.clone()],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "stale manifest diagnostics should be dropped when the final summary keeps only the latent variant"
        );
        assert!(
            summary.pre_posts.iter().any(|pp| {
                pp.kind == PrePostKind::LatentAbortProgram
                    && pp.diagnostic.as_ref() == Some(&diagnostic)
            }),
            "the latent pre/post should remain available for caller reification"
        );
    }

    #[test]
    fn test_of_proc_keeps_imported_arithmetic_guarded_local_invalid_access_latent() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(x_pvar)))
            .unwrap();
        let x_val = astate.read_heap(x_addr, Access::Dereference);
        let neg_x = AbstractValue::mk_fresh();
        assert!(
            astate
                .path_condition
                .and_equal_linear(
                    neg_x,
                    crate::formula::lin_arith::LinArith::of_var(x_val).neg()
                )
                .is_sat(),
            "negated formal arithmetic should stay satisfiable"
        );
        let imported_guard = crate::formula::atom::Atom::Equal(
            crate::formula::term::Term::Var(neg_x),
            crate::formula::term::Term::Const(0),
        );
        assert!(
            astate
                .path_condition
                .and_condition_direct(imported_guard, 1)
                .is_sat(),
            "imported arithmetic guard should stay in the summary path condition"
        );

        let local_root = AbstractValue::mk_fresh();
        let local_null = AbstractValue::mk_fresh();
        let local_var = Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0));
        astate.post.stack.add(local_var, local_root);
        astate.write_heap(local_root, Access::Dereference, local_null);
        astate.mark_must_be_valid(local_null);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        astate.invalidate(
            local_null,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );

        let diagnostic = dummy_invalid_access_diagnostic(local_null, invalidation);
        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert!(
            summary.diagnostics.is_empty(),
            "imported arithmetic guards should keep local invalid accesses latent"
        );
        assert!(
            summary.pre_posts.iter().all(|pp| {
                pp.kind != PrePostKind::AbortProgram || pp.diagnostic.as_ref() != Some(&diagnostic)
            }),
            "no manifest abort summary should be exported for the local invalid access"
        );
        assert!(
            summary.pre_posts.iter().any(|pp| {
                pp.kind == PrePostKind::LatentAbortProgram
                    && pp.diagnostic.as_ref() == Some(&diagnostic)
            }),
            "expected the local invalid access to remain latent under imported arithmetic"
        );
    }

    #[test]
    fn test_of_proc_does_not_recover_imported_call_must_be_valid_from_local_abort() {
        let mut pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let call_loc = Location {
            line: 7,
            col: 3,
            ..Location::dummy()
        };
        let callee = Procname::c_from_string("callee");
        let call_node = pdesc.add_node(
            sil::procdesc::NodeKind::StmtNode(sil::procdesc::StmtNodeKind::MethodBody),
            vec![sil::instr::Instr::Call {
                ret: (Ident::create_none(), Typ::void()),
                fun_exp: sil::exp::Exp::Const(sil::const_val::Const::Cfun(callee.clone())),
                args: vec![],
                loc: call_loc.clone(),
                flags: sil::call_flags::CallFlags::default(),
            }],
            call_loc.clone(),
        );
        pdesc.set_succs(0, vec![call_node]);
        pdesc.set_succs(call_node, vec![1]);

        astate.mark_must_be_valid(formal_val);
        astate.pre.attrs.add_one(
            formal_val,
            crate::attribute::Attribute::MustBeValid(0, call_loc.clone(), None),
        );
        astate.write_heap_with_history(
            formal_addr,
            Access::Dereference,
            crate::value_history::ValueWithHistory::new(
                formal_val,
                ValueHistory::formal_argument(pvar.clone()).wrap_call(&callee, &call_loc),
            ),
        );

        let local_null = AbstractValue::mk_fresh();
        let diagnostic = dummy_invalid_access_diagnostic(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic.clone()),
            }],
            vec![],
            false,
        );

        assert_eq!(
            summary.pre_posts.len(),
            1,
            "imported callee preconditions should not synthesize extra latent summaries"
        );
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::AbortProgram
        ));
        assert_eq!(
            summary.diagnostics,
            vec![diagnostic],
            "the local abort should stay the only published issue"
        );
    }

    #[test]
    fn test_potential_invalid_access_requires_earlier_direct_formals_nonzero() {
        let (pdesc, mut pre_post, x_val, y_val, _x_loc, y_loc) =
            make_continue_pre_post_with_two_direct_formals();

        assert!(pre_post
            .post
            .path_condition
            .prune_eq_const(y_val, 0, false)
            .is_sat());
        pre_post.diagnostic = Some(dummy_invalid_access_diagnostic_at(
            y_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            y_loc,
        ));

        assert!(
            require_earlier_direct_formals_nonzero_for_potential_invalid_access(
                &pdesc,
                &mut pre_post,
                y_val,
            ),
            "later direct-formal latent accesses should keep the earlier access-success prefix"
        );

        assert_eq!(
            pre_post
                .post
                .path_condition
                .conditions()
                .get(&Atom::Equal(Term::Var(y_val), Term::Const(0))),
            Some(&0)
        );
        assert_eq!(
            pre_post
                .post
                .path_condition
                .conditions()
                .get(&Atom::LessThan(Term::Const(0), Term::Var(x_val))),
            Some(&0)
        );
    }

    #[test]
    fn test_direct_formal_ordering_prefers_timestamp_over_location() {
        let pdesc = make_pdesc_with_formals(&["x", "y"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_var = Var::ProgramVar(Box::new(x_pvar.clone()));
        let x_formal_addr = astate.post.stack.find(&x_var).unwrap();
        let x_val = astate.read_heap(x_formal_addr, Access::Dereference);

        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let y_var = Var::ProgramVar(Box::new(y_pvar.clone()));
        let y_formal_addr = astate.post.stack.find(&y_var).unwrap();
        let y_val = astate.read_heap(y_formal_addr, Access::Dereference);

        let x_loc = Location {
            line: 200,
            col: 1,
            ..Location::dummy()
        };
        let y_loc = Location {
            line: 100,
            col: 1,
            ..Location::dummy()
        };

        // Record x first even though its textual location sorts after y.
        astate.mark_must_be_valid_at(x_val, &x_loc);
        astate.mark_must_be_valid_at(y_val, &y_loc);

        let pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(x_pvar, x_formal_addr), (y_pvar, y_formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let ordering = direct_formal_value_must_be_valid_ordering(&pdesc, &pre_post);
        assert!(
            ordering[&x_val] < ordering[&y_val],
            "direct-formal ordering should follow dynamic access order, not raw location order"
        );
    }

    #[test]
    fn test_potential_invalid_access_forgets_later_direct_formal_constraints() {
        let (pdesc, mut pre_post, x_val, y_val, x_loc, _y_loc) =
            make_continue_pre_post_with_two_direct_formals();

        assert!(pre_post
            .post
            .path_condition
            .prune_eq_const(x_val, 0, false)
            .is_sat());
        assert!(pre_post
            .post
            .path_condition
            .prune_less_than(&Operand::ConstOperand(0), &Operand::AbstractValue(y_val))
            .is_sat());
        pre_post.diagnostic = Some(dummy_invalid_access_diagnostic_at(
            x_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            x_loc,
        ));

        prune_later_direct_formal_artifacts_for_potential_invalid_access(
            &pdesc,
            &mut pre_post,
            x_val,
        );

        assert_eq!(
            pre_post
                .post
                .path_condition
                .conditions()
                .get(&Atom::Equal(Term::Var(x_val), Term::Const(0))),
            Some(&0)
        );
        assert!(
            !pre_post
                .post
                .path_condition
                .conditions()
                .contains_key(&Atom::LessThan(Term::Const(0), Term::Var(y_val))),
            "later direct-formal success guards should not survive when exporting an earlier latent access"
        );
        assert!(
            !pre_post
                .post
                .path_condition
                .phi()
                .atoms
                .contains(&Atom::LessThan(Term::Const(0), Term::Var(y_val))),
            "later direct-formal pure atoms should be erased alongside remembered conditions"
        );
    }

    #[test]
    fn test_potential_invalid_access_keeps_later_direct_formal_is_int_facts() {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let int_typ = Typ::int(sil::typ::IKind::IInt);
        let int_ptr_typ = Typ::mk_ptr(int_typ);
        pdesc.formals = vec![
            (
                Mangled::from_string("x"),
                int_ptr_typ.clone(),
                Default::default(),
            ),
            (Mangled::from_string("y"), int_ptr_typ, Default::default()),
        ];

        let mut astate = AbductiveDomain::mk_initial(&pdesc);

        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_var = Var::ProgramVar(Box::new(x_pvar.clone()));
        let x_formal_addr = astate.post.stack.find(&x_var).unwrap();
        let x_val = astate.read_heap(x_formal_addr, Access::Dereference);
        let _x_loaded = astate.read_heap(x_val, Access::Dereference);

        let y_pvar = Pvar::mk(Mangled::from_string("y"), pdesc.proc_name.clone());
        let y_var = Var::ProgramVar(Box::new(y_pvar.clone()));
        let y_formal_addr = astate.post.stack.find(&y_var).unwrap();
        let y_val = astate.read_heap(y_formal_addr, Access::Dereference);
        let y_loaded = astate.read_heap(y_val, Access::Dereference);

        let x_loc = Location {
            line: 79,
            col: 3,
            ..Location::dummy()
        };
        let y_loc = Location {
            line: 80,
            col: 3,
            ..Location::dummy()
        };
        astate.mark_must_be_valid_at(x_val, &x_loc);
        astate.mark_must_be_valid_at(y_val, &y_loc);
        astate.path_condition.and_is_int(y_loaded);
        assert!(astate.and_equal_const(x_val, 0).is_sat());
        assert!(astate
            .path_condition
            .prune_less_than(&Operand::ConstOperand(0), &Operand::AbstractValue(y_val))
            .is_sat());

        let mut pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(x_pvar, x_formal_addr), (y_pvar, y_formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: Some(dummy_invalid_access_diagnostic_at(
                x_val,
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                x_loc,
            )),
        };

        prune_later_direct_formal_artifacts_for_potential_invalid_access(
            &pdesc,
            &mut pre_post,
            x_val,
        );

        assert!(
            !pre_post
                .post
                .path_condition
                .conditions()
                .contains_key(&Atom::LessThan(Term::Const(0), Term::Var(y_val))),
            "later direct-formal success guards should still be erased"
        );
        assert!(
            pre_post.post.path_condition.phi().is_marked_int(y_loaded),
            "later restored direct-formal value typing should survive latent pruning"
        );
    }

    #[test]
    fn test_classify_abort_kind_keeps_post_written_invalid_access_latent_through_repr() {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(Typ::mk_ptr(Typ::void())),
            Default::default(),
        )];
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);

        let alias = AbstractValue::mk_fresh();
        let slot_val = AbstractValue::mk_fresh();
        astate.write_heap(formal_val, Access::Dereference, slot_val);
        astate.mark_must_be_valid(slot_val);
        assert!(astate.and_equal(slot_val, alias).is_sat());
        astate.invalidate(
            slot_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        let diagnostic = dummy_invalid_access_diagnostic(
            slot_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        assert!(matches!(
            classify_abort_kind(&pdesc, &astate, &diagnostic),
            PrePostKind::LatentInvalidAccess
        ));
    }

    #[test]
    fn test_add_specialized_summary_skips_latent_specialized_diagnostic() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        astate.mark_must_be_valid(formal_val);
        astate.invalidate(
            formal_val,
            crate::invalidation::Invalidation::CFree,
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
        );

        let diagnostic =
            dummy_invalid_access_diagnostic(formal_val, crate::invalidation::Invalidation::CFree);
        let specialized = PulseSummary {
            pre_posts: vec![PrePost {
                pre: astate.pre.clone(),
                post: astate,
                formals: vec![(pvar, formal_addr)],
                result: None,
                kind: PrePostKind::LatentInvalidAccess,
                diagnostic: Some(diagnostic.clone()),
            }],
            has_dropped_disjuncts: false,
            specialized: vec![],
            diagnostics: vec![diagnostic],
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: vec![],
        };

        let mut summary = PulseSummary::intra_only(vec![]);
        summary.add_specialized_summary(PulseSpecialization::bottom(), specialized);

        assert!(
            summary.diagnostics.is_empty(),
            "latent specialized diagnostics should stay latent and not be published on the owner"
        );
    }

    #[test]
    fn test_add_specialized_summary_strips_latent_abort_diagnostic_from_cached_pre_post() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _ = astate
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(formal_val), Term::Const(4)), 1);

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );
        let specialized = PulseSummary {
            pre_posts: vec![PrePost {
                pre: astate.pre.clone(),
                post: astate,
                formals: vec![(pvar, formal_addr)],
                result: None,
                kind: PrePostKind::LatentAbortProgram,
                diagnostic: Some(diagnostic.clone()),
            }],
            has_dropped_disjuncts: false,
            specialized: vec![],
            diagnostics: vec![],
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: vec![],
        };

        let mut summary = PulseSummary::intra_only(vec![]);
        let spec = PulseSpecialization::bottom();
        summary.add_specialized_summary(spec.clone(), specialized);

        let stored = summary
            .get_specialized_data(&spec)
            .expect("specialized summary should be stored");
        assert_eq!(stored.pre_posts.len(), 1);
        assert!(
            stored.pre_posts[0].diagnostic.is_none(),
            "cached latent abort pre/posts should export no concrete diagnostic, matching OCaml latent_issue serialization"
        );
        assert_eq!(
            stored.latent_abort_diagnostics,
            vec![Some(diagnostic)],
            "latent abort issue should remain available for caller-side reification"
        );
    }

    #[test]
    fn test_of_proc_keeps_manifest_abort_diagnostic() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let astate = AbductiveDomain::mk_initial(&pdesc);
        let local_null = AbstractValue::mk_fresh();

        let diagnostic = dummy_invalid_access_diagnostic(
            local_null,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic),
            }],
            vec![],
            false,
        );

        assert_eq!(summary.diagnostics.len(), 1);
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    fn test_of_proc_reports_entrypoint_abort_even_if_formal_dependent() {
        let pdesc = make_named_pdesc_with_formals("main", &["argc"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("argc"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _ = astate
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(formal_val), Term::Const(1)), 1);

        let diagnostic = dummy_invalid_access_diagnostic(
            formal_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::AbortProgram {
                state: Box::new(astate),
                diagnostic: Box::new(diagnostic),
            }],
            vec![],
            false,
        );

        assert_eq!(summary.diagnostics.len(), 1);
        assert!(matches!(
            summary.pre_posts[0].kind,
            PrePostKind::AbortProgram
        ));
    }

    #[test]
    #[ignore = "debug parity probe against local /tmp latent.sil fixture"]
    fn test_debug_real_latent_subset_summary_counts() {
        let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
        if !sil.exists() {
            eprintln!("skip");
            return;
        }

        let mut tm = textual_utils::parse_file_and_convert(sil);
        let targets = [
            "traverse_and_crash_if_equal_to_root",
            "crash_after_one_node_bad",
            "crash_after_two_nodes_bad",
            "FN_crash_after_six_nodes_bad",
        ];
        retain_named_procs(&mut tm, &targets);

        let checker = TestPulseInterChecker;
        let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

        for target in targets {
            let summary = store
                .to_vec()
                .into_iter()
                .find(|(pname, _)| format!("{pname}") == target)
                .map(|(_, summary)| summary)
                .unwrap_or_else(|| panic!("summary for {target} should exist"));
            let kinds: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.kind))
                .collect();
            eprintln!(
                "{target}: count={} kinds={kinds:?}",
                summary.pre_posts.len()
            );
        }
    }

    #[test]
    #[ignore = "debug parity probe against local /tmp latent.sil fixture"]
    fn test_debug_real_traverse_summary_shape() {
        let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
        if !sil.exists() {
            eprintln!("skip");
            return;
        }

        let mut tm = textual_utils::parse_file_and_convert(sil);
        retain_named_procs(&mut tm, &["traverse_and_crash_if_equal_to_root"]);
        let pdesc = tm
            .cfg
            .iter_proc_descs()
            .find(|pdesc| format!("{}", pdesc.proc_name) == "traverse_and_crash_if_equal_to_root")
            .expect("proc should exist");
        let summary = checker::analyze(pdesc);

        let kinds: Vec<_> = summary
            .pre_posts
            .iter()
            .map(|pp| format!("{:?}", pp.kind))
            .collect();
        let diagnostics: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|diag| diag.get_issue_type())
            .collect();
        eprintln!(
            "traverse_and_crash_if_equal_to_root: count={} kinds={kinds:?} diagnostics={diagnostics:?}",
            summary.pre_posts.len(),
        );
    }

    #[test]
    #[ignore = "debug parity probe against local /tmp latent.sil fixture"]
    fn test_debug_real_abort_recovery_report_keys() {
        let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
        if !sil.exists() {
            eprintln!("skip");
            return;
        }

        let mut tm = textual_utils::parse_file_and_convert(sil);
        let targets = [
            "traverse_and_crash_if_equal_to_root",
            "crash_after_one_node_bad",
            "crash_after_two_nodes_bad",
            "FN_crash_after_six_nodes_bad",
        ];
        retain_named_procs(
            &mut tm,
            &[
                "traverse_and_crash_if_equal_to_root",
                "crash_after_one_node_bad",
                "crash_after_two_nodes_bad",
                "FN_crash_after_six_nodes_bad",
            ],
        );

        let checker = TestPulseInterChecker;
        let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

        for target in targets {
            let pdesc = tm
                .cfg
                .iter_proc_descs()
                .find(|pdesc| format!("{}", pdesc.proc_name) == target)
                .unwrap_or_else(|| panic!("procdesc for {target} should exist"));
            let summary = store
                .to_vec()
                .into_iter()
                .find(|(pname, _)| format!("{pname}") == target)
                .map(|(_, summary)| summary)
                .unwrap_or_else(|| panic!("summary for {target} should exist"));

            eprintln!("TARGET {target}");
            for (i, pre_post) in summary.pre_posts.iter().enumerate() {
                let report_key = latent_invalid_access_report_key(pre_post);
                let heap_path = pre_post.diagnostic.as_ref().and_then(|diag| match diag {
                    Diagnostic::AccessToInvalidAddress { addr, .. } => {
                        latent_invalid_access_heap_path(
                            pre_post,
                            pre_post.post.path_condition.get_var_repr(*addr),
                        )
                        .map(|path| format!("{path}"))
                    }
                    _ => None,
                });
                let recovered_keys = if pre_post.kind == PrePostKind::AbortProgram {
                    let recovered = recovered_invalid_access_pre_posts_from_abort_state(
                        pdesc,
                        pre_post,
                        &std::collections::HashSet::new(),
                    );
                    recovered
                        .iter()
                        .map(|recovered| {
                            let recovered_path =
                                recovered.diagnostic.as_ref().and_then(|diag| match diag {
                                    Diagnostic::AccessToInvalidAddress { addr, .. } => {
                                        latent_invalid_access_heap_path(
                                            recovered,
                                            recovered.post.path_condition.get_var_repr(*addr),
                                        )
                                        .map(|path| format!("{path}"))
                                    }
                                    _ => None,
                                });
                            latent_invalid_access_report_key(recovered).unwrap_or_else(|| {
                                format!("{:?}:{recovered_path:?}", recovered.kind)
                            })
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let manifest = pre_post_is_manifest(pdesc, pre_post);
                let imported_from_call =
                    abort_invalid_access_is_imported_from_call(pdesc, pre_post);
                let caller_sensitive_field_write =
                    abort_state_has_caller_sensitive_field_write(pdesc, pre_post);
                let conditions = format!("{:?}", pre_post.post.path_condition.conditions());
                eprintln!(
                    "  pp[{i}] kind={:?} manifest={manifest} imported_from_call={imported_from_call} caller_sensitive_field_write={caller_sensitive_field_write} heap_path={heap_path:?} report_key={report_key:?} recovered_keys={recovered_keys:?} conditions={conditions}",
                    pre_post.kind,
                );

                if (target == "FN_crash_after_six_nodes_bad"
                    && ((i == 0 && pre_post.kind == PrePostKind::ContinueProgram)
                        || (i == 1 && pre_post.kind == PrePostKind::AbortProgram)))
                    || (target == "traverse_and_crash_if_equal_to_root"
                        && matches!(
                            pre_post.kind,
                            PrePostKind::ContinueProgram | PrePostKind::LatentInvalidAccess
                        ))
                {
                    eprintln!("    candidate scan for pp[{i}]:");
                    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
                    let formal_stack_addrs = formal_stack_addrs(pdesc, pre_post);
                    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
                    let mut candidates: Vec<_> =
                        pre_post.post.must_be_valid.iter().copied().collect();
                    candidates.sort();
                    for addr in candidates {
                        let repr = pre_post.post.path_condition.get_var_repr(addr);
                        let access_history =
                            pre_post.post.history_of_value(repr).unwrap_or_default();
                        let path = latent_invalid_access_heap_path(pre_post, repr)
                            .map(|path| format!("{path}"));
                        let location = pre_post
                            .pre
                            .attrs
                            .get(&repr)
                            .and_then(|attrs| attrs.get_must_be_valid())
                            .map(|(ts, loc, _)| format!("{ts}@{loc}"));
                        let has_invalid = pre_post
                            .post
                            .post
                            .attrs
                            .get(&repr)
                            .is_some_and(|attrs| attrs.get_invalid().is_some());
                        let compared_to_null = post_addr_was_compared_to_null(pre_post, repr);
                        let used_as_branch = addr_was_used_as_branch_cond(pre_post, repr);
                        let imported = latent_invalid_access_is_imported_from_call(
                            pdesc,
                            pre_post,
                            repr,
                            &access_history,
                        );
                        let caller_visible = caller_controlled.contains(&repr)
                            || access_history.contains_formal_origin();
                        eprintln!(
                            "      {repr}: path={path:?} formal_stack={} deref_target={} has_invalid={} compared_to_null={} used_as_branch={} imported={} caller_visible={} location={location:?} access_history={}",
                            formal_stack_addrs.contains(&repr),
                            deref_value_targets.contains(&repr),
                            has_invalid,
                            compared_to_null,
                            used_as_branch,
                            imported,
                            caller_visible,
                            access_history.signature(),
                        );
                    }
                }
            }
        }
    }
}
