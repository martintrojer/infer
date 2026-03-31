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
use sil::procdesc::{NodeId, Procdesc};
use sil::procname::Procname;
use sil::specialization::PulseSpecialization;

use crate::abductive::AbductiveDomain;
use crate::execution_domain::ExecutionDomain;
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

    // Some manifest aborts do not reach the exit node (for example, paths that
    // stop mid-procedure). Keep the non-exit scan for those, but filter each
    // abort through the same latent-vs-manifest classification used during
    // summary creation so we do not publish latent issues too early.
    let mut diagnostics = Vec::new();
    let mut seen_diags = std::collections::HashSet::new();
    for (node_id, state) in &inv_map {
        if *node_id == pdesc.exit_node {
            continue;
        }
        for d in &state.post.disjuncts {
            if let ExecutionDomain::AbortProgram { state, diagnostic } = d {
                if matches!(
                    crate::summary::classify_abort_kind(pdesc, state, diagnostic),
                    crate::summary::PrePostKind::AbortProgram
                ) {
                    let key = diagnostic.dedup_key();
                    if seen_diags.insert(key) {
                        diagnostics.push(diagnostic.as_ref().clone());
                    }
                }
            }
        }
    }

    // Collect all disjuncts at exit for the summary: ContinueProgram,
    // ExitProgram, and AbortProgram. OCaml keeps all paths (including
    // error paths) in the pre_post_list.
    let mut exit_disjuncts = Vec::new();
    let mut has_any_disjuncts = false;
    let mut has_continue = false;
    if let Some(exit_state) = inv_map.get(&pdesc.exit_node) {
        has_any_disjuncts = !exit_state.post.disjuncts.is_empty();
        for d in &exit_state.post.disjuncts {
            if d.is_continue() {
                has_continue = true;
            }
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

    // Cross-ref: OCaml consults ProcAttributes.is_no_return at call sites in
    // Pulse.ml, so preserve that source-level fact even when the exported
    // Textual body is empty and would otherwise analyze as a normal return.
    let is_noreturn = pdesc.is_no_return || (has_any_disjuncts && !has_continue);

    let summary = PulseSummary::of_proc(pdesc, &exit_disjuncts, diagnostics, is_noreturn);
    let spec_requests = pulse_tf.spec_requests.into_inner();
    (summary, spec_requests)
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
    mut state: AbductiveDomain,
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
        // __call_c_function_ptr(funptr, args...): resolve the function
        // pointer to a procname via Closure attribute, then dispatch.
        // Cross-ref: OCaml PulseModelsC.ml call_c_function_ptr.
        if callee_pname.get_method_name() == "__call_c_function_ptr" {
            return exec_call_c_function_ptr(pdesc, ret_id, args, loc, state, callee_summaries);
        }

        // Models take priority over summaries (e.g., exit() may have an
        // empty define in textual but should still be modeled as noreturn)
        if crate::models::has_model(callee_pname) {
            log::debug!("  [call] model: {callee_pname}");
            return transfer::exec_instr_with_pdesc(Some(pdesc), instr, state);
        }

        if let Some(callee_summary) = callee_summaries.get(callee_pname) {
            if let (Some(requests), Some(first_pp)) =
                (spec_requests, callee_summary.pre_posts.first())
            {
                if let Some(spec) = crate::specialization::make_specialization_from_caller(
                    &callee_summary.needs_specialization,
                    &state,
                    &first_pp.formals,
                    &callee_summary.formal_types,
                    args,
                ) {
                    if callee_summary.get_specialized(&spec).is_none() {
                        let mut requests = requests.borrow_mut();
                        if !requests
                            .iter()
                            .any(|(pname, existing)| pname == callee_pname && existing == &spec)
                        {
                            requests.push((callee_pname.clone(), spec));
                        }
                    }
                }
            }

            if callee_summary.is_noreturn {
                log::debug!("  [call] noreturn: {callee_pname}");
                return vec![ExecutionDomain::ExitProgram(state)];
            }

            // Empty-body callees (extern stubs): treat as unknown with
            // type-aware havoc. Only pointer-typed formals get havoced.
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
                        crate::operations::refresh_unknown_lvalue_root(
                            arg_exp, arg_val, &mut state,
                        );
                    }
                }
                // Pure functions (no pointer args havoced): record
                // FunctionApplication so f(x)==f(x) is detected.
                // Cross-ref: OCaml PulseCallOperations.ml L220-235.
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

            // Propagate needs_specialization from callee to caller.
            if !callee_summary.needs_specialization.is_empty() {
                propagate_specialization_need(callee_summary, args, loc, &mut state);
            }

            let pre_posts = select_pre_posts(callee_summary, args, &state);
            if !pre_posts.is_empty() {
                log::debug!(
                    "  [call] applying {} pre/posts for {callee_pname}",
                    pre_posts.len()
                );
                let mut results = Vec::new();
                for (j, pre_post) in pre_posts.iter().enumerate() {
                    let applied = crate::interproc::apply_summary(
                        pdesc,
                        pre_post,
                        ret_id,
                        args,
                        loc,
                        state.clone(),
                    );
                    log::debug!("    pre/post #{j}: {} results", applied.len());
                    results.extend(applied);
                }
                return results;
            }

            log::debug!("  [call] no applicable pre/posts for {callee_pname}");
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

/// Extract the root Pvar from a HeapPath.
fn extract_root_pvar(path: &sil::specialization::HeapPath) -> Option<&sil::pvar::Pvar> {
    use sil::specialization::HeapPath;
    match path {
        HeapPath::Pvar(pv) => Some(pv),
        HeapPath::Dereference(inner) | HeapPath::FieldAccess(_, inner) => extract_root_pvar(inner),
    }
}

/// Select the best pre/posts for a callee summary, potentially using
/// a specialized version if the caller provides relevant Closure info.
///
/// Cross-ref: OCaml PulseCallOperations.ml iter_call which tries
/// dynamic type specialization when needed.
fn select_pre_posts<'a>(
    callee_summary: &'a PulseSummary,
    actuals: &[(Exp, sil::typ::Typ)],
    caller_state: &crate::abductive::AbductiveDomain,
) -> &'a [crate::summary::PrePost] {
    // Check if the caller can provide either alias or dynamic-type specialization.
    if let Some(first_pp) = callee_summary.pre_posts.first() {
        if let Some(spec) = crate::specialization::make_specialization_from_caller(
            &callee_summary.needs_specialization,
            caller_state,
            &first_pp.formals,
            &callee_summary.formal_types,
            actuals,
        ) {
            if let Some(specialized) = callee_summary.get_specialized(&spec) {
                return specialized;
            }
        }
    }

    &callee_summary.pre_posts
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
    pdesc: &Procdesc,
    ret_id: &sil::ident::Ident,
    args: &[(Exp, sil::typ::Typ)],
    loc: &sil::location::Location,
    mut state: crate::abductive::AbductiveDomain,
    callee_summaries: &HashMap<Procname, PulseSummary>,
) -> Vec<ExecutionDomain> {
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
                ret: (ret_id.clone(), sil::typ::Typ::void()),
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
            if callee_summary.is_noreturn {
                return vec![ExecutionDomain::ExitProgram(state)];
            }
            if !callee_summary.pre_posts.is_empty() {
                let mut results = Vec::new();
                for pre_post in &callee_summary.pre_posts {
                    results.extend(crate::interproc::apply_summary(
                        pdesc,
                        pre_post,
                        ret_id,
                        actual_args,
                        loc,
                        state.clone(),
                    ));
                }
                return results;
            }
        }
    }

    // Unresolved function pointer: record the need for dynamic type
    // specialization so the caller can re-analyze us with the known type.
    // Cross-ref: OCaml PulseModelsC.ml call_c_function_ptr None branch.
    state.add_need_dynamic_type_specialization(funptr_val);
    let ret_val = crate::abstract_value::AbstractValue::mk_fresh();
    crate::operations::write_id(ret_id, ret_val, &mut state);
    // Havoc actual args (not the funptr itself) for unresolved calls
    for (arg_exp, _arg_typ) in actual_args {
        let arg_val = crate::operations::eval_or_fresh(arg_exp, loc, &mut state);
        state.apply_unknown_effect(arg_val);
        crate::operations::refresh_unknown_lvalue_root(arg_exp, arg_val, &mut state);
    }
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Convert Pulse diagnostics to an IssueLog for reporting.
pub fn to_issue_log(summary: &PulseSummary, proc_name: &str) -> IssueLog {
    let mut log = IssueLog::new();
    for diag in &summary.diagnostics {
        log.report(diag.to_issue(proc_name));
    }
    log.sort();
    log
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::ident::{Ident, IdentName};
    use sil::instr::Instr;
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::{NodeKind, StmtNodeKind};
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::typ::Typ;

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
