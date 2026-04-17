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
use std::sync::Arc;
use std::time::{Duration, Instant};

use absint::disjunctive::DisjunctiveDomain;
use absint::domain::{AbstractDomain, Comparable};
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

use crate::abductive::{AbductiveDomain, AstateSizeStats};
use crate::diagnostic::Diagnostic;
use crate::execution_domain::ExecutionDomain;
use crate::pulse_result::PulseResult;
use crate::summary::PulseSummary;
use crate::transfer;

/// Cross-ref: OCaml `AbstractInterpreter.ml` already emits detailed
/// per-instruction / fixpoint debug in HTML and debug logs. Rust keeps the
/// existing `debug_level_analysis` traces and adds a coarse heartbeat for
/// long-running procedures when logger-based progress is enabled.
const PROC_PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(10);
const PROC_SLOW_LOG_THRESHOLD: Duration = Duration::from_secs(5);

fn pulse_progress_enabled() -> bool {
    log::log_enabled!(target: "ondemand", log::Level::Info)
}

fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        format!("{:.1}s", duration.as_secs_f64())
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!(
            "{}h{:02}m{:02}s",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        )
    }
}

/// Read-only view over already-computed callee summaries.
///
/// The CLI specialization driver keeps shared `Arc<PulseSummary>` handles
/// locally to avoid cloning large summaries per caller, while many focused
/// tests still use plain owned `HashMap<Procname, PulseSummary>` fixtures.
/// This trait lets the analysis code read either representation.
pub trait SummaryLookup {
    fn get(&self, pname: &Procname) -> Option<&PulseSummary>;

    fn contains_key(&self, pname: &Procname) -> bool {
        self.get(pname).is_some()
    }
}

impl SummaryLookup for HashMap<Procname, PulseSummary> {
    fn get(&self, pname: &Procname) -> Option<&PulseSummary> {
        HashMap::get(self, pname)
    }
}

impl SummaryLookup for HashMap<Procname, Arc<PulseSummary>> {
    fn get(&self, pname: &Procname) -> Option<&PulseSummary> {
        HashMap::get(self, pname).map(Arc::as_ref)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DisjunctiveStateStats {
    disjuncts: usize,
    continue_count: usize,
    exit_count: usize,
    abort_count: usize,
    latent_abort_count: usize,
    latent_invalid_count: usize,
    exception_count: usize,
    sum: AstateSizeStats,
    max: AstateSizeStats,
}

impl DisjunctiveStateStats {
    fn from_domain(domain: &DisjunctiveDomain<ExecutionDomain>) -> Self {
        let mut stats = Self {
            disjuncts: domain.disjuncts.len(),
            ..Self::default()
        };

        for disjunct in &domain.disjuncts {
            match disjunct {
                ExecutionDomain::ContinueProgram(_) => stats.continue_count += 1,
                ExecutionDomain::ExitProgram(_) => stats.exit_count += 1,
                ExecutionDomain::AbortProgram { .. } => stats.abort_count += 1,
                ExecutionDomain::LatentAbortProgram { .. } => stats.latent_abort_count += 1,
                ExecutionDomain::LatentInvalidAccess { .. } => stats.latent_invalid_count += 1,
                ExecutionDomain::ExceptionRaised(_) => stats.exception_count += 1,
            }

            let astate_stats = disjunct.get_astate().size_stats();
            stats.sum.add_assign(astate_stats);
            stats.max.max_assign(astate_stats);
        }

        stats
    }

    fn add_assign(&mut self, other: Self) {
        self.disjuncts += other.disjuncts;
        self.continue_count += other.continue_count;
        self.exit_count += other.exit_count;
        self.abort_count += other.abort_count;
        self.latent_abort_count += other.latent_abort_count;
        self.latent_invalid_count += other.latent_invalid_count;
        self.exception_count += other.exception_count;
        self.sum.add_assign(other.sum);
        self.max.max_assign(other.max);
    }
}

impl std::fmt::Display for DisjunctiveStateStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "disj={} kinds[c={} x={} a={} la={} li={} exn={}] sum{{{}}} max{{{}}}",
            self.disjuncts,
            self.continue_count,
            self.exit_count,
            self.abort_count,
            self.latent_abort_count,
            self.latent_invalid_count,
            self.exception_count,
            self.sum,
            self.max,
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FixpointTopNode {
    node: NodeId,
    disjuncts: usize,
    visit_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FixpointStats {
    nodes: usize,
    revisited_nodes: usize,
    max_visit_count: usize,
    max_node_disjuncts: usize,
    states: DisjunctiveStateStats,
    top_nodes: Vec<FixpointTopNode>,
}

impl FixpointStats {
    fn from_inv_map(inv_map: &interp::InvariantMap<DisjunctiveDomain<ExecutionDomain>>) -> Self {
        let mut stats = Self {
            nodes: inv_map.len(),
            ..Self::default()
        };

        for (node_id, state) in inv_map {
            if state.visit_count > 1 {
                stats.revisited_nodes += 1;
            }
            stats.max_visit_count = stats.max_visit_count.max(state.visit_count);
            let disjuncts = state.post.disjuncts.len();
            stats.max_node_disjuncts = stats.max_node_disjuncts.max(disjuncts);
            stats
                .states
                .add_assign(DisjunctiveStateStats::from_domain(&state.post));
            if disjuncts > 1 {
                stats.top_nodes.push(FixpointTopNode {
                    node: *node_id,
                    disjuncts,
                    visit_count: state.visit_count,
                });
            }
        }
        stats.top_nodes.sort_by(|lhs, rhs| {
            rhs.disjuncts
                .cmp(&lhs.disjuncts)
                .then_with(|| rhs.visit_count.cmp(&lhs.visit_count))
                .then_with(|| lhs.node.cmp(&rhs.node))
        });
        stats.top_nodes.truncate(8);

        stats
    }

    fn top_nodes_summary(&self) -> String {
        if self.top_nodes.is_empty() {
            return "none".to_string();
        }
        self.top_nodes
            .iter()
            .map(|node| format!("{}:{}d:{}v", node.node, node.disjuncts, node.visit_count))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug)]
struct ProcProgress {
    started: Instant,
    last_log: Instant,
    last_fixpoint_log: Instant,
    exec_steps: usize,
    max_disjuncts: usize,
    node_visits: HashMap<NodeId, usize>,
    hottest_node: Option<(NodeId, usize)>,
    logged_heartbeat: bool,
}

impl ProcProgress {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_log: now,
            last_fixpoint_log: now,
            exec_steps: 0,
            max_disjuncts: 0,
            node_visits: HashMap::new(),
            hottest_node: None,
            logged_heartbeat: false,
        }
    }

    fn note_step(
        &mut self,
        proc_name: &str,
        node_id: NodeId,
        instr_idx: usize,
        state: &DisjunctiveDomain<ExecutionDomain>,
        spec_requests: usize,
    ) {
        let total_disjuncts = state.disjuncts.len();
        self.exec_steps += 1;
        self.max_disjuncts = self.max_disjuncts.max(total_disjuncts);
        let node_visits = {
            let visits = self.node_visits.entry(node_id).or_insert(0);
            *visits += 1;
            *visits
        };
        if self
            .hottest_node
            .is_none_or(|(_hot_node, hot_visits)| node_visits > hot_visits)
        {
            self.hottest_node = Some((node_id, node_visits));
        }

        if !pulse_progress_enabled() || self.last_log.elapsed() < PROC_PROGRESS_LOG_INTERVAL {
            return;
        }

        self.last_log = Instant::now();
        self.logged_heartbeat = true;
        let state_stats = DisjunctiveStateStats::from_domain(state);
        let hottest = self
            .hottest_node
            .map(|(hot_node, hot_visits)| format!("{hot_node}:{hot_visits}"))
            .unwrap_or_else(|| "none".to_string());
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={proc_name} elapsed={} steps={} node={node_id} instr={instr_idx} node_visits={} hottest_node={} disjuncts={} continue={} spec_requests={} max_disjuncts={} state={}",
            format_duration(self.started.elapsed()),
            self.exec_steps,
            node_visits,
            hottest,
            total_disjuncts,
            state_stats.continue_count,
            spec_requests,
            self.max_disjuncts,
            state_stats,
        );
    }

    fn note_fixpoint_snapshot(
        &mut self,
        proc_name: &str,
        updated_node: NodeId,
        inv_map: &interp::InvariantMap<DisjunctiveDomain<ExecutionDomain>>,
    ) {
        if !pulse_progress_enabled()
            || self.last_fixpoint_log.elapsed() < PROC_PROGRESS_LOG_INTERVAL
        {
            return;
        }

        self.last_fixpoint_log = Instant::now();
        self.logged_heartbeat = true;
        let fixpoint_stats = FixpointStats::from_inv_map(inv_map);
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={proc_name} live-fixpoint: elapsed={} updated_node={} nodes={} revisited_nodes={} max_visit_count={} max_node_disjuncts={} states={}",
            format_duration(self.started.elapsed()),
            updated_node,
            fixpoint_stats.nodes,
            fixpoint_stats.revisited_nodes,
            fixpoint_stats.max_visit_count,
            fixpoint_stats.max_node_disjuncts,
            fixpoint_stats.states,
        );
    }

    fn log_done(
        &self,
        proc_name: &str,
        exit_disjuncts: usize,
        spec_requests: usize,
        fixpoint_stats: Option<&FixpointStats>,
    ) {
        if !pulse_progress_enabled() {
            return;
        }
        let elapsed = self.started.elapsed();
        if !self.logged_heartbeat && elapsed < PROC_SLOW_LOG_THRESHOLD {
            return;
        }

        let hottest = self
            .hottest_node
            .map(|(hot_node, hot_visits)| format!("{hot_node}:{hot_visits}"))
            .unwrap_or_else(|| "none".to_string());
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={proc_name} done: elapsed={} steps={} exit_disjuncts={} spec_requests={} max_disjuncts={} hottest_node={}",
            format_duration(elapsed),
            self.exec_steps,
            exit_disjuncts,
            spec_requests,
            self.max_disjuncts,
            hottest,
        );
        if let Some(fixpoint_stats) = fixpoint_stats {
            log::info!(
                target: "ondemand",
                "[pulse-progress] proc={proc_name} fixpoint-shape: nodes={} revisited_nodes={} max_visit_count={} max_node_disjuncts={} states={}",
                fixpoint_stats.nodes,
                fixpoint_stats.revisited_nodes,
                fixpoint_stats.max_visit_count,
                fixpoint_stats.max_node_disjuncts,
                fixpoint_stats.states,
            );
            log::info!(
                target: "ondemand",
                "[pulse-progress] proc={proc_name} fixpoint-top-nodes: {}",
                fixpoint_stats.top_nodes_summary(),
            );
        }
    }
}

/// Run Pulse analysis on a procedure (intraprocedural, default config).
pub fn analyze(pdesc: &Procdesc) -> PulseSummary {
    analyze_with_summaries(pdesc, &HashMap::<Procname, PulseSummary>::new())
}

/// Run Pulse analysis on a procedure with access to callee summaries.
///
/// Uses the WTO fixpoint engine with a disjunctive domain, matching
/// OCaml's `MakeDisjunctive(PulseTransferFunctions)`.
pub fn analyze_with_summaries(
    pdesc: &Procdesc,
    callee_summaries: &dyn SummaryLookup,
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
    callee_summaries: &dyn SummaryLookup,
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
    callee_summaries: &dyn SummaryLookup,
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
        progress: RefCell::new(ProcProgress::new()),
    };

    let inv_map = interp::compute_fixpoint_wto(&pulse_tf, &(), pdesc, initial_domain);
    let fixpoint_stats = pulse_progress_enabled().then(|| FixpointStats::from_inv_map(&inv_map));
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
    let non_exit_scan_start = Instant::now();
    if pulse_progress_enabled() {
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={} non-exit scan start: nodes={} exit_has_normal_path={}",
            pulse_tf.proc_name,
            inv_map.len(),
            exit_has_normal_path,
        );
    }
    for (node_id, state) in &inv_map {
        let node_scan_start = Instant::now();
        if *node_id == pdesc.exit_node {
            continue;
        }
        let continue_count = state
            .post
            .disjuncts
            .iter()
            .filter(|d| d.is_continue())
            .count();
        let abort_count = state
            .post
            .disjuncts
            .iter()
            .filter(|d| matches!(d, ExecutionDomain::AbortProgram { .. }))
            .count();
        if pulse_progress_enabled() {
            log::info!(
                target: "ondemand",
                "[pulse-progress] proc={} non-exit scan node start: node={} disjuncts={} continue={} abort={}",
                pulse_tf.proc_name,
                node_id,
                state.post.disjuncts.len(),
                continue_count,
                abort_count,
            );
        }
        for d in &state.post.disjuncts {
            match d {
                ExecutionDomain::AbortProgram { state, diagnostic } => {
                    let classify_start = Instant::now();
                    let classified_kind =
                        crate::summary::classify_abort_kind(pdesc, state, diagnostic);
                    let is_manifest_abort = diagnostic_originates_in_proc(pdesc, diagnostic)
                        && matches!(classified_kind, crate::summary::PrePostKind::AbortProgram);
                    if pulse_progress_enabled()
                        && classify_start.elapsed() >= PROC_SLOW_LOG_THRESHOLD
                    {
                        log::info!(
                            target: "ondemand",
                            "[pulse-progress] proc={} non-exit slow classify: node={} elapsed={}",
                            pulse_tf.proc_name,
                            node_id,
                            format_duration(classify_start.elapsed()),
                        );
                    }
                    if is_manifest_abort {
                        let key = diagnostic.dedup_key();
                        if seen_diags.insert(key) {
                            diagnostics.push(diagnostic.as_ref().clone());
                        }
                    }
                    let recovered = match classified_kind {
                        crate::summary::PrePostKind::LatentAbortProgram => {
                            Some(ExecutionDomain::LatentAbortProgram {
                                state: state.clone(),
                                diagnostic: diagnostic.clone(),
                            })
                        }
                        crate::summary::PrePostKind::LatentInvalidAccess => {
                            Some(ExecutionDomain::LatentInvalidAccess {
                                state: state.clone(),
                                diagnostic: diagnostic.clone(),
                            })
                        }
                        _ => None,
                    };
                    if let Some(recovered) = recovered {
                        let recovered_key = stopped_summary_key(&recovered);
                        if !recovered_non_exit_disjuncts.iter().any(|existing| {
                            stopped_summary_key(existing).as_ref() == recovered_key.as_ref()
                        }) {
                            recovered_non_exit_disjuncts.push(recovered);
                        }
                    }
                }
                ExecutionDomain::LatentAbortProgram { .. }
                | ExecutionDomain::LatentInvalidAccess { .. } => {
                    let key = stopped_summary_key(d);
                    if !recovered_non_exit_disjuncts
                        .iter()
                        .any(|existing| stopped_summary_key(existing).as_ref() == key.as_ref())
                    {
                        recovered_non_exit_disjuncts.push(d.clone());
                    }
                }
                ExecutionDomain::ContinueProgram(astate) if !exit_has_normal_path => {
                    let recover_start = Instant::now();
                    let recovered_candidates =
                        crate::summary::recovered_invalid_accesses_from_continue_state(
                            pdesc, astate,
                        );
                    if pulse_progress_enabled()
                        && recover_start.elapsed() >= PROC_SLOW_LOG_THRESHOLD
                    {
                        log::info!(
                            target: "ondemand",
                            "[pulse-progress] proc={} non-exit slow recover: node={} elapsed={} recovered={}",
                            pulse_tf.proc_name,
                            node_id,
                            format_duration(recover_start.elapsed()),
                            recovered_candidates.len(),
                        );
                    }
                    for recovered in recovered_candidates {
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
        if pulse_progress_enabled() && node_scan_start.elapsed() >= PROC_SLOW_LOG_THRESHOLD {
            log::info!(
                target: "ondemand",
                "[pulse-progress] proc={} non-exit scan slow-node: node={} disjuncts={} elapsed={}",
                pulse_tf.proc_name,
                node_id,
                state.post.disjuncts.len(),
                format_duration(node_scan_start.elapsed()),
            );
        }
    }
    if pulse_progress_enabled() {
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={} non-exit scan done: elapsed={} diagnostics={} recovered_non_exit={}",
            pulse_tf.proc_name,
            format_duration(non_exit_scan_start.elapsed()),
            diagnostics.len(),
            recovered_non_exit_disjuncts.len(),
        );
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
    let recovered_non_exit_count = recovered_non_exit_disjuncts.len();
    for recovered in recovered_non_exit_disjuncts {
        let recovered_key = stopped_summary_key(&recovered);
        let already_present = exit_disjuncts.iter().any(|existing| {
            stopped_summary_key(existing).as_ref() == recovered_key.as_ref()
                || existing == &recovered
        });
        if !already_present {
            exit_disjuncts.push(recovered);
        }
    }
    // Cross-ref: OCaml consults ProcAttributes.is_no_return at call sites in
    // Pulse.ml. Do not infer noreturn from "no ContinueProgram at exit":
    // latent/error-only summaries still need normal summary application at
    // callers, whereas source-level noreturn metadata is the intended fast
    // path for empty stubs / declarations.
    let is_noreturn = pdesc.is_no_return;

    let has_dropped_disjuncts = inv_map
        .values()
        .any(|domain| domain.post.had_dropped_disjuncts);
    let summary_start = Instant::now();
    if pulse_progress_enabled() {
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={} summary-build start: exit_disjuncts={} recovered_non_exit={} has_dropped_disjuncts={}",
            pulse_tf.proc_name,
            exit_disjuncts.len(),
            recovered_non_exit_count,
            has_dropped_disjuncts,
        );
    }
    let summary = PulseSummary::of_proc_with_metadata(
        pdesc,
        &exit_disjuncts,
        diagnostics,
        is_noreturn,
        has_dropped_disjuncts,
    );
    if pulse_progress_enabled() {
        log::info!(
            target: "ondemand",
            "[pulse-progress] proc={} summary-build done: elapsed={} diagnostics={} pre_posts={}",
            pulse_tf.proc_name,
            format_duration(summary_start.elapsed()),
            summary.diagnostics.len(),
            summary.pre_posts.len(),
        );
    }
    let spec_request_count = pulse_tf.spec_requests.borrow().len();
    pulse_tf.progress.borrow().log_done(
        &pulse_tf.proc_name,
        exit_disjuncts.len(),
        spec_request_count,
        fixpoint_stats.as_ref(),
    );
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

fn stopped_summary_key(exec: &ExecutionDomain) -> Option<(u8, String)> {
    match exec {
        ExecutionDomain::AbortProgram { diagnostic, .. } => Some((0, diagnostic.dedup_key())),
        ExecutionDomain::LatentAbortProgram { diagnostic, .. } => Some((1, diagnostic.dedup_key())),
        ExecutionDomain::LatentInvalidAccess { diagnostic, .. } => {
            Some((2, diagnostic.dedup_key()))
        }
        _ => None,
    }
}

/// Pulse transfer functions for the disjunctive abstract interpreter.
///
/// Wraps `transfer::exec_instr` to operate on `DisjunctiveDomain<ExecutionDomain>`.
/// Each instruction is executed on each ContinueProgram disjunct independently.
struct PulseTransferFunctions<'a> {
    callee_summaries: &'a dyn SummaryLookup,
    pdesc: &'a Procdesc,
    proc_name: String,
    spec_requests: RefCell<Vec<(Procname, PulseSpecialization)>>,
    progress: RefCell<ProcProgress>,
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
        let spec_request_count = self.spec_requests.borrow().len();
        self.progress
            .borrow_mut()
            .note_step(pn, node_id, instr_idx, state, spec_request_count);

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
            // Cross-ref: OCaml carries dropped-disjunct metadata in the
            // non-disjunctive state, so once a path has been under-approximated
            // the bit remains set for the rest of the procedure.
            had_dropped_disjuncts: state.had_dropped_disjuncts,
        };
        result.dedup();
        result.bound();

        let rc = result.disjuncts.iter().filter(|d| d.is_continue()).count();
        log::debug!(
            "[{pn}] result disjuncts={} (continue={rc})",
            result.disjuncts.len()
        );

        result
    }

    fn exec_node(
        &self,
        old_state: Option<&interp::State<Self::Domain>>,
        pre: &Self::Domain,
        data: &Self::AnalysisData,
        node_id: NodeId,
        pdesc: &Procdesc,
        reverse_instrs: bool,
    ) -> Self::Domain {
        let node = match pdesc.get_node(node_id) {
            Some(node) => node,
            None => return pre.clone(),
        };

        // Cross-ref: OCaml
        // `AbstractInterpreter.MakeDisjunctiveTransferFunctions.exec_node_instrs`
        // re-executes only the pre disjuncts that are new w.r.t. the retained
        // node pre-state, then joins those results into the retained post.
        // Re-executing the whole `new_pre` on every WTO revisit can keep
        // regenerating fresh-but-equivalent post disjuncts on hot loop heads.
        let mut current_post = old_state
            .map(|state| state.post.clone())
            .unwrap_or_else(|| DisjunctiveDomain::empty(pre.max_disjuncts, pre.max_widen_iters));
        current_post.had_dropped_disjuncts |= pre.had_dropped_disjuncts;

        let mut input = pre.clone();
        if let Some(old_state) = old_state {
            input.disjuncts.retain(|disjunct| {
                !old_state
                    .pre
                    .disjuncts
                    .iter()
                    .any(|old| disjunct.equal_fast(old))
            });
            if input.disjuncts.is_empty() {
                return current_post;
            }
        }

        let mut state = input;
        if reverse_instrs {
            for (idx, instr) in node.instrs.iter().enumerate().rev() {
                state = self.exec_instr(&state, data, node_id, idx, instr);
            }
        } else {
            for (idx, instr) in node.instrs.iter().enumerate() {
                state = self.exec_instr(&state, data, node_id, idx, instr);
            }
        }

        current_post.join(&state)
    }

    fn observe_fixpoint(&self, node_id: NodeId, inv_map: &interp::InvariantMap<Self::Domain>) {
        self.progress
            .borrow_mut()
            .note_fixpoint_snapshot(&self.proc_name, node_id, inv_map);
    }
}

/// Execute an instruction, checking for interprocedural summary application.
///
/// Priority: arg validity check > models > noreturn summaries > pre/post summaries > transfer.
fn exec_instr_with_summaries(
    pdesc: &Procdesc,
    instr: &Instr,
    state: AbductiveDomain,
    callee_summaries: &dyn SummaryLookup,
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
        flags,
        ..
    } = instr
    {
        let callsite = CallSite {
            pdesc,
            ret_id,
            ret_typ,
            args,
            loc,
            call_flags: flags,
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

        if *callee_pname == pdesc.proc_name {
            // Cross-ref: OCaml `PulseCallOperations.on_recursive_call` only
            // annotates the state before `call_aux_unknown` still applies the
            // ordinary unknown-call fallback for the recursive call.
            log::debug!(
                "  [call] direct self recursion without summary: treating {callee_pname} as unknown call"
            );
            let fallback_state = state.clone();
            let results = exec_known_call_as_unknown(callsite, callee_pname, state);
            return merge_return_history_from_equal_actuals(
                results,
                ret_id,
                args,
                loc,
                &fallback_state,
            );
        }
    }

    transfer::exec_instr_with_pdesc(Some(pdesc), instr, state)
}

fn maybe_inline_global_initializer_load(
    pdesc: &Procdesc,
    instr: &Instr,
    state: AbductiveDomain,
    callee_summaries: &dyn SummaryLookup,
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
    call_flags: &'a sil::call_flags::CallFlags,
    spec_requests: Option<&'a RefCell<Vec<(Procname, PulseSpecialization)>>>,
}

#[derive(Clone, Copy)]
struct KnownCalleeCall<'a> {
    callee_pname: &'a Procname,
    callee_summary: &'a PulseSummary,
    callsite: CallSite<'a>,
}

#[derive(Clone)]
struct SelectedSummary<'a> {
    pre_posts: &'a [crate::summary::PrePost],
    latent_abort_diagnostics: Option<&'a [Option<Diagnostic>]>,
    specialization: PulseSpecialization,
    has_dropped_disjuncts: bool,
}

struct KnownCalleeResults {
    results: Vec<ExecutionDomain>,
    used_summary_has_dropped_disjuncts: bool,
    used_summary_was_empty: bool,
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
/// actual to get the caller's abstract value, and if it doesn't already carry
/// known dynamic-type information, propagate the need.
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
                        // Only propagate if the caller does not already know
                        // a dynamic type / direct target for this value.
                        if crate::specialization::resolve_procname_for_value(
                            caller_state,
                            actual_val,
                        )
                        .is_none()
                        {
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
) -> SelectedSummary<'a> {
    if let Some(spec) = caller_spec {
        if let Some(specialized) = callee_summary.get_specialized_data(spec) {
            return SelectedSummary {
                pre_posts: &specialized.pre_posts,
                latent_abort_diagnostics: Some(&specialized.latent_abort_diagnostics),
                specialization: spec.clone(),
                has_dropped_disjuncts: specialized.has_dropped_disjuncts,
            };
        }
    }

    SelectedSummary {
        pre_posts: &callee_summary.pre_posts,
        latent_abort_diagnostics: None,
        specialization: PulseSpecialization::bottom(),
        has_dropped_disjuncts: callee_summary.has_dropped_disjuncts,
    }
}

fn apply_pre_posts_with_specialization_loop(
    known_callee: KnownCalleeCall<'_>,
    caller_state: &crate::abductive::AbductiveDomain,
    initial_summary: SelectedSummary<'_>,
) -> KnownCalleeResults {
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
    let mut current_pre_posts = initial_summary.pre_posts;
    let mut current_latent_abort_diagnostics = initial_summary.latent_abort_diagnostics;
    let mut current_spec = initial_summary.specialization;
    let mut current_has_dropped_disjuncts = initial_summary.has_dropped_disjuncts;
    let mut tried_specs: Vec<PulseSpecialization> = Vec::new();

    loop {
        if current_pre_posts.is_empty() {
            return KnownCalleeResults {
                results: vec![],
                used_summary_has_dropped_disjuncts: current_has_dropped_disjuncts,
                used_summary_was_empty: true,
            };
        }

        log::debug!(
            "  [call] applying {} pre/posts for {callee_pname} with specialization {current_spec}",
            current_pre_posts.len()
        );

        let mut results = Vec::new();
        let mut alias_groups = Vec::new();
        for (j, pre_post) in current_pre_posts.iter().enumerate() {
            let effective_pre_post = current_latent_abort_diagnostics
                .and_then(|diagnostics| diagnostics.get(j))
                .and_then(|diag| diag.as_ref())
                .filter(|_| {
                    pre_post.kind == crate::summary::PrePostKind::LatentAbortProgram
                        && pre_post.diagnostic.is_none()
                })
                .map(|diag| {
                    let mut recovered = pre_post.clone();
                    recovered.diagnostic = Some(diag.clone());
                    recovered
                });
            let pre_post = effective_pre_post.as_ref().unwrap_or(pre_post);
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
            // Cross-ref: OCaml keeps dropped-disjunct bookkeeping in the
            // hidden non-disj summary sideband. Callers can still
            // `pulse_force_continue` when the selected alias-specialized
            // summary contains only stopped states rooted in latent invalid
            // accesses
            // (`specialization.c:call_may_double_free_if_alias_bad`).
            //
            // Rust does not yet mirror that non-disj sideband, so preserve
            // the same observable caller behavior here: a selected
            // alias-specialized latent-invalid-access summary with no
            // ContinueProgram is treated as incomplete for the narrow purpose
            // of force-continue.
            let alias_specialization_needs_force_continue = current_spec.aliases.is_some()
                && !has_continue_program(&results)
                && current_pre_posts
                    .iter()
                    .any(|pp| matches!(pp.kind, crate::summary::PrePostKind::LatentInvalidAccess));
            return KnownCalleeResults {
                results,
                used_summary_has_dropped_disjuncts: current_has_dropped_disjuncts
                    || alias_specialization_needs_force_continue,
                used_summary_was_empty: false,
            };
        }

        let next_spec = specialization_with_aliases(&current_spec, alias_groups);
        if next_spec == current_spec || tried_specs.iter().any(|spec| spec == &next_spec) {
            return KnownCalleeResults {
                results: vec![],
                used_summary_has_dropped_disjuncts: current_has_dropped_disjuncts,
                used_summary_was_empty: false,
            };
        }
        tried_specs.push(next_spec.clone());

        if let Some(specialized) = callee_summary.get_specialized_data(&next_spec) {
            log::debug!("  [call] retrying {callee_pname} with alias specialization {next_spec}");
            current_spec = next_spec;
            current_pre_posts = &specialized.pre_posts;
            current_latent_abort_diagnostics = Some(&specialized.latent_abort_diagnostics);
            current_has_dropped_disjuncts = specialized.has_dropped_disjuncts;
            continue;
        }

        log::debug!("  [call] requesting alias specialization {next_spec}");
        queue_specialization_request(spec_requests, callee_pname, callee_summary, next_spec);
        return KnownCalleeResults {
            results: vec![],
            used_summary_has_dropped_disjuncts: current_has_dropped_disjuncts,
            used_summary_was_empty: false,
        };
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
        call_flags: _,
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

    let selected_summary =
        select_pre_posts_and_specialization(callee_summary, caller_spec.as_ref());
    if selected_summary.pre_posts.is_empty() {
        log::debug!("  [call] no applicable pre/posts for {callee_pname}");
        let results = maybe_force_continue_after_known_call(
            known_callee.callsite,
            state.clone(),
            callee_pname,
            vec![],
            true,
        );
        return merge_return_history_from_equal_actuals(results, ret_id, args, loc, &state);
    }

    let applied = apply_pre_posts_with_specialization_loop(known_callee, &state, selected_summary);
    let results = maybe_force_continue_after_known_call(
        known_callee.callsite,
        state.clone(),
        callee_pname,
        applied.results,
        applied.used_summary_was_empty || applied.used_summary_has_dropped_disjuncts,
    );
    merge_return_history_from_equal_actuals(results, ret_id, args, loc, &state)
}

fn has_continue_program(results: &[ExecutionDomain]) -> bool {
    results
        .iter()
        .any(|result| matches!(result, ExecutionDomain::ContinueProgram(_)))
}

fn exec_known_call_as_unknown(
    callsite: CallSite<'_>,
    callee_pname: &Procname,
    state: crate::abductive::AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let CallSite {
        pdesc,
        ret_id,
        ret_typ,
        args,
        loc,
        call_flags,
        ..
    } = callsite;
    let call_instr = Instr::Call {
        ret: (ret_id.clone(), ret_typ.clone()),
        fun_exp: Exp::Const(Const::Cfun(callee_pname.clone())),
        args: args.to_vec(),
        loc: loc.clone(),
        flags: call_flags.clone(),
    };
    transfer::exec_instr_with_pdesc(Some(pdesc), &call_instr, state)
}

fn maybe_force_continue_after_known_call(
    callsite: CallSite<'_>,
    state: crate::abductive::AbductiveDomain,
    callee_pname: &Procname,
    mut results: Vec<ExecutionDomain>,
    should_force_continue: bool,
) -> Vec<ExecutionDomain> {
    if !config::get().pulse_force_continue
        || has_continue_program(&results)
        || !should_force_continue
    {
        return results;
    }

    log::debug!("  [call] forcing continue for {callee_pname}: treating known callee as unknown");
    let mut unknown_results = exec_known_call_as_unknown(callsite, callee_pname, state);
    unknown_results.append(&mut results);
    unknown_results
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
    callee_summaries: &dyn SummaryLookup,
) -> Vec<ExecutionDomain> {
    let CallSite {
        pdesc,
        ret_id,
        ret_typ,
        args,
        loc,
        call_flags,
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

    // Resolve the target from dynamic type information first, then fall back
    // to direct Closure attrs for concrete Cfun / closure values.
    log::debug!(
        "  [call_c_function_ptr] funptr_val={funptr_val}, dynamic_type={:?}, closure={:?}",
        state.get_dynamic_type(funptr_val),
        state.get_closure_proc_name(funptr_val),
    );
    if let Some(target_pname) =
        crate::specialization::resolve_procname_for_value(&state, funptr_val)
    {
        // Resolved! Dispatch as a direct call to the target procedure.
        // First check models
        if crate::models::has_model(&target_pname) {
            let call_instr = Instr::Call {
                ret: (ret_id.clone(), ret_typ.clone()),
                fun_exp: Exp::Const(Const::Cfun(target_pname)),
                args: actual_args.to_vec(),
                loc: loc.clone(),
                flags: call_flags.clone(),
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
                        call_flags,
                        spec_requests,
                    },
                },
                state,
            );
        }

        if target_pname == pdesc.proc_name {
            // Cross-ref: once the function pointer has been resolved, OCaml
            // treats this like an ordinary direct call. Recursive known calls
            // still go through the known-call unknown fallback rather than the
            // unresolved-funptr path.
            log::debug!(
                "  [call_c_function_ptr] direct self recursion without summary: treating {target_pname} as unknown call"
            );
            let fallback_state = state.clone();
            let results = exec_known_call_as_unknown(
                CallSite {
                    pdesc,
                    ret_id,
                    ret_typ,
                    args: actual_args,
                    loc,
                    call_flags,
                    spec_requests,
                },
                &target_pname,
                state,
            );
            return merge_return_history_from_equal_actuals(
                results,
                ret_id,
                actual_args,
                loc,
                &fallback_state,
            );
        }

        // Resolved closure target, but no summary is available yet. This is
        // not the same as an unresolved function pointer: the dynamic target
        // is already known, so match the direct-call fallback path instead of
        // adding `UnknownEffect` / specialization requests for the funptr.
        // Cross-ref: OCaml `PulseModelsC.call_c_function_ptr` dispatches to
        // the resolved target, after which ordinary known-call recursion /
        // unknown-call handling applies.
        log::debug!(
            "  [call_c_function_ptr] resolved target {target_pname} has no summary; treating as direct unknown call"
        );
        let fallback_state = state.clone();
        let results = exec_known_call_as_unknown(
            CallSite {
                pdesc,
                ret_id,
                ret_typ,
                args: actual_args,
                loc,
                call_flags,
                spec_requests,
            },
            &target_pname,
            state,
        );
        return merge_return_history_from_equal_actuals(
            results,
            ret_id,
            actual_args,
            loc,
            &fallback_state,
        );
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
    to_issue_log_impl(summary, proc_name, None)
}

/// Convert Pulse diagnostics to an IssueLog for reporting with procedure
/// context available for OCaml-style latent-issue publication filtering.
pub fn to_issue_log_with_pdesc(summary: &PulseSummary, pdesc: &Procdesc) -> IssueLog {
    to_issue_log_impl(summary, &format!("{}", pdesc.proc_name), Some(pdesc))
}

fn to_issue_log_impl(
    summary: &PulseSummary,
    proc_name: &str,
    pdesc: Option<&Procdesc>,
) -> IssueLog {
    let mut log = IssueLog::new();
    let mut seen = std::collections::HashSet::new();
    let report_suppressed = config::get().pulse_report_issues_for_tests;
    for diag in &summary.diagnostics {
        report_diagnostic_issue(
            &mut log,
            &mut seen,
            diag,
            proc_name,
            false,
            report_suppressed,
            None,
        );
    }
    for pre_post in &summary.pre_posts {
        match pre_post.kind {
            crate::summary::PrePostKind::LatentAbortProgram => {
                if let Some(diag) = &pre_post.diagnostic {
                    report_diagnostic_issue(
                        &mut log,
                        &mut seen,
                        diag,
                        proc_name,
                        true,
                        report_suppressed,
                        None,
                    );
                }
            }
            crate::summary::PrePostKind::LatentInvalidAccess => {
                let should_report = pdesc.is_none_or(|pdesc| {
                    crate::summary::exported_latent_invalid_access_is_reportable(pdesc, pre_post)
                });
                let diag = pdesc
                    .map(|_| {
                        crate::summary::latent_invalid_access_diagnostic_from_summary_state(
                            pre_post,
                        )
                    })
                    .unwrap_or_else(|| {
                        pre_post.diagnostic.as_ref().cloned().or_else(|| {
                            crate::summary::latent_invalid_access_diagnostic_from_exported_pre_post(
                                pre_post,
                            )
                        })
                    })
                    .or_else(|| pre_post.diagnostic.clone());
                if should_report {
                    if let Some(diag) = diag {
                        let latent_key = pdesc
                            .and_then(|_| {
                                crate::summary::latent_invalid_access_report_key(pre_post)
                                    .map(|key| format!("true|{key}"))
                            })
                            .unwrap_or_else(|| diagnostic_dedup_key(&diag, true));
                        report_diagnostic_issue(
                            &mut log,
                            &mut seen,
                            &diag,
                            proc_name,
                            true,
                            report_suppressed,
                            Some(&latent_key),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    log.sort();
    log
}

fn report_diagnostic_issue(
    log: &mut IssueLog,
    seen: &mut std::collections::HashSet<String>,
    diagnostic: &Diagnostic,
    proc_name: &str,
    latent: bool,
    report_suppressed: bool,
    dedup_key_override: Option<&str>,
) {
    let suppressed = diagnostic.is_suppressed();
    if suppressed && !report_suppressed {
        return;
    }
    let dedup_key = dedup_key_override
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| diagnostic_dedup_key(diagnostic, latent));
    if !seen.insert(dedup_key) {
        return;
    }
    log.report(diagnostic.to_issue_with_reporting(proc_name, latent, suppressed));
}

fn diagnostic_dedup_key(diagnostic: &Diagnostic, latent: bool) -> String {
    format!("{latent}|{}", diagnostic.dedup_key())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use absint::disjunctive::DisjunctiveDomain;
    use absint::interp;
    use test_harness::textual_utils;

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

    fn retain_named_procs(tm: &mut textual_utils::TestModule, proc_names: &[&str]) {
        let keep: std::collections::HashSet<_> = proc_names.iter().copied().collect();
        tm.cfg
            .proc_descs
            .retain(|pname, _| keep.contains(format!("{pname}").as_str()));
    }

    fn summarize_exec_domain(exec: &ExecutionDomain) -> String {
        match exec {
            ExecutionDomain::ContinueProgram(state) => {
                format!(
                    "ContinueProgram conditions={:?}",
                    state.path_condition.conditions()
                )
            }
            ExecutionDomain::ExitProgram(state) => {
                format!(
                    "ExitProgram conditions={:?}",
                    state.path_condition.conditions()
                )
            }
            ExecutionDomain::ExceptionRaised(state) => {
                format!(
                    "ExceptionRaised conditions={:?}",
                    state.path_condition.conditions()
                )
            }
            ExecutionDomain::AbortProgram { state, diagnostic }
            | ExecutionDomain::LatentAbortProgram { state, diagnostic }
            | ExecutionDomain::LatentInvalidAccess { state, diagnostic } => {
                let diag = match diagnostic.as_ref() {
                    crate::diagnostic::Diagnostic::AccessToInvalidAddress {
                        addr,
                        access_location,
                        ..
                    } => format!("invalid@{access_location} addr={addr}"),
                    other => format!("{other:?}"),
                };
                format!(
                    "{} {diag} conditions={:?}",
                    match exec {
                        ExecutionDomain::AbortProgram { .. } => "AbortProgram",
                        ExecutionDomain::LatentAbortProgram { .. } => "LatentAbortProgram",
                        ExecutionDomain::LatentInvalidAccess { .. } => "LatentInvalidAccess",
                        _ => unreachable!(),
                    },
                    state.path_condition.conditions()
                )
            }
        }
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
                crate::summary::SpecializedSummary {
                    pre_posts: vec![PrePost {
                        pre: specialized_state.pre.clone(),
                        post: specialized_state,
                        formals: vec![(formal_pvar.clone(), specialized_formal_addr)],
                        result: None,
                        kind: PrePostKind::ContinueProgram,
                        diagnostic: None,
                    }],
                    latent_abort_diagnostics: vec![None],
                    has_dropped_disjuncts: false,
                },
            )]
        } else {
            Vec::new()
        };

        (
            callee_pname,
            PulseSummary {
                pre_posts: vec![unspecialized_pre_post],
                has_dropped_disjuncts: false,
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

    fn make_alias_specialization_latent_invalid_summary(
    ) -> (Procname, PulseSummary, PulseSpecialization, Fieldname) {
        let (callee_pname, mut callee_summary, alias_spec, next_field) =
            make_alias_specialization_summary(true);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: invalidation.clone(),
            access_location: Location::dummy(),
            access_history: ValueHistory::epoch(),
            invalidation_history: ValueHistory::invalidated(invalidation, Location::dummy()),
        };
        let specialized = callee_summary
            .specialized
            .iter_mut()
            .find(|(spec, _)| spec == &alias_spec)
            .expect("cached alias specialization should exist");
        let pre_post = specialized
            .1
            .pre_posts
            .first_mut()
            .expect("specialized summary should contain one pre/post");
        pre_post.kind = PrePostKind::LatentInvalidAccess;
        pre_post.diagnostic = Some(diagnostic.clone());
        callee_summary.diagnostics = vec![];
        (callee_pname, callee_summary, alias_spec, next_field)
    }

    fn make_abort_only_summary(has_dropped_disjuncts: bool) -> (Procname, PulseSummary) {
        let callee_pname = Procname::c_from_string("abort_only");
        let callee_pdesc = Procdesc::new(callee_pname.clone(), Typ::void(), Location::dummy());
        let state = crate::abductive::AbductiveDomain::mk_initial(&callee_pdesc);
        let invalidation = crate::invalidation::Invalidation::ConstantDereference(IntLit::zero());
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: AbstractValue::mk_fresh(),
            invalidation: invalidation.clone(),
            access_location: Location::dummy(),
            access_history: ValueHistory::epoch(),
            invalidation_history: ValueHistory::invalidated(invalidation, Location::dummy()),
        };
        (
            callee_pname,
            PulseSummary {
                pre_posts: vec![PrePost {
                    pre: state.pre.clone(),
                    post: state,
                    formals: vec![],
                    result: None,
                    kind: PrePostKind::AbortProgram,
                    diagnostic: Some(diagnostic.clone()),
                }],
                has_dropped_disjuncts,
                specialized: vec![],
                diagnostics: vec![diagnostic],
                is_noreturn: false,
                needs_specialization: HashMap::new(),
                is_empty_body: false,
                formal_types: vec![],
            },
        )
    }

    fn make_empty_known_summary() -> (Procname, PulseSummary) {
        let callee_pname = Procname::c_from_string("empty_known");
        (
            callee_pname,
            PulseSummary {
                pre_posts: vec![],
                has_dropped_disjuncts: false,
                specialized: vec![],
                diagnostics: vec![],
                is_noreturn: false,
                needs_specialization: HashMap::new(),
                is_empty_body: false,
                formal_types: vec![],
            },
        )
    }

    #[test]
    fn test_exec_instr_preserves_dropped_disjuncts_metadata() {
        let pname = Procname::c_from_string("preserve_drop_flag");
        let pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let callee_summaries: HashMap<Procname, PulseSummary> = HashMap::new();
        let tf = PulseTransferFunctions {
            callee_summaries: &callee_summaries,
            pdesc: &pdesc,
            proc_name: format!("{pname}"),
            spec_requests: RefCell::new(vec![]),
            progress: RefCell::new(ProcProgress::new()),
        };
        let state = DisjunctiveDomain {
            disjuncts: vec![ExecutionDomain::ContinueProgram(
                crate::abductive::AbductiveDomain::mk_initial(&pdesc),
            )],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: true,
        };
        let instr = Instr::Load {
            id: Ident::create_normal(IdentName::from_string("n"), 0),
            e: Exp::Const(Const::Cint(IntLit::zero())),
            typ: Typ::void(),
            loc: Location::dummy(),
        };

        let result = tf.exec_instr(&state, &(), pdesc.start_node, 0, &instr);

        assert!(
            result.had_dropped_disjuncts,
            "instruction execution should preserve earlier dropped-disjunct metadata"
        );
    }

    #[test]
    fn test_exec_node_skips_reexecuting_old_pre_disjuncts() {
        let pname = Procname::c_from_string("skip_old_pre_disjuncts");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Load {
                id: Ident::create_normal(IdentName::from_string("n"), 0),
                e: Exp::Const(Const::Cint(IntLit::zero())),
                typ: Typ::void(),
                loc: Location::dummy(),
            }],
            Location::dummy(),
        );
        let callee_summaries: HashMap<Procname, PulseSummary> = HashMap::new();
        let tf = PulseTransferFunctions {
            callee_summaries: &callee_summaries,
            pdesc: &pdesc,
            proc_name: format!("{pname}"),
            spec_requests: RefCell::new(vec![]),
            progress: RefCell::new(ProcProgress::new()),
        };
        let state = DisjunctiveDomain {
            disjuncts: vec![ExecutionDomain::ContinueProgram(
                crate::abductive::AbductiveDomain::mk_initial(&pdesc),
            )],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };
        let old_state = interp::State {
            pre: state.clone(),
            post: state.clone(),
            visit_count: 1,
        };

        let result = tf.exec_node(Some(&old_state), &state, &(), node, &pdesc, false);

        assert_eq!(result, old_state.post);
        assert_eq!(
            tf.progress.borrow().exec_steps,
            0,
            "revisiting a node with no new pre disjuncts should not re-execute its instructions"
        );
    }

    #[test]
    fn test_fixpoint_stats_reports_top_nodes_by_disjuncts_then_visits() {
        let pname = Procname::c_from_string("fixpoint_top_nodes");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        let exec =
            ExecutionDomain::ContinueProgram(crate::abductive::AbductiveDomain::mk_initial(&pdesc));
        let mk_domain = |count| DisjunctiveDomain {
            disjuncts: vec![exec.clone(); count],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };

        let mut inv_map = interp::InvariantMap::new();
        inv_map.insert(
            10,
            interp::State {
                pre: mk_domain(1),
                post: mk_domain(4),
                visit_count: 2,
            },
        );
        inv_map.insert(
            3,
            interp::State {
                pre: mk_domain(1),
                post: mk_domain(4),
                visit_count: 4,
            },
        );
        inv_map.insert(
            7,
            interp::State {
                pre: mk_domain(1),
                post: mk_domain(3),
                visit_count: 3,
            },
        );
        inv_map.insert(
            1,
            interp::State {
                pre: mk_domain(1),
                post: mk_domain(1),
                visit_count: 5,
            },
        );

        let stats = FixpointStats::from_inv_map(&inv_map);

        assert_eq!(stats.max_node_disjuncts, 4);
        assert_eq!(
            stats.top_nodes,
            vec![
                FixpointTopNode {
                    node: 3,
                    disjuncts: 4,
                    visit_count: 4,
                },
                FixpointTopNode {
                    node: 10,
                    disjuncts: 4,
                    visit_count: 2,
                },
                FixpointTopNode {
                    node: 7,
                    disjuncts: 3,
                    visit_count: 3,
                },
            ]
        );
        assert_eq!(stats.top_nodes_summary(), "3:4d:4v, 10:4d:2v, 7:3d:3v");
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
    fn test_to_issue_log_reports_latent_abort_pre_post() {
        let pdesc = make_safe_proc();
        let mut state = crate::abductive::AbductiveDomain::mk_initial(&pdesc);
        let local_root = AbstractValue::mk_fresh();
        let local_val = AbstractValue::mk_fresh();
        state.post.stack.add(
            Var::LogicalVar(Ident::create_normal(IdentName::from_string("tmp"), 0)),
            local_root,
        );
        state.write_heap(local_root, Access::Dereference, local_val);
        let invalidation = crate::invalidation::Invalidation::CFree;
        let diagnostic = Diagnostic::AccessToInvalidAddress {
            addr: local_val,
            invalidation: invalidation.clone(),
            access_location: Location::dummy(),
            access_history: ValueHistory::assignment(Location::dummy()),
            invalidation_history: ValueHistory::invalidated(invalidation, Location::dummy()),
        };
        let mut summary = PulseSummary::intra_only(vec![]);
        summary.pre_posts.push(PrePost {
            pre: state.pre.clone(),
            post: state,
            formals: vec![],
            result: None,
            kind: PrePostKind::LatentAbortProgram,
            diagnostic: Some(diagnostic),
        });

        let log = to_issue_log(&summary, "latent_abort");

        assert!(
            log.to_issues_exp().contains("USE_AFTER_FREE_LATENT"),
            "latent abort pre/posts should be published into the issue log"
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
        let call_flags = sil::call_flags::CallFlags::default();

        let results = exec_call_c_function_ptr(
            CallSite {
                pdesc: &pdesc,
                ret_id: &ret_id,
                ret_typ: &Typ::int(sil::typ::IKind::IInt),
                args: &args,
                loc: &Location::dummy(),
                call_flags: &call_flags,
                spec_requests: Some(&requests),
            },
            state,
            &HashMap::<Procname, PulseSummary>::new(),
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

    #[test]
    fn test_exec_call_c_function_ptr_resolved_target_without_summary_uses_direct_unknown_call() {
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
        let target_pname = Procname::c_from_string("recursive_target");
        state.add_attr(
            funptr_val,
            crate::attribute::Attribute::Closure(target_pname.clone()),
        );

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
            (Exp::Var(fp_id), Typ::mk_ptr(Typ::void())),
            (Exp::Var(arg_id), Typ::int(sil::typ::IKind::IInt)),
        ];
        let requests = RefCell::new(Vec::new());
        let call_flags = sil::call_flags::CallFlags::default();

        let results = exec_call_c_function_ptr(
            CallSite {
                pdesc: &pdesc,
                ret_id: &ret_id,
                ret_typ: &Typ::int(sil::typ::IKind::IInt),
                args: &args,
                loc: &Location::dummy(),
                call_flags: &call_flags,
                spec_requests: Some(&requests),
            },
            state,
            &HashMap::<Procname, PulseSummary>::new(),
        );

        let [ExecutionDomain::ContinueProgram(state)] = results.as_slice() else {
            panic!(
                "expected resolved known target without summary to keep one continue state, got {results:?}"
            );
        };

        assert!(
            !state.need_dynamic_type_specialization.contains(&funptr_val),
            "resolved target should not be treated as unresolved funptr specialization work"
        );
        assert!(
            !state
                .post
                .attrs
                .get(&arg_val)
                .is_some_and(|attrs| attrs.contains(&crate::attribute::Attribute::UnknownEffect)),
            "resolved known target without summary should not mark integer actuals UnknownEffect"
        );

        let ret_val = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return id should be written");
        assert!(
            state.path_condition.phi().is_marked_int(ret_val),
            "integer return type should keep the is_int fact on the fallback result"
        );

        let fn_apps: Vec<_> = state.path_condition.phi().iter_fn_app_eqs().collect();
        assert_eq!(
            fn_apps.len(),
            1,
            "direct unknown-call fallback should retain the pure function application"
        );
        let (key, ret) = fn_apps[0];
        assert_eq!(key.callee, format!("{target_pname}"));
        assert_eq!(
            state.path_condition.get_var_repr(*ret),
            state.path_condition.get_var_repr(ret_val),
            "the pure function application should define the written return value"
        );
    }

    #[test]
    fn test_exec_call_c_function_ptr_dynamic_type_target_without_summary_uses_direct_unknown_call()
    {
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
        state.add_dynamic_type_unsafe(
            funptr_val,
            Typ::mk_struct(sil::typ::TypeName::CFunction(
                match Procname::c_from_string("recursive_target") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("c procname expected"),
                },
            )),
        );

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
            (Exp::Var(fp_id), Typ::mk_ptr(Typ::void())),
            (Exp::Var(arg_id), Typ::int(sil::typ::IKind::IInt)),
        ];
        let requests = RefCell::new(Vec::new());
        let call_flags = sil::call_flags::CallFlags::default();

        let results = exec_call_c_function_ptr(
            CallSite {
                pdesc: &pdesc,
                ret_id: &ret_id,
                ret_typ: &Typ::int(sil::typ::IKind::IInt),
                args: &args,
                loc: &Location::dummy(),
                call_flags: &call_flags,
                spec_requests: Some(&requests),
            },
            state,
            &HashMap::<Procname, PulseSummary>::new(),
        );

        let [ExecutionDomain::ContinueProgram(state)] = results.as_slice() else {
            panic!(
                "expected dynamic-type-resolved target without summary to keep one continue state, got {results:?}"
            );
        };

        assert!(
            !state.need_dynamic_type_specialization.contains(&funptr_val),
            "resolved dynamic type should satisfy specialization without a new request"
        );
        assert!(
            state.get_closure_proc_name(funptr_val).is_none(),
            "dynamic-type-driven resolution should not require a Closure attr"
        );

        let ret_val = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return id should be written");
        assert!(
            state.path_condition.phi().is_marked_int(ret_val),
            "integer return type should keep the is_int fact on the fallback result"
        );

        let fn_apps: Vec<_> = state.path_condition.phi().iter_fn_app_eqs().collect();
        assert_eq!(fn_apps.len(), 1);
        assert_eq!(fn_apps[0].0.callee, "recursive_target");
    }

    #[test]
    fn test_exec_instr_direct_self_recursion_uses_unknown_call_fallback() {
        let pname = Procname::c_from_string("self_rec");
        let mut pdesc = Procdesc::new(
            pname.clone(),
            Typ::int(sil::typ::IKind::IInt),
            Location::dummy(),
        );
        pdesc.formals = vec![(
            Mangled::from_string("p"),
            Typ::mk_ptr(Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt))),
            Default::default(),
        )];

        let mut state = crate::abductive::AbductiveDomain::mk_initial(&pdesc);
        let p_pvar = Pvar::mk(Mangled::from_string("p"), pname.clone());
        let p_root = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(p_pvar)))
            .expect("formal should be bound");
        let p_val = state.read_heap(p_root, Access::Dereference);
        let mid_ptr = state.read_heap(p_val, Access::Dereference);
        let leaf_int = state.read_heap(mid_ptr, Access::Dereference);
        state.path_condition.and_is_int(leaf_int);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 1);
        crate::operations::write_id(&arg_id, p_val, &mut state);

        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let instr = Instr::Call {
            ret: (ret_id.clone(), Typ::int(sil::typ::IKind::IInt)),
            fun_exp: Exp::Const(Const::Cfun(pname)),
            args: vec![(
                Exp::Var(arg_id),
                Typ::mk_ptr(Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt))),
            )],
            loc: Location::dummy(),
            flags: sil::call_flags::CallFlags::default(),
        };

        let results = exec_instr_with_summaries(
            &pdesc,
            &instr,
            state,
            &HashMap::<Procname, PulseSummary>::new(),
            None,
        );

        let [ExecutionDomain::ContinueProgram(state)] = results.as_slice() else {
            panic!("expected direct self recursion to keep one continue state, got {results:?}");
        };

        let ret_val = state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("recursive unknown-call fallback should write a return value");
        assert!(
            state.path_condition.phi().is_marked_int(ret_val),
            "recursive unknown-call fallback should keep integer return typing"
        );

        let actual_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(p_val))
            .expect("pointer actual should keep post attrs");
        assert!(
            actual_attrs.contains(&crate::attribute::Attribute::UnknownEffect),
            "recursive unknown-call fallback should record UnknownEffect on pointer actual roots"
        );
        assert!(
            actual_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "recursive unknown-call fallback should record WrittenTo on pointer actual roots"
        );
        let mid_ptr_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(mid_ptr))
            .expect("reachable pointer should keep post attrs");
        assert!(
            mid_ptr_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "recursive unknown-call fallback should record WrittenTo on reachable pointers"
        );
        let leaf_attrs = state
            .post
            .attrs
            .get(&state.path_condition.get_var_repr(leaf_int))
            .expect("reachable integer leaf should keep post attrs");
        assert!(
            !leaf_attrs
                .iter()
                .any(|attr| matches!(attr, crate::attribute::Attribute::WrittenTo(_, _))),
            "recursive unknown-call fallback should not mark integer leaves WrittenTo"
        );
    }

    #[test]
    fn test_exec_instr_direct_self_recursion_materializes_function_pointer_pointee_shape() {
        let pname = Procname::c_from_string("self_rec_funptr");
        let funptr_typ = Typ::mk_ptr(Typ::void());
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![
            (
                Mangled::from_string("f"),
                funptr_typ.clone(),
                Default::default(),
            ),
            (
                Mangled::from_string("i"),
                Typ::int(sil::typ::IKind::IInt),
                Default::default(),
            ),
        ];

        let mut state = crate::abductive::AbductiveDomain::mk_initial(&pdesc);
        let f_pvar = Pvar::mk(Mangled::from_string("f"), pname.clone());
        let f_root = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(f_pvar)))
            .expect("formal f should be bound");
        let funptr_val = state.read_heap(f_root, Access::Dereference);
        assert!(
            state.pre.heap.get_edges(funptr_val).is_some(),
            "loading the function pointer formal should register its pointee root in pre"
        );
        assert!(
            state
                .pre
                .heap
                .find_edge(funptr_val, &Access::Dereference)
                .is_none(),
            "the recursive function-value pointee should start unmaterialized"
        );

        let i_pvar = Pvar::mk(Mangled::from_string("i"), pname.clone());
        let i_root = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(i_pvar)))
            .expect("formal i should be bound");
        let i_val = state.read_heap(i_root, Access::Dereference);

        let fp_id = Ident::create_normal(IdentName::from_string("fp"), 0);
        crate::operations::write_id(&fp_id, funptr_val, &mut state);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 1);
        crate::operations::write_id(&arg_id, i_val, &mut state);
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 2);

        let instr = Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(pname)),
            args: vec![
                (Exp::Var(fp_id), funptr_typ),
                (Exp::Var(arg_id), Typ::int(sil::typ::IKind::IInt)),
            ],
            loc: Location::dummy(),
            flags: sil::call_flags::CallFlags::default(),
        };

        let results = exec_instr_with_summaries(
            &pdesc,
            &instr,
            state,
            &HashMap::<Procname, PulseSummary>::new(),
            None,
        );
        let [ExecutionDomain::ContinueProgram(state)] = results.as_slice() else {
            panic!("expected direct self recursion to keep one continue state, got {results:?}");
        };

        let pre_target = state
            .pre
            .heap
            .find_edge(funptr_val, &Access::Dereference)
            .expect("recursive unknown-call fallback should materialize a pre funptr pointee");
        let post_target = state
            .post
            .heap
            .find_edge(funptr_val, &Access::Dereference)
            .expect("recursive unknown-call fallback should keep a post funptr pointee");
        assert_ne!(
            state.path_condition.get_var_repr(pre_target),
            state.path_condition.get_var_repr(post_target),
            "recursive unknown-call fallback should preserve the overwritten pre/post funptr shape"
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

    fn make_formal_load_then_exit_proc() -> Procdesc {
        let pname = Procname::c_from_string("formal_load_then_exit");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        let int_typ = Typ::int(sil::typ::IKind::IInt);
        let int_ptr_typ = Typ::mk_ptr(int_typ.clone());
        pdesc.formals = vec![(
            Mangled::from_string("q"),
            int_ptr_typ.clone(),
            Default::default(),
        )];

        let formal = Pvar::mk(Mangled::from_string("q"), pname);
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![
                Instr::Load {
                    id: n0.clone(),
                    e: Exp::Lvar(formal),
                    typ: int_ptr_typ,
                    loc: Location::dummy(),
                },
                Instr::Load {
                    id: n1,
                    e: Exp::Var(n0),
                    typ: int_typ,
                    loc: Location::dummy(),
                },
            ],
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
    #[ignore = "debug latent UAF node states"]
    fn test_debug_latent_uaf_node_states() {
        let tm = textual_utils::parse_and_convert(
            r#"
            .source_language = "C"

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
        let pdesc = tm
            .cfg
            .iter_proc_descs()
            .find(|pdesc| format!("{}", pdesc.proc_name) == "latent_use_after_free")
            .expect("latent_use_after_free proc should exist")
            .clone();
        let mut callee_summaries = HashMap::new();
        let conditional_free = tm
            .cfg
            .iter_proc_descs()
            .find(|pdesc| format!("{}", pdesc.proc_name) == "conditional_free2")
            .expect("conditional_free2 proc should exist")
            .clone();
        callee_summaries.insert(
            conditional_free.proc_name.clone(),
            analyze(&conditional_free),
        );

        let cfg = config::get();
        let initial_state = crate::abductive::AbductiveDomain::mk_initial(&pdesc);
        let initial_exec = ExecutionDomain::ContinueProgram(initial_state);
        let initial_domain = DisjunctiveDomain::singleton(
            initial_exec,
            cfg.pulse_max_disjuncts,
            cfg.pulse_widen_threshold,
        );
        let pulse_tf = PulseTransferFunctions {
            callee_summaries: &callee_summaries,
            pdesc: &pdesc,
            proc_name: format!("{}", pdesc.proc_name),
            spec_requests: RefCell::new(Vec::new()),
            progress: RefCell::new(ProcProgress::new()),
        };
        let inv_map = interp::compute_fixpoint_wto(&pulse_tf, &(), &pdesc, initial_domain);
        for node in &pdesc.nodes {
            if let Some(state) = inv_map.get(&node.id) {
                eprintln!("node {}", node.id);
                for (i, disjunct) in state.post.disjuncts.iter().enumerate() {
                    eprintln!("  [{i}] {}", summarize_exec_domain(disjunct));
                    if let ExecutionDomain::AbortProgram { state, diagnostic } = disjunct {
                        let kind = crate::summary::classify_abort_kind(&pdesc, state, diagnostic);
                        let manifest = crate::summary::abort_is_manifest(&pdesc, state);
                        eprintln!("      classify={kind:?} manifest={}", manifest,);
                    }
                }
            }
        }
    }

    #[test]
    fn test_formal_load_then_exit_stays_continue_only() {
        let pdesc = make_formal_load_then_exit_proc();
        let summary = analyze(&pdesc);

        let continue_paths = summary
            .pre_posts
            .iter()
            .filter(|pp| pp.kind == PrePostKind::ContinueProgram)
            .count();
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

        // Cross-ref: OCaml `infer --pulse-only` on the matching tiny C repro
        // exports a single `ContinueProgram` summary for this shape.
        assert_eq!(continue_paths, 1, "summary={summary:?}");
        assert_eq!(latent_null_derefs, 0, "summary={summary:?}");
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
        let call_flags = sil::call_flags::CallFlags::default();

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
                    call_flags: &call_flags,
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
        let call_flags = sil::call_flags::CallFlags::default();

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
                    call_flags: &call_flags,
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

    #[test]
    fn test_exec_known_callee_summary_force_continue_for_alias_specialized_latent_invalid_summary_without_continue(
    ) {
        let (callee_pname, callee_summary, _alias_spec, next_field) =
            make_alias_specialization_latent_invalid_summary();
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
        let call_flags = sil::call_flags::CallFlags::default();

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
                    call_flags: &call_flags,
                    spec_requests: Some(&requests),
                },
            },
            caller_state,
        );

        assert!(
            results
                .iter()
                .any(|result| matches!(result, ExecutionDomain::ContinueProgram(_))),
            "alias-specialized summaries with only stopped states should still gain the OCaml-style unknown-call continue, got {results:?}"
        );
        assert!(
            requests.into_inner().is_empty(),
            "cached specialization should avoid re-enqueueing the same alias request"
        );
    }

    #[test]
    fn test_exec_known_callee_summary_force_continue_for_empty_summary() {
        let (callee_pname, callee_summary) = make_empty_known_summary();
        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let ret_id = Ident::create_none();
        let ret_typ = Typ::void();
        let args: [(Exp, Typ); 0] = [];
        let loc = Location::dummy();
        let call_flags = sil::call_flags::CallFlags::default();
        let state = crate::abductive::AbductiveDomain::mk_initial(&caller_pdesc);

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
                    call_flags: &call_flags,
                    spec_requests: None,
                },
            },
            state,
        );

        assert!(
            results
                .iter()
                .any(|result| matches!(result, ExecutionDomain::ContinueProgram(_))),
            "empty known summaries should fall back to unknown-call continue, got {results:?}"
        );
    }

    #[test]
    fn test_exec_known_callee_summary_force_continue_appends_unknown_continue_for_dropped_summary()
    {
        let (callee_pname, callee_summary) = make_abort_only_summary(true);
        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let ret_id = Ident::create_none();
        let ret_typ = Typ::void();
        let args: [(Exp, Typ); 0] = [];
        let loc = Location::dummy();
        let call_flags = sil::call_flags::CallFlags::default();
        let state = crate::abductive::AbductiveDomain::mk_initial(&caller_pdesc);

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
                    call_flags: &call_flags,
                    spec_requests: None,
                },
            },
            state,
        );

        assert!(
            results
                .iter()
                .any(|result| matches!(result, ExecutionDomain::ContinueProgram(_))),
            "dropped-summary known calls should gain an unknown-call continue, got {results:?}"
        );
        assert_eq!(
            results.len(),
            1,
            "manifest callee-local aborts should still stay unpublished on callers; force-continue only restores the missing continue path"
        );
    }

    #[test]
    fn test_exec_known_callee_summary_does_not_force_continue_for_precise_abort_only_summary() {
        let (callee_pname, callee_summary) = make_abort_only_summary(false);
        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let ret_id = Ident::create_none();
        let ret_typ = Typ::void();
        let args: [(Exp, Typ); 0] = [];
        let loc = Location::dummy();
        let call_flags = sil::call_flags::CallFlags::default();
        let state = crate::abductive::AbductiveDomain::mk_initial(&caller_pdesc);

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
                    call_flags: &call_flags,
                    spec_requests: None,
                },
            },
            state,
        );

        assert!(
            !results
                .iter()
                .any(|result| matches!(result, ExecutionDomain::ContinueProgram(_))),
            "precise abort-only summaries should not be widened into unknown-call continues"
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

    #[test]
    #[ignore = "debug parity probe against local /tmp latent.sil fixture"]
    fn test_debug_latent_exit_disjuncts_before_summary_export() {
        let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
        if !sil.exists() {
            eprintln!("skip");
            return;
        }

        let mut tm = textual_utils::parse_file_and_convert(sil);
        retain_named_procs(
            &mut tm,
            &[
                "traverse_and_crash_if_equal_to_root",
                "crash_after_two_nodes_bad",
                "FN_crash_after_six_nodes_bad",
            ],
        );

        let traverse = tm
            .cfg
            .iter_proc_descs()
            .find(|pdesc| format!("{}", pdesc.proc_name) == "traverse_and_crash_if_equal_to_root")
            .expect("callee procdesc should exist")
            .clone();
        let traverse_summary = analyze(&traverse);
        eprintln!(
            "CALLEE summary {:?}",
            traverse_summary
                .pre_posts
                .iter()
                .map(|pp| {
                    format!(
                        "{:?}:{:?}",
                        pp.kind,
                        crate::summary::latent_invalid_access_report_key(pp)
                    )
                })
                .collect::<Vec<_>>()
        );

        let mut callee_summaries = HashMap::new();
        callee_summaries.insert(traverse.proc_name.clone(), traverse_summary);

        for caller_name in ["crash_after_two_nodes_bad", "FN_crash_after_six_nodes_bad"] {
            let caller = tm
                .cfg
                .iter_proc_descs()
                .find(|pdesc| format!("{}", pdesc.proc_name) == caller_name)
                .expect("caller procdesc should exist")
                .clone();

            AbstractValue::reset_counters();
            let cfg = config::get();
            let initial_state = crate::abductive::AbductiveDomain::mk_initial(&caller);
            let initial_exec = ExecutionDomain::ContinueProgram(initial_state);
            let initial_domain = DisjunctiveDomain::singleton(
                initial_exec,
                cfg.pulse_max_disjuncts,
                cfg.pulse_widen_threshold,
            );
            let pulse_tf = PulseTransferFunctions {
                callee_summaries: &callee_summaries,
                pdesc: &caller,
                proc_name: format!("{}", caller.proc_name),
                spec_requests: RefCell::new(Vec::new()),
                progress: RefCell::new(ProcProgress::new()),
            };
            let inv_map = interp::compute_fixpoint_wto(&pulse_tf, &(), &caller, initial_domain);
            let exit_state = inv_map
                .get(&caller.exit_node)
                .expect("caller exit state should exist");

            let local_node = caller
                .nodes
                .iter()
                .find(|node| {
                    node.instrs
                        .iter()
                        .any(|instr| matches!(instr, Instr::Store { .. }))
                        && node
                            .instrs
                            .iter()
                            .any(|instr| matches!(instr, Instr::Load { .. }))
                })
                .expect("local field-write node should exist");
            let mut local_domain = DisjunctiveDomain::singleton(
                ExecutionDomain::ContinueProgram(crate::abductive::AbductiveDomain::mk_initial(
                    &caller,
                )),
                cfg.pulse_max_disjuncts,
                cfg.pulse_widen_threshold,
            );
            eprintln!("{caller_name} local replay node={}", local_node.id);
            for (instr_idx, instr) in local_node.instrs.iter().enumerate() {
                let mut replayed = Vec::new();
                for disjunct in &local_domain.disjuncts {
                    match disjunct {
                        ExecutionDomain::ContinueProgram(state) => {
                            replayed.extend(crate::transfer::exec_instr_with_pdesc(
                                Some(&caller),
                                instr,
                                state.clone(),
                            ))
                        }
                        other => replayed.push(other.clone()),
                    }
                }
                local_domain = DisjunctiveDomain {
                    disjuncts: replayed,
                    max_disjuncts: cfg.pulse_max_disjuncts,
                    max_widen_iters: cfg.pulse_widen_threshold,
                    had_dropped_disjuncts: false,
                };
                local_domain.dedup();
                local_domain.bound();
                eprintln!("  after instr[{instr_idx}] {instr}");
                for (disj_idx, disjunct) in local_domain.disjuncts.iter().enumerate() {
                    eprintln!("    replay[{disj_idx}] {}", summarize_exec_domain(disjunct));
                }
            }

            let caller_state_after_store = match local_domain.disjuncts.as_slice() {
                [ExecutionDomain::ContinueProgram(state)] => state.clone(),
                other => panic!("expected one continue state after local replay, got {other:?}"),
            };
            let call_node = caller
                .nodes
                .iter()
                .find(|node| {
                    node.instrs
                        .iter()
                        .any(|instr| matches!(instr, Instr::Call { .. }))
                })
                .expect("call node should exist");
            let (ret_id, actuals, call_loc) = call_node
                .instrs
                .iter()
                .find_map(|instr| match instr {
                    Instr::Call {
                        ret: (ret_id, _ret_typ),
                        args,
                        loc,
                        ..
                    } => Some((ret_id.clone(), args.clone(), loc.clone())),
                    _ => None,
                })
                .expect("call instruction should exist");

            eprintln!("{caller_name} per-pre-post apply:");
            for (pp_idx, pre_post) in callee_summaries
                .get(&traverse.proc_name)
                .expect("callee summary should exist")
                .pre_posts
                .iter()
                .enumerate()
            {
                let outcome = crate::interproc::apply_summary_with_aliasing(
                    &caller,
                    pre_post,
                    &ret_id,
                    &actuals,
                    &call_loc,
                    caller_state_after_store.clone(),
                );
                let rendered = outcome
                    .results
                    .iter()
                    .map(summarize_exec_domain)
                    .collect::<Vec<_>>();
                eprintln!(
                    "  pp[{pp_idx}] kind={:?} report_key={:?} -> {rendered:?}",
                    pre_post.kind,
                    crate::summary::latent_invalid_access_report_key(pre_post)
                );
            }

            eprintln!(
                "{caller_name} EXIT disjunct count={}",
                exit_state.post.disjuncts.len()
            );
            for (i, disjunct) in exit_state.post.disjuncts.iter().enumerate() {
                let isolated = PulseSummary::of_proc_with_metadata(
                    &caller,
                    std::slice::from_ref(disjunct),
                    Vec::new(),
                    caller.is_no_return,
                    false,
                );
                let isolated_shape = isolated
                    .pre_posts
                    .iter()
                    .map(|pp| {
                        format!(
                            "{:?}:{:?}",
                            pp.kind,
                            crate::summary::latent_invalid_access_report_key(pp)
                        )
                    })
                    .collect::<Vec<_>>();
                eprintln!("  exit[{i}] {}", summarize_exec_domain(disjunct));
                eprintln!("  exit[{i}] isolated={isolated_shape:?}");
            }
        }
    }
}
