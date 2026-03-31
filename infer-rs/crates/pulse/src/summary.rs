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

use std::collections::HashMap;

use sil::procdesc::Procdesc;
use sil::pvar::Pvar;
use sil::specialization::{HeapPath, PulseSpecialization};
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;

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
    /// Specialized summaries, each paired with the specialization used.
    /// Matches OCaml's `PulseSummary.specialized`.
    pub specialized: Vec<(PulseSpecialization, Vec<PrePost>)>,
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

#[derive(Clone, Debug)]
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

    /// Normalize the summary by discarding unreachable state.
    ///
    /// Matches OCaml's `discard_unreachable_ ~for_summary:true` which trims
    /// dead heap cells and address attributes from exported summaries, then
    /// simplifies the path condition to live values only.
    fn normalize(&mut self) -> Vec<Diagnostic> {
        use std::collections::HashSet;

        // OCaml checks leaks from the pre-filter state, before restoring and
        // trimming the post stack for summary creation.
        let locally_reachable = self.collect_reachable_from_seeds(
            self.post.post.stack.iter().map(|(_, addr)| *addr),
            false,
            true,
        );

        self.restore_formals_for_summary();

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

        let canonical_reachable: HashSet<_> = reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        let mut heap_reachable = reachable.clone();
        heap_reachable.extend(canonical_reachable.iter().copied());
        let mut formula_seeds = canonical_reachable.clone();
        formula_seeds.extend(self.collect_reachable_array_indices(&heap_reachable));
        let formula_reachable = expand_formula_reachable(&self.post.path_condition, &formula_seeds);
        let precondition_seed_reachable = self.collect_reachable_from_seeds(
            self.pre.stack.iter().map(|(_, addr)| *addr),
            true,
            false,
        );
        let mut precondition_vocabulary = precondition_seed_reachable.clone();
        precondition_vocabulary.extend(expand_formula_reachable(
            &self.post.path_condition,
            &precondition_seed_reachable,
        ));

        let leaks = self.check_memory_leaks(&formula_reachable, &locally_reachable);

        self.pre.heap.retain_reachable(&heap_reachable);
        self.pre.attrs.retain_reachable(&canonical_reachable);
        self.post.post.heap.retain_reachable(&heap_reachable);
        self.post.post.attrs.retain_reachable(&canonical_reachable);
        self.post
            .must_be_valid
            .retain(|addr| canonical_reachable.contains(addr));
        self.post
            .need_dynamic_type_specialization
            .retain(|addr| canonical_reachable.contains(addr));

        // Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` calls
        // `PulseFormula.simplify ~precondition_vocabulary ~keep`. The key
        // effect here is that exported conditions keep caller-visible vars in
        // their original shape while dead callee-local alias vars are
        // rewritten through phi and dropped if they become tautological.
        self.post
            .path_condition
            .simplify_for_summary(&precondition_vocabulary, &formula_reachable);

        leaks
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
        // Build a PrePost for each execution path (ContinueProgram, ExitProgram,
        // AbortProgram). Matches OCaml's `pre_post_list` which keeps ALL paths
        // including error paths. AbortProgram disjuncts stay in the summary so
        // callers see all possible execution outcomes.
        // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common keeps
        // AbortProgram/LatentAbortProgram in the pre_post_list.
        let mut diagnostics = diagnostics;
        let pre_posts: Vec<PrePost> = exec_states
            .iter()
            .filter(|s| {
                matches!(
                    s,
                    ExecutionDomain::ContinueProgram(_)
                        | ExecutionDomain::ExitProgram(_)
                        | ExecutionDomain::AbortProgram { .. }
                        | ExecutionDomain::LatentAbortProgram { .. }
                        | ExecutionDomain::LatentInvalidAccess { .. }
                )
            })
            .map(|state| {
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
                let leak_diags = pp.normalize();
                // Only report leaks from ContinueProgram paths — error paths
                // (ExitProgram/AbortProgram) typically produce spurious leaks.
                // Cross-ref: OCaml PulseReport.ml summary_of_error_post ignores leaks.
                if pp.kind == PrePostKind::ContinueProgram {
                    diagnostics.extend(leak_diags);
                }

                // Classify AbortProgram as manifest or latent.
                // Manifest errors: publish the diagnostic now.
                // Latent errors: keep the disjunct in the summary but do NOT
                // publish a manifest diagnostic at this procedure.
                // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common
                // reports only after latent-vs-manifest classification.
                if pp.kind == PrePostKind::AbortProgram {
                    if !proc_is_entry_point(pdesc)
                        && pre_post_has_caller_visible_constant_deref(pdesc, &mut pp)
                    {
                        pp.kind = PrePostKind::LatentInvalidAccess;
                    }
                    // Reclassify as latent if the error depends on caller inputs.
                    // Latent pre_posts propagate to callers for re-evaluation.
                    else if !pre_post_is_manifest(pdesc, &pp) {
                        pp.kind = PrePostKind::LatentAbortProgram;
                    } else if let Some(diag) = &pp.diagnostic {
                        diagnostics.push(diag.clone());
                    }
                }

                pp
            })
            .collect();

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
        self.specialized.push((spec, summary.pre_posts));
    }

    /// Look up a specialized summary.
    pub fn get_specialized(&self, spec: &PulseSpecialization) -> Option<&Vec<PrePost>> {
        self.specialized
            .iter()
            .find(|(s, _)| s == spec)
            .map(|(_, pp)| pp)
    }

    /// Check if the specialization limit has been reached.
    pub fn is_specialization_limit_reached(&self) -> bool {
        self.specialized.len() >= 5 // matches Config.pulse_specialization_limit default
    }
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

fn classify_non_exit_abort_pre_post(pdesc: &Procdesc, pre_post: &mut PrePost) {
    if pre_post.kind != PrePostKind::AbortProgram {
        return;
    }

    if !proc_is_entry_point(pdesc) && pre_post_has_caller_visible_constant_deref(pdesc, pre_post) {
        pre_post.kind = PrePostKind::LatentInvalidAccess;
    } else if !pre_post_is_manifest(pdesc, pre_post) {
        pre_post.kind = PrePostKind::LatentAbortProgram;
    }
}

fn pre_post_has_caller_visible_invalid_access(pdesc: &Procdesc, pre_post: &mut PrePost) -> bool {
    let Some(diag_addr) = pre_post.diagnostic.as_ref().and_then(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { addr, .. } => {
            Some(pre_post.post.path_condition.get_var_repr(*addr))
        }
        _ => None,
    }) else {
        return false;
    };

    if let Some(Diagnostic::AccessToInvalidAddress { addr, .. }) = pre_post.diagnostic.as_mut() {
        *addr = diag_addr;
    }

    if !pre_post.post.must_be_valid.contains(&diag_addr) {
        return false;
    }

    let canonicalize = |addrs: std::collections::HashSet<AbstractValue>,
                        pre_post: &PrePost|
     -> std::collections::HashSet<AbstractValue> {
        addrs
            .into_iter()
            .map(|addr| pre_post.post.path_condition.get_var_repr(addr))
            .collect()
    };

    let caller_visible_in_pre = canonicalize(
        pre_post.collect_reachable_from_seeds(
            pre_post.pre.stack.iter().map(|(_, addr)| *addr),
            true,
            false,
        ),
        pre_post,
    );

    caller_visible_in_pre.contains(&diag_addr)
        || pre_post_has_post_written_byref_invalid_access(pdesc, pre_post, diag_addr)
}

fn proc_is_entry_point(pdesc: &Procdesc) -> bool {
    pdesc.proc_name.get_method_name() == "main"
}

/// Cross-ref: OCaml keeps caller-controlled null dereferences as
/// `LatentInvalidAccess` summaries, while local invalidations like `free(x)`
/// stay regular aborts and are then classified by manifestness.
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

fn pre_post_has_caller_visible_constant_deref(pdesc: &Procdesc, pre_post: &mut PrePost) -> bool {
    matches!(
        pre_post.diagnostic.as_ref(),
        Some(Diagnostic::AccessToInvalidAddress {
            invalidation: crate::invalidation::Invalidation::ConstantDereference(_),
            ..
        })
    ) && pre_post_has_caller_visible_invalid_access(pdesc, pre_post)
        && !pre_post_diag_addr_has_non_null_invalidation(pre_post)
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

    let is_allocatedish =
        |v: AbstractValue| {
            let repr = pre_post.post.path_condition.get_var_repr(v);
            pre_post.post.post.attrs.get(&repr).is_some_and(|attrs| {
                attrs.get_allocated().is_some() || attrs.get_invalid().is_some()
            }) || pre_post.post.must_be_valid.contains(&repr)
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
    use crate::formula::atom::Atom;
    use crate::formula::lin_arith::LinArith;
    use crate::formula::term::Term;
    use crate::value_history::ValueHistory;
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

    fn make_pdesc_with_formals(formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|name| (Mangled::from_string(*name), Typ::void(), Default::default()))
            .collect();
        pdesc
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
}
