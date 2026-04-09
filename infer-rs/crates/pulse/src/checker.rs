// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse checker: runs Pulse analysis on a procedure and collects diagnostics.
//!
//! Uses the absint framework's WTO fixpoint engine with a disjunctive domain,
//! matching OCaml's `MakeDisjunctive(PulseTransferFunctions)`.
//!
//! Each program point maintains a bounded set of abstract states (disjuncts).
//! At branch points, disjuncts flow independently. At merge points, disjuncts
//! from all predecessors are unioned. The WTO engine handles loops with proper
//! widening at loop heads.

use std::cell::RefCell;
use std::collections::HashMap;

use absint::disjunctive::DisjunctiveDomain;
use absint::interp;
use absint::transfer::TransferFunctions;
use diagnostics::issue::IssueLog;

use sil::const_val::Const;
use sil::exp::Exp;
use sil::ident::{Ident, IdentName};
use sil::instr::Instr;
use sil::location::Location;
use sil::procdesc::{NodeId, Procdesc};
use sil::procname::Procname;
use sil::specialization::PulseSpecialization;

use crate::abductive::AbductiveDomain;
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::pulse_result::PulseResult;
use crate::summary::PulseSummary;
use crate::transfer;

/// Run Pulse analysis on a procedure (intraprocedural, default config).
pub fn analyze(pdesc: &Procdesc) -> PulseSummary {
    analyze_with_summaries(pdesc, &HashMap::new())
}

/// Run Pulse analysis on a procedure with access to callee summaries.
///
/// Uses the WTO fixpoint engine with a disjunctive domain, matching
/// OCaml's `MakeDisjunctive(PulseTransferFunctions)`.
pub fn analyze_with_summaries(
    pdesc: &Procdesc,
    callee_summaries: &HashMap<Procname, PulseSummary>,
) -> PulseSummary {
    analyze_with_specialization(pdesc, callee_summaries, None)
}

/// Run Pulse analysis with optional specialization applied to the initial state.
///
/// When `specialization` is Some, the initial state is modified to bind
/// heap paths to known dynamic types (e.g., function pointer targets).
/// Cross-ref: OCaml Pulse.ml analyze with specialization parameter.
pub fn analyze_with_specialization(
    pdesc: &Procdesc,
    callee_summaries: &HashMap<Procname, PulseSummary>,
    specialization: Option<&sil::specialization::PulseSpecialization>,
) -> PulseSummary {
    analyze_with_specialization_and_requests(pdesc, callee_summaries, specialization).0
}

/// Run Pulse analysis and also collect specialization requests discovered at
/// actual call sites during the fixpoint.
///
/// This is the semantically correct source of specialization requests: the
/// current abstract state at the call already reflects prior calls, stores,
/// loads, and branch pruning in the caller.
pub fn analyze_with_specialization_and_requests(
    pdesc: &Procdesc,
    callee_summaries: &HashMap<Procname, PulseSummary>,
    specialization: Option<&sil::specialization::PulseSpecialization>,
) -> (PulseSummary, Vec<(Procname, PulseSpecialization)>) {
    // Reset per-thread counters so each procedure gets deterministic IDs.
    crate::abstract_value::AbstractValue::reset_counters();

    log::info!("[pulse] analyzing {}", pdesc.proc_name);

    let cfg = config::get();
    let max_disjuncts = cfg.pulse_max_disjuncts;
    let max_widen_iters = cfg.pulse_widen_threshold;

    let mut initial_state = AbductiveDomain::mk_initial(pdesc);

    // Apply specialization to initial state if provided
    if let Some(spec) = specialization {
        crate::specialization::apply(spec, &mut initial_state);
    }

    let initial_exec = ExecutionDomain::ContinueProgram(initial_state);
    let initial_domain = DisjunctiveDomain::singleton(initial_exec, max_disjuncts, max_widen_iters);

    let pulse_tf = PulseTransferFunctions {
        callee_summaries,
        pdesc,
        proc_name: format!("{}", pdesc.proc_name),
        spec_requests: RefCell::new(Vec::new()),
    };

    let inv_map = interp::compute_fixpoint_wto(&pulse_tf, &(), pdesc, initial_domain);
    let exit_has_normal_path = inv_map.get(&pdesc.exit_node).is_some_and(|exit_state| {
        exit_state.post.disjuncts.iter().any(|d| {
            matches!(
                d,
                ExecutionDomain::ContinueProgram(_) | ExecutionDomain::ExitProgram(_)
            )
        })
    });

    // Some manifest aborts do not reach the exit node (for example, paths that
    // stop mid-procedure). Keep the non-exit scan for those, but filter each
    // abort through the same latent-vs-manifest classification used during
    // summary creation so we do not publish latent issues too early.
    let mut diagnostics = Vec::new();
    let mut seen_diags = std::collections::HashSet::new();
    let mut recovered_non_exit_disjuncts = Vec::new();
    for (node_id, state) in &inv_map {
        if *node_id == pdesc.exit_node {
            continue;
        }
        for d in &state.post.disjuncts {
            match d {
                ExecutionDomain::AbortProgram { state, diagnostic } => {
                    if diagnostic_originates_in_proc(pdesc, diagnostic)
                        && matches!(
                            crate::summary::classify_abort_kind(pdesc, state, diagnostic),
                            crate::summary::PrePostKind::AbortProgram
                        )
                    {
                        let key = diagnostic.dedup_key();
                        if seen_diags.insert(key) {
                            diagnostics.push(diagnostic.as_ref().clone());
                        }
                    }
                }
                ExecutionDomain::ContinueProgram(astate) if !exit_has_normal_path => {
                    for recovered in crate::summary::recovered_invalid_accesses_from_continue_state(
                        pdesc, astate,
                    ) {
                        let diagnostic = match &recovered {
                            ExecutionDomain::AbortProgram { diagnostic, .. }
                            | ExecutionDomain::LatentInvalidAccess { diagnostic, .. } => {
                                diagnostic.as_ref()
                            }
                            _ => continue,
                        };
                        if diagnostic_originates_in_proc(pdesc, diagnostic)
                            && !recovered_non_exit_disjuncts
                                .iter()
                                .any(|existing| existing == &recovered)
                        {
                            recovered_non_exit_disjuncts.push(recovered);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Collect all disjuncts at exit for the summary: ContinueProgram,
    // ExitProgram, and AbortProgram. OCaml keeps all paths (including
    // error paths) in the pre_post_list.
    let mut exit_disjuncts = Vec::new();
    if let Some(exit_state) = inv_map.get(&pdesc.exit_node) {
        for d in &exit_state.post.disjuncts {
            match d {
                ExecutionDomain::ContinueProgram(_)
                | ExecutionDomain::ExitProgram(_)
                | ExecutionDomain::AbortProgram { .. }
                | ExecutionDomain::LatentAbortProgram { .. }
                | ExecutionDomain::LatentInvalidAccess { .. } => {
                    exit_disjuncts.push(d.clone());
                }
                _ => {}
            }
        }
    }
    for recovered in recovered_non_exit_disjuncts {
        if !exit_disjuncts.iter().any(|existing| existing == &recovered) {
            exit_disjuncts.push(recovered);
        }
    }
    // Cross-ref: OCaml consults ProcAttributes.is_no_return at call sites in
    // Pulse.ml. Do not infer noreturn from "no ContinueProgram at exit":
    // latent/error-only summaries still need normal summary application at
    // callers, whereas source-level noreturn metadata is the intended fast
    // path for empty stubs / declarations.
    let is_noreturn = pdesc.is_no_return;

    let summary = PulseSummary::of_proc(pdesc, &exit_disjuncts, diagnostics, is_noreturn);
    let spec_requests = pulse_tf.spec_requests.into_inner();
    (summary, spec_requests)
}

fn diagnostic_originates_in_proc(pdesc: &Procdesc, diagnostic: &Diagnostic) -> bool {
    let loc = diagnostic.get_location();

    // Keep reporting when we do not have reliable source ranges. The filter is
    // only meant to suppress callee-local manifest aborts that are already
    // published on the callee itself and leak into the caller's non-exit scan.
    if loc.is_dummy() {
        return true;
    }

    let mut proc_file = None;
    let mut proc_start = i32::MAX;
    let mut proc_end = i32::MIN;
    for node in &pdesc.nodes {
        if node.loc.is_dummy() {
            continue;
        }
        if proc_file.is_none() {
            proc_file = Some(node.loc.file.clone());
        }
        if proc_file.as_ref() != Some(&node.loc.file) {
            continue;
        }
        proc_start = proc_start.min(node.loc.line);
        proc_end = proc_end.max(node.loc.line);
    }

    let Some(proc_file) = proc_file else {
        return true;
    };

    loc.file == proc_file && proc_start <= loc.line && loc.line <= proc_end
}

/// Pulse transfer functions for the disjunctive abstract interpreter.
///
/// Wraps `transfer::exec_instr` to operate on `DisjunctiveDomain<ExecutionDomain>`.
/// Each instruction is executed on each ContinueProgram disjunct independently.
struct PulseTransferFunctions<'a> {
    callee_summaries: &'a HashMap<Procname, PulseSummary>,
    pdesc: &'a Procdesc,
    proc_name: String,
    spec_requests: RefCell<Vec<(Procname, PulseSpecialization)>>,
}

impl TransferFunctions for PulseTransferFunctions<'_> {
    type Domain = DisjunctiveDomain<ExecutionDomain>;
    type AnalysisData = ();

    fn exec_instr(
        &self,
        state: &Self::Domain,
        _data: &(),
        node_id: NodeId,
        instr_idx: usize,
        instr: &Instr,
    ) -> Self::Domain {
        let continue_count = state.disjuncts.iter().filter(|d| d.is_continue()).count();

        let pn = &self.proc_name;
        log::debug!(
            "[{pn}] exec node={node_id} instr={instr_idx} disjuncts={} (continue={continue_count}) {instr}",
            state.disjuncts.len()
        );

        let mut new_disjuncts = Vec::new();

        for (i, disjunct) in state.disjuncts.iter().enumerate() {
            match disjunct {
                ExecutionDomain::ContinueProgram(astate) => {
                    log::trace!("[{pn}]   disjunct #{i}: {astate:?}");
                    let results = exec_instr_with_summaries(
                        self.pdesc,
                        instr,
                        astate.clone(),
                        self.callee_summaries,
                        Some(&self.spec_requests),
                    );
                    let nc = results.iter().filter(|d| d.is_continue()).count();
                    let na = results
                        .iter()
                        .filter(|d| matches!(d, ExecutionDomain::AbortProgram { .. }))
                        .count();
                    log::debug!(
                        "[{pn}]   disjunct #{i}: got {} back (continue={nc}, abort={na})",
                        results.len()
                    );
                    new_disjuncts.extend(results);
                }
                other => {
                    new_disjuncts.push(other.clone());
                }
            }
        }

        let mut result = DisjunctiveDomain {
            disjuncts: new_disjuncts,
            max_disjuncts: state.max_disjuncts,
            max_widen_iters: state.max_widen_iters,
        };
        result.bound();

        let rc = result.disjuncts.iter().filter(|d| d.is_continue()).count();
        log::debug!(
            "[{pn}] result disjuncts={} (continue={rc})",
            result.disjuncts.len()
        );

        result
    }
}

/// Execute an instruction, checking for interprocedural summary application.
///
/// Priority: arg validity check > models > noreturn summaries > pre/post summaries > transfer.
fn exec_instr_with_summaries(
    pdesc: &Procdesc,
    instr: &Instr,
    state: AbductiveDomain,
    callee_summaries: &HashMap<Procname, PulseSummary>,
    spec_requests: Option<&RefCell<Vec<(Procname, PulseSpecialization)>>>,
) -> Vec<ExecutionDomain> {
    if let Some(results) =
        maybe_inline_global_initializer_load(pdesc, instr, state.clone(), callee_summaries)
    {
        return results;
    }

    if let Instr::Call {
        ret: (ret_id, ret_typ),
        fun_exp: Exp::Const(Const::Cfun(callee_pname)),
        args,
        loc,
        ..
    } = instr
    {
        let callsite = CallSite {
            pdesc,
            ret_id,
            ret_typ,
            args,
            loc,
            spec_requests,
        };

        // __call_c_function_ptr(funptr, args...): resolve the function
        // pointer to a procname via Closure attribute, then dispatch.
        // Cross-ref: OCaml PulseModelsC.ml call_c_function_ptr.
        if callee_pname.get_method_name() == "__call_c_function_ptr" {
            return exec_call_c_function_ptr(callsite, state, callee_summaries);
        }

        // Models take priority over summaries (e.g., exit() may have an
        // empty define in textual but should still be modeled as noreturn)
        if crate::models::has_model(callee_pname) {
            log::debug!("  [call] model: {callee_pname}");
            let results = transfer::exec_instr_with_pdesc(Some(pdesc), instr, state.clone());
            return merge_return_history_from_equal_actuals(results, ret_id, args, loc, &state);
        }

        if let Some(callee_summary) = callee_summaries.get(callee_pname) {
            return exec_known_callee_summary(
                KnownCalleeCall {
                    callee_pname,
                    callee_summary,
                    callsite,
                },
                state,
            );
        }
    }

    transfer::exec_instr_with_pdesc(Some(pdesc), instr, state)
}

fn maybe_inline_global_initializer_load(
    pdesc: &Procdesc,
    instr: &Instr,
    state: AbductiveDomain,
    callee_summaries: &HashMap<Procname, PulseSummary>,
) -> Option<Vec<ExecutionDomain>> {
    let Instr::Load {
        e: Exp::Lvar(pvar),
        typ,
        loc,
        ..
    } = instr
    else {
        return None;
    };

    if !should_inline_global_initializer(pvar, typ, &state) {
        return None;
    }

    let init_pname = pvar.initializer_procname()?;
    if pdesc.proc_name == init_pname {
        return None;
    }
    let init_summary = callee_summaries.get(&init_pname)?;
    if init_summary.pre_posts.is_empty() {
        return None;
    }

    let init_ret = Ident::create_normal(IdentName::from_string("__global_init"), -1);
    let mut initialized_states = Vec::new();
    for pre_post in &init_summary.pre_posts {
        for result in
            crate::interproc::apply_summary(pdesc, pre_post, &init_ret, &[], loc, state.clone())
        {
            if let ExecutionDomain::ContinueProgram(astate) = result {
                initialized_states.push(astate);
            }
        }
    }

    if initialized_states.is_empty() {
        return None;
    }

    let mut results = Vec::new();
    for initialized in initialized_states {
        results.extend(transfer::exec_instr_with_pdesc(
            Some(pdesc),
            instr,
            initialized,
        ));
    }
    Some(results)
}

#[derive(Clone, Copy)]
struct CallSite<'a> {
    pdesc: &'a Procdesc,
    ret_id: &'a sil::ident::Ident,
    ret_typ: &'a sil::typ::Typ,
    args: &'a [(Exp, sil::typ::Typ)],
    loc: &'a sil::location::Location,
    spec_requests: Option<&'a RefCell<Vec<(Procname, PulseSpecialization)>>>,
}

#[derive(Clone, Copy)]
struct KnownCalleeCall<'a> {
    callee_pname: &'a Procname,
    callee_summary: &'a PulseSummary,
    callsite: CallSite<'a>,
}

fn merge_return_history_from_equal_actuals(
    results: Vec<ExecutionDomain>,
    ret_id: &Ident,
    args: &[(Exp, sil::typ::Typ)],
    loc: &Location,
    state_before_call: &AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let mut tmp_state = state_before_call.clone();
    let actuals: Vec<_> = args
        .iter()
        .map(|(arg_exp, _)| {
            crate::operations::eval_or_fresh_with_history(arg_exp, loc, &mut tmp_state)
        })
        .collect();
    let ret_var = sil::var::Var::LogicalVar(ret_id.clone());

    results
        .into_iter()
        .map(|mut result| {
            let state = match &mut result {
                ExecutionDomain::ContinueProgram(state)
                | ExecutionDomain::ExitProgram(state)
                | ExecutionDomain::ExceptionRaised(state) => state,
                ExecutionDomain::AbortProgram { state, .. }
                | ExecutionDomain::LatentAbortProgram { state, .. }
                | ExecutionDomain::LatentInvalidAccess { state, .. } => state,
            };

            let Some(ret_value) = state.post.stack.find_with_history(&ret_var).cloned() else {
                return result;
            };

            let merged_history =
                actuals
                    .iter()
                    .fold(ret_value.history.clone(), |history, actual| {
                        if values_are_equal_in_state(state, ret_value.addr, actual.addr) {
                            history.merge(&actual.history)
                        } else {
                            history
                        }
                    });

            if merged_history != ret_value.history {
                state.post.stack.add_with_history(
                    ret_var.clone(),
                    crate::value_history::ValueWithHistory::new(ret_value.addr, merged_history),
                );
            }

            result
        })
        .collect()
}

fn values_are_equal_in_state(
    state: &AbductiveDomain,
    lhs: crate::abstract_value::AbstractValue,
    rhs: crate::abstract_value::AbstractValue,
) -> bool {
    state.path_condition.get_var_repr(lhs) == state.path_condition.get_var_repr(rhs)
        || matches!(
            (state.get_const(lhs), state.get_const(rhs)),
            (Some(lhs_const), Some(rhs_const)) if lhs_const == rhs_const
        )
}

fn should_inline_global_initializer(
    pvar: &sil::pvar::Pvar,
    typ: &sil::typ::Typ,
    state: &AbductiveDomain,
) -> bool {
    if !pvar.is_global() || !is_pointer_to_function(typ) {
        return false;
    }

    let var = sil::var::Var::ProgramVar(Box::new(pvar.clone()));
    let Some(addr) = state.post.stack.find(&var) else {
        return true;
    };
    state
        .post
        .heap
        .find_edge(addr, &crate::access::Access::Dereference)
        .is_none()
}

fn is_pointer_to_function(typ: &sil::typ::Typ) -> bool {
    matches!(
        &*typ.desc,
        sil::typ::TypeDesc::Tptr(inner, _) if matches!(&*inner.desc, sil::typ::TypeDesc::Tfun(_))
    )
}

/// Propagate needs_specialization from callee to caller.
///
/// When a callee needs dynamic type info for some of its formals, and the
/// caller passes those formals through from ITS OWN formals (without adding
/// Closure info), the need must propagate upward. This enables multi-level
/// function pointer dispatch: the ultimate caller with the known Closure
/// triggers specialization through the entire chain.
///
/// Strategy: for each heap path in needs_specialization, find the formal
/// Pvar at the root, map it to the corresponding actual, evaluate the
/// actual to get the caller's abstract value, and if it doesn't have a
/// Closure attribute, propagate the need.
///
/// Cross-ref: OCaml PulseCallOperations.ml propagation of needs_from_caller.
fn propagate_specialization_need(
    callee_summary: &PulseSummary,
    actuals: &[(Exp, sil::typ::Typ)],
    loc: &sil::location::Location,
    caller_state: &mut crate::abductive::AbductiveDomain,
) {
    if let Some(first_pp) = callee_summary.pre_posts.first() {
        for heap_path in callee_summary.needs_specialization.keys() {
            // Extract the root Pvar from the heap path
            let root_pvar = extract_root_pvar(heap_path);
            let Some(root_pvar) = root_pvar else {
                continue;
            };

            // Find which formal this Pvar corresponds to
            for (i, (formal_pvar, _)) in first_pp.formals.iter().enumerate() {
                if formal_pvar == root_pvar {
                    if let Some((actual_exp, _)) = actuals.get(i) {
                        let actual_val =
                            crate::operations::eval_or_fresh(actual_exp, loc, caller_state);
                        // Only propagate if the caller doesn't have a Closure for this
                        if caller_state.get_closure_proc_name(actual_val).is_none() {
                            caller_state.add_need_dynamic_type_specialization(actual_val);
                        }
                    }
                    break;
                }
            }
        }
    }
}

fn infer_caller_specialization(
    callee_summary: &PulseSummary,
    actuals: &[(Exp, sil::typ::Typ)],
    caller_state: &crate::abductive::AbductiveDomain,
) -> Option<PulseSpecialization> {
    let first_pp = callee_summary.pre_posts.first()?;
    crate::specialization::make_specialization_from_caller(
        &callee_summary.needs_specialization,
        caller_state,
        &first_pp.formals,
        &callee_summary.formal_types,
        actuals,
    )
}

fn queue_specialization_request(
    spec_requests: Option<&RefCell<Vec<(Procname, PulseSpecialization)>>>,
    callee_pname: &Procname,
    callee_summary: &PulseSummary,
    spec: PulseSpecialization,
) {
    if callee_summary.get_specialized(&spec).is_some() {
        return;
    }
    let Some(requests) = spec_requests else {
        return;
    };
    let mut requests = requests.borrow_mut();
    if !requests
        .iter()
        .any(|(pname, existing)| pname == callee_pname && existing == &spec)
    {
        requests.push((callee_pname.clone(), spec));
    }
}

fn heap_path_sort_key(path: &sil::specialization::HeapPath) -> String {
    format!("{path}")
}

fn canonicalize_alias_groups(
    mut groups: Vec<Vec<sil::specialization::HeapPath>>,
) -> Vec<Vec<sil::specialization::HeapPath>> {
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

fn merge_alias_groups(
    merged: &mut Vec<Vec<sil::specialization::HeapPath>>,
    new_groups: Vec<Vec<sil::specialization::HeapPath>>,
) {
    merged.extend(new_groups);
    *merged = canonicalize_alias_groups(std::mem::take(merged));
}

fn specialization_with_aliases(
    base_spec: &PulseSpecialization,
    alias_groups: Vec<Vec<sil::specialization::HeapPath>>,
) -> PulseSpecialization {
    let mut spec = base_spec.clone();
    spec.aliases = Some(canonicalize_alias_groups(alias_groups));
    spec
}

/// Extract the root Pvar from a HeapPath.
fn extract_root_pvar(path: &sil::specialization::HeapPath) -> Option<&sil::pvar::Pvar> {
    use sil::specialization::HeapPath;
    match path {
        HeapPath::Pvar(pv) => Some(pv),
        HeapPath::Dereference(inner) | HeapPath::FieldAccess(_, inner) => extract_root_pvar(inner),
    }
}

/// Select the best currently available pre/posts for a callee summary.
///
/// Cross-ref: OCaml `iter_call` starts from the current specialization state
/// and falls back to the unspecialized summary when that specialized summary
/// has not been computed yet.
fn select_pre_posts_and_specialization<'a>(
    callee_summary: &'a PulseSummary,
    caller_spec: Option<&PulseSpecialization>,
) -> (&'a [crate::summary::PrePost], PulseSpecialization) {
    if let Some(spec) = caller_spec {
        if let Some(specialized) = callee_summary.get_specialized(spec) {
            return (specialized, spec.clone());
        }
    }

    (&callee_summary.pre_posts, PulseSpecialization::bottom())
}

fn apply_pre_posts_with_specialization_loop(
    known_callee: KnownCalleeCall<'_>,
    caller_state: &crate::abductive::AbductiveDomain,
    initial_pre_posts: &[crate::summary::PrePost],
    initial_spec: PulseSpecialization,
) -> Vec<ExecutionDomain> {
    // Cross-ref: OCaml `PulseCallOperations.iter_call` first tries the
    // currently available summary, then turns alias contradictions from
    // `PulseInterproc.apply_summary` into alias specialization requests or an
    // immediate retry if the specialized summary is already cached.
    let CallSite {
        pdesc,
        ret_id,
        args,
        loc,
        spec_requests,
        ..
    } = known_callee.callsite;
    let callee_pname = known_callee.callee_pname;
    let callee_summary = known_callee.callee_summary;
    let mut current_pre_posts = initial_pre_posts;
    let mut current_spec = initial_spec;
    let mut tried_specs: Vec<PulseSpecialization> = Vec::new();

    loop {
        if current_pre_posts.is_empty() {
            return vec![];
        }

        log::debug!(
            "  [call] applying {} pre/posts for {callee_pname} with specialization {current_spec}",
            current_pre_posts.len()
        );

        let mut results = Vec::new();
        let mut alias_groups = Vec::new();
        for (j, pre_post) in current_pre_posts.iter().enumerate() {
            let outcome = crate::interproc::apply_summary_with_aliasing(
                pdesc,
                pre_post,
                ret_id,
                args,
                loc,
                caller_state.clone(),
            );
            log::debug!("    pre/post #{j}: {} results", outcome.results.len());
            results.extend(outcome.results);
            if let Some(groups) = outcome.alias_specialization {
                merge_alias_groups(&mut alias_groups, groups);
            }
        }

        if !results.is_empty() || alias_groups.is_empty() {
            return results;
        }

        let next_spec = specialization_with_aliases(&current_spec, alias_groups);
        if next_spec == current_spec || tried_specs.iter().any(|spec| spec == &next_spec) {
            return vec![];
        }
        tried_specs.push(next_spec.clone());

        if let Some(specialized) = callee_summary.get_specialized(&next_spec) {
            log::debug!("  [call] retrying {callee_pname} with alias specialization {next_spec}");
            current_spec = next_spec;
            current_pre_posts = specialized;
            continue;
        }

        log::debug!("  [call] requesting alias specialization {next_spec}");
        queue_specialization_request(spec_requests, callee_pname, callee_summary, next_spec);
        return vec![];
    }
}

fn exec_known_callee_summary(
    known_callee: KnownCalleeCall<'_>,
    mut state: crate::abductive::AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let CallSite {
        pdesc: _,
        ret_id,
        ret_typ,
        args,
        loc,
        spec_requests,
    } = known_callee.callsite;
    let callee_pname = known_callee.callee_pname;
    let callee_summary = known_callee.callee_summary;
    let caller_spec = infer_caller_specialization(callee_summary, args, &state);
    if let Some(spec) = caller_spec.clone() {
        queue_specialization_request(spec_requests, callee_pname, callee_summary, spec);
    }

    if callee_summary.is_noreturn {
        log::debug!("  [call] noreturn: {callee_pname}");
        return vec![ExecutionDomain::ExitProgram(state)];
    }

    // Empty-body callees (extern stubs): treat as unknown with type-aware
    // havoc. Only pointer-typed formals get havoced.
    // Cross-ref: OCaml PulseCallOperations.ml should_havoc checks Tptr.
    if callee_summary.is_empty_body && callee_pname.is_c() {
        log::debug!("  [call] empty-body havoc: {callee_pname}");

        let ret_val = crate::abstract_value::AbstractValue::mk_fresh();
        crate::operations::write_id(ret_id, ret_val, &mut state);
        if ret_typ.is_int() {
            state.path_condition.and_is_int(ret_val);
        }
        let mut is_pure = true;
        let mut actual_vals = Vec::new();
        for (i, (arg_exp, _arg_typ)) in args.iter().enumerate() {
            let arg_val = crate::operations::eval_or_fresh(arg_exp, loc, &mut state);
            actual_vals.push(arg_val);
            let formal_is_ptr = callee_summary
                .formal_types
                .get(i)
                .is_some_and(|t| t.is_pointer());
            if formal_is_ptr {
                is_pure = false;
                state.apply_unknown_effect(arg_val);
                crate::operations::refresh_unknown_lvalue_root(arg_exp, arg_val, &mut state);
            }
        }
        // Pure functions (no pointer args havoced): record FunctionApplication
        // so f(x)==f(x) is detected. Cross-ref: OCaml
        // PulseCallOperations.ml L220-235.
        if is_pure {
            let callee_name = format!("{callee_pname}");
            if state
                .path_condition
                .and_fn_app(ret_val, &callee_name, &actual_vals)
                .is_unsat()
            {
                return vec![];
            }
        }
        return vec![ExecutionDomain::ContinueProgram(state)];
    }

    if !callee_summary.needs_specialization.is_empty() {
        propagate_specialization_need(callee_summary, args, loc, &mut state);
    }

    let (pre_posts, active_spec) =
        select_pre_posts_and_specialization(callee_summary, caller_spec.as_ref());
    if pre_posts.is_empty() {
        log::debug!("  [call] no applicable pre/posts for {callee_pname}");
        return vec![];
    }

    let results =
        apply_pre_posts_with_specialization_loop(known_callee, &state, pre_posts, active_spec);
    merge_return_history_from_equal_actuals(results, ret_id, args, loc, &state)
}

/// Handle `__call_c_function_ptr(funptr, args...)`.
///
/// Resolves the function pointer (first arg) to a procname via the Closure
/// attribute, then dispatches the call as if it were a direct call to that
/// procedure. If the function pointer can't be resolved, returns a fresh
/// value (unknown call).
///
/// Cross-ref: OCaml PulseModelsC.ml call_c_function_ptr.
fn exec_call_c_function_ptr(
    callsite: CallSite<'_>,
    mut state: crate::abductive::AbductiveDomain,
    callee_summaries: &HashMap<Procname, PulseSummary>,
) -> Vec<ExecutionDomain> {
    let CallSite {
        pdesc,
        ret_id,
        ret_typ,
        args,
        loc,
        spec_requests,
    } = callsite;
    // First arg is the function pointer, rest are the actual arguments
    let (funptr_exp, actual_args) = match args.split_first() {
        Some(((fp_exp, _), rest)) => (fp_exp, rest),
        None => {
            // No args — treat as unknown
            let ret_val = crate::abstract_value::AbstractValue::mk_fresh();
            crate::operations::write_id(ret_id, ret_val, &mut state);
            return vec![ExecutionDomain::ContinueProgram(state)];
        }
    };

    // Evaluate the function pointer expression to get its abstract value
    let funptr_val = crate::operations::eval_or_fresh(funptr_exp, loc, &mut state);
    let actual_arg_values: Vec<_> = actual_args
        .iter()
        .map(|(arg_exp, _arg_typ)| crate::operations::eval_or_fresh(arg_exp, loc, &mut state))
        .collect();

    // Cross-ref: OCaml Pulse.ml conservatively initializes model arguments
    // before entering `PulseModelsC.call_c_function_ptr`, so exported summaries
    // keep `Initialized` on the function pointer and actual argument values.
    state.conservatively_initialize_args(
        std::iter::once(funptr_val).chain(actual_arg_values.iter().copied()),
    );

    // Look up the Closure attribute to find the target procname
    log::debug!(
        "  [call_c_function_ptr] funptr_val={funptr_val}, closure={:?}",
        state.get_closure_proc_name(funptr_val)
    );
    if let Some(target_pname) = state.get_closure_proc_name(funptr_val).cloned() {
        // Resolved! Dispatch as a direct call to the target procedure.
        // First check models
        if crate::models::has_model(&target_pname) {
            let call_instr = Instr::Call {
                ret: (ret_id.clone(), ret_typ.clone()),
                fun_exp: Exp::Const(Const::Cfun(target_pname)),
                args: actual_args.to_vec(),
                loc: loc.clone(),
                flags: sil::call_flags::CallFlags::default(),
            };
            return transfer::exec_instr_with_pdesc(Some(pdesc), &call_instr, state);
        }

        // Then check summaries
        log::debug!(
            "  [call_c_function_ptr] looking up summary for {target_pname}, available={}",
            callee_summaries.contains_key(&target_pname)
        );
        if let Some(callee_summary) = callee_summaries.get(&target_pname) {
            return exec_known_callee_summary(
                KnownCalleeCall {
                    callee_pname: &target_pname,
                    callee_summary,
                    callsite: CallSite {
                        pdesc,
                        ret_id,
                        ret_typ,
                        args: actual_args,
                        loc,
                        spec_requests,
                    },
                },
                state,
            );
        }
    }

    // Unresolved function pointer: record the need for dynamic type
    // specialization so the caller can re-analyze us with the known type.
    // Cross-ref: OCaml PulseModelsC.ml call_c_function_ptr None branch.
    match crate::operations::eval_deref_with_history(funptr_exp, loc, &mut state) {
        PulseResult::Ok(_) => {}
        PulseResult::Recoverable(_, errors) => {
            let mut results = Vec::new();
            for diag in errors {
                results.push(ExecutionDomain::AbortProgram {
                    state: Box::new(state.clone()),
                    diagnostic: Box::new(diag),
                });
            }
            return results;
        }
        PulseResult::FatalError(diag, _) => {
            return vec![ExecutionDomain::AbortProgram {
                state: Box::new(state),
                diagnostic: Box::new(diag),
            }];
        }
    }
    state.add_need_dynamic_type_specialization(funptr_val);
    let ret_val = crate::abstract_value::AbstractValue::mk_fresh();
    crate::operations::write_id(ret_id, ret_val, &mut state);
    if ret_typ.is_int() {
        state.path_condition.and_is_int(ret_val);
    }
    // Havoc actual args (not the funptr itself) for unresolved calls
    for ((arg_exp, _arg_typ), arg_val) in actual_args.iter().zip(actual_arg_values.iter().copied())
    {
        state.add_attr(arg_val, crate::attribute::Attribute::UnknownEffect);
        state.apply_unknown_effect(arg_val);
        crate::operations::refresh_unknown_lvalue_root(arg_exp, arg_val, &mut state);
    }
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Convert Pulse diagnostics to an IssueLog for reporting.
pub fn to_issue_log(summary: &PulseSummary, proc_name: &str) -> IssueLog {
    let mut log = IssueLog::new();
    let report_suppressed = config::get().pulse_report_issues_for_tests;
    for diag in &summary.diagnostics {
        let suppressed = diag.is_suppressed();
        if suppressed && !report_suppressed {
            continue;
        }
        log.report(diag.to_issue_with_reporting(proc_name, false, suppressed));
    }
    log.sort();
    log
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstract_value::AbstractValue;
    use crate::access::Access;
    use crate::summary::{PrePost, PrePostKind};
    use crate::value_history::ValueHistory;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::fieldname::Fieldname;
    use sil::ident::{Ident, IdentName};
    use sil::instr::Instr;
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::{NodeKind, StmtNodeKind};
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::specialization::{HeapPath, PulseSpecialization};
    use sil::typ::Typ;
    use sil::var::Var;

    fn formal_value_heap_path(formal_pvar: &Pvar) -> HeapPath {
        HeapPath::Dereference(Box::new(HeapPath::Pvar(formal_pvar.clone())))
    }

    fn make_alias_specialization_summary(
        with_cached_specialization: bool,
    ) -> (Procname, PulseSummary, PulseSpecialization, Fieldname) {
        let node_struct = sil::typ::TypeName::CStruct(QualifiedCppName::from_string("node"));
        let next_field = Fieldname::make(node_struct, "next");
        let callee_pname = Procname::c_from_string("callee");
        let mut callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        callee_pdesc.formals = vec![(
            Mangled::from_string("p"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];

        let formal_pvar = Pvar::mk(Mangled::from_string("p"), callee_pname.clone());

        let mut unspecialized_state = crate::abductive::AbductiveDomain::mk_initial(&callee_pdesc);
        let unspecialized_formal_addr = unspecialized_state
            .post
            .stack
            .find(&sil::var::Var::ProgramVar(Box::new(formal_pvar.clone())))
            .unwrap();
        let formal_val =
            unspecialized_state.read_heap(unspecialized_formal_addr, Access::Dereference);
        let next_val =
            unspecialized_state.read_heap(formal_val, Access::FieldAccess(next_field.clone()));
        unspecialized_state.read_heap(next_val, Access::Dereference);
        let unspecialized_pre_post = PrePost {
            pre: unspecialized_state.pre.clone(),
            post: unspecialized_state,
            formals: vec![(formal_pvar.clone(), unspecialized_formal_addr)],
            result: None,
            kind: PrePostKind::ContinueProgram,
            diagnostic: None,
        };

        let alias_spec = PulseSpecialization {
            aliases: Some(vec![vec![
                formal_value_heap_path(&formal_pvar),
                HeapPath::FieldAccess(
                    next_field.clone(),
                    Box::new(formal_value_heap_path(&formal_pvar)),
                ),
            ]]),
            dynamic_types: HashMap::new(),
        };

        let specialized = if with_cached_specialization {
            let mut specialized_state =
                crate::abductive::AbductiveDomain::mk_initial(&callee_pdesc);
            let specialized_formal_addr = specialized_state
                .post
                .stack
                .find(&sil::var::Var::ProgramVar(Box::new(formal_pvar.clone())))
                .unwrap();
            let specialized_formal_val =
                specialized_state.read_heap(specialized_formal_addr, Access::Dereference);
            let written = AbstractValue::mk_fresh();
            specialized_state.write_heap(specialized_formal_val, Access::Dereference, written);
            vec![(
                alias_spec.clone(),
                vec![PrePost {
                    pre: specialized_state.pre.clone(),
                    post: specialized_state,
                    formals: vec![(formal_pvar.clone(), specialized_formal_addr)],
                    result: None,
                    kind: PrePostKind::ContinueProgram,
                    diagnostic: None,
                }],
            )]
        } else {
            Vec::new()
        };

        (
            callee_pname,
            PulseSummary {
                pre_posts: vec![unspecialized_pre_post],
                specialized,
                diagnostics: vec![],
                is_noreturn: false,
                needs_specialization: HashMap::new(),
                is_empty_body: false,
                formal_types: vec![Typ::mk_ptr(Typ::void())],
            },
            alias_spec,
            next_field,
        )
    }

    /// Build: void f() { int *p = NULL; *p = 42; }
    fn make_null_deref_proc() -> Procdesc {
        let pname = Procname::c_from_string("null_deref");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());

        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let instrs = vec![
            Instr::Load {
                id: n0.clone(),
                e: Exp::Const(Const::Cint(IntLit::zero())),
                typ: Typ::void(),
                loc: Location::dummy(),
            },
            Instr::Store {
                e1: Box::new(Exp::Var(n0)),
                typ: Typ::void(),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    /// Build: void f() { int x = 5; }  (no bugs)
    fn make_safe_proc() -> Procdesc {
        let pname = Procname::c_from_string("safe");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());

        let pvar = Pvar::mk(Mangled::from_string("x"), pname);
        let instrs = vec![Instr::Store {
            e1: Box::new(Exp::Lvar(pvar)),
            typ: Typ::int(sil::typ::IKind::IInt),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(5)))),
            loc: Location::dummy(),
        }];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    #[test]
    fn test_pulse_detects_null_deref() {
        let pdesc = make_null_deref_proc();
        let summary = analyze(&pdesc);
        assert!(
            !summary.diagnostics.is_empty(),
            "should detect null dereference"
        );
        assert_eq!(
            summary.diagnostics[0].get_issue_type_id(),
            diagnostics::issue_type::IssueTypeId::NullptrDereference
        );
    }

    #[test]
    fn test_pulse_no_false_positive_on_safe() {
        let pdesc = make_safe_proc();
        let summary = analyze(&pdesc);
        assert!(
            summary.diagnostics.is_empty(),
            "safe procedure should have no diagnostics, got: {:?}",
            summary.diagnostics
        );
    }

    #[test]
    fn test_pulse_issue_log() {
        let pdesc = make_null_deref_proc();
        let summary = analyze(&pdesc);
        let log = to_issue_log(&summary, "null_deref");
        assert!(!log.is_empty());
        assert!(log
            .to_issues_exp()
            .contains(diagnostics::issue_type::IssueTypeId::NullptrDereference.id()));
    }

    #[test]
    fn test_to_issue_log_filters_suppressed_null_deref_by_default() {
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: invalidation.clone(),
            access_location: Location::dummy(),
            access_history: ValueHistory::epoch(),
            invalidation_history: ValueHistory::invalidated(invalidation, Location::dummy()),
        };
        let summary = PulseSummary::intra_only(vec![diagnostic]);

        let log = to_issue_log(&summary, "suppressed_null_deref");

        assert!(
            log.is_empty(),
            "suppressed constant-dereference diagnostics should stay out of default reporting"
        );
    }

    #[test]
    fn test_exec_call_c_function_ptr_unknown_derefs_funptr_and_marks_unknown_effect() {
        let caller_pname = Procname::c_from_string("invoke");
        let mut pdesc = Procdesc::new(
            caller_pname.clone(),
            Typ::int(sil::typ::IKind::IInt),
            Location::dummy(),
        );
        pdesc.formals = vec![
            (
                Mangled::from_string("f"),
                Typ::mk_ptr(Typ::void()),
                Default::default(),
            ),
            (
                Mangled::from_string("i"),
                Typ::int(sil::typ::IKind::IInt),
                Default::default(),
            ),
        ];

        let mut state = crate::abductive::AbductiveDomain::mk_initial(&pdesc);
        let f_pvar = Pvar::mk(Mangled::from_string("f"), caller_pname.clone());
        let f_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(f_pvar)))
            .unwrap();
        let funptr_val = state.read_heap(f_addr, Access::Dereference);

        let i_pvar = Pvar::mk(Mangled::from_string("i"), caller_pname);
        let i_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(i_pvar)))
            .unwrap();
        let arg_val = state.read_heap(i_addr, Access::Dereference);

        let fp_id = Ident::create_normal(IdentName::from_string("fp"), 0);
        crate::operations::write_id(&fp_id, funptr_val, &mut state);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 1);
        crate::operations::write_id(&arg_id, arg_val, &mut state);
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 2);

        let args = vec![
            (Exp::Var(fp_id.clone()), Typ::mk_ptr(Typ::void())),
            (Exp::Var(arg_id), Typ::int(sil::typ::IKind::IInt)),
        ];
        let requests = RefCell::new(Vec::new());

        let results = exec_call_c_function_ptr(
            CallSite {
                pdesc: &pdesc,
                ret_id: &ret_id,
                ret_typ: &Typ::int(sil::typ::IKind::IInt),
                args: &args,
                loc: &Location::dummy(),
                spec_requests: Some(&requests),
            },
            state,
            &HashMap::new(),
        );

        let [ExecutionDomain::ContinueProgram(state)] = results.as_slice() else {
            panic!("expected unresolved funptr call to keep one continue state, got {results:?}");
        };

        assert!(
            state.need_dynamic_type_specialization.contains(&funptr_val),
            "unresolved call should request specialization from the function pointer value"
        );
        assert!(
            state
                .pre
                .heap
                .find_edge(funptr_val, &Access::Dereference)
                .is_some(),
            "unresolved call should read through the function pointer in the pre-state"
        );
        assert!(
            state
                .post
                .heap
                .find_edge(funptr_val, &Access::Dereference)
                .is_some(),
            "unresolved call should keep the function-pointer dereference in the post-state"
        );
        assert!(
            state
                .pre
                .attrs
                .get(&funptr_val)
                .is_some_and(|attrs| attrs.get_must_be_valid().is_some()),
            "dereferencing the function pointer should abduce MustBeValid on that value"
        );
        assert!(
            state
                .pre
                .attrs
                .get(&funptr_val)
                .is_some_and(|attrs| attrs.get_must_be_initialized().is_some()),
            "dereferencing the function pointer should abduce MustBeInitialized on that value"
        );
        assert!(
            state
                .post
                .attrs
                .get(&funptr_val)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::Initialized)),
            "model dispatch should conservatively initialize the function pointer value"
        );
        assert!(
            state.post.attrs.get(&arg_val).is_some_and(|attrs| {
                attrs.contains(&crate::attribute::Attribute::Initialized)
                    && attrs.contains(&crate::attribute::Attribute::UnknownEffect)
            }),
            "unresolved call should conservatively initialize actual values and keep UnknownEffect"
        );

        let ret_val = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return id should be written");
        assert!(
            state.path_condition.phi().is_marked_int(ret_val),
            "integer return type should keep the is_int fact on the fresh unknown result"
        );
    }

    /// Diamond CFG: start → a → {b, c} → d → exit
    fn make_diamond_proc() -> Procdesc {
        let pname = Procname::c_from_string("diamond");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());

        let pvar = Pvar::mk(Mangled::from_string("x"), pname.clone());
        let node_a = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Store {
                e1: Box::new(Exp::Lvar(pvar.clone())),
                typ: Typ::int(sil::typ::IKind::IInt),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(1)))),
                loc: Location::dummy(),
            }],
            Location::dummy(),
        );
        let node_b = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Store {
                e1: Box::new(Exp::Lvar(pvar.clone())),
                typ: Typ::int(sil::typ::IKind::IInt),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(2)))),
                loc: Location::dummy(),
            }],
            Location::dummy(),
        );
        let node_c = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Store {
                e1: Box::new(Exp::Lvar(pvar)),
                typ: Typ::int(sil::typ::IKind::IInt),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(3)))),
                loc: Location::dummy(),
            }],
            Location::dummy(),
        );
        let node_d = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![],
            Location::dummy(),
        );

        pdesc.set_succs(0, vec![node_a]);
        pdesc.set_succs(node_a, vec![node_b, node_c]);
        pdesc.set_succs(node_b, vec![node_d]);
        pdesc.set_succs(node_c, vec![node_d]);
        pdesc.set_succs(node_d, vec![1]);
        pdesc
    }

    #[test]
    fn test_cfg_diamond_produces_disjuncts() {
        let pdesc = make_diamond_proc();
        let summary = analyze(&pdesc);
        assert!(!summary.pre_posts.is_empty(), "should have a post-state");
        assert!(
            summary.diagnostics.is_empty(),
            "safe diamond should have no diagnostics: {:?}",
            summary.diagnostics
        );
    }

    /// Build: int* returns_null() { int *p = NULL; return p; }
    fn make_returns_null_proc() -> Procdesc {
        let pname = Procname::c_from_string("returns_null");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::mk_ptr(Typ::void()), Location::dummy());

        let p_pvar = Pvar::mk(Mangled::from_string("p"), pname);
        let instrs = vec![
            Instr::Store {
                e1: Box::new(Exp::Lvar(p_pvar.clone())),
                typ: Typ::void(),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
                loc: Location::dummy(),
            },
            Instr::Load {
                id: Ident::create_normal(IdentName::from_string("n"), 0),
                e: Exp::Lvar(p_pvar),
                typ: Typ::void(),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    /// Build: void caller() { int *p = returns_null(); *p = 42; }
    fn make_caller_proc() -> Procdesc {
        let pname = Procname::c_from_string("caller");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());

        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let instrs = vec![
            Instr::Call {
                ret: (n0.clone(), Typ::void()),
                fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string("returns_null"))),
                args: vec![],
                loc: Location::dummy(),
                flags: CallFlags::default(),
            },
            Instr::Store {
                e1: Box::new(Exp::Var(n0)),
                typ: Typ::void(),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    fn make_formal_deref_proc() -> Procdesc {
        let pname = Procname::c_from_string("formal_deref");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(
            Mangled::from_string("x"),
            Typ::mk_ptr(Typ::void()),
            Default::default(),
        )];

        let formal = Pvar::mk(Mangled::from_string("x"), pname);
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let instrs = vec![
            Instr::Load {
                id: n0.clone(),
                e: Exp::Lvar(formal),
                typ: Typ::mk_ptr(Typ::void()),
                loc: Location::dummy(),
            },
            Instr::Store {
                e1: Box::new(Exp::Var(n0)),
                typ: Typ::void(),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(42)))),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    fn make_two_hop_field_write_proc() -> Procdesc {
        let pname = Procname::c_from_string("two_hop_field_write");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let node_struct = sil::typ::TypeName::CStruct(QualifiedCppName::from_string("node"));
        let next_field = Fieldname::make(node_struct.clone(), "next");
        let node_ptr_typ = Typ::mk_ptr(Typ::mk_struct(node_struct));
        pdesc.formals = vec![(
            Mangled::from_string("q"),
            node_ptr_typ.clone(),
            Default::default(),
        )];

        let formal = Pvar::mk(Mangled::from_string("q"), pname);
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let field_write_instrs = vec![
            Instr::Load {
                id: n0.clone(),
                e: Exp::Lvar(formal),
                typ: node_ptr_typ.clone(),
                loc: Location::dummy(),
            },
            Instr::Load {
                id: n1.clone(),
                e: Exp::Lfield(
                    sil::exp::LfieldObjData {
                        exp: Box::new(Exp::Var(n0.clone())),
                        is_implicit: false,
                    },
                    next_field.clone(),
                    Typ::mk_struct(sil::typ::TypeName::CStruct(QualifiedCppName::from_string(
                        "node",
                    ))),
                ),
                typ: node_ptr_typ.clone(),
                loc: Location::dummy(),
            },
            Instr::Store {
                e1: Box::new(Exp::Lfield(
                    sil::exp::LfieldObjData {
                        exp: Box::new(Exp::Var(n1)),
                        is_implicit: false,
                    },
                    next_field,
                    Typ::mk_struct(sil::typ::TypeName::CStruct(QualifiedCppName::from_string(
                        "node",
                    ))),
                )),
                typ: node_ptr_typ,
                e2: Box::new(Exp::Var(n0)),
                loc: Location::dummy(),
            },
        ];
        let abort_instrs = vec![Instr::Store {
            e1: Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
            typ: Typ::int(sil::typ::IKind::IInt),
            e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(1)))),
            loc: Location::dummy(),
        }];
        let field_write_node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            field_write_instrs,
            Location::dummy(),
        );
        let abort_node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            abort_instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![field_write_node]);
        pdesc.set_succs(field_write_node, vec![abort_node]);
        pdesc.set_succs(abort_node, vec![1]);
        pdesc
    }

    fn make_two_hop_field_write_same_block_abort_proc() -> Procdesc {
        let pname = Procname::c_from_string("two_hop_field_write_same_block_abort");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let node_struct = sil::typ::TypeName::CStruct(QualifiedCppName::from_string("node"));
        let next_field = Fieldname::make(node_struct.clone(), "next");
        let node_ptr_typ = Typ::mk_ptr(Typ::mk_struct(node_struct));
        pdesc.formals = vec![(
            Mangled::from_string("q"),
            node_ptr_typ.clone(),
            Default::default(),
        )];

        let formal = Pvar::mk(Mangled::from_string("q"), pname);
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let instrs = vec![
            Instr::Load {
                id: n0.clone(),
                e: Exp::Lvar(formal),
                typ: node_ptr_typ.clone(),
                loc: Location::dummy(),
            },
            Instr::Load {
                id: n1.clone(),
                e: Exp::Lfield(
                    sil::exp::LfieldObjData {
                        exp: Box::new(Exp::Var(n0.clone())),
                        is_implicit: false,
                    },
                    next_field.clone(),
                    Typ::mk_struct(sil::typ::TypeName::CStruct(QualifiedCppName::from_string(
                        "node",
                    ))),
                ),
                typ: node_ptr_typ.clone(),
                loc: Location::dummy(),
            },
            Instr::Store {
                e1: Box::new(Exp::Lfield(
                    sil::exp::LfieldObjData {
                        exp: Box::new(Exp::Var(n1)),
                        is_implicit: false,
                    },
                    next_field,
                    Typ::mk_struct(sil::typ::TypeName::CStruct(QualifiedCppName::from_string(
                        "node",
                    ))),
                )),
                typ: node_ptr_typ,
                e2: Box::new(Exp::Var(n0)),
                loc: Location::dummy(),
            },
            Instr::Store {
                e1: Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
                typ: Typ::int(sil::typ::IKind::IInt),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::of_int(1)))),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    #[test]
    fn test_two_hop_field_write_keeps_local_null_derefs_latent() {
        let pdesc = make_two_hop_field_write_proc();
        let summary = analyze(&pdesc);

        let latent_null_derefs = summary
            .pre_posts
            .iter()
            .filter(|pp| {
                pp.kind == PrePostKind::LatentInvalidAccess
                    && pp.diagnostic.as_ref().is_some_and(|diag| {
                        diag.get_issue_type_id()
                            == diagnostics::issue_type::IssueTypeId::NullptrDereference
                    })
            })
            .count();

        assert_eq!(
            latent_null_derefs, 2,
            "expected the non-exit scan to recover both `q == 0` and `q->next == 0` latent dereferences once the field write reaches its own CFG node, summary={summary:?}"
        );
        assert!(
            summary
                .diagnostics
                .iter()
                .any(|diag| diag.get_issue_type_id()
                    == diagnostics::issue_type::IssueTypeId::NullptrDereference),
            "expected the trailing local abort to stay manifest, summary={summary:?}"
        );
    }

    #[test]
    fn test_same_block_local_abort_keeps_earlier_null_derefs_latent() {
        let pdesc = make_two_hop_field_write_same_block_abort_proc();
        let summary = analyze(&pdesc);

        let latent_null_derefs = summary
            .pre_posts
            .iter()
            .filter(|pp| {
                pp.kind == PrePostKind::LatentInvalidAccess
                    && pp.diagnostic.as_ref().is_some_and(|diag| {
                        diag.get_issue_type_id()
                            == diagnostics::issue_type::IssueTypeId::NullptrDereference
                    })
            })
            .count();

        assert_eq!(
            latent_null_derefs, 2,
            "expected abort-state summarization to preserve both earlier caller-controlled null dereferences even when the block later aborts locally, summary={summary:?}"
        );
        assert!(
            summary
                .diagnostics
                .iter()
                .any(|diag| diag.get_issue_type_id()
                    == diagnostics::issue_type::IssueTypeId::NullptrDereference),
            "expected the trailing local abort to stay manifest, summary={summary:?}"
        );
    }

    #[test]
    fn test_interprocedural_null_deref() {
        let callee_pdesc = make_returns_null_proc();
        let callee_summary = analyze(&callee_pdesc);

        assert!(
            !callee_summary.pre_posts.is_empty(),
            "callee should have pre_posts"
        );

        let caller_pdesc = make_caller_proc();
        let mut summaries = HashMap::new();
        summaries.insert(Procname::c_from_string("returns_null"), callee_summary);

        let caller_summary = analyze_with_summaries(&caller_pdesc, &summaries);

        assert!(
            !caller_summary.diagnostics.is_empty(),
            "should detect null deref from callee returning null. diagnostics: {:?}",
            caller_summary.diagnostics
        );
        assert_eq!(
            caller_summary.diagnostics[0].get_issue_type_id(),
            diagnostics::issue_type::IssueTypeId::NullptrDereference
        );
    }

    #[test]
    fn test_interprocedural_safe_callee() {
        let safe_pdesc = make_safe_proc();
        let safe_summary = analyze(&safe_pdesc);

        let caller_pdesc = make_caller_proc();
        let mut summaries = HashMap::new();
        summaries.insert(Procname::c_from_string("returns_null"), safe_summary);

        let caller_summary = analyze_with_summaries(&caller_pdesc, &summaries);

        assert!(
            caller_summary.diagnostics.is_empty(),
            "safe callee should not cause issues in caller. diagnostics: {:?}",
            caller_summary.diagnostics
        );
    }

    #[test]
    fn test_formal_deref_does_not_publish_manifest_diagnostic() {
        let pdesc = make_formal_deref_proc();
        let summary = analyze(&pdesc);

        assert!(
            summary.diagnostics.is_empty(),
            "caller-controlled formal dereference should stay latent, got {:?}",
            summary.diagnostics
        );
    }

    #[test]
    fn test_exec_known_callee_summary_requests_alias_specialization_from_contradiction() {
        let (callee_pname, callee_summary, alias_spec, next_field) =
            make_alias_specialization_summary(false);
        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = crate::abductive::AbductiveDomain::mk_initial(&caller_pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let x_addr = AbstractValue::mk_fresh();
        caller_state
            .post
            .stack
            .add(sil::var::Var::ProgramVar(Box::new(x_pvar.clone())), x_addr);
        caller_state
            .post
            .heap
            .add_edge(x_addr, Access::FieldAccess(next_field), x_addr);
        let requests = RefCell::new(Vec::new());
        let ret_id = Ident::create_none();
        let ret_typ = Typ::void();
        let args = [(Exp::Lvar(x_pvar), Typ::mk_ptr(Typ::void()))];
        let loc = Location::dummy();

        let results = exec_known_callee_summary(
            KnownCalleeCall {
                callee_pname: &callee_pname,
                callee_summary: &callee_summary,
                callsite: CallSite {
                    pdesc: &caller_pdesc,
                    ret_id: &ret_id,
                    ret_typ: &ret_typ,
                    args: &args,
                    loc: &loc,
                    spec_requests: Some(&requests),
                },
            },
            caller_state,
        );

        assert!(
            results.is_empty(),
            "missing cached alias specialization should defer the call, got {results:?}"
        );
        assert_eq!(
            requests.into_inner(),
            vec![(callee_pname, alias_spec)],
            "alias contradiction should enqueue the OCaml-style alias specialization request"
        );
    }

    #[test]
    fn test_exec_known_callee_summary_uses_cached_alias_specialization() {
        let (callee_pname, callee_summary, _alias_spec, next_field) =
            make_alias_specialization_summary(true);
        let caller_pname = Procname::c_from_string("caller");
        let caller_pdesc = Procdesc::new(caller_pname.clone(), Typ::void(), Location::dummy());
        let mut caller_state = crate::abductive::AbductiveDomain::mk_initial(&caller_pdesc);
        let x_pvar = Pvar::mk(Mangled::from_string("x"), caller_pname);
        let x_addr = AbstractValue::mk_fresh();
        caller_state
            .post
            .stack
            .add(sil::var::Var::ProgramVar(Box::new(x_pvar.clone())), x_addr);
        caller_state
            .post
            .heap
            .add_edge(x_addr, Access::FieldAccess(next_field), x_addr);
        let requests = RefCell::new(Vec::new());
        let ret_id = Ident::create_none();
        let ret_typ = Typ::void();
        let args = [(Exp::Lvar(x_pvar), Typ::mk_ptr(Typ::void()))];
        let loc = Location::dummy();

        let results = exec_known_callee_summary(
            KnownCalleeCall {
                callee_pname: &callee_pname,
                callee_summary: &callee_summary,
                callsite: CallSite {
                    pdesc: &caller_pdesc,
                    ret_id: &ret_id,
                    ret_typ: &ret_typ,
                    args: &args,
                    loc: &loc,
                    spec_requests: Some(&requests),
                },
            },
            caller_state,
        );

        let continue_state = results
            .into_iter()
            .find_map(|result| match result {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("cached alias specialization should be retried immediately");
        assert!(
            continue_state
                .post
                .heap
                .find_edge(x_addr, &Access::Dereference)
                .is_some(),
            "specialized summary effect should apply after alias-specialized retry"
        );
        assert!(
            requests.into_inner().is_empty(),
            "cached specialization should avoid re-enqueueing the same alias request"
        );
    }

    /// Null deref via Var: store 0 to p, load p (= null), deref p.
    /// Uses the Textual-realistic pattern: Store + Load(Lvar) + Load(Var).
    #[test]
    fn test_null_deref_via_const_zero() {
        let pname = Procname::c_from_string("many_paths");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());

        let p_pvar = Pvar::mk(Mangled::from_string("p"), pname);
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let instrs = vec![
            // store &p <- 0  (p = NULL)
            Instr::Store {
                e1: Box::new(Exp::Lvar(p_pvar.clone())),
                typ: Typ::void(),
                e2: Box::new(Exp::Const(Const::Cint(IntLit::zero()))),
                loc: Location::dummy(),
            },
            // n0 = load &p  (n0 = NULL)
            Instr::Load {
                id: n0.clone(),
                e: Exp::Lvar(p_pvar),
                typ: Typ::void(),
                loc: Location::dummy(),
            },
            // n1 = load n0  (deref NULL → NPE)
            Instr::Load {
                id: n1,
                e: Exp::Var(n0),
                typ: Typ::void(),
                loc: Location::dummy(),
            },
        ];
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);

        let summary = analyze(&pdesc);
        assert!(
            !summary.diagnostics.is_empty(),
            "should find null deref diagnostics"
        );
    }
}
