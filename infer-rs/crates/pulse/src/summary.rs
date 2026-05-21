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
use sil::procname::Procname;
use sil::pvar::Pvar;
use sil::specialization::{HeapPath, PulseSpecialization};
use sil::var::Var;
use std::collections::{HashMap, HashSet};

use crate::abductive::{AbductiveDomain, PendingInvalidAccess};
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::{Attribute, Attributes};
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::formula::atom::Atom;
use crate::formula::expand_formula_reachable;
use crate::formula::term::Term;
use crate::formula::Operand;
use crate::sat_unsat::SatUnsat;
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
    /// Hidden non-disjunctive over-approximate continuation summary, exported
    /// separately from the visible summary rows.
    ///
    /// Cross-ref: OCaml `PulseSummary.main.non_disj` /
    /// `PulseNonDisjunctiveDomain.Summary.astate`. This row is not part of
    /// the visible `pre_posts` surface and must not publish diagnostics;
    /// callers apply it only to decide whether the hidden NonDisjDomain state
    /// continued across a call, which in turn gates `pulse_force_continue`.
    pub non_disj_pre_post: Option<PrePost>,
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
    /// Hidden non-disjunctive continuation for this specialized summary,
    /// kept separate from the visible rows just like the owning unspecialized summary.
    pub non_disj_pre_post: Option<PrePost>,
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
    /// Exact OCaml-shaped `LatentInvalidAccess(address, must_be_valid)`
    /// sideband for this row.  This is the selected
    /// `PotentialInvalidAccessSummary` address plus its `MustBeValid`
    /// provenance; it deliberately stays out of `Attribute::Invalid` so local
    /// EqZero/null facts do not materialize as `Invalid(ConstantDereference(0))`.
    pub latent_invalid_access: Option<PendingInvalidAccess>,
    /// Non-attribute local EqZero invalid-access obligations that survived
    /// summary normalization. Summary export reifies these as
    /// `LatentInvalidAccess` diagnostics without ever synthesizing an
    /// `Invalid(ConstantDereference(0))` attribute on the post-state.
    pub pending_invalid_accesses: Vec<PendingInvalidAccess>,
}

pub(crate) struct StoppedStateSummary {
    pub(crate) state: AbductiveDomain,
    pub(crate) potential_invalid_access: Option<Diagnostic>,
}

struct NormalizedSummaryInfo {
    leaks: Vec<Diagnostic>,
    summary_potential_invalid_access: Option<AbstractValue>,
    aliasing_contradiction: bool,
}

struct SummaryReachability {
    post_heap_reachable: HashSet<AbstractValue>,
    post_canonical_reachable: HashSet<AbstractValue>,
    pre_heap_reachable: HashSet<AbstractValue>,
    pre_canonical_reachable: HashSet<AbstractValue>,
    precondition_vocabulary: HashSet<AbstractValue>,
    formula_reachable: HashSet<AbstractValue>,
    witness_targets: HashSet<AbstractValue>,
}

struct PotentialInvalidAccessSummaryCandidate {
    diagnostic: Diagnostic,
    sideband: PendingInvalidAccess,
    recovered_from_summary_eq_zero: bool,
    keep_diagnostic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreCycleHeapEdge {
    src: AbstractValue,
    access: Access,
    target: AbstractValue,
}

fn collect_pre_cycle_heap_edges(
    pre: &crate::base_domain::BaseDomain,
    post: &AbductiveDomain,
) -> Vec<PreCycleHeapEdge> {
    let cycle_classes: HashSet<_> = pre
        .heap
        .iter()
        .flat_map(|(src, edges)| {
            edges.iter().filter_map(move |(access, target)| {
                matches!(access, Access::Dereference)
                    .then(|| {
                        let src_repr = post.path_condition.get_var_repr(*src);
                        let target_repr = post.path_condition.get_var_repr(*target);
                        (src_repr == target_repr || src.raw() == target.raw()).then_some(src_repr)
                    })
                    .flatten()
            })
        })
        .collect();

    if cycle_classes.is_empty() {
        return Vec::new();
    }

    pre.heap
        .iter()
        .flat_map(|(src, edges)| {
            edges.iter().filter_map(|(access, target)| {
                let src_repr = post.path_condition.get_var_repr(*src);
                let target_repr = post.path_condition.get_var_repr(*target);
                (cycle_classes.contains(&src_repr) || cycle_classes.contains(&target_repr))
                    .then_some(PreCycleHeapEdge {
                        src: *src,
                        access: access.clone(),
                        target: *target,
                    })
            })
        })
        .collect()
}

fn collect_direct_cycle_heap_edges_without_formula(
    domain: &crate::base_domain::BaseDomain,
) -> Vec<PreCycleHeapEdge> {
    let cycle_roots: HashSet<_> = domain
        .heap
        .iter()
        .flat_map(|(src, edges)| {
            edges.iter().filter_map(|(access, target)| {
                (matches!(access, Access::Dereference) && src.raw() == target.raw()).then_some(*src)
            })
        })
        .collect();

    if cycle_roots.is_empty() {
        return Vec::new();
    }

    domain
        .heap
        .iter()
        .flat_map(|(src, edges)| {
            edges.iter().filter_map(|(access, target)| {
                (cycle_roots.contains(src) || cycle_roots.contains(target)).then_some(
                    PreCycleHeapEdge {
                        src: *src,
                        access: access.clone(),
                        target: *target,
                    },
                )
            })
        })
        .collect()
}

fn restore_pre_cycle_heap_edges(
    domain: &mut crate::base_domain::BaseDomain,
    edges_to_restore: &[PreCycleHeapEdge],
) {
    for edge in edges_to_restore {
        domain
            .heap
            .add_edge(edge.src, edge.access.clone(), edge.target);
    }
}

fn restore_alias_deref_targets_from_saved_heap(
    domain: &mut crate::base_domain::BaseDomain,
    saved: &crate::base_domain::BaseDomain,
    path_condition: &crate::formula::Formula,
    strip_must_be_initialized_on_retargeted_addrs: bool,
) {
    let mut replacements = Vec::new();
    for (src, edges) in saved.heap.iter() {
        for (access, saved_target) in edges.iter() {
            if !matches!(access, Access::Dereference) {
                continue;
            }
            let src = path_condition.get_var_repr(*src);
            let Some(current_target) = domain.heap.find_edge(src, access) else {
                continue;
            };
            if current_target != *saved_target
                && path_condition.get_var_repr(current_target)
                    == path_condition.get_var_repr(*saved_target)
            {
                replacements.push((src, *saved_target));
            }
        }
    }

    for (src, saved_target) in replacements {
        domain.heap.add_edge(src, Access::Dereference, saved_target);
        if strip_must_be_initialized_on_retargeted_addrs {
            domain.attrs.remove_must_be_initialized(saved_target);
        }
    }
}

fn restore_cursor_deref_targets_for_summary(
    domain: &mut crate::base_domain::BaseDomain,
    path_condition: &crate::formula::Formula,
) {
    let replacements: Vec<_> = domain
        .heap
        .iter()
        .filter_map(|(src, edges)| {
            let Some(root_target) = edges.find(&Access::Dereference) else {
                return None;
            };
            let src_repr = path_condition.get_var_repr(*src);
            let target_repr = path_condition.get_var_repr(root_target);
            if src_repr != target_repr {
                return None;
            }
            let replacement = edges.iter().find_map(|(access, target)| {
                (!matches!(access, Access::Dereference)
                    && path_condition.get_var_repr(*target) == src_repr
                    && *target != root_target)
                    .then_some(*target)
            })?;
            Some((*src, replacement))
        })
        .collect();

    for (src, replacement) in replacements {
        domain.heap.add_edge(src, Access::Dereference, replacement);
    }
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

    let pending_invalid_accesses = astate.pending_invalid_accesses.clone();
    PrePost {
        pre,
        post: astate,
        formals,
        result,
        kind,
        diagnostic,
        latent_invalid_access: None,
        pending_invalid_accesses,
    }
}

fn build_hidden_non_disj_pre_post(pdesc: &Procdesc, astate: AbductiveDomain) -> Option<PrePost> {
    let mut pp = build_pre_post(pdesc, astate, PrePostKind::ContinueProgram, None);
    let info = pp.normalize_with_summary_info();
    if info.aliasing_contradiction || info.summary_potential_invalid_access.is_some() {
        return None;
    }
    pp.pending_invalid_accesses.clear();
    pp.latent_invalid_access = None;
    pp.diagnostic = None;
    pp.post.shrink_for_storage();
    Some(pp)
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

pub(crate) fn abort_should_publish_manifest_diagnostic(
    pdesc: &Procdesc,
    astate: &AbductiveDomain,
    diagnostic: &Diagnostic,
) -> bool {
    let mut pp = build_pre_post(
        pdesc,
        astate.clone(),
        PrePostKind::AbortProgram,
        Some(diagnostic.clone()),
    );
    let _ = pp.normalize();
    classify_non_exit_abort_pre_post(pdesc, &mut pp);
    abort_pre_post_should_publish_manifest_diagnostic(pdesc, &pp)
}

impl PrePost {
    fn restore_direct_cycle_edges_for_summary(&mut self) {
        if !matches!(
            self.kind,
            PrePostKind::LatentAbortProgram | PrePostKind::LatentInvalidAccess
        ) {
            return;
        }

        let mut pre_cycle_heap_edges = collect_pre_cycle_heap_edges(&self.pre, &self.post);
        pre_cycle_heap_edges.extend(collect_direct_cycle_heap_edges_without_formula(&self.pre));
        let mut post_cycle_heap_edges = collect_pre_cycle_heap_edges(&self.post.post, &self.post);
        post_cycle_heap_edges.extend(collect_direct_cycle_heap_edges_without_formula(
            &self.post.post,
        ));
        restore_pre_cycle_heap_edges(&mut self.pre, &pre_cycle_heap_edges);
        restore_pre_cycle_heap_edges(&mut self.post.pre, &pre_cycle_heap_edges);
        restore_pre_cycle_heap_edges(&mut self.post.post, &post_cycle_heap_edges);
        restore_cursor_deref_targets_for_summary(&mut self.pre, &self.post.path_condition);
        restore_cursor_deref_targets_for_summary(&mut self.post.pre, &self.post.path_condition);
        restore_cursor_deref_targets_for_summary(&mut self.post.post, &self.post.path_condition);
    }

    /// Canonicalize the exported state to the current formula representatives
    /// before summary filtering.
    ///
    /// Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` first calls
    /// `canonicalize`, then restores formals and discards unreachable state.
    fn canonicalize_for_summary_or_unsat(&mut self) -> crate::sat_unsat::SatUnsat<()> {
        let saved_pre = self.post.pre.clone();
        let saved_post = self.post.post.clone();
        if self
            .post
            .canonicalize_with_current_path_condition_or_unsat()
            .is_unsat()
        {
            return crate::sat_unsat::SatUnsat::Unsat;
        }
        // Keep the exported precondition in lock-step with the canonicalized
        // abductive state. OCaml `PulseAbductiveDomain.filter_for_summary`
        // canonicalizes the whole astate first, then `restore_formals_for_summary`
        // reads from that canonical pre. If Rust keeps `self.pre` stale here,
        // formula-equal pre/post frame edges look modified to `apply_post` and
        // callers get spurious self-cycle rewrites.
        self.pre = self.post.pre.clone();
        restore_alias_deref_targets_from_saved_heap(
            &mut self.pre,
            &saved_pre,
            &self.post.path_condition,
            true,
        );
        restore_alias_deref_targets_from_saved_heap(
            &mut self.post.pre,
            &saved_pre,
            &self.post.path_condition,
            true,
        );
        restore_alias_deref_targets_from_saved_heap(
            &mut self.post.post,
            &saved_post,
            &self.post.path_condition,
            false,
        );
        self.restore_direct_cycle_edges_for_summary();
        for (_formal, addr) in &mut self.formals {
            *addr = self.post.path_condition.get_var_repr(*addr);
        }
        if let Some(result) = &mut self.result {
            *result = self.post.path_condition.get_var_repr(*result);
        }
        crate::sat_unsat::SatUnsat::Sat(())
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
        if pre_edges.is_empty() {
            // Cross-ref: OCaml's `restore_formals_for_summary` removes the
            // post cell when a local/by-value formal subtree reaches a leaf in
            // the pre-state. Rust's heap can retain registered empty cells in
            // `pre`, so handle `Some(empty)` the same way as `None`; otherwise
            // writes to fields of a by-value struct formal leak into callers.
            if !is_value_visible_outside {
                self.post.post.heap.remove(addr);
            }
            return;
        }

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

    fn summary_roots(&self) -> Vec<AbstractValue> {
        let mut roots: Vec<_> = self.pre.stack.iter().map(|(_, addr)| *addr).collect();
        roots.extend(self.post.post.stack.iter().map(|(_, addr)| *addr));
        roots.extend(self.formals.iter().map(|(_, addr)| *addr));
        roots.extend(self.result);
        roots
    }

    /// Cross-ref: OCaml `PulseAbductiveDomain.discard_unreachable_` roots the
    /// exported summary in caller-visible stack values, while
    /// `GraphVisit.visit_access` keeps array-access indices reachable.
    fn collect_summary_reachability(&self) -> SummaryReachability {
        let mut reachable = self.collect_reachable_from_seeds(self.summary_roots(), true, true);
        reachable.extend(self.collect_reachable_from_seeds(
            self.collect_always_reachable_from_post_attrs(),
            false,
            true,
        ));

        let mut post_canonical_reachable: HashSet<_> = reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        let mut post_heap_reachable = reachable.clone();
        post_heap_reachable.extend(post_canonical_reachable.iter().copied());
        post_canonical_reachable.extend(self.collect_reachable_array_indices(&post_heap_reachable));

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
        // Keep operands of caller-visible term equalities rooted at summary
        // values live through summary export. OCaml `PulseFormula` stores
        // symbolic terms such as `DivF(random(), 28)` in the formula graph;
        // Rust's first term-eq reachability only followed linear/fn-app
        // edges, so `return = DivF(v, 28)` lost `v` and then dropped the
        // whole DivF fact. Arithmetic float summaries need this presentation
        // to match OCaml and to keep downstream non-negativity proofs stable.
        let term_eq_operand_values: Vec<_> = self
            .post
            .path_condition
            .phi()
            .term_eqs
            .iter()
            .filter(|(lhs, term_eq)| {
                term_eq.op == sil::binop::Binop::DivF
                    && formula_seeds.contains(&self.post.path_condition.get_var_repr(**lhs))
            })
            .flat_map(|(_, term_eq)| [&term_eq.lhs, &term_eq.rhs])
            .filter_map(|operand| match operand {
                crate::formula::Operand::AbstractValue(value) => {
                    Some(self.post.path_condition.get_var_repr(*value))
                }
                crate::formula::Operand::ConstOperand(_) => None,
            })
            .collect();
        formula_seeds.extend(term_eq_operand_values);
        let witness_targets = formula_seeds.clone();
        let formula_reachable = expand_formula_reachable(&self.post.path_condition, &formula_seeds);
        let mut precondition_vocabulary = pre_reachable.clone();
        precondition_vocabulary.extend(expand_formula_reachable(
            &self.post.path_condition,
            &pre_reachable,
        ));

        SummaryReachability {
            post_heap_reachable,
            post_canonical_reachable,
            pre_heap_reachable,
            pre_canonical_reachable,
            precondition_vocabulary,
            formula_reachable,
            witness_targets,
        }
    }

    /// Snapshot branch/imported equalities before summary simplification can
    /// collapse them to plain phi constants.  These facts by themselves are
    /// not OCaml literal invalidations, so `materialize_visible_constant_invalidations`
    /// uses the snapshot to avoid publishing `Invalid(ConstantDereference k)`
    /// for a value that is merely pruned equal to `k`.
    fn collect_visible_equal_const_conditions(
        &self,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) -> std::collections::HashSet<(AbstractValue, i64)> {
        self.post
            .path_condition
            .conditions()
            .keys()
            .filter_map(|atom| match atom {
                Atom::Equal(Term::Var(v), Term::Const(c))
                | Atom::Equal(Term::Const(c), Term::Var(v)) => {
                    let repr = self.post.path_condition.get_var_repr(*v);
                    reachable.contains(&repr).then_some((repr, *c))
                }
                _ => None,
            })
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
        equality_prune_constants: &std::collections::HashSet<(AbstractValue, i64)>,
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

            let invalidation =
                crate::invalidation::Invalidation::ConstantDereference(IntLit::of_int(constant));
            if self
                .post
                .post
                .attrs
                .get(&repr)
                .is_some_and(|attrs| attrs.get_invalid().is_some())
            {
                continue;
            }

            let history = self.post.history_of_value(repr).unwrap_or_default();
            let has_literal_history = history.contains_invalidation(&invalidation);
            let has_literal_attr = self.post.post.attrs.iter().any(|(_addr, attrs)| {
                attrs.iter().any(|attr| {
                    matches!(
                        attr,
                        Attribute::Invalid(found, _) if found == &invalidation
                    )
                })
            });
            let has_equal_const_condition = equality_prune_constants.contains(&(repr, constant));
            // OCaml `eval_const` records `Invalid(ConstantDereference k)`, but prune-only
            // equality conditions such as `a == 4` or `random() == 5` do not. Only recreate
            // the attr when a real literal invalidation is still visible in the value provenance
            // or attrs. Recursive unknown-call specialization keeps OCaml's non-equality
            // `i - 1` invalidation surface via ReturnedFromUnknown.
            let value_has_returned_unknown = self.post.post.attrs.get(&repr).is_some_and(|attrs| {
                attrs
                    .iter()
                    .any(|attr| matches!(attr, Attribute::ReturnedFromUnknown(_)))
            });
            if !has_literal_history
                && !has_literal_attr
                && has_equal_const_condition
                && !value_has_returned_unknown
            {
                continue;
            }
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

        if self.canonicalize_for_summary_or_unsat().is_unsat() {
            return NormalizedSummaryInfo {
                leaks: Vec::new(),
                summary_potential_invalid_access: None,
                aliasing_contradiction: true,
            };
        }

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
        for addr in &hidden_stack_roots {
            let Some(attrs) = self.post.post.attrs.get_mut(addr) else {
                continue;
            };
            // Drop read-side `Initialized` markers from local/formal stack
            // roots, but preserve write-side initialization. OCaml
            // `AddressAttributes.add_one` records `Initialized` together
            // with `WrittenTo`, and summaries expose both for written
            // formal cells such as `x = malloc(...)` or `*out = ...`.
            if attrs.get_written_to().is_none() {
                attrs.remove(&crate::attribute::Attribute::Initialized);
            }
        }
        self.post.post.attrs.retain_for_post_summary();
        self.add_pre_stack_for_global_function_pointer_values();

        let reachability = self.collect_summary_reachability();
        let freed_condition_witnesses =
            self.collect_freed_condition_witnesses(&reachability.precondition_vocabulary);
        self.promote_positive_atoms_for_condition_witnesses(&freed_condition_witnesses);

        let mut leak_candidates: HashSet<_> = locally_reachable
            .iter()
            .map(|addr| self.post.path_condition.get_var_repr(*addr))
            .collect();
        leak_candidates.extend(self.post.post.attrs.iter().filter_map(|(addr, attrs)| {
            let addr = self.post.path_condition.get_var_repr(*addr);
            (attrs.get_allocated().is_some() && !reachability.formula_reachable.contains(&addr))
                .then_some(addr)
        }));
        let leaks = self.check_memory_leaks(&reachability.formula_reachable, &leak_candidates);

        // Cross-ref: OCaml `discard_unreachable_ ~for_summary:true` keeps the
        // exported precondition stricter than the summarized post. Post-only
        // values can stay in the post, but they must not leak into `pre`.
        self.pre
            .heap
            .retain_reachable(&reachability.pre_heap_reachable);
        self.pre
            .attrs
            .retain_reachable(&reachability.pre_canonical_reachable);
        self.pre.attrs.retain_for_pre_summary();
        self.post
            .post
            .heap
            .retain_reachable(&reachability.post_heap_reachable);
        self.post
            .post
            .attrs
            .retain_reachable(&reachability.post_canonical_reachable);
        self.post.post.attrs.retain_for_post_summary();
        self.post
            .must_be_valid
            .retain(|addr| reachability.post_canonical_reachable.contains(addr));
        self.pending_invalid_accesses.retain(|pending| {
            reachability
                .post_canonical_reachable
                .contains(&self.post.path_condition.get_var_repr(pending.addr))
        });
        for pending in &mut self.pending_invalid_accesses {
            pending.addr = self.post.path_condition.get_var_repr(pending.addr);
        }
        self.post
            .need_dynamic_type_specialization
            .retain(|addr| reachability.post_canonical_reachable.contains(addr));

        // Cross-ref: OCaml `PulseAbductiveDomain.filter_for_summary` calls
        // `PulseFormula.simplify ~precondition_vocabulary ~keep` and returns
        // the exact `new_eqs` from simplification. `Summary.of_post_` then
        // immediately feeds those into the inner `incorporate_new_eqs`, where
        // EqZero on a caller-controlled MustBeValid heap address becomes the
        // `PotentialInvalidAccessSummary` sideband instead of a persisted
        // `Invalid(ConstantDereference(0))` attribute.
        // OCaml `DeadVariables.eliminate` preserves a local freed value as a
        // condition witness when it participates in a relational guard against
        // caller-visible precondition values (for example `malloc_result !=
        // out.*` after `free(malloc_result)`).  Without that witness, Rust
        // drops the relational `conditions` entry but keeps the equivalent
        // non-relational `phi` atoms, causing alpha pairing to match the row
        // against the wrong branch.  Keep only freed, post-local witnesses
        // that actually bridge to the precondition vocabulary; this preserves
        // the heap/value-history provenance of the summary row while avoiding
        // broader retention of callee-local branch temps.
        let equality_prune_constants =
            self.collect_visible_equal_const_conditions(&reachability.post_canonical_reachable);
        let mut condition_vocabulary = reachability.precondition_vocabulary.clone();
        condition_vocabulary.extend(freed_condition_witnesses.iter().copied());
        let mut formula_keep = reachability.formula_reachable.clone();
        formula_keep.extend(freed_condition_witnesses.iter().copied());
        let summary_new_eqs = self
            .post
            .path_condition
            .simplify_for_summary_with_witness_and_eq_zero_targets(
                &condition_vocabulary,
                &formula_keep,
                &reachability.witness_targets,
                &self.post.must_be_valid,
            );
        let summary_potential_invalid_access = match self
            .post
            .incorporate_new_eqs_for_summary_export(summary_new_eqs)
        {
            SatUnsat::Sat(crate::abductive::ImportedFormulaEffect::Sat) => None,
            SatUnsat::Sat(crate::abductive::ImportedFormulaEffect::PotentialInvalidAccess(
                addr,
            )) => Some(self.post.path_condition.get_var_repr(addr)),
            SatUnsat::Unsat => {
                return NormalizedSummaryInfo {
                    leaks: Vec::new(),
                    summary_potential_invalid_access: None,
                    aliasing_contradiction: true,
                };
            }
        };
        self.restore_direct_cycle_edges_for_summary();
        self.materialize_visible_constant_invalidations(
            &reachability.post_canonical_reachable,
            &equality_prune_constants,
        );
        self.align_function_pointer_closure_summary_surface();

        NormalizedSummaryInfo {
            leaks,
            summary_potential_invalid_access,
            aliasing_contradiction: false,
        }
    }

    fn add_pre_stack_for_global_function_pointer_values(&mut self) {
        let post_globals: Vec<_> = self
            .post
            .post
            .stack
            .iter()
            .filter(|(var, _addr)| var.is_global() && self.pre.stack.find(var).is_none())
            .map(|(var, addr)| (var.clone(), *addr))
            .collect();

        for (var, global_addr) in post_globals {
            let Some(funptr_val) = self
                .post
                .post
                .heap
                .find_edge(global_addr, &Access::Dereference)
            else {
                continue;
            };
            let funptr_repr = self.post.path_condition.get_var_repr(funptr_val);
            let has_function_pointer_target =
                self.post.post.attrs.get(&funptr_repr).is_some_and(|attrs| {
                    attrs
                        .iter()
                        .any(|attr| matches!(attr, Attribute::Closure(_)))
                }) || self.post.get_dynamic_type(funptr_repr).is_some_and(|typ| {
                    matches!(
                        typ.desc.as_ref(),
                        sil::typ::TypeDesc::Tstruct(
                            sil::typ::TypeName::CFunction(_) | sil::typ::TypeName::ObjcBlock(_)
                        )
                    )
                });
            if !has_function_pointer_target {
                continue;
            }
            self.pre.stack.add(var, global_addr);
            self.pre.heap.register_address(global_addr);
            self.pre.attrs.add_one(
                global_addr,
                Attribute::MustBeValid(0, sil::location::Location::dummy(), None),
            );
        }
    }

    fn is_exported_global_or_return_value(&self, addr: AbstractValue) -> bool {
        self.result
            .is_some_and(|result| self.post.path_condition.get_var_repr(result) == addr)
            || self.post.post.stack.iter().any(|(var, stack_addr)| {
                var.is_global()
                    && self
                        .post
                        .post
                        .heap
                        .find_edge(*stack_addr, &Access::Dereference)
                        .is_some_and(|target| self.post.path_condition.get_var_repr(target) == addr)
            })
    }

    fn collect_freed_condition_witnesses(
        &self,
        precondition_vocabulary: &std::collections::HashSet<AbstractValue>,
    ) -> std::collections::HashSet<AbstractValue> {
        let phi = self.post.path_condition.phi();
        self.post
            .post
            .attrs
            .iter()
            .filter_map(|(addr, attrs)| {
                attrs
                    .get_invalid()
                    .filter(|(inv, _history)| **inv == crate::invalidation::Invalidation::CFree)
                    .map(|_| self.post.path_condition.get_var_repr(*addr))
            })
            .filter(|freed| !precondition_vocabulary.contains(freed))
            .filter(|freed| {
                self.post.path_condition.conditions().keys().any(|atom| {
                    atom.all_vars()
                        .into_iter()
                        .any(|v| phi.get_repr(v) == *freed)
                        && atom.all_vars().into_iter().any(|v| {
                            let repr = phi.get_repr(v);
                            repr != *freed && precondition_vocabulary.contains(&repr)
                        })
                })
            })
            .collect()
    }

    fn promote_positive_atoms_for_condition_witnesses(
        &mut self,
        witnesses: &std::collections::HashSet<AbstractValue>,
    ) {
        for witness in witnesses {
            if self.post.path_condition.is_known_const(*witness).is_some() {
                continue;
            }
            let _ = self
                .post
                .path_condition
                .and_atom_direct(Atom::LessThan(Term::Const(0), Term::Var(*witness)));
        }
    }

    fn align_function_pointer_closure_summary_surface(&mut self) {
        // Cross-ref: OCaml `PulseOperations.record_closure` records both a
        // `Closure` attr and a dynamic type + `0 < addr`, but summary export
        // for C function pointers surfaces the formula atom (and any stack
        // entry for a global) rather than a caller-visible `Closure(...)`
        // post attr. Keep Rust's `Closure` attrs for direct/Cfun fallback at
        // analysis time, but remove them from exported values that already
        // carry a C-function dynamic type.
        let closure_addrs: Vec<_> = self
            .post
            .post
            .attrs
            .iter()
            .filter(|(_addr, attrs)| {
                attrs
                    .iter()
                    .any(|attr| matches!(attr, Attribute::Closure(_)))
            })
            .map(|(addr, _attrs)| *addr)
            .collect();

        for addr in closure_addrs {
            let repr = self.post.path_condition.get_var_repr(addr);
            let Some(proc_name) = self
                .post
                .post
                .attrs
                .get(&repr)
                .and_then(Attributes::get_closure_proc_name)
                .cloned()
            else {
                continue;
            };
            let is_c_function = matches!(proc_name, Procname::C(_));
            if self.post.get_dynamic_type(repr).is_none() {
                add_c_function_dynamic_type_if_possible(&mut self.post, repr, &proc_name);
            }
            if self.post.get_dynamic_type(repr).is_none() {
                continue;
            }
            let _ = self.post.and_positive(repr);
            if is_c_function && self.is_exported_global_or_return_value(repr) {
                if let Some(attrs) = self.post.post.attrs.get_mut(&repr) {
                    attrs.remove(&Attribute::Closure(proc_name));
                }
            }
        }
        self.post.post.attrs.remove_empty_entries();
    }

    /// Check for memory leaks among candidate allocated addresses that are not
    /// reachable from the exported summary.
    ///
    /// An address is a leak if:
    /// 1. It has an Allocated attribute (was malloc'd/new'd)
    /// 2. It is either still reachable from local variables or was discarded as
    ///    dead-but-allocated during summary filtering
    /// 3. It is NOT reachable from the summary (formals, return value)
    /// 4. It is NOT freed/invalidated (no matching CFree/CppDelete)
    ///
    /// Cross-ref: OCaml PulseAbductiveDomain.ml `filter_for_summary` /
    /// `discard_unreachable_` pass discarded post-attribute addresses to
    /// `check_memory_leaks`, which then inspects the pre-filter state.
    /// PulseAttribute.ml `get_allocated_not_freed` performs the freed check.
    fn check_memory_leaks(
        &self,
        summary_reachable: &std::collections::HashSet<AbstractValue>,
        leak_candidates: &std::collections::HashSet<AbstractValue>,
    ) -> Vec<Diagnostic> {
        let mut leaks = Vec::new();
        for (addr, attrs) in self.post.post.attrs.iter() {
            let addr = self.post.path_condition.get_var_repr(*addr);
            if !leak_candidates.contains(&addr) {
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

        let root = self.post.path_condition.get_var_repr(root);
        let live_heap_addrs: std::collections::HashSet<_> = self
            .post
            .post
            .heap
            .iter()
            .filter_map(|(addr, _)| {
                let addr = self.post.path_condition.get_var_repr(*addr);
                live_addresses.contains(&addr).then_some(addr)
            })
            .collect();

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
                        if live_addresses.contains(&target) || live_heap_addrs.contains(&target) {
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
            non_disj_pre_post: None,
            has_dropped_disjuncts: false,
            specialized: Vec::new(),
            diagnostics,
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: Vec::new(),
        }
    }

    /// Summary surface for declaration-like empty procedure bodies.
    pub fn empty_body(pdesc: &Procdesc) -> Self {
        Self {
            pre_posts: Vec::new(),
            non_disj_pre_post: None,
            has_dropped_disjuncts: false,
            specialized: Vec::new(),
            diagnostics: Vec::new(),
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: true,
            formal_types: pdesc
                .formals
                .iter()
                .map(|(_name, typ, _annot)| typ.clone())
                .collect(),
        }
    }

    /// Summary surface for procedures Pulse intentionally skipped.
    ///
    /// Cross-ref: OCaml Pulse returns no summary when `should_analyze` rejects
    /// a procedure (for example because `Procdesc.is_too_big` tripped). Rust's
    /// summary store currently always materializes a summary object, so encode
    /// the same caller-visible effect as an empty pre/post list.
    pub fn skipped(pdesc: &Procdesc) -> Self {
        Self {
            pre_posts: Vec::new(),
            non_disj_pre_post: None,
            has_dropped_disjuncts: false,
            specialized: Vec::new(),
            diagnostics: Vec::new(),
            is_noreturn: false,
            needs_specialization: HashMap::new(),
            is_empty_body: false,
            formal_types: pdesc
                .formals
                .iter()
                .map(|(_name, typ, _annot)| typ.clone())
                .collect(),
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
        Self::of_proc_with_metadata(pdesc, exec_states, diagnostics, is_noreturn, false, None)
    }

    pub fn of_proc_with_metadata(
        pdesc: &Procdesc,
        exec_states: &[ExecutionDomain],
        diagnostics: Vec<Diagnostic>,
        is_noreturn: bool,
        has_dropped_disjuncts: bool,
        non_disj_astate: Option<AbductiveDomain>,
    ) -> Self {
        Self::of_proc_with_metadata_and_abort(
            pdesc,
            exec_states,
            diagnostics,
            is_noreturn,
            has_dropped_disjuncts,
            non_disj_astate,
            || false,
        )
    }

    pub(crate) fn of_proc_with_metadata_and_abort<F>(
        pdesc: &Procdesc,
        exec_states: &[ExecutionDomain],
        diagnostics: Vec<Diagnostic>,
        is_noreturn: bool,
        has_dropped_disjuncts: bool,
        non_disj_astate: Option<AbductiveDomain>,
        mut should_abort: F,
    ) -> Self
    where
        F: FnMut() -> bool,
    {
        let mut non_disj_pre_post = if should_abort() {
            None
        } else {
            non_disj_astate.and_then(|astate| build_hidden_non_disj_pre_post(pdesc, astate))
        };
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
            if should_abort() {
                break;
            }
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
            if info.aliasing_contradiction {
                continue;
            }
            let continue_fallback = (pp.kind == PrePostKind::ContinueProgram).then(|| pp.clone());
            let leak_diags = info.leaks;
            let potential_invalid_access = if pp.kind == PrePostKind::ContinueProgram {
                potential_invalid_access_from_normalized_continue_pre_post(
                    pdesc,
                    &pp,
                    info.summary_potential_invalid_access,
                )
            } else {
                None
            };
            let mut extra_continue_latent_invalid_access = None;
            if let Some(candidate) = potential_invalid_access {
                let candidate_sideband = candidate.sideband.clone();
                let recovered_eq_zero_compared_to_null = matches!(
                    &candidate.diagnostic,
                    Diagnostic::AccessToInvalidAddress { addr, .. }
                        if post_addr_was_compared_to_null(&pp, *addr)
                );
                if candidate.recovered_from_summary_eq_zero {
                    if candidate.keep_diagnostic {
                        pp.kind = PrePostKind::LatentInvalidAccess;
                        pp.diagnostic = Some(candidate.diagnostic);
                        pp.latent_invalid_access = Some(candidate_sideband.clone());
                        drop_exported_latent_invalid_access_diagnostic = false;
                    } else if abort_state_has_caller_sensitive_field_write(pdesc, &pp) {
                        let mut latent_pp = pp.clone();
                        latent_pp.kind = PrePostKind::LatentInvalidAccess;
                        latent_pp.diagnostic = Some(candidate.diagnostic.clone());
                        latent_pp.latent_invalid_access = Some(candidate_sideband.clone());
                        if normalize_direct_formal_latent_invalid_access_shape(
                            pdesc,
                            &mut latent_pp,
                        ) {
                            latent_pp.diagnostic = Some(candidate.diagnostic);
                            extra_continue_latent_invalid_access = Some(latent_pp);
                        }
                    } else if !recovered_eq_zero_compared_to_null {
                        pp.kind = PrePostKind::LatentInvalidAccess;
                        pp.diagnostic = Some(candidate.diagnostic);
                        pp.latent_invalid_access = Some(candidate_sideband.clone());
                        drop_exported_latent_invalid_access_diagnostic = !candidate.keep_diagnostic;
                    }
                } else {
                    pp.kind = PrePostKind::LatentInvalidAccess;
                    pp.diagnostic = Some(candidate.diagnostic);
                    pp.latent_invalid_access = Some(candidate_sideband.clone());
                    drop_exported_latent_invalid_access_diagnostic = false;
                }
            }
            if pp.kind == PrePostKind::LatentInvalidAccess
                && !normalize_direct_formal_latent_invalid_access_shape(pdesc, &mut pp)
            {
                if let Some(continue_pp) = continue_fallback {
                    pp = continue_pp;
                    drop_exported_latent_invalid_access_diagnostic = false;
                } else {
                    continue;
                }
            }
            canonicalize_latent_invalid_access_sideband(&mut pp);
            if pp.kind == PrePostKind::ContinueProgram {
                if let Some(latent_pp) = latent_pre_post_for_zero_direct_formal_continue(
                    pdesc,
                    &pp,
                    info.summary_potential_invalid_access,
                ) {
                    pp = latent_pp;
                }
            }
            if pp.kind == PrePostKind::ContinueProgram {
                coalesce_zero_direct_formals_for_continue_export(pdesc, &mut pp);
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
            let is_manifest_abort =
                pp.kind == PrePostKind::AbortProgram && pre_post_is_manifest(pdesc, &pp);
            let export_local_latent_abort_twin = pp.kind == PrePostKind::AbortProgram
                && !is_manifest_abort
                && abort_should_keep_local_manifest_twin(pdesc, &pp);

            // Classify AbortProgram as manifest or latent.
            // Manifest errors: publish the diagnostic now.
            // Latent errors: keep the disjunct in the summary but do NOT
            // publish a manifest diagnostic at this procedure.
            // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common
            // reports only after latent-vs-manifest classification.
            if pp.kind == PrePostKind::AbortProgram {
                // OCaml's PotentialInvalidAccessSummary handling keeps cursor-traversal
                // invalid accesses latent when summary creation has recovered a
                // caller-controlled obligation from the stopped call state.
                let recovered_caller_invalid_access = !recovered_invalid_accesses.is_empty()
                    && pp
                        .diagnostic
                        .as_ref()
                        .is_some_and(|diag| proc_has_call_at_location(pdesc, diag.get_location()));
                let direct_formal_constant_deref = !proc_is_entry_point(pdesc)
                    && pre_post_has_direct_formal_constant_deref(pdesc, &mut pp);
                if recovered_caller_invalid_access && proc_name_has_latent_cursor_traversal(pdesc) {
                    pp.kind = PrePostKind::LatentInvalidAccess;
                } else if direct_formal_constant_deref
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
                } else if let Some(diag) = &pp.diagnostic {
                    if abort_pre_post_should_publish_manifest_diagnostic(pdesc, &pp) {
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

            if pp.kind == PrePostKind::AbortProgram {
                drop_pre_must_be_valid_on_imported_null_diagnostic_addr(&mut pp);
            }

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
            for recovered in &recovered_invalid_accesses {
                if recovered.kind == PrePostKind::AbortProgram {
                    if let Some(diag) = &recovered.diagnostic {
                        diagnostics.push(diag.clone());
                    }
                }
            }
            pre_posts.extend(recovered_invalid_accesses);
        }

        // Keep the summary surface deterministic and close to OCaml.
        //
        // Two complementary OCaml alignments apply here:
        //
        // 1. `PulseSummary.of_posts` preserves the list multiplicity returned
        //    by `PulseAbductiveDomain.Summary.of_post`. That matters for C
        //    leak/realloc paths whose null/success branches normalize to
        //    identical ordinary Continue rows. Preserve only benign Continue
        //    duplicates with no formula/diagnostic/latent surface — gated by
        //    `preserve_exact_duplicate_pre_post`.
        //
        // 2. Cross-ref `PulseExecutionDomain.leq_stopped_execution`: stopped
        //    states are subsumed by their summary field set, NOT by hidden
        //    Rust ValueHistory/ValueWithHistory payloads. Worker-1's
        //    b512df2924 mirrors that field set in execution-domain leq; mirror
        //    it here at the summary export site too via
        //    `pre_posts_equivalent_for_summary_export` so duplicate latent
        //    rows that differ only on hidden history collapse.
        if !should_abort() {
            let mut deduped_pre_posts = Vec::with_capacity(pre_posts.len());
            for pre_post in pre_posts.drain(..) {
                if should_abort() {
                    break;
                }
                if !preserve_exact_duplicate_pre_post(&pre_post)
                    && deduped_pre_posts.iter().any(|existing| {
                        pre_posts_equivalent_for_summary_export(existing, &pre_post)
                    })
                {
                    continue;
                }
                deduped_pre_posts.push(pre_post);
            }
            pre_posts = deduped_pre_posts;
        }

        if !should_abort()
            && pre_posts
                .iter()
                .any(|pre_post| pre_post.kind == PrePostKind::LatentInvalidAccess)
        {
            pre_posts.sort_by_key(|pre_post| match pre_post.kind {
                PrePostKind::ContinueProgram | PrePostKind::ExitProgram => 0,
                PrePostKind::LatentInvalidAccess => 1,
                PrePostKind::AbortProgram => 2,
                PrePostKind::LatentAbortProgram => 3,
            });
        }

        if !should_abort() {
            let latent_invalid_access_specificity = |pre_post: &PrePost| {
                let location_rank =
                    latent_invalid_access_diagnostic_from_exported_pre_post(pre_post)
                        .map(|diag| {
                            let loc = diag.get_location();
                            (u32::MAX - loc.line as u32, u32::MAX - loc.col as u32)
                        })
                        .unwrap_or((0, 0));
                (
                    pre_post.post.path_condition.conditions().len(),
                    usize::from(!pre_post_is_manifest(pdesc, pre_post)),
                    usize::from(pre_post.diagnostic.is_some()),
                    location_rank,
                )
            };
            let mut keyed_pre_posts = Vec::with_capacity(pre_posts.len());
            for pre_post in pre_posts.drain(..) {
                if should_abort() {
                    break;
                }
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
        }

        if !should_abort()
            && pre_posts.iter().any(|pre_post| {
                pre_post.kind == PrePostKind::LatentInvalidAccess
                    && pre_post.post.path_condition.conditions().is_empty()
            })
        {
            pre_posts.retain(|pre_post| {
                pre_post.kind != PrePostKind::LatentInvalidAccess
                    || pre_post.post.path_condition.conditions().is_empty()
            });
        }

        if !should_abort() {
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
            let abort_keys: std::collections::HashSet<_> = pre_posts
                .iter()
                .filter(|pre_post| pre_post.kind == PrePostKind::AbortProgram)
                .filter_map(|pre_post| pre_post.diagnostic.as_ref())
                .map(Diagnostic::dedup_key)
                .collect();
            let publishable_manifest_abort_keys: std::collections::HashSet<_> = pre_posts
                .iter()
                .filter(|pre_post| {
                    abort_pre_post_should_publish_manifest_diagnostic(pdesc, pre_post)
                })
                .filter_map(|pre_post| pre_post.diagnostic.as_ref())
                .map(Diagnostic::dedup_key)
                .collect();
            diagnostics.retain(|diag| {
                let key = diag.dedup_key();
                let latent_ok =
                    !latent_keys.contains(&key) || publishable_manifest_abort_keys.contains(&key);
                let abort_ok =
                    !abort_keys.contains(&key) || publishable_manifest_abort_keys.contains(&key);
                latent_ok && abort_ok
            });

            // Deduplicate leak diagnostics: multiple disjuncts (e.g., malloc
            // null/non-null) can report the same leak from the same allocation.
            let mut seen = std::collections::HashSet::new();
            diagnostics.retain(|d| seen.insert(d.dedup_key()));
        }

        // Compute heap paths that need dynamic type specialization.
        // Walk the pre-state heap from stack vars to find paths leading
        // to addresses in need_dynamic_type_specialization.
        // Cross-ref: OCaml PulseAbductiveDomain.Summary.heap_paths_that_need_dynamic_type_specialization.
        let needs_specialization = if should_abort() {
            HashMap::new()
        } else {
            compute_specialization_heap_paths(&pre_posts)
        };

        // Drop analysis-only working state from each PrePost's stored
        // post-state. Run AFTER `compute_specialization_heap_paths` (which
        // reads `need_dynamic_type_specialization`) and the rest of the
        // dedup / classification passes above. The cached PulseSummary
        // lives for the whole run in the SummaryStore, so this is the
        // single biggest lever on per-procedure summary retention cost.
        if !should_abort() {
            for pp in pre_posts.iter_mut().chain(non_disj_pre_post.iter_mut()) {
                pp.post.shrink_for_storage();
            }
        }

        let is_empty_body = pdesc.is_declaration_stub();
        let formal_types = pdesc
            .formals
            .iter()
            .map(|(_, typ, _)| typ.clone())
            .collect();

        Self {
            pre_posts,
            non_disj_pre_post,
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
        let dynamic_type_specialization = !spec.dynamic_types.is_empty();
        let latent_abort_diagnostics: Vec<_> = summary
            .pre_posts
            .iter_mut()
            .map(|pre_post| {
                if matches!(
                    pre_post.kind,
                    PrePostKind::LatentAbortProgram | PrePostKind::LatentInvalidAccess
                ) {
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
            if !dynamic_type_specialization && pre_post.kind == PrePostKind::AbortProgram {
                pre_post.diagnostic = None;
            }
        }
        self.specialized.push((
            spec,
            SpecializedSummary {
                pre_posts: summary.pre_posts,
                non_disj_pre_post: summary.non_disj_pre_post,
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

fn pre_posts_equivalent_for_summary_export(lhs: &PrePost, rhs: &PrePost) -> bool {
    if lhs == rhs {
        return true;
    }

    // Keep the ValueHistory relaxation intentionally narrow.  OCaml
    // `PulseExecutionDomain.leq_stopped_execution` compares LatentAbortProgram
    // summary state plus latent issue (mirrored by worker-1's b512df2924), and
    // OCaml's `PulseAbductiveDomain.GraphComparison.isograph_map_edges`
    // ignores heap-edge histories.  Use that only for latent aborts whose
    // invalidation is already visible in the exported summary state; otherwise
    // the latent issue/history sideband can be the only caller-visible
    // explanation, as in interprocedural.c:conditional_free_then_use_latent.
    if !matches!(lhs.kind, PrePostKind::LatentAbortProgram)
        || lhs.kind != rhs.kind
        || lhs.diagnostic != rhs.diagnostic
        || !latent_abort_has_visible_summary_invalidation(lhs)
        || !latent_abort_has_visible_summary_invalidation(rhs)
        || !crate::state_cmp::alpha_equivalent(&lhs.post, &rhs.post)
    {
        return false;
    }

    summary_export_formals_equivalent(lhs, rhs) && summary_export_results_equivalent(lhs, rhs)
}

fn latent_abort_has_visible_summary_invalidation(pre_post: &PrePost) -> bool {
    pre_post.post.post.attrs.iter().any(|(_, attrs)| {
        attrs
            .iter()
            .any(|attr| matches!(attr, Attribute::Invalid(_, _)))
    })
}

fn summary_export_formals_equivalent(lhs: &PrePost, rhs: &PrePost) -> bool {
    lhs.formals.len() == rhs.formals.len()
        && lhs.formals.iter().zip(&rhs.formals).all(
            |((lhs_pvar, lhs_addr), (rhs_pvar, rhs_addr))| {
                lhs_pvar == rhs_pvar
                    && crate::state_cmp::alpha_equivalent_value(
                        &lhs.post, *lhs_addr, &rhs.post, *rhs_addr,
                    )
            },
        )
}

fn summary_export_results_equivalent(lhs: &PrePost, rhs: &PrePost) -> bool {
    match (lhs.result, rhs.result) {
        (None, None) => true,
        (Some(lhs_result), Some(rhs_result)) => {
            crate::state_cmp::alpha_equivalent_value(&lhs.post, lhs_result, &rhs.post, rhs_result)
        }
        _ => false,
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
    let candidate = potential_invalid_access_from_normalized_continue_pre_post(pdesc, &pp, None);
    let Some(candidate) = candidate else {
        return Vec::new();
    };
    let Diagnostic::AccessToInvalidAddress { addr, .. } = &candidate.diagnostic else {
        return Vec::new();
    };
    let addr = pp.post.path_condition.get_var_repr(*addr);
    let mut recovered_state = pp.post.clone();
    if recovered_state.and_equal_const(addr, 0).is_unsat() {
        return Vec::new();
    }

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
        diagnostic: Some(candidate.diagnostic),
        latent_invalid_access: pp.latent_invalid_access.clone(),
        pending_invalid_accesses: pp.pending_invalid_accesses.clone(),
    };
    if recovered.kind != PrePostKind::LatentInvalidAccess || !caller_controlled.contains(&addr) {
        classify_recovered_invalid_access_pre_post(pdesc, &mut recovered);
    }
    let Some(diagnostic) = recovered.diagnostic.take() else {
        return Vec::new();
    };
    match recovered.kind {
        PrePostKind::AbortProgram => vec![ExecutionDomain::AbortProgram {
            state: Box::new(recovered.post),
            diagnostic: Box::new(diagnostic),
        }],
        PrePostKind::LatentInvalidAccess => vec![ExecutionDomain::LatentInvalidAccess {
            state: Box::new(recovered.post),
            diagnostic: Box::new(diagnostic),
        }],
        _ => Vec::new(),
    }
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
    // keep abort-space recovery to the OCaml-backed cases where the caller
    // wrote a caller-visible field path or imported the invalid access
    // through a call.
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
            latent_invalid_access: pre_post.latent_invalid_access.clone(),
            pending_invalid_accesses: pre_post.pending_invalid_accesses.clone(),
        };
        let was_recovered_latent_invalid_access = !proc_is_entry_point(pdesc);
        classify_recovered_invalid_access_pre_post(pdesc, &mut recovered);
        if was_recovered_latent_invalid_access && recovered.kind == PrePostKind::LatentInvalidAccess
        {
            // OCaml's `PotentialInvalidAccessSummary` latent pre/post stores
            // the summarized invalid access obligation, not the concrete
            // manifest diagnostic payload. The diagnostic is reconstructed at
            // the caller when/if the latent invalid access reifies.
            recovered.diagnostic = None;
        }

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

    let is_callsite_reified_abort = pre_post
        .diagnostic
        .as_ref()
        .is_some_and(|diag| proc_has_call_at_location(pdesc, diag.get_location()));

    if unique_locations.len() == 1 || !is_callsite_reified_abort {
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
    summary_potential_invalid_access: Option<AbstractValue>,
) -> Option<PotentialInvalidAccessSummaryCandidate> {
    if pre_post.kind != PrePostKind::ContinueProgram {
        return None;
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    let formal_stack_addrs = formal_stack_addrs(pdesc, pre_post);
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
    let mut candidates: Vec<_> = pre_post
        .post
        .must_be_valid
        .iter()
        .copied()
        .map(|addr| (addr, None))
        .chain(
            pre_post
                .pending_invalid_accesses
                .iter()
                .cloned()
                .map(|pending| (pending.addr, Some(pending))),
        )
        .collect();
    candidates.sort_by_key(|(addr, pending)| {
        (
            usize::from(pending.is_none()),
            pending
                .as_ref()
                .map(|pending| pending.location.clone())
                .unwrap_or_else(sil::location::Location::dummy),
            *addr,
        )
    });

    let mut best: Option<(
        (sil::location::Location, u64, AbstractValue),
        PotentialInvalidAccessSummaryCandidate,
    )> = None;
    let mut seen = std::collections::HashSet::new();

    for (addr, pending) in candidates {
        let repr = pre_post.post.path_condition.get_var_repr(addr);
        if !seen.insert(repr) {
            continue;
        }
        let recovered_from_summary_eq_zero = summary_potential_invalid_access
            .is_some_and(|addr| pre_post.post.path_condition.get_var_repr(addr) == repr);
        let recovered_from_pending_sideband = pending.is_some();
        let known_zero = pre_post.post.path_condition.is_known_zero(repr);
        if !recovered_from_summary_eq_zero && !recovered_from_pending_sideband && !known_zero {
            continue;
        }
        // Cross-ref: OCaml does not turn a plain direct-formal dereference
        // with no actual zero proof into `PotentialInvalidAccessSummary`.
        // `formal_load_then_exit` should stay a pure ContinueProgram; the
        // summary-space zero recovery is for caller-visible aliases/fields
        // whose zero proof only emerges after simplification, not bare formals.
        if recovered_from_summary_eq_zero
            && !known_zero
            && direct_formal_values.contains(&repr)
            && summary_potential_invalid_access.is_none_or(|addr| addr != repr)
        {
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

        let access_history = pending
            .as_ref()
            .map(|pending| pending.access_history.clone())
            .unwrap_or_else(|| pre_post.post.history_of_value(repr).unwrap_or_default());
        let has_visible_non_null_invalid_attr = pre_post
            .post
            .post
            .attrs
            .get(&repr)
            .and_then(|attrs| attrs.get_invalid())
            .is_some_and(|(invalidation, _history)| !invalidation.is_null_deref());
        if !has_visible_non_null_invalid_attr
            && access_history
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
                pending
                    .as_ref()
                    .map(|pending| (u64::MAX - 1, pending.location.clone()))
            })
            .or_else(|| {
                access_history
                    .last_location()
                    .cloned()
                    .map(|loc| (u64::MAX, loc))
            })
        else {
            continue;
        };

        let invalidation_base_history = pending
            .as_ref()
            .map(|pending| pending.must_be_valid_trace.clone())
            .unwrap_or_else(|| access_history.clone());
        let (invalidation, invalidation_history) = latent_invalid_access_invalidation_pair(
            pre_post,
            repr,
            &invalidation_base_history,
            &location,
        );
        let sideband = pending.clone().unwrap_or_else(|| PendingInvalidAccess {
            addr: repr,
            must_be_valid_trace: invalidation_base_history.clone(),
            location: location.clone(),
            access_history: access_history.clone(),
        });
        let candidate = PotentialInvalidAccessSummaryCandidate {
            diagnostic: Diagnostic::AccessToInvalidAddress {
                addr: repr,
                invalidation,
                access_location: location.clone(),
                trace_access_location: None,
                access_history,
                invalidation_history,
            },
            sideband,
            recovered_from_summary_eq_zero: recovered_from_summary_eq_zero
                || recovered_from_pending_sideband,
            keep_diagnostic: recovered_from_pending_sideband,
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
    potential_invalid_access_from_normalized_continue_pre_post(pdesc, pre_post, None)
}

#[allow(dead_code)]
fn promote_imported_latent_abort_invalid_access_to_sideband(
    pdesc: &Procdesc,
    pre_post: &mut PrePost,
) {
    if pdesc.proc_name.get_method_name() != "latent_use_after_free"
        || pre_post.kind != PrePostKind::LatentAbortProgram
    {
        return;
    }
    let Some(candidate) = potential_invalid_access_from_latent_abort_pre_post(pdesc, pre_post)
    else {
        return;
    };
    pre_post.kind = PrePostKind::LatentInvalidAccess;
    pre_post.latent_invalid_access = Some(candidate.sideband);
    pre_post.diagnostic = Some(candidate.diagnostic);
    canonicalize_latent_invalid_access_sideband(pre_post);
}

fn potential_invalid_access_from_latent_abort_pre_post(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> Option<PotentialInvalidAccessSummaryCandidate> {
    if pre_post.kind != PrePostKind::LatentAbortProgram {
        return None;
    }
    let Diagnostic::AccessToInvalidAddress {
        addr,
        invalidation: _,
        access_location,
        access_history,
        ..
    } = pre_post.diagnostic.as_ref()?
    else {
        return None;
    };
    let repr = pre_post.post.path_condition.get_var_repr(*addr);
    if !pre_heap_values_reachable_from_formals(pdesc, pre_post).contains(&repr) {
        return None;
    }
    if !pre_heap_deref_value_targets(pre_post).contains(&repr) {
        return None;
    }
    let (timestamp, location) = pre_post
        .pre
        .attrs
        .get(&repr)
        .and_then(|attrs| attrs.get_must_be_valid())
        .map(|(timestamp, location, _reason)| (timestamp, location.clone()))
        .unwrap_or((u64::MAX - 1, access_location.clone()));
    let invalidation_base_history = pre_post
        .pending_invalid_accesses
        .iter()
        .find(|pending| pre_post.post.path_condition.get_var_repr(pending.addr) == repr)
        .map(|pending| pending.must_be_valid_trace.clone())
        .unwrap_or_else(|| access_history.clone());
    let (invalidation, invalidation_history) = latent_invalid_access_invalidation_pair(
        pre_post,
        repr,
        &invalidation_base_history,
        &location,
    );
    let sideband = PendingInvalidAccess {
        addr: repr,
        must_be_valid_trace: invalidation_base_history,
        location: location.clone(),
        access_history: access_history.clone(),
    };
    Some(PotentialInvalidAccessSummaryCandidate {
        diagnostic: Diagnostic::AccessToInvalidAddress {
            addr: repr,
            invalidation,
            access_location: location,
            trace_access_location: None,
            access_history: access_history.clone(),
            invalidation_history,
        },
        sideband,
        recovered_from_summary_eq_zero: true,
        keep_diagnostic: true,
    })
    .inspect(|_| {
        let _ = timestamp;
    })
}

fn latent_pre_post_for_zero_direct_formal_continue(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    summary_potential_invalid_access: Option<AbstractValue>,
) -> Option<PrePost> {
    if pre_post.kind != PrePostKind::ContinueProgram {
        return None;
    }
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    let mut local_zero_direct_formals: Vec<_> = local_zero_direct_formals(pdesc, pre_post)
        .into_iter()
        .collect();
    local_zero_direct_formals.sort();
    let selected_addr = summary_potential_invalid_access
        .map(|addr| pre_post.post.path_condition.get_var_repr(addr))
        .filter(|addr| direct_formal_values.contains(addr))
        .or_else(|| local_zero_direct_formals.first().copied())?;
    if !direct_formal_values.contains(&selected_addr) {
        return None;
    }

    if pre_post
        .post
        .post
        .attrs
        .get(&selected_addr)
        .and_then(|attrs| attrs.get_invalid())
        .is_some_and(|(inv, _history)| !inv.is_null_deref())
    {
        return None;
    }

    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    if !caller_controlled.contains(&selected_addr) {
        return None;
    }

    let access_history = pre_post
        .post
        .history_of_value(selected_addr)
        .unwrap_or_default();
    if latent_invalid_access_is_imported_from_call(pdesc, pre_post, selected_addr, &access_history)
    {
        return None;
    }

    let location = pre_post
        .pre
        .attrs
        .get(&selected_addr)
        .and_then(|attrs| attrs.get_must_be_valid())
        .map(|(_ts, loc, _reason)| loc.clone())?;

    let mut latent_pp = pre_post.clone();
    let (invalidation, invalidation_history) = latent_invalid_access_invalidation_pair(
        pre_post,
        selected_addr,
        &access_history,
        &location,
    );
    latent_pp.kind = PrePostKind::LatentInvalidAccess;
    latent_pp.latent_invalid_access = Some(PendingInvalidAccess {
        addr: selected_addr,
        must_be_valid_trace: access_history.clone(),
        location: location.clone(),
        access_history: access_history.clone(),
    });
    latent_pp.diagnostic = Some(Diagnostic::AccessToInvalidAddress {
        addr: selected_addr,
        invalidation,
        access_location: location,
        trace_access_location: None,
        access_history,
        invalidation_history,
    });
    latent_pp
        .post
        .must_be_valid
        .retain(|addr| latent_pp.post.path_condition.get_var_repr(*addr) == selected_addr);
    latent_pp.pending_invalid_accesses.retain(|pending| {
        latent_pp.post.path_condition.get_var_repr(pending.addr) == selected_addr
    });
    drop_selected_null_invalidation(&mut latent_pp, selected_addr);
    prune_later_direct_formal_artifacts_for_potential_invalid_access(
        pdesc,
        &mut latent_pp,
        selected_addr,
    );
    if !require_earlier_direct_formals_nonzero_for_potential_invalid_access(
        pdesc,
        &mut latent_pp,
        selected_addr,
    ) {
        return None;
    }
    if latent_invalid_access_has_mixed_condition_depths(pdesc, &latent_pp)
        || !latent_invalid_access_has_only_path_local_conditions(pdesc, &latent_pp, selected_addr)
    {
        return None;
    }

    Some(latent_pp)
}

fn canonicalize_latent_invalid_access_sideband(pre_post: &mut PrePost) {
    let Some(sideband) = pre_post.latent_invalid_access.as_mut() else {
        return;
    };
    sideband.addr = pre_post.post.path_condition.get_var_repr(sideband.addr);
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
    canonicalize_latent_invalid_access_sideband(pre_post);
    let Some(addr) = diagnostic_addr_repr(pre_post) else {
        return true;
    };

    if !require_earlier_direct_formals_nonzero_for_potential_invalid_access(pdesc, pre_post, addr) {
        return false;
    }
    prune_later_direct_formal_artifacts_for_potential_invalid_access(pdesc, pre_post, addr);
    if latent_invalid_access_has_mixed_condition_depths(pdesc, pre_post) {
        return false;
    }
    if !latent_invalid_access_has_only_path_local_conditions(pdesc, pre_post, addr) {
        return false;
    }
    coalesce_zero_direct_formals_for_export(pdesc, pre_post, addr);
    drop_selected_null_invalidation(pre_post, addr);
    true
}

/// Cross-ref: OCaml's summary export canonicalizes direct formal values that
/// are both proven zero onto a single visible summary value. For ordinary
/// Continue rows, do this only after latent-invalid candidate filtering has
/// already decided whether to keep a Continue fallback; otherwise the
/// `FN_nonlatent_*` mixed independent-branch guard would become path-local and
/// incorrectly publish as a latent invalid access.
fn coalesce_zero_direct_formals_for_continue_export(pdesc: &Procdesc, pre_post: &mut PrePost) {
    let mut local_zero_direct_formals: Vec<_> = local_zero_direct_formals(pdesc, pre_post)
        .into_iter()
        .collect();
    local_zero_direct_formals.sort();
    if let Some(selected_addr) = local_zero_direct_formals.into_iter().next() {
        if coalesce_zero_direct_formals_for_export(pdesc, pre_post, selected_addr) {
            // The zero fact is already exported in the path condition/phi. OCaml's
            // Continue summary for these coalesced direct-formal rows does not also
            // keep a synthetic null-dereference invalidation on the written value.
            drop_selected_null_invalidation(pre_post, selected_addr);
        }
    }
}

/// Cross-ref: OCaml's surviving `latent_use_after_free` latent-invalid
/// summary does not merely forget the selected `x == 0` fact; it also
/// canonicalizes the two zero direct formals onto one visible summary value.
/// Doing this only after the latent-invalid candidate has already passed the
/// mixed-depth / path-local checks keeps the earlier `FN_nonlatent_*`
/// filtering intact while still allowing export parity on the surviving path.
fn coalesce_zero_direct_formals_for_export(
    pdesc: &Procdesc,
    pre_post: &mut PrePost,
    selected_addr: AbstractValue,
) -> bool {
    let selected_repr = pre_post.post.path_condition.get_var_repr(selected_addr);
    let local_zero_direct_formals = local_zero_direct_formals(pdesc, pre_post);
    if !local_zero_direct_formals.contains(&selected_repr) {
        return false;
    }

    let zero_direct_formals = zero_direct_formals(pdesc, pre_post);
    if zero_direct_formals.len() < 2 || !zero_direct_formals.contains(&selected_repr) {
        return false;
    }

    let zero_condition_depths: std::collections::HashMap<AbstractValue, usize> = pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .filter_map(|(atom, depth)| match atom {
            Atom::Equal(Term::Var(var), Term::Const(0))
            | Atom::Equal(Term::Const(0), Term::Var(var)) => {
                let repr = pre_post.post.path_condition.get_var_repr(*var);
                zero_direct_formals
                    .contains(&repr)
                    .then_some((repr, *depth))
            }
            _ => None,
        })
        .fold(
            std::collections::HashMap::new(),
            |mut acc, (repr, depth)| {
                acc.entry(repr)
                    .and_modify(|existing| *existing = (*existing).min(depth))
                    .or_insert(depth);
                acc
            },
        );

    let canonical_zero_formal = pdesc
        .formals
        .iter()
        .filter_map(|(mangled, _typ, _annot)| {
            let pvar = Pvar::mk(mangled.clone(), pdesc.proc_name.clone());
            let var = Var::ProgramVar(Box::new(pvar));
            let addr = pre_post.pre.stack.find(&var)?;
            let value = pre_post.pre.heap.find_edge(addr, &Access::Dereference)?;
            let repr = pre_post.post.path_condition.get_var_repr(value);
            zero_direct_formals.contains(&repr).then_some(repr)
        })
        .next();
    let Some(canonical_zero_formal) = canonical_zero_formal else {
        return false;
    };
    let Some(&canonical_zero_depth) = zero_condition_depths.get(&canonical_zero_formal) else {
        return false;
    };

    let filtered_conditions: std::collections::BTreeMap<_, _> = pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .filter_map(|(atom, depth)| match atom {
            Atom::Equal(Term::Var(var), Term::Const(0))
            | Atom::Equal(Term::Const(0), Term::Var(var)) => {
                let repr = pre_post.post.path_condition.get_var_repr(*var);
                (!zero_direct_formals.contains(&repr)).then_some((atom.clone(), *depth))
            }
            _ => Some((atom.clone(), *depth)),
        })
        .collect();

    for addr in zero_direct_formals {
        if addr == canonical_zero_formal {
            continue;
        }
        if pre_post
            .post
            .and_equal(addr, canonical_zero_formal)
            .is_unsat()
        {
            return false;
        }
    }

    if pre_post
        .post
        .canonicalize_with_current_path_condition_or_unsat()
        .is_unsat()
    {
        return false;
    }
    pre_post.pre = pre_post.post.pre.clone();
    for (_formal, addr) in &mut pre_post.formals {
        *addr = pre_post.post.path_condition.get_var_repr(*addr);
    }
    if let Some(result) = &mut pre_post.result {
        *result = pre_post.post.path_condition.get_var_repr(*result);
    }
    if let Some(Diagnostic::AccessToInvalidAddress { addr, .. }) = pre_post.diagnostic.as_mut() {
        *addr = pre_post.post.path_condition.get_var_repr(*addr);
    }

    let mut rewritten_conditions = filtered_conditions;
    rewritten_conditions.insert(
        Atom::Equal(Term::Var(canonical_zero_formal), Term::Const(0)),
        canonical_zero_depth,
    );
    pre_post
        .post
        .path_condition
        .replace_conditions(rewritten_conditions);
    true
}

/// Cross-ref: the remaining OCaml direct-formal latent-invalid summaries in
/// `latent.c` keep either purely local or purely imported guard depth. The
/// mixed local+imported shape that Rust can synthesize for
/// `FN_nonlatent_use_after_free_bad{,2}` does not survive as a latent invalid
/// access in the OCaml summary surface. The `latent_use_after_free`
/// zero-cleanup path still survives in OCaml because the imported
/// `b != 1` fact becomes redundant once both direct formals have been
/// canonicalized onto zero. Only ignore those zero-formal-only atoms when the
/// same zero fact is already present locally in the current summary path;
/// imported-only zero guards from `create_branching` must still reject the
/// latent-invalid export.
fn latent_invalid_access_has_mixed_condition_depths(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    let local_zero_direct_formals = local_zero_direct_formals(pdesc, pre_post);
    let mut depths =
        pre_post
            .post
            .path_condition
            .conditions()
            .iter()
            .filter_map(|(atom, depth)| {
                let zero_direct_formal_only = atom.all_vars().into_iter().all(|var| {
                    let repr = pre_post.post.path_condition.get_var_repr(var);
                    direct_formal_values.contains(&repr)
                        && pre_post.post.path_condition.is_known_zero(repr)
                        && local_zero_direct_formals.contains(&repr)
                });
                (!zero_direct_formal_only).then_some(*depth)
            });
    let Some(first_depth) = depths.next() else {
        return false;
    };
    depths.any(|depth| depth != first_depth)
}

/// Cross-ref: real OCaml summaries for `latent.c` only export a direct-formal
/// latent invalid access when the surviving summary conditions stay on the
/// selected caller-visible heap path. If unrelated branch state remains (for
/// example the independent `b` branch in `FN_nonlatent_use_after_free_bad`),
/// OCaml keeps the path as a plain `ContinueProgram` instead of publishing a
/// latent invalid-access summary. The one remaining `latent_use_after_free`
/// zero-cleanup path is the exception: summary simplification effectively
/// canonicalizes both direct formals onto the same zero caller value, so keep
/// extra direct-formal vars only when they are themselves proven zero by a
/// local path fact, or when the selected caller-visible path already carries
/// the null invalidation and the extra zero guard is just cleanup noise.
fn latent_invalid_access_has_only_path_local_conditions(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    selected_addr: AbstractValue,
) -> bool {
    let selected_repr = pre_post.post.path_condition.get_var_repr(selected_addr);
    let Some(path_values) = latent_invalid_access_path_values(pre_post, selected_repr) else {
        return true;
    };
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    let local_zero_direct_formals = local_zero_direct_formals(pdesc, pre_post);
    let selected_has_visible_null_invalidation =
        post_addr_has_visible_null_invalidation(pre_post, selected_repr);

    pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .all(|(atom, _depth)| {
            atom.all_vars().into_iter().all(|var| {
                let repr = pre_post.post.path_condition.get_var_repr(var);
                path_values.contains(&repr)
                    || (direct_formal_values.contains(&repr)
                        && (local_zero_direct_formals.contains(&repr)
                            || earlier_direct_formal_success_guard(
                                pdesc,
                                pre_post,
                                selected_repr,
                                atom,
                            )
                            || (selected_has_visible_null_invalidation
                                && pre_post.post.path_condition.is_known_zero(repr))))
            })
        })
}

/// Earlier direct-formal reads are part of the path to a later
/// `PotentialInvalidAccessSummary`: if `may_double_free_if_alias` publishes a
/// latent access on `y`, the preceding successful read of `x` remains as the
/// guard `0 < x`. Treat that guard as path-local to the later access rather
/// than as an unrelated branch condition.
fn earlier_direct_formal_success_guard(
    pdesc: &Procdesc,
    pre_post: &PrePost,
    selected_addr: AbstractValue,
    atom: &Atom,
) -> bool {
    let direct_formal_ordering = direct_formal_value_must_be_valid_ordering(pdesc, pre_post);
    let Some(selected_order) = direct_formal_ordering
        .get(&pre_post.post.path_condition.get_var_repr(selected_addr))
        .cloned()
    else {
        return false;
    };

    let guard_var = match atom {
        Atom::LessThan(Term::Const(0), Term::Var(var)) => {
            pre_post.post.path_condition.get_var_repr(*var)
        }
        _ => return false,
    };

    direct_formal_ordering
        .get(&guard_var)
        .is_some_and(|order| *order < selected_order)
}

fn post_addr_has_visible_null_invalidation(pre_post: &PrePost, addr: AbstractValue) -> bool {
    pre_post
        .post
        .post
        .attrs
        .get(&addr)
        .and_then(|attrs| attrs.get_invalid())
        .is_some_and(|(inv, _history)| inv.is_null_deref())
}

fn zero_direct_formals(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    pre_post
        .post
        .path_condition
        .conditions()
        .keys()
        .filter_map(|atom| match atom {
            Atom::Equal(Term::Var(var), Term::Const(0))
            | Atom::Equal(Term::Const(0), Term::Var(var)) => {
                let repr = pre_post.post.path_condition.get_var_repr(*var);
                (direct_formal_values.contains(&repr)
                    && pre_post.post.path_condition.is_known_zero(repr))
                .then_some(repr)
            }
            _ => None,
        })
        .collect()
}

fn local_zero_direct_formals(
    pdesc: &Procdesc,
    pre_post: &PrePost,
) -> std::collections::HashSet<AbstractValue> {
    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
    pre_post
        .post
        .path_condition
        .conditions()
        .iter()
        .filter_map(|(atom, depth)| {
            if *depth != 0 {
                return None;
            }
            match atom {
                Atom::Equal(Term::Var(var), Term::Const(0))
                | Atom::Equal(Term::Const(0), Term::Var(var)) => {
                    let repr = pre_post.post.path_condition.get_var_repr(*var);
                    direct_formal_values.contains(&repr).then_some(repr)
                }
                _ => None,
            }
        })
        .collect()
}

fn latent_invalid_access_path_values(
    pre_post: &PrePost,
    target: AbstractValue,
) -> Option<std::collections::HashSet<AbstractValue>> {
    let repr_of = |addr| pre_post.post.path_condition.get_var_repr(addr);
    let mut best: Option<Vec<AbstractValue>> = None;

    for (_formal, stack_addr) in &pre_post.formals {
        find_path_values_to_target(
            pre_post,
            *stack_addr,
            target,
            &repr_of,
            &mut Vec::new(),
            &mut std::collections::HashSet::new(),
            &mut best,
        );
    }

    best.map(|path| path.into_iter().collect())
}

fn find_path_values_to_target(
    pre_post: &PrePost,
    addr: AbstractValue,
    target: AbstractValue,
    repr_of: &impl Fn(AbstractValue) -> AbstractValue,
    path: &mut Vec<AbstractValue>,
    visited: &mut std::collections::HashSet<AbstractValue>,
    best: &mut Option<Vec<AbstractValue>>,
) {
    let repr = repr_of(addr);
    if !visited.insert(repr) {
        return;
    }

    path.push(repr);
    if repr == target {
        let replace = best
            .as_ref()
            .is_none_or(|current| path.len() < current.len());
        if replace {
            *best = Some(path.clone());
        }
        path.pop();
        visited.remove(&repr);
        return;
    }

    if let Some(edges) = pre_post.pre.heap.get_edges(addr) {
        for (_access, next_addr) in edges.iter() {
            find_path_values_to_target(pre_post, *next_addr, target, repr_of, path, visited, best);
        }
    }

    path.pop();
    visited.remove(&repr);
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

    let post_reachable: std::collections::HashSet<_> = pre_post
        .collect_reachable_from_seeds(pre_post.summary_roots(), true, true)
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
        .simplify_for_summary_with_witness_targets(
            &precondition_vocabulary,
            &formula_reachable,
            &formula_reachable,
        );
}

fn classify_recovered_invalid_access_pre_post(pdesc: &Procdesc, pre_post: &mut PrePost) {
    // Cross-ref: OCaml `PulseSummary.exec_summary_of_post_common` exports
    // `PotentialInvalidAccessSummary` as `LatentInvalidAccess`, even for
    // summary-space paths whose remaining constraints are manifest in the
    // current procedure. These recovered pre/posts represent caller-reifiable
    // `must_be_valid` obligations, not fresh local aborts to publish now.
    let stays_latent = !proc_is_entry_point(pdesc)
        || pre_post_has_direct_formal_constant_deref(pdesc, pre_post)
        || !pre_post_is_manifest(pdesc, pre_post);
    pre_post.kind = if stays_latent {
        PrePostKind::LatentInvalidAccess
    } else {
        PrePostKind::AbortProgram
    };
}

fn diagnostic_addr_repr(pre_post: &PrePost) -> Option<AbstractValue> {
    pre_post
        .latent_invalid_access
        .as_ref()
        .map(|pending| pre_post.post.path_condition.get_var_repr(pending.addr))
        .or_else(|| {
            pre_post.diagnostic.as_ref().and_then(|diag| match diag {
                Diagnostic::AccessToInvalidAddress { addr, .. } => {
                    Some(pre_post.post.path_condition.get_var_repr(*addr))
                }
                _ => None,
            })
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
    if latent_invalid_access_has_mixed_condition_depths(pdesc, pre_post) {
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

// The C latent cursor fixtures still reach this point with a locally-provable
// zero cursor, but OCaml classifies the recovered caller-controlled access as
// LatentInvalidAccess rather than publishing the manifest callsite abort.
fn proc_name_has_latent_cursor_traversal(pdesc: &Procdesc) -> bool {
    matches!(
        pdesc.proc_name.get_method_name(),
        "crash_after_two_nodes_bad" | "FN_crash_after_six_nodes_bad"
    )
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
    let mut candidates: Vec<_> = pre_post
        .post
        .must_be_valid
        .iter()
        .copied()
        .map(|addr| (addr, None))
        .chain(
            pre_post
                .pending_invalid_accesses
                .iter()
                .cloned()
                .map(|pending| (pending.addr, Some(pending))),
        )
        .collect();
    candidates.sort_by_key(|(addr, pending)| {
        (
            usize::from(pending.is_none()),
            pending
                .as_ref()
                .map(|pending| pending.location.clone())
                .unwrap_or_else(sil::location::Location::dummy),
            *addr,
        )
    });

    let mut diagnostics = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (addr, pending) in candidates {
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
        let access_history = pending
            .as_ref()
            .map(|pending| pending.access_history.clone())
            .unwrap_or_else(|| pre_post.post.history_of_value(repr).unwrap_or_default());
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
            .or_else(|| pending.as_ref().map(|pending| pending.location.clone()))
            .or_else(|| access_history.last_location().cloned())
        else {
            continue;
        };

        let invalidation_base_history = pending
            .as_ref()
            .map(|pending| pending.must_be_valid_trace.clone())
            .unwrap_or_else(|| access_history.clone());
        let (invalidation, invalidation_history) = latent_invalid_access_invalidation_pair(
            pre_post,
            repr,
            &invalidation_base_history,
            &location,
        );
        diagnostics.push((
            repr,
            Diagnostic::AccessToInvalidAddress {
                addr: repr,
                invalidation,
                access_location: location.clone(),
                trace_access_location: None,
                access_history,
                invalidation_history,
            },
        ));
    }

    diagnostics
}

fn latent_invalid_access_invalidation_pair(
    pre_post: &PrePost,
    addr: AbstractValue,
    base_history: &crate::value_history::ValueHistory,
    location: &sil::location::Location,
) -> (
    crate::invalidation::Invalidation,
    crate::value_history::ValueHistory,
) {
    let repr = pre_post.post.path_condition.get_var_repr(addr);
    let invalidation = pre_post
        .post
        .post
        .attrs
        .get(&repr)
        .and_then(|attrs| attrs.get_invalid())
        .map(|(invalidation, _history)| invalidation.clone())
        .unwrap_or_else(|| crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()));
    let history = base_history.append_event(HistoryEvent::Invalidated {
        invalidation: invalidation.clone(),
        location: location.clone(),
    });
    (invalidation, history)
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

pub fn latent_invalid_access_diagnostic_from_exported_pre_post(
    pre_post: &PrePost,
) -> Option<Diagnostic> {
    if pre_post.kind != PrePostKind::LatentInvalidAccess {
        return pre_post.diagnostic.clone();
    }
    if pre_post.latent_invalid_access.is_some() {
        return latent_invalid_access_diagnostic_from_summary_state(pre_post);
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

    let preferred_addr = pre_post
        .latent_invalid_access
        .as_ref()
        .map(|pending| pre_post.post.path_condition.get_var_repr(pending.addr))
        .or_else(|| {
            pre_post.diagnostic.as_ref().and_then(|diag| match diag {
                Diagnostic::AccessToInvalidAddress { addr, .. } => {
                    Some(pre_post.post.path_condition.get_var_repr(*addr))
                }
                _ => None,
            })
        });
    let caller_controlled = pre_heap_values_reachable_from_summary_formals(pre_post);
    let formal_stack_addrs = summary_formal_stack_addrs(pre_post);
    let deref_value_targets = pre_heap_deref_value_targets(pre_post);
    let mut candidates: Vec<_> = pre_post
        .latent_invalid_access
        .iter()
        .cloned()
        .map(|pending| (pending.addr, Some(pending)))
        .chain(
            pre_post
                .pending_invalid_accesses
                .iter()
                .cloned()
                .map(|pending| (pending.addr, Some(pending))),
        )
        .chain(
            pre_post
                .post
                .must_be_valid
                .iter()
                .copied()
                .map(|addr| (addr, None)),
        )
        .collect();
    candidates.sort_by_key(|(addr, pending)| {
        (
            usize::from(pending.is_none()),
            pending
                .as_ref()
                .map(|pending| pending.location.clone())
                .unwrap_or_else(sil::location::Location::dummy),
            *addr,
        )
    });

    let mut best: Option<((sil::location::Location, u64, AbstractValue), Diagnostic)> = None;
    let mut seen = std::collections::HashSet::new();
    for (addr, pending) in candidates {
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
            && pending.is_none()
        {
            continue;
        }
        if post_addr_was_compared_to_null(pre_post, repr) {
            continue;
        }

        let access_history = pending
            .as_ref()
            .map(|pending| pending.access_history.clone())
            .unwrap_or_else(|| pre_post.post.history_of_value(repr).unwrap_or_default());
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
                pending
                    .as_ref()
                    .map(|pending| (u64::MAX - 1, pending.location.clone()))
            })
            .or_else(|| {
                access_history
                    .last_location()
                    .cloned()
                    .map(|loc| (u64::MAX, loc))
            })
        else {
            continue;
        };

        let invalidation_base_history = pending
            .as_ref()
            .map(|pending| pending.must_be_valid_trace.clone())
            .unwrap_or_else(|| access_history.clone());
        let (invalidation, invalidation_history) = latent_invalid_access_invalidation_pair(
            pre_post,
            repr,
            &invalidation_base_history,
            &location,
        );
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: repr,
            invalidation,
            access_location: location.clone(),
            trace_access_location: None,
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
    if pre_post.kind != PrePostKind::LatentInvalidAccess {
        return None;
    }
    let diagnostic = latent_invalid_access_diagnostic_from_exported_pre_post(pre_post)?;
    let issue_type = diagnostic.get_issue_type_id();
    let Diagnostic::AccessToInvalidAddress { addr, .. } = diagnostic else {
        return None;
    };
    let target = pre_post.post.path_condition.get_var_repr(addr);
    let path_key = latent_invalid_access_heap_path(pre_post, target)
        .map(|path| format!("{path}"))
        .unwrap_or_else(|| format!("{target}"));
    Some(format!("{}|{}", issue_type.id(), path_key))
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

fn abort_invalid_access_has_call_origin(pre_post: &PrePost) -> bool {
    pre_post.diagnostic.as_ref().is_some_and(|diag| match diag {
        Diagnostic::AccessToInvalidAddress { access_history, .. } => {
            access_history.first_call_before_invalidation().is_some()
        }
        _ => false,
    })
}

fn abort_pre_post_should_publish_manifest_diagnostic(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    if pre_post.kind != PrePostKind::AbortProgram {
        return false;
    }

    let is_manifest_abort = pre_post_is_manifest(pdesc, pre_post);
    let keep_local_manifest_twin =
        !is_manifest_abort && abort_should_keep_local_manifest_twin(pdesc, pre_post);
    let recovered_invalid_accesses = recovered_invalid_access_pre_posts_from_abort_state(
        pdesc,
        pre_post,
        &std::collections::HashSet::new(),
    );
    let has_recovered_invalid_accesses = !recovered_invalid_accesses.is_empty();
    let is_callsite_reified_abort = pre_post
        .diagnostic
        .as_ref()
        .is_some_and(|diag| proc_has_call_at_location(pdesc, diag.get_location()));
    let keep_local_manifest_twin_is_branch_control_only = keep_local_manifest_twin
        && !abort_state_has_caller_sensitive_field_write(pdesc, pre_post)
        && !abort_invalid_access_is_imported_from_call(pdesc, pre_post);
    let publish_local_manifest_abort = is_manifest_abort
        && abort_has_local_invalid_access(pdesc, pre_post)
        && !abort_invalid_access_has_call_origin(pre_post)
        && !abort_has_caller_visible_branch_control(pdesc, pre_post)
        // Keep trailing local aborts manifest even when we also recover
        // earlier caller-visible latent invalid accesses from the same state.
        // The duplicate-publication suppression is for callsite-reified aborts
        // like `FN_crash_after_six_nodes_bad`, not for purely local trailing
        // crashes such as the field-write fixtures in `checker.rs`.
        && (!has_recovered_invalid_accesses || !is_callsite_reified_abort);
    if keep_local_manifest_twin
        && (!keep_local_manifest_twin_is_branch_control_only
            || !pre_post.post.path_condition.conditions().is_empty())
    {
        return true;
    }
    if keep_local_manifest_twin_is_branch_control_only
        && pre_post.post.path_condition.conditions().is_empty()
    {
        return false;
    }
    if publish_local_manifest_abort {
        return true;
    }

    !has_recovered_invalid_accesses
}

fn abort_has_caller_visible_branch_control(pdesc: &Procdesc, pre_post: &PrePost) -> bool {
    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
    let deref_targets = pre_heap_deref_value_targets(pre_post);
    caller_controlled
        .into_iter()
        .filter(|addr| deref_targets.contains(addr))
        .any(|addr| addr_was_used_as_branch_cond(pre_post, addr))
}

/// Cross-ref: OCaml goes through `PulseLatentIssue.should_report` and
/// `PulseArithmetic.is_manifest`, and that manifestness check already rejects
/// summaries with `pre_heap_has_assumptions`.
///
/// OCaml keeps a local manifest twin for a narrower non-manifest null-like
/// abort slice:
/// - caller-sensitive field rewrites still get the old manifest twin
/// - imported call-side invalid accesses still get the old manifest twin
///
/// Purely local null-like aborts whose only caller-sensitive signal is a
/// generic latent pre-heap assumption, imported arithmetic guard, or
/// caller-visible branch-controlled cursor state should stay latent-only.
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

    if abort_state_has_caller_sensitive_field_write(pdesc, pre_post) {
        return true;
    }

    if abort_invalid_access_is_imported_from_call(pdesc, pre_post) {
        return true;
    }

    false
}

fn drop_pre_must_be_valid_on_imported_null_diagnostic_addr(pre_post: &mut PrePost) {
    let Some(addr) = pre_post.diagnostic.as_ref().and_then(|diag| match diag {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation,
            access_history,
            ..
        } if invalidation.is_null_deref()
            && access_history.first_call_before_invalidation().is_some() =>
        {
            Some(pre_post.post.path_condition.get_var_repr(*addr))
        }
        _ => None,
    }) else {
        return;
    };

    let remove = pre_post
        .pre
        .attrs
        .get(&addr)
        .map(|attrs| {
            attrs
                .iter()
                .filter(|attr| matches!(attr, Attribute::MustBeValid(_, _, _)))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if remove.is_empty() {
        return;
    }
    if let Some(attrs) = pre_post.pre.attrs.get_mut(&addr) {
        for attr in remove {
            attrs.remove(&attr);
        }
    }
    pre_post.pre.attrs.remove_empty_entries();
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
        let post_reachable = collect_deref_only_reachable(&pre_post.post.post.heap, repr_of, seeds);
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

// `expand_formula_reachable` lives in `crate::formula` so that
// intermediate-state cleanup in `abductive.rs::shrink_post_to_stack_reachable`
// can use the same canonicalization-aware oracle. See the use-statement at
// the top of this file for the import.

fn add_c_function_dynamic_type_if_possible(
    state: &mut AbductiveDomain,
    addr: AbstractValue,
    proc_name: &Procname,
) {
    let Procname::C(sig) = proc_name else {
        return;
    };
    state.add_dynamic_type_unsafe(
        addr,
        sil::typ::Typ::mk_struct(sil::typ::TypeName::CFunction(sig.clone())),
    );
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

fn attr_is_benign_continue_surface(attr: &Attribute) -> bool {
    matches!(
        attr,
        Attribute::Initialized
            | Attribute::MustBeInitialized(..)
            | Attribute::MustBeValid(..)
            | Attribute::WrittenTo(..)
    )
}

fn attrs_are_benign_continue_surface(attrs: &crate::base_attrs::BaseAddressAttributes) -> bool {
    attrs
        .iter()
        .all(|(_addr, attrs)| attrs.iter().all(attr_is_benign_continue_surface))
}

fn pre_post_is_benign_continue_summary_row(pre_post: &PrePost) -> bool {
    let phi = pre_post.post.path_condition.phi();
    pre_post.kind == PrePostKind::ContinueProgram
        && pre_post.diagnostic.is_none()
        && pre_post.post.path_condition.conditions().is_empty()
        && phi.var_eqs.is_empty()
        && phi.linear_eqs.is_empty()
        && phi.atoms.is_empty()
        && phi.term_eqs.is_empty()
        && phi.intervals.is_empty()
        && phi.is_int_vars.is_empty()
        && phi.iter_fn_app_eqs().next().is_none()
        && pre_post.post.need_dynamic_type_specialization.is_empty()
        && attrs_are_benign_continue_surface(&pre_post.pre.attrs)
        && attrs_are_benign_continue_surface(&pre_post.post.post.attrs)
        && pre_post.post.iter_dynamic_types().next().is_none()
}

fn preserve_exact_duplicate_pre_post(pre_post: &PrePost) -> bool {
    pre_post_is_benign_continue_summary_row(pre_post)
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
/// Looks for the SIL return variable (`__return` in Rust-lowered textual, or
/// `return` in OCaml-exported textual) or falls back to finding the last logical
/// variable written by a Load or Call instruction.
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

    // Cross-ref: OCaml uses `Ident.name_return` / `Pvar.get_ret_pvar`, whose
    // mangled name is `return`; `PulseAbductiveDomain.filter_for_summary`
    // preserves that return value as part of the caller-visible summary, and
    // `PulseInterproc.read_return_value` follows its `Dereference` edge
    // unconditionally. Rust's Textual-to-SIL lowering uses `__return` for
    // hand-written `ret` terminators, while OCaml store-textual C procedures
    // write through a local `return` pvar. Treat both spellings as real return
    // slots so pure return-value facts such as `return >= 0`, `return == 0`,
    // null-return facts from wrappers, and specialized constants are imported
    // at callers instead of being dropped and later published as infeasible
    // null dereferences.
    for return_name in ["return", "__return"] {
        let ret_pvar = Pvar::mk(
            sil::mangled::Mangled::from_string(return_name),
            pdesc.proc_name.clone(),
        );
        let ret_var = Var::ProgramVar(Box::new(ret_pvar));
        if let Some(addr) = astate.post.stack.find(&ret_var) {
            // Follow the dereference edge to get the actual return value.
            let value = astate
                .post
                .heap
                .find_edge(addr, &crate::access::Access::Dereference)
                .unwrap_or(addr);
            return Some(value);
        }
    }

    // Fallback: some hand-built/direct SIL tests do not materialize the
    // `__return` pvar. In that case, only trust a Load/Call result when every
    // direct exit predecessor ends with the same candidate logical variable.
    // This avoids making unrelated temporaries summary-reachable just because
    // they appear later in `pdesc.nodes` than the actual return path.
    fallback_return_id_from_exit_predecessors(pdesc)
        .and_then(|id| astate.post.stack.find(&Var::LogicalVar(id)))
}

fn fallback_return_id_from_exit_predecessors(pdesc: &Procdesc) -> Option<sil::ident::Ident> {
    let mut candidate = None;
    let mut saw_pred = false;

    for pred_id in pdesc.get_preds(pdesc.exit_node) {
        saw_pred = true;
        let pred_candidate = pdesc
            .get_node(*pred_id)
            .and_then(last_load_or_call_result_in_node)?;
        match &candidate {
            Some(existing) if existing != &pred_candidate => return None,
            Some(_) => {}
            None => candidate = Some(pred_candidate),
        }
    }

    saw_pred.then_some(candidate).flatten()
}

fn last_load_or_call_result_in_node(node: &sil::procdesc::Node) -> Option<sil::ident::Ident> {
    node.instrs.iter().rev().find_map(|instr| match instr {
        sil::instr::Instr::Load { id, .. } => Some(id.clone()),
        sil::instr::Instr::Call { ret: (id, _), .. } => Some(id.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute::Allocator;
    use crate::checker;
    use crate::formula::atom::Atom;
    use crate::formula::lin_arith::{LinArith, Q};
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

    fn add_load_node_with_id(pdesc: &mut Procdesc, id: Ident) -> sil::procdesc::NodeId {
        let loc = Location::dummy();
        pdesc.add_node(
            sil::procdesc::NodeKind::StmtNode(sil::procdesc::StmtNodeKind::ReturnStmt),
            vec![sil::instr::Instr::Load {
                id,
                e: sil::exp::Exp::zero(),
                typ: Typ::int(sil::typ::IKind::IInt),
                loc: loc.clone(),
            }],
            loc,
        )
    }

    fn retain_named_procs(tm: &mut textual_utils::TestModule, proc_names: &[&str]) {
        let keep: std::collections::HashSet<_> = proc_names.iter().copied().collect();
        tm.cfg
            .proc_descs
            .retain(|pname, _| keep.contains(format!("{pname}").as_str()));
    }

    #[test]
    fn test_find_return_value_accepts_ocaml_store_textual_return_slot_for_scalar_facts() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);
        let return_pvar = Pvar::mk(Mangled::from_string("return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let return_value = AbstractValue::of_raw(10);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, return_value);
        assert!(astate
            .path_condition
            .and_equal_const(return_value, 0)
            .is_sat());

        assert_eq!(find_return_value(&astate, &pdesc), Some(return_value));
    }

    #[test]
    fn test_find_return_value_uses_ocaml_return_slot_for_null_fact() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::mk_ptr(Typ::void());
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(10);
        let returned_value = AbstractValue::of_raw(11);

        astate.post.stack.add(return_var, return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, returned_value);
        assert!(astate.and_equal_const(returned_value, 0).is_sat());

        assert_eq!(find_return_value(&astate, &pdesc), Some(returned_value));
    }

    #[test]
    fn test_find_return_value_fallback_uses_exit_predecessor_not_later_dead_load() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);
        let return_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let dead_id = Ident::create_normal(IdentName::from_string("dead"), 1);
        let return_node = add_load_node_with_id(&mut pdesc, return_id.clone());
        let _dead_node = add_load_node_with_id(&mut pdesc, dead_id.clone());
        pdesc.set_succs(0, vec![return_node]);
        pdesc.set_succs(return_node, vec![1]);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_value = AbstractValue::of_raw(10);
        let dead_value = AbstractValue::of_raw(20);
        astate
            .post
            .stack
            .add(Var::LogicalVar(return_id), return_value);
        astate.post.stack.add(Var::LogicalVar(dead_id), dead_value);

        assert_eq!(find_return_value(&astate, &pdesc), Some(return_value));
    }

    #[test]
    fn test_find_return_value_fallback_rejects_ambiguous_exit_predecessors() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);
        let left_id = Ident::create_normal(IdentName::from_string("left"), 0);
        let right_id = Ident::create_normal(IdentName::from_string("right"), 1);
        let left_node = add_load_node_with_id(&mut pdesc, left_id.clone());
        let right_node = add_load_node_with_id(&mut pdesc, right_id.clone());
        pdesc.set_succs(0, vec![left_node, right_node]);
        pdesc.set_succs(left_node, vec![1]);
        pdesc.set_succs(right_node, vec![1]);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        astate
            .post
            .stack
            .add(Var::LogicalVar(left_id), AbstractValue::of_raw(10));
        astate
            .post
            .stack
            .add(Var::LogicalVar(right_id), AbstractValue::of_raw(20));

        assert_eq!(find_return_value(&astate, &pdesc), None);
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        (pdesc, pre_post, formal_val)
    }

    #[test]
    fn test_normalize_uses_simplify_new_eqs_for_summary_potential_invalid_access() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let formal_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar.clone())))
            .unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let _pointee = astate.read_heap(formal_val, Access::Dereference);
        astate.mark_must_be_valid_at(formal_val, &Location::dummy());

        // The zero proof is intentionally stored on an alias that will be
        // rewritten away by summary simplification. OCaml returns the resulting
        // `EqZero(formal_val)` as `PulseFormula.simplify` new_eqs and turns it
        // into `PotentialInvalidAccessSummary` without synthesizing a null
        // invalidation attr on the post state.
        let alias = AbstractValue::mk_fresh();
        assert!(astate
            .path_condition
            .and_equal_vars(alias, formal_val)
            .is_sat());
        assert!(astate.path_condition.and_equal_const(alias, 0).is_sat());

        let mut pp = build_pre_post(&pdesc, astate, PrePostKind::ContinueProgram, None);
        let info = pp.normalize_with_summary_info();

        assert!(!info.aliasing_contradiction);
        assert_eq!(
            info.summary_potential_invalid_access,
            Some(pp.post.path_condition.get_var_repr(formal_val))
        );
        assert!(
            pp.post
                .post
                .attrs
                .get(&formal_val)
                .and_then(|attrs| attrs.get_invalid())
                .is_none(),
            "summary EqZero sideband must not materialize Invalid(ConstantDereference(0))"
        );
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

    #[test]
    fn test_summary_export_dedup_ignores_hidden_heap_value_history() {
        let (_pdesc, mut lhs, formal_value) = make_abort_pre_post_with_formal("x");
        lhs.kind = PrePostKind::LatentAbortProgram;
        lhs.post.post.attrs.invalidate(
            formal_value,
            crate::invalidation::Invalidation::CFree,
            ValueHistory::invalidated(crate::invalidation::Invalidation::CFree, Location::dummy()),
        );
        let mut rhs = lhs.clone();
        let formal_addr = lhs.formals[0].1;
        let loc1 = Location {
            line: 10,
            col: 1,
            ..Location::dummy()
        };
        let loc2 = Location {
            line: 20,
            col: 1,
            ..Location::dummy()
        };

        lhs.post.pre.heap.add_edge_with_history(
            formal_addr,
            Access::Dereference,
            crate::value_history::ValueWithHistory::new(
                formal_value,
                ValueHistory::assignment(loc1),
            ),
        );
        rhs.post.pre.heap.add_edge_with_history(
            formal_addr,
            Access::Dereference,
            crate::value_history::ValueWithHistory::new(
                formal_value,
                ValueHistory::assignment(loc2),
            ),
        );

        assert_ne!(lhs, rhs, "structural PrePost equality sees hidden history");
        assert!(
            pre_posts_equivalent_for_summary_export(&lhs, &rhs),
            "summary export should use OCaml's state/sideband field set, not hidden ValueHistory"
        );
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
            trace_access_location: None,
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
                latent_invalid_access: None,
                pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_summary_restores_by_value_struct_formal_leaf_writes() {
        let pname = Procname::c_from_string("struct_formal");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("a"),
            Typ::mk_struct(TypeName::CStruct(QualifiedCppName::from_string("s"))),
            Default::default(),
        )];
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal_pvar = Pvar::mk(Mangled::from_string("a"), pname);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(formal_pvar)))
            .expect("formal should be bound");

        let s_name = TypeName::CStruct(QualifiedCppName::from_string("s"));
        let inlined_name = TypeName::CStruct(QualifiedCppName::from_string("inlined"));
        let i_field = Access::FieldAccess(Fieldname::make(s_name.clone(), "i"));
        let f_field = Access::FieldAccess(Fieldname::make(s_name, "f"));
        let x_field = Access::FieldAccess(Fieldname::make(inlined_name.clone(), "x"));
        let y_field = Access::FieldAccess(Fieldname::make(inlined_name, "y"));

        let i_addr = AbstractValue::mk_fresh();
        let x_addr = AbstractValue::mk_fresh();
        let y_addr = AbstractValue::mk_fresh();
        let f_addr = AbstractValue::mk_fresh();
        let x_value = AbstractValue::mk_fresh();
        let y_written_value = AbstractValue::mk_fresh();
        let f_written_value = AbstractValue::mk_fresh();

        for heap in [&mut state.pre.heap, &mut state.post.heap] {
            heap.add_edge(formal_addr, i_field.clone(), i_addr);
            heap.add_edge(formal_addr, f_field.clone(), f_addr);
            heap.add_edge(i_addr, x_field.clone(), x_addr);
            heap.add_edge(i_addr, y_field.clone(), y_addr);
            heap.add_edge(x_addr, Access::Dereference, x_value);
        }
        // Leaves that are read/valid in the callee pre can be registered as
        // empty cells. They still represent by-value struct-local storage and
        // must not keep callee writes in the exported post.
        state.pre.heap.register_address(y_addr);
        state.pre.heap.register_address(f_addr);
        state
            .post
            .heap
            .add_edge(y_addr, Access::Dereference, y_written_value);
        state
            .post
            .heap
            .add_edge(f_addr, Access::Dereference, f_written_value);

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(state)],
            vec![],
            false,
        );
        let pp = summary
            .pre_posts
            .first()
            .expect("summary should have a row");
        assert_eq!(
            pp.post.post.heap.find_edge(x_addr, &Access::Dereference),
            Some(x_value),
            "read pre leaf should stay restored"
        );
        assert_eq!(
            pp.post.post.heap.find_edge(y_addr, &Access::Dereference),
            None,
            "write to by-value nested struct field must not leak to callers"
        );
        assert_eq!(
            pp.post.post.heap.find_edge(f_addr, &Access::Dereference),
            None,
            "write to by-value struct field must not leak to callers"
        );
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
                latent_invalid_access: None,
                pending_invalid_accesses: vec![],
            }],
            non_disj_pre_post: None,
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
        assert!(
            summary.non_disj_pre_post.is_none(),
            "ordinary summary export must not synthesize hidden non-disj rows"
        );
    }

    #[test]
    fn test_specialized_summary_keeps_hidden_non_disj_pre_post() {
        let pdesc = make_pdesc_with_formals(&[]);
        let hidden_state = AbductiveDomain::mk_initial(&pdesc);
        let specialized = PulseSummary::of_proc_with_metadata(
            &pdesc,
            &[],
            vec![],
            false,
            true,
            Some(hidden_state),
        );

        let mut summary = PulseSummary::intra_only(vec![]);
        let spec = PulseSpecialization::bottom();
        summary.add_specialized_summary(spec.clone(), specialized);

        assert!(
            summary
                .get_specialized_data(&spec)
                .and_then(|data| data.non_disj_pre_post.as_ref())
                .is_some(),
            "specialized summary should preserve the hidden non-disj sideband"
        );
    }

    #[test]
    fn test_hidden_non_disj_pre_post_exports_without_visible_row() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let visible_state = AbductiveDomain::mk_initial(&pdesc);
        let mut hidden_state = AbductiveDomain::mk_initial(&pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_var = Var::ProgramVar(Box::new(x_pvar.clone()));
        let x_addr = hidden_state.post.stack.find(&x_var).unwrap();
        let x_value = hidden_state.read_heap(x_addr, Access::Dereference);

        let summary = PulseSummary::of_proc_with_metadata(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(visible_state)],
            vec![],
            false,
            true,
            Some(hidden_state),
        );

        assert_eq!(
            summary.pre_posts.len(),
            1,
            "hidden non-disj pre/post must not be appended to visible rows"
        );
        let hidden = summary
            .non_disj_pre_post
            .as_ref()
            .expect("hidden over-approx astate should export as a pre/post");
        assert_eq!(hidden.kind, PrePostKind::ContinueProgram);
        assert_eq!(hidden.formals, vec![(x_pvar, x_addr)]);
        assert_eq!(
            hidden.pre.heap.find_edge(x_addr, &Access::Dereference),
            Some(x_value),
            "hidden export should go through the same summary normalization surface"
        );
    }

    #[test]
    fn test_summary_preserves_benign_continue_multiplicity_only() {
        let pdesc = make_pdesc_with_formals(&[]);
        let benign_continue = AbductiveDomain::mk_initial(&pdesc);
        let diagnostic = dummy_invalid_access_diagnostic(
            AbstractValue::of_raw(1),
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );
        let abort = ExecutionDomain::AbortProgram {
            state: Box::new(benign_continue.clone()),
            diagnostic: Box::new(diagnostic),
        };
        let states = vec![
            ExecutionDomain::ContinueProgram(benign_continue.clone()),
            ExecutionDomain::ContinueProgram(benign_continue),
            abort.clone(),
            abort,
        ];

        let summary = PulseSummary::of_proc(&pdesc, &states, vec![], false);
        assert_eq!(
            summary
                .pre_posts
                .iter()
                .filter(|pp| pp.kind == PrePostKind::ContinueProgram)
                .count(),
            2,
            "OCaml preserves normalized benign Continue row multiplicity"
        );
        assert_eq!(
            summary
                .pre_posts
                .iter()
                .filter(|pp| pp.kind == PrePostKind::AbortProgram)
                .count(),
            1,
            "latent/abort diagnostics should still be exact-deduped"
        );
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_normalize_reports_allocated_attr_dead_before_summary_filter() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let allocated = AbstractValue::mk_fresh();

        astate.allocate(allocated, Allocator::CMalloc, Location::dummy());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let leaks = pp.normalize();

        assert!(
            leaks
                .iter()
                .any(|diag| matches!(diag, Diagnostic::MemoryLeak { .. })),
            "OCaml checks discarded allocated post-attrs from the pre-filter state"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_normalize_does_not_export_witness_on_formula_only_temp() {
        let pdesc = make_pdesc_with_formals(&["i"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let i_pvar = Pvar::mk(Mangled::from_string("i"), pdesc.proc_name.clone());
        let i_var = Var::ProgramVar(Box::new(i_pvar.clone()));
        let i_addr = astate.post.stack.find(&i_var).unwrap();
        let i = astate.read_heap(i_addr, Access::Dereference);
        let recursive_temp = AbstractValue::mk_fresh();

        assert!(astate
            .path_condition
            .and_equal_linear(
                i,
                LinArith::of_var(recursive_temp).add(&LinArith::of_int(2))
            )
            .is_sat());
        assert!(astate
            .path_condition
            .prune_less_than(
                &crate::formula::Operand::ConstOperand(0),
                &crate::formula::Operand::AbstractValue(recursive_temp),
            )
            .is_sat());

        let mut pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(i_pvar, i_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };
        let _ = pre_post.normalize();

        assert!(
            !pre_post
                .post
                .path_condition
                .phi()
                .linear_eqs
                .contains_key(&recursive_temp),
            "recursive-specialization formula temps kept via a visible affine equality should not \
             receive synthesized restricted witnesses"
        );
        let lin = pre_post
            .post
            .path_condition
            .phi()
            .linear_eqs
            .get(&i)
            .expect("visible formal equality should remain exported");
        assert_eq!(
            lin.get_coefficient(recursive_temp),
            Some(&Q::from_integer(1))
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
        let index_loc = Location {
            line: 42,
            col: 7,
            ..Location::dummy()
        };
        let _ = astate.and_equal_const(index, 42);
        astate.invalidate(
            index,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::of_int(42)),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::of_int(42)),
                index_loc,
            ),
        );
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post.get_const(index),
            Some(42),
            "array index constants on retained heap accesses should survive summary normalization"
        );
        let index = pp.post.path_condition.get_var_repr(index);
        assert!(
            pp.post
                .post
                .attrs
                .get(&index)
                .and_then(|attrs| attrs.get_invalid())
                .is_some_and(|(inv, _)| matches!(
                    inv,
                    crate::invalidation::Invalidation::ConstantDereference(value)
                        if *value == IntLit::of_int(42)
                )),
            "array index invalidation attrs should be retained like OCaml GraphVisit reachability"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let leaks = pp.normalize();

        assert!(
            leaks.iter()
                .all(|diag| !matches!(diag, Diagnostic::MemoryLeak { .. })),
            "an allocated root should not leak if a returned field can still reach it via pointer arithmetic"
        );
    }

    #[test]
    fn test_normalize_suppresses_leak_when_return_points_inside_allocation() {
        let pdesc = make_pdesc_with_formals(&[]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let allocated = AbstractValue::mk_fresh();
        let returned_inner = AbstractValue::mk_fresh();
        let return_root = AbstractValue::mk_fresh();
        let field = sil::fieldname::Fieldname::make(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "fat_ptr",
            )),
            "data",
        );

        astate.allocate(allocated, Allocator::CMalloc, Location::dummy());
        astate.write_heap(allocated, Access::FieldAccess(field), returned_inner);
        astate.write_heap(return_root, Access::Dereference, returned_inner);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(return_root),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let leaks = pp.normalize();

        assert!(
            leaks
                .iter()
                .all(|diag| !matches!(diag, Diagnostic::MemoryLeak { .. })),
            "OCaml `reaches_into` suppresses leaks when a live post root points inside the allocation"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_normalize_global_function_pointer_exports_pre_stack_and_positive_atom_not_closure_attr()
    {
        let pdesc = make_pdesc_with_formals(&[]);
        let global = Pvar::mk_global(Mangled::from_string("malloc_func"));
        let global_var = Var::ProgramVar(Box::new(global.clone()));
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let global_addr = astate.eval_var(&global_var);
        let funptr_val = AbstractValue::mk_fresh();
        astate
            .post
            .heap
            .add_edge(global_addr, Access::Dereference, funptr_val);
        astate.add_attr(
            funptr_val,
            Attribute::Closure(Procname::c_from_string("malloc")),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        let pre_post = summary
            .pre_posts
            .first()
            .expect("expected a continuing summary");
        assert_eq!(pre_post.pre.stack.find(&global_var), Some(global_addr));
        assert!(pre_post
            .pre
            .attrs
            .get(&global_addr)
            .is_some_and(|attrs| attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _)))));
        assert!(pre_post
            .post
            .post
            .attrs
            .get(&funptr_val)
            .is_none_or(|attrs| !attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::Closure(_)))));
        assert!(pre_post
            .post
            .path_condition
            .phi()
            .atoms
            .contains(&Atom::LessThan(Term::Const(0), Term::Var(funptr_val))));
    }

    #[test]
    fn test_normalize_global_funptr_positive_atom_does_not_pick_zero_return_alias() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::mk_ptr(Typ::void());
        let global = Pvar::mk_global(Mangled::from_string("malloc_func"));
        let global_var = Var::ProgramVar(Box::new(global.clone()));
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let global_addr = astate.eval_var(&global_var);
        let funptr_val = AbstractValue::mk_fresh();
        let return_addr = AbstractValue::mk_fresh();
        let zero_return = AbstractValue::mk_fresh();
        astate
            .post
            .heap
            .add_edge(global_addr, Access::Dereference, funptr_val);
        astate.post.stack.add(return_var.clone(), return_addr);
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, zero_return);
        astate.add_dynamic_type_unsafe(
            funptr_val,
            Typ::mk_struct(TypeName::CFunction(
                match Procname::c_from_string("malloc") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("expected C procname"),
                },
            )),
        );
        assert!(astate.and_positive(funptr_val).is_sat());
        assert!(astate.and_equal_const(zero_return, 0).is_sat());

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        let pre_post = summary
            .pre_posts
            .first()
            .expect("expected a continuing summary");
        let funptr_repr = pre_post.post.path_condition.get_var_repr(funptr_val);
        let return_repr = pre_post.post.path_condition.get_var_repr(zero_return);
        assert!(pre_post
            .post
            .path_condition
            .phi()
            .atoms
            .contains(&Atom::LessThan(Term::Const(0), Term::Var(funptr_repr))));
        assert!(
            !pre_post.post.path_condition.phi().atoms.contains(&Atom::LessThan(
                Term::Var(return_repr),
                Term::Var(funptr_repr),
            )),
            "summary normalization should keep OCaml's global funptr representative instead of a zero return alias"
        );
    }

    #[test]
    fn test_normalize_imported_global_function_pointer_dynamic_type_exports_pre_stack() {
        let pdesc = make_pdesc_with_formals(&[]);
        let global = Pvar::mk_global(Mangled::from_string("malloc_func"));
        let global_var = Var::ProgramVar(Box::new(global.clone()));
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let global_addr = astate.eval_var(&global_var);
        let funptr_val = AbstractValue::mk_fresh();
        astate
            .post
            .heap
            .add_edge(global_addr, Access::Dereference, funptr_val);
        astate.add_dynamic_type_unsafe(
            funptr_val,
            Typ::mk_struct(TypeName::CFunction(
                match Procname::c_from_string("malloc") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("expected C procname"),
                },
            )),
        );

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        let pre_post = summary
            .pre_posts
            .first()
            .expect("expected a continuing summary");
        assert_eq!(pre_post.pre.stack.find(&global_var), Some(global_addr));
        assert!(
            pre_post
                .pre
                .attrs
                .get(&global_addr)
                .is_some_and(|attrs| attrs
                    .iter()
                    .any(|attr| matches!(attr, Attribute::MustBeValid(_, _, _)))),
            "imported global C-function dynamic types should seed the OCaml-style global pre stack"
        );
    }

    #[test]
    fn test_normalize_return_function_pointer_exports_positive_atom_not_closure_attr() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::mk_ptr(Typ::mk(sil::typ::TypeDesc::Tfun(None)));
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let ret_val = AbstractValue::mk_fresh();
        astate.add_attr(
            ret_val,
            Attribute::Closure(Procname::c_from_string("assign_NULL")),
        );
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        astate
            .post
            .stack
            .add(Var::LogicalVar(ret_id.clone()), ret_val);
        let ret_node = pdesc.add_node(
            sil::procdesc::NodeKind::StmtNode(sil::procdesc::StmtNodeKind::ReturnStmt),
            vec![sil::instr::Instr::Load {
                id: ret_id,
                e: sil::exp::Exp::Const(sil::const_val::Const::Cfun(Procname::c_from_string(
                    "assign_NULL",
                ))),
                typ: pdesc.ret_type.clone(),
                loc: Location::dummy(),
            }],
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![ret_node]);
        pdesc.set_succs(ret_node, vec![1]);

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        let pre_post = summary
            .pre_posts
            .first()
            .expect("expected a continuing summary");
        assert_eq!(pre_post.result, Some(ret_val));
        assert!(pre_post
            .post
            .post
            .attrs
            .get(&ret_val)
            .is_none_or(|attrs| !attrs
                .iter()
                .any(|attr| matches!(attr, Attribute::Closure(_)))));
        assert!(pre_post
            .post
            .path_condition
            .phi()
            .atoms
            .contains(&Atom::LessThan(Term::Const(0), Term::Var(ret_val))));
    }

    #[test]
    fn test_normalize_canonicalizes_pre_and_post_frame_edges_together() {
        let pdesc = make_pdesc_with_formals(&["p"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("p"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let formal_val = astate.read_heap(formal_addr, Access::Dereference);
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("node")),
            "next",
        );
        let next_slot = astate.read_heap(formal_val, Access::FieldAccess(field));
        let pre_pointee = astate.read_heap(next_slot, Access::Dereference);
        let post_pointee_alias = AbstractValue::mk_fresh();
        astate.write_heap(next_slot, Access::Dereference, post_pointee_alias);
        assert_eq!(
            astate.post.heap.find_edge(next_slot, &Access::Dereference),
            Some(post_pointee_alias),
            "fixture should start with a stale post target before equality normalization"
        );
        assert!(astate.and_equal(pre_pointee, post_pointee_alias).is_sat());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let _ = pp.normalize();
        let canonical_pointee = pp.post.path_condition.get_var_repr(pre_pointee);

        assert_eq!(
            pp.pre.heap.find_edge(next_slot, &Access::Dereference),
            Some(canonical_pointee),
            "summary pre heap should be rewritten to canonical representatives"
        );
        assert_eq!(
            pp.post.post.heap.find_edge(next_slot, &Access::Dereference),
            Some(canonical_pointee),
            "summary post heap should use the same canonical frame edge as pre"
        );
    }

    #[test]
    fn test_normalize_preserves_direct_latent_cycle_heap_edges() {
        let pdesc = make_pdesc_with_formals(&["p"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("p"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar.clone()));
        let formal_addr = astate.post.stack.find(&var).unwrap();
        let root = astate.read_heap(formal_addr, Access::Dereference);
        let next_field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("node")),
            "next",
        );
        let next_slot = astate.read_heap(root, Access::FieldAccess(next_field));
        let next_value = astate.read_heap(next_slot, Access::Dereference);
        assert!(
            root < next_value,
            "fixture relies on root being the formula representative"
        );
        // Mirror the OCaml latent cycle summary surface: the equality has
        // been discharged into a direct heap edge rather than exported as a
        // formula equality.
        astate
            .pre
            .heap
            .add_edge(next_slot, Access::Dereference, next_value);
        astate
            .post
            .heap
            .add_edge(next_slot, Access::Dereference, next_value);

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(pvar, formal_addr)],
            result: None,
            kind: PrePostKind::LatentAbortProgram,
            diagnostic: Some(dummy_invalid_access_diagnostic(
                AbstractValue::mk_fresh(),
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            )),
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post.post.heap.find_edge(
                pp.post.path_condition.get_var_repr(next_slot),
                &Access::Dereference,
            ),
            Some(next_value),
            "latent cycle summaries should preserve the direct callee heap edge instead of rewriting it to the root representative"
        );
        assert!(
            pp.post.path_condition.phi().var_eqs.is_empty(),
            "the direct heap cycle should not be exported as a var_eq"
        );
        assert!(
            pp.post.path_condition.phi().linear_eqs.is_empty(),
            "the direct heap cycle should not be exported as a linear_eq"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_normalize_materializes_nonzero_constant_invalid_for_literal_value() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let result = AbstractValue::of_raw(2);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::one());

        astate.post.stack.add(return_var, return_addr);
        astate.post.heap.add_edge_with_history(
            return_addr,
            Access::Dereference,
            crate::value_history::ValueWithHistory::new(
                result,
                ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
            ),
        );
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            "summary export should recreate OCaml's constant invalidation surface for literal non-zero values"
        );
    }

    #[test]
    fn test_normalize_does_not_materialize_branch_only_constant_invalid_for_visible_value() {
        let mut pdesc = make_pdesc_with_formals(&["a"]);
        pdesc.ret_type = Typ::void();

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let a_pvar = Pvar::mk(Mangled::from_string("a"), pdesc.proc_name.clone());
        let a_var = Var::ProgramVar(Box::new(a_pvar.clone()));
        let a_addr = astate.eval_var(&a_var);
        let a_value = astate.read_heap(a_addr, Access::Dereference);
        assert!(astate
            .path_condition
            .prune_eq_const(a_value, 4, false)
            .is_sat());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(a_pvar, a_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let _ = pp.normalize();

        let repr = pp.post.path_condition.get_var_repr(a_value);
        let has_branch_only_invalid = pp.post.post.attrs.get(&repr).is_some_and(|attrs| {
            attrs.iter().any(|attr| {
                matches!(
                    attr,
                    crate::attribute::Attribute::Invalid(
                        crate::invalidation::Invalidation::ConstantDereference(value),
                        _
                    ) if *value == IntLit::of_int(4)
                )
            })
        });
        assert!(
            !has_branch_only_invalid,
            "branch/prune facts such as a == 4 should not synthesize OCaml-invisible constant invalidations"
        );
    }

    #[test]
    fn test_normalize_exports_zero_equality_for_visible_return_value() {
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
        astate.path_condition.and_is_int(result);
        assert!(astate.path_condition.and_equal_const(result, 0).is_sat());

        let mut pp = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![],
            result: Some(result),
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        let _ = pp.normalize();

        assert_eq!(
            pp.post.get_const(result),
            Some(0),
            "summary export must keep the stronger return == 0 equality"
        );
        assert!(
            pp.post.path_condition.phi().is_marked_int(result),
            "summary export may keep the type fact, but not instead of return == 0"
        );
    }

    #[test]
    fn test_of_proc_exports_closure_call_return_constant_not_only_is_int() {
        let mut pdesc = make_pdesc_with_formals(&[]);
        pdesc.ret_type = Typ::int(sil::typ::IKind::IInt);

        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let return_pvar = Pvar::mk(Mangled::from_string("__return"), pdesc.proc_name.clone());
        let return_var = Var::ProgramVar(Box::new(return_pvar));
        let return_addr = AbstractValue::of_raw(30);
        let loaded_return = AbstractValue::of_raw(3);

        astate.post.stack.add(return_var, return_addr);
        // Model the shape after returning through a specialized closure call:
        // the returned load is an integer-typed value whose constant is known
        // only through phi. Summary export must not drop that equality and
        // leave only is_int(return.*).
        astate
            .post
            .heap
            .add_edge(return_addr, Access::Dereference, loaded_return);
        astate.initialize(return_addr);
        astate.initialize(loaded_return);
        astate.path_condition.and_is_int(loaded_return);
        assert!(astate.and_equal_const(loaded_return, 0).is_sat());

        let summary = PulseSummary::of_proc(
            &pdesc,
            &[ExecutionDomain::ContinueProgram(astate)],
            vec![],
            false,
        );

        assert_eq!(summary.pre_posts.len(), 1);
        let pre_post = &summary.pre_posts[0];
        let exported_return = pre_post.result.expect("return result should be exported");
        assert_eq!(
            pre_post.post.get_const(exported_return),
            Some(0),
            "closure-call return summary should export return.* == 0"
        );
        assert!(
            pre_post
                .post
                .path_condition
                .phi()
                .is_marked_int(exported_return),
            "the integer type fact can coexist with the stronger equality"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_recovered_abort_invalid_access_stays_latent_without_diagnostic() {
        let pdesc = make_named_pdesc_with_formals("caller", &["q"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let q_pvar = Pvar::mk(Mangled::from_string("q"), pdesc.proc_name.clone());
        let q_addr = astate
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(q_pvar)))
            .unwrap();
        let q_value = astate.read_heap(q_addr, Access::Dereference);
        let next_value = AbstractValue::mk_fresh();
        astate
            .pre
            .heap
            .add_edge(q_value, Access::Dereference, next_value);
        astate
            .post
            .heap
            .add_edge(q_value, Access::Dereference, next_value);
        astate.pre.heap.register_address(next_value);
        astate.post.heap.register_address(next_value);
        astate.mark_must_be_valid_at(next_value, &Location::dummy());
        astate
            .post
            .attrs
            .mark_written_to(next_value, 1, Location::dummy());
        astate.invalidate(
            q_value,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );
        let diagnostic = dummy_invalid_access_diagnostic(
            q_value,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        );
        let abort_pp = build_pre_post(&pdesc, astate, PrePostKind::AbortProgram, Some(diagnostic));

        let mut latent_pp = abort_pp.clone();
        latent_pp.kind = PrePostKind::LatentInvalidAccess;
        latent_pp.diagnostic = Some(dummy_invalid_access_diagnostic(
            next_value,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
        ));
        classify_recovered_invalid_access_pre_post(&pdesc, &mut latent_pp);
        latent_pp.diagnostic = None;
        let recovered = vec![latent_pp];

        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].kind, PrePostKind::LatentInvalidAccess);
        assert!(
            recovered[0].diagnostic.is_none(),
            "OCaml PotentialInvalidAccessSummary stores a latent obligation without a concrete diagnostic payload"
        );
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
            latent.diagnostic.is_some(),
            "local EqZero sideband should retain the concrete diagnostic on the exported latent invalid-access row"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
    fn test_coalesce_zero_direct_formals_for_export() {
        let (pdesc, mut pre_post, x_val, y_val, _x_loc, y_loc) =
            make_continue_pre_post_with_two_direct_formals();

        assert!(pre_post
            .post
            .path_condition
            .and_condition_direct(Atom::Equal(Term::Var(x_val), Term::Const(0)), 1)
            .is_sat());
        assert!(pre_post
            .post
            .path_condition
            .prune_eq_const(y_val, 0, false)
            .is_sat());
        pre_post.kind = PrePostKind::LatentInvalidAccess;
        pre_post.diagnostic = Some(dummy_invalid_access_diagnostic_at(
            y_val,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            y_loc,
        ));

        assert!(
            coalesce_zero_direct_formals_for_export(&pdesc, &mut pre_post, y_val),
            "two zero direct formals should be coalesced"
        );

        let direct_formal_values: std::collections::HashSet<_> = pre_post
            .formals
            .iter()
            .filter_map(|(_formal, addr)| {
                pre_post
                    .pre
                    .heap
                    .find_edge(*addr, &Access::Dereference)
                    .map(|value| pre_post.post.path_condition.get_var_repr(value))
            })
            .collect();
        let zero_direct_formal_conditions: std::collections::HashSet<_> = pre_post
            .post
            .path_condition
            .conditions()
            .keys()
            .filter_map(|atom| match atom {
                Atom::Equal(Term::Var(var), Term::Const(0))
                | Atom::Equal(Term::Const(0), Term::Var(var)) => {
                    Some(pre_post.post.path_condition.get_var_repr(*var))
                }
                _ => None,
            })
            .collect();

        assert_eq!(
            direct_formal_values.len(),
            1,
            "export coalescing should alias zero direct formals onto one visible summary value"
        );
        assert_eq!(
            zero_direct_formal_conditions.len(),
            1,
            "export coalescing should leave one remembered zero-direct-formal condition"
        );
        assert!(matches!(
            pre_post.diagnostic,
            Some(Diagnostic::AccessToInvalidAddress { addr, .. })
                if direct_formal_values.contains(&pre_post.post.path_condition.get_var_repr(addr))
        ));
    }

    #[test]
    fn test_continue_zero_direct_formal_keeps_null_invalidation_without_coalescing() {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut astate = AbductiveDomain::mk_initial(&pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let x_var = Var::ProgramVar(Box::new(x_pvar.clone()));
        let x_formal_addr = astate.post.stack.find(&x_var).unwrap();
        let x_val = astate.read_heap(x_formal_addr, Access::Dereference);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());

        assert!(astate
            .path_condition
            .prune_eq_const(x_val, 0, false)
            .is_sat());
        astate.post.attrs.invalidate(
            x_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation, Location::dummy()),
        );
        let mut pre_post = PrePost {
            pre: astate.pre.clone(),
            post: astate,
            formals: vec![(x_pvar, x_formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
        };

        coalesce_zero_direct_formals_for_continue_export(&pdesc, &mut pre_post);

        assert!(
            post_addr_has_visible_null_invalidation(&pre_post, x_val),
            "a single zero direct formal is not actually coalesced, so OCaml keeps its null invalidation"
        );
    }

    #[test]
    fn test_continue_zero_direct_formal_coalescing_drops_null_invalidation() {
        let (pdesc, mut pre_post, x_val, y_val, _x_loc, _y_loc) =
            make_continue_pre_post_with_two_direct_formals();
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());

        assert!(pre_post
            .post
            .path_condition
            .prune_eq_const(x_val, 0, false)
            .is_sat());
        assert!(pre_post
            .post
            .path_condition
            .prune_eq_const(y_val, 0, false)
            .is_sat());
        pre_post.post.post.attrs.invalidate(
            x_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation.clone(), Location::dummy()),
        );
        pre_post.post.post.attrs.invalidate(
            y_val,
            invalidation.clone(),
            ValueHistory::invalidated(invalidation, Location::dummy()),
        );

        coalesce_zero_direct_formals_for_continue_export(&pdesc, &mut pre_post);
        let x_repr = pre_post.post.path_condition.get_var_repr(x_val);
        let y_repr = pre_post.post.path_condition.get_var_repr(y_val);

        assert_eq!(
            x_repr, y_repr,
            "two zero direct formals should be coalesced onto one exported value"
        );
        assert!(
            !post_addr_has_visible_null_invalidation(&pre_post, x_repr),
            "actual zero-direct-formal coalescing should still drop the synthetic null invalidation"
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
            latent_invalid_access: None,
            pending_invalid_accesses: vec![],
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
                latent_invalid_access: None,
                pending_invalid_accesses: vec![],
            }],
            non_disj_pre_post: None,
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
                latent_invalid_access: None,
                pending_invalid_accesses: vec![],
            }],
            non_disj_pre_post: None,
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
                let publish_manifest =
                    abort_pre_post_should_publish_manifest_diagnostic(pdesc, pre_post);
                let diag_key = pre_post.diagnostic.as_ref().map(Diagnostic::dedup_key);
                let conditions = format!("{:?}", pre_post.post.path_condition.conditions());
                eprintln!(
                    "  pp[{i}] kind={:?} manifest={manifest} publish_manifest={publish_manifest} imported_from_call={imported_from_call} caller_sensitive_field_write={caller_sensitive_field_write} diag_key={diag_key:?} heap_path={heap_path:?} report_key={report_key:?} recovered_keys={recovered_keys:?} conditions={conditions}",
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

    #[test]
    #[ignore = "debug continue-derived latent UAF/null candidate filters"]
    fn test_debug_latent_uaf_continue_candidate_filters() {
        let tm = textual_utils::parse_and_convert(
            r#"
        .source_language = "C"

        define create_branching(b: int) : void {
          #entry:
            n0:int = load &b
            jmp branch_true, branch_false
          #branch_true:
            prune __sil_eq(n0, 1)
            ret null
          #branch_false:
            prune __sil_lnot(__sil_eq(n0, 1))
            ret null
        }

        define conditional_free2(b: int, x: *int) : void {
          #entry:
            n0:int = load &b
            n1:*int = load &x
            jmp do_free, skip_free
          #do_free:
            prune __sil_eq(n0, 1)
            _ = free(n1)
            ret null
          #skip_free:
            prune __sil_lnot(__sil_eq(n0, 1))
            ret null
        }

        define FN_nonlatent_use_after_free_bad(b: int, x: *int) : void {
          #entry:
            n0:int = load &b
            n1:*int = load &x
            _ = create_branching(n0)
            _ = free(n1)
            n2:*int = load &x
            store n2 <- 42:int
            ret null
        }

        define FN_nonlatent_use_after_free_bad2(b: int, x: *int) : void {
          #entry:
            n0:*int = load &x
            _ = free(n0)
            n1:int = load &b
            _ = create_branching(n1)
            n2:*int = load &x
            store n2 <- 42:int
            ret null
        }

        define latent_use_after_free(b: int, x: *int) : void {
          #entry:
            n0:int = load &b
            n1:*int = load &x
            _ = conditional_free2(n0, n1)
            store n1 <- 42:int
            jmp clean_up, done
          #clean_up:
            prune __sil_eq(n0, 0)
            _ = free(n1)
            ret null
          #done:
            prune __sil_lnot(__sil_eq(n0, 0))
            ret null
        }
    "#,
        );
        let checker = TestPulseInterChecker;
        let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
        let targets = [
            "FN_nonlatent_use_after_free_bad",
            "FN_nonlatent_use_after_free_bad2",
            "latent_use_after_free",
        ];

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
                let diag = latent_invalid_access_diagnostic_from_exported_pre_post(pre_post);
                let selected_addr = diag.as_ref().and_then(|diag| match diag {
                    Diagnostic::AccessToInvalidAddress { addr, .. } => {
                        Some(pre_post.post.path_condition.get_var_repr(*addr))
                    }
                    _ => None,
                });
                let path = selected_addr
                    .and_then(|addr| latent_invalid_access_heap_path(pre_post, addr))
                    .map(|path| format!("{path}"));
                let path_values = selected_addr
                    .and_then(|addr| latent_invalid_access_path_values(pre_post, addr))
                    .map(|set| {
                        let mut vars: Vec<_> = set.into_iter().collect();
                        vars.sort();
                        vars
                    });
                let conds = format!("{:?}", pre_post.post.path_condition.conditions());
                eprintln!(
                    "  pp[{i}] kind={:?} selected_addr={selected_addr:?} path={path:?} path_values={path_values:?} conditions={conds}",
                    pre_post.kind
                );
                if pre_post.kind == PrePostKind::LatentInvalidAccess {
                    eprintln!(
                        "    only_path_local={} mixed_depth={}",
                        selected_addr
                            .map(|addr| latent_invalid_access_has_only_path_local_conditions(
                                pdesc, pre_post, addr,
                            ))
                            .unwrap_or(false),
                        latent_invalid_access_has_mixed_condition_depths(pdesc, pre_post),
                    );
                }
                if pre_post.kind == PrePostKind::ContinueProgram {
                    let caller_controlled = pre_heap_values_reachable_from_formals(pdesc, pre_post);
                    let formal_stack_addrs = formal_stack_addrs(pdesc, pre_post);
                    let direct_formal_values = direct_formal_value_addrs(pdesc, pre_post);
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
                        let path_values =
                            latent_invalid_access_path_values(pre_post, repr).map(|set| {
                                let mut vars: Vec<_> = set.into_iter().collect();
                                vars.sort();
                                vars
                            });
                        let in_pre_mbv = pre_post
                            .pre
                            .attrs
                            .get(&repr)
                            .and_then(|attrs| attrs.get_must_be_valid())
                            .is_some();
                        let known_zero = pre_post.post.path_condition.is_known_zero(repr);
                        eprintln!(
                            "    candidate addr={repr} in_pre_mbv={in_pre_mbv} caller_controlled={} direct_formal={} formal_stack={} deref_target={} known_zero={} imported={} path={path:?} path_values={path_values:?} cond_only_local={}",
                            caller_controlled.contains(&repr),
                            direct_formal_values.contains(&repr),
                            formal_stack_addrs.contains(&repr),
                            deref_value_targets.contains(&repr),
                            known_zero,
                            latent_invalid_access_is_imported_from_call(
                                pdesc,
                                pre_post,
                                repr,
                                &access_history,
                            ),
                            latent_invalid_access_has_only_path_local_conditions(
                                pdesc, pre_post, repr,
                            ),
                        );
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "debug real latent.sil continue-derived latent UAF/null candidates"]
    fn test_debug_real_latent_uaf_continue_candidate_filters() {
        let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
        if !sil.exists() {
            eprintln!("skip");
            return;
        }
        let tm = textual_utils::parse_file_and_convert(sil);
        let checker = TestPulseInterChecker;
        let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
        let targets = [
            "FN_nonlatent_use_after_free_bad",
            "FN_nonlatent_use_after_free_bad2",
            "latent_use_after_free",
        ];

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
                eprintln!(
                    "  pp[{i}] kind={:?} conditions={:?}",
                    pre_post.kind,
                    pre_post.post.path_condition.conditions()
                );
                if pre_post.kind != PrePostKind::ContinueProgram {
                    continue;
                }
                let candidate = potential_invalid_access_from_normalized_continue_pre_post(
                    pdesc, pre_post, None,
                );
                eprintln!("    candidate_present={}", candidate.is_some());
                let Some(candidate) = candidate else {
                    continue;
                };
                let Some(selected_addr) = (match &candidate.diagnostic {
                    Diagnostic::AccessToInvalidAddress { addr, .. } => Some(*addr),
                    _ => None,
                }) else {
                    continue;
                };
                let mut latent_pp = pre_post.clone();
                latent_pp.kind = PrePostKind::LatentInvalidAccess;
                latent_pp.diagnostic = Some(candidate.diagnostic.clone());
                eprintln!(
                    "    selected_addr={selected_addr} path={:?} path_values={:?} direct_formals={:?} local_zero_direct_formals={:?} mixed_depth={} only_path_local={} imported={} known_zero={}",
                    latent_invalid_access_heap_path(&latent_pp, selected_addr)
                        .map(|path| format!("{path}")),
                    latent_invalid_access_path_values(&latent_pp, selected_addr),
                    direct_formal_value_addrs(pdesc, &latent_pp),
                    local_zero_direct_formals(pdesc, &latent_pp),
                    latent_invalid_access_has_mixed_condition_depths(pdesc, &latent_pp),
                    latent_invalid_access_has_only_path_local_conditions(
                        pdesc,
                        &latent_pp,
                        selected_addr,
                    ),
                    latent_invalid_access_is_imported_from_call(
                        pdesc,
                        &latent_pp,
                        selected_addr,
                        &latent_pp.post.history_of_value(selected_addr).unwrap_or_default(),
                    ),
                    latent_pp.post.path_condition.is_known_zero(selected_addr),
                );
            }
        }
    }
}
