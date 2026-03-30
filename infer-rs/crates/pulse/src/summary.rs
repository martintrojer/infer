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

impl PrePost {
    /// Normalize the summary by discarding attributes on unreachable addresses.
    ///
    /// Matches OCaml's `discard_unreachable_ ~for_summary:true` which removes
    /// attributes on addresses not reachable from the pre/post stacks.
    /// This strips spurious Invalid attrs (e.g., on integer constants stored
    /// through pointers) that don't affect callers.
    fn normalize(&mut self) -> Vec<Diagnostic> {
        use std::collections::HashSet;

        // Collect all addresses reachable from pre stack, post stack,
        // formals, and return value
        let mut reachable = HashSet::new();
        let mut worklist = Vec::new();

        // Seeds: pre stack values
        for (_, addr) in self.pre.stack.iter() {
            worklist.push(*addr);
        }
        // Note: we intentionally do NOT seed from the post stack.
        // Local variables are not visible to callers, so their attrs
        // shouldn't be in the summary. Only formals and return value
        // matter. Matches OCaml's restore_formals_for_summary which
        // removes the post stack before discarding unreachable.
        // Seeds: formal addresses
        for (_, addr) in &self.formals {
            worklist.push(*addr);
        }
        // Seeds: return value
        if let Some(rv) = self.result {
            worklist.push(rv);
        }

        // BFS through heap edges
        while let Some(addr) = worklist.pop() {
            if !reachable.insert(addr) {
                continue;
            }
            // Follow pre heap edges
            if let Some(edges) = self.pre.heap.get_edges(addr) {
                for (_, target) in edges.iter() {
                    worklist.push(*target);
                }
            }
            // Follow post heap edges
            if let Some(edges) = self.post.post.heap.get_edges(addr) {
                for (_, target) in edges.iter() {
                    worklist.push(*target);
                }
            }
        }

        // Check for memory leaks: find addresses reachable from local
        // variables (post stack) but NOT from the summary's public interface
        // (formals, return value). These are procedure-local allocations
        // that the caller can never free.
        // Cross-ref: OCaml PulseAbductiveDomain.ml check_memory_leaks +
        // filter_live_addresses.
        let mut locally_reachable = HashSet::new();
        let mut local_worklist = Vec::new();
        for (_, addr) in self.post.post.stack.iter() {
            local_worklist.push(*addr);
        }
        while let Some(addr) = local_worklist.pop() {
            if !locally_reachable.insert(addr) {
                continue;
            }
            if let Some(edges) = self.post.post.heap.get_edges(addr) {
                for (_, target) in edges.iter() {
                    local_worklist.push(*target);
                }
            }
        }
        let leaks = self.check_memory_leaks(&reachable, &locally_reachable);

        // Remove attrs on unreachable addresses
        self.post.post.attrs.retain_reachable(&reachable);

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
        for (addr, attrs) in self.post.post.attrs.iter() {
            if !locally_reachable.contains(addr) {
                continue;
            }
            if summary_reachable.contains(addr) {
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
            leaks.push(Diagnostic::MemoryLeak {
                addr: *addr,
                allocator: allocator.clone(),
                allocation_location: alloc_loc.clone(),
            });
        }
        leaks
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
                )
            })
            .map(|state| {
                let (initial_kind, abort_diag) = match state {
                    ExecutionDomain::ExitProgram(_) => (PrePostKind::ExitProgram, None),
                    ExecutionDomain::AbortProgram { diagnostic, .. } => {
                        // Temporarily mark as AbortProgram; will reclassify below
                        (PrePostKind::AbortProgram, Some(diagnostic.as_ref().clone()))
                    }
                    _ => (PrePostKind::ContinueProgram, None),
                };
                let astate = state.get_astate().clone();

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
                let mut pp = PrePost {
                    pre,
                    post: astate,
                    formals,
                    result,
                    kind: initial_kind,
                    diagnostic: abort_diag,
                };
                let leak_diags = pp.normalize();
                // Only report leaks from ContinueProgram paths — error paths
                // (ExitProgram/AbortProgram) typically produce spurious leaks.
                // Cross-ref: OCaml PulseReport.ml summary_of_error_post ignores leaks.
                if pp.kind == PrePostKind::ContinueProgram {
                    diagnostics.extend(leak_diags);
                }

                // Classify AbortProgram as manifest or latent.
                // Manifest errors: report diagnostic now (at callee level).
                // Latent errors: defer to callers, re-evaluate at each call site.
                // Cross-ref: OCaml PulseSummary.ml exec_summary_of_post_common
                // calls report_summary_error which calls LatentIssue.should_report.
                if pp.kind == PrePostKind::AbortProgram {
                    // Reclassify as latent if the error depends on caller inputs.
                    // Latent pre_posts propagate to callers for re-evaluation.
                    // Diagnostics are extracted from the all-node scan in the
                    // checker (covering unreachable-exit cases like infinite loops).
                    if !is_manifest(&pp) {
                        pp.kind = PrePostKind::LatentAbortProgram;
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
    pub fn add_specialized(&mut self, spec: PulseSpecialization, pre_posts: Vec<PrePost>) {
        self.specialized.push((spec, pre_posts));
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
/// An error is manifest if no formula atoms reference formal parameter values.
/// If atoms constrain formal-derived values (e.g., `a == 4` where `a` is a formal),
/// the error is latent — it only manifests when a caller provides specific values.
///
/// Cross-ref: OCaml PulseArithmetic.ml is_manifest checks whether the path condition
/// only constrains allocated pointers to be non-null (benign constraints).
fn is_manifest(pre_post: &PrePost) -> bool {
    use std::collections::HashSet;

    // No formals → the error is entirely internal → manifest
    if pre_post.formals.is_empty() {
        return true;
    }

    // Collect formal parameter values by following the deref edge from
    // each formal's stack address in the post heap.
    let phi = pre_post.post.path_condition.phi();
    let mut param_vals: HashSet<AbstractValue> = HashSet::new();
    for (_, formal_addr) in &pre_post.formals {
        // Follow deref in post heap to find the parameter's current value
        if let Some(target) = pre_post
            .post
            .post
            .heap
            .find_edge(*formal_addr, &crate::access::Access::Dereference)
        {
            param_vals.insert(target);
        }
    }

    if param_vals.is_empty() {
        return true; // no parameter values found → manifest
    }

    // Check atoms for constraints on parameter values.
    // Atoms come from prune instructions (if/while conditions) and
    // interproc formula translation. If an atom constrains a formal
    // parameter value, the error depends on the caller's input.
    // We only check atoms, not linear_eqs, because linear_eqs can
    // contain spurious entries from local stores that overwrite formals.
    for atom in &phi.atoms {
        for v in atom.all_vars() {
            if param_vals.contains(&v) {
                return false; // constraint involves parameter → latent
            }
        }
    }

    true
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
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procname::Procname;
    use sil::typ::Typ;

    fn make_pdesc_with_formals(formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string("test_proc");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|name| (Mangled::from_string(*name), Typ::void(), Default::default()))
            .collect();
        pdesc
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
}
