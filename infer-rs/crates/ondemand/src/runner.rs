// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Analysis runner: parallel procedure analysis.
//!
//! Replaces OCaml's `InferAnalyze.ml` + `ondemand.ml` with a Rust-native
//! design using rayon for parallelism and DashMap for summary storage.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use sil::cfg::Cfg;
use sil::procname::Procname;
use sil::source_file::SourceFile;
use sil::tenv::Tenv;

use crate::callgraph::CallGraph;
use crate::checker::{AnalysisContext, FileChecker, InterChecker, IntraChecker};
use crate::summary::SummaryStore;

/// Cross-ref: OCaml `Config.trace_ondemand`.
const PROGRESS_LOG_INTERVAL: Duration = Duration::from_secs(10);
const ACTIVE_PROC_LOG_LIMIT: usize = 3;
const SLOW_PROC_LOG_THRESHOLD: Duration = Duration::from_secs(5);

/// Statistics from an analysis run.
#[derive(Debug)]
pub struct RunStats {
    pub total_procs: usize,
    pub analyzed_procs: usize,
    pub num_waves: usize,
    pub elapsed_ms: u128,
}

#[derive(Clone, Copy)]
enum RoundKind {
    Dynamic,
    CycleCut,
}

impl RoundKind {
    fn label(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::CycleCut => "cycle-cut",
        }
    }
}

struct RoundProgress<'a, S> {
    checker_id: &'a str,
    round_num: usize,
    logical_waves: usize,
    kind: RoundKind,
    seed_size: usize,
    completed_before_round: usize,
    total_procs: usize,
    analyzed: &'a AtomicUsize,
    store: &'a SummaryStore<S>,
    remaining: &'a Mutex<HashSet<Procname>>,
    active: &'a Mutex<HashMap<Procname, Instant>>,
    analysis_start: Instant,
    round_start: Instant,
}

struct DynamicSchedule {
    callers_of: HashMap<Procname, Vec<Procname>>,
    dependency_counts: HashMap<Procname, AtomicUsize>,
    scheduled: HashMap<Procname, AtomicBool>,
    remaining: Mutex<HashSet<Procname>>,
}

impl DynamicSchedule {
    fn new(call_graph: &CallGraph, defined: &HashSet<Procname>) -> Self {
        let callers_of = call_graph.callers_of_defined(defined);
        let dependency_counts = call_graph
            .defined_dependency_counts(defined)
            .into_iter()
            .map(|(pname, deps)| (pname, AtomicUsize::new(deps)))
            .collect();
        let scheduled = defined
            .iter()
            .cloned()
            .map(|pname| (pname, AtomicBool::new(false)))
            .collect();

        Self {
            callers_of,
            dependency_counts,
            scheduled,
            remaining: Mutex::new(defined.clone()),
        }
    }

    fn collect_ready_seed(&self) -> Vec<Procname> {
        let remaining = self.remaining.lock().expect("remaining set poisoned");
        let mut ready: Vec<_> = remaining
            .iter()
            .filter(|pname| {
                self.dependency_counts
                    .get(*pname)
                    .is_some_and(|count| count.load(Ordering::Acquire) == 0)
                    && self
                        .scheduled
                        .get(*pname)
                        .is_some_and(|scheduled| !scheduled.load(Ordering::Acquire))
            })
            .cloned()
            .collect();
        ready.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));
        ready
    }

    fn remaining_snapshot(&self) -> HashSet<Procname> {
        self.remaining
            .lock()
            .expect("remaining set poisoned")
            .clone()
    }

    fn remaining_count(&self) -> usize {
        self.remaining.lock().expect("remaining set poisoned").len()
    }

    fn try_mark_scheduled(&self, pname: &Procname) -> bool {
        self.scheduled
            .get(pname)
            .expect("scheduled state should exist for proc")
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn mark_completed(&self, pname: &Procname) {
        self.remaining
            .lock()
            .expect("remaining set poisoned")
            .remove(pname);
    }
}

struct InterRunCtx<'a, C: InterChecker> {
    checker: &'a C,
    cfg: &'a Cfg,
    tenv: &'a Tenv,
    store: &'a SummaryStore<C::Summary>,
    analyzed: &'a AtomicUsize,
    schedule: &'a DynamicSchedule,
    active: &'a Mutex<HashMap<Procname, Instant>>,
}

fn ondemand_progress_enabled() -> bool {
    log::log_enabled!(log::Level::Info)
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

fn log_round_snapshot<S>(phase: &str, progress: &RoundProgress<'_, S>)
where
    S: Send + Sync + 'static,
{
    let completed = progress.store.len();
    let completed_since_round = completed.saturating_sub(progress.completed_before_round);
    let analyzed_procs = progress.analyzed.load(Ordering::Relaxed);
    let remaining = progress
        .remaining
        .lock()
        .expect("remaining set poisoned")
        .len();
    let total_elapsed = progress.analysis_start.elapsed();
    let total_elapsed_secs = total_elapsed.as_secs_f64();
    let throughput = if total_elapsed_secs > 0.0 {
        completed as f64 / total_elapsed_secs
    } else {
        0.0
    };
    let eta = if throughput > 0.0 && remaining > 0 {
        format_duration(Duration::from_secs_f64(remaining as f64 / throughput))
    } else {
        "unknown".to_string()
    };
    let (active_count, active_top) = {
        let active = progress.active.lock().expect("active set poisoned");
        let mut longest_running: Vec<_> = active
            .iter()
            .map(|(pname, started)| (pname.to_string(), started.elapsed()))
            .collect();
        longest_running.sort_by(|lhs, rhs| rhs.1.cmp(&lhs.1).then_with(|| lhs.0.cmp(&rhs.0)));
        longest_running.truncate(ACTIVE_PROC_LOG_LIMIT);
        let summary = if longest_running.is_empty() {
            "none".to_string()
        } else {
            longest_running
                .into_iter()
                .map(|(pname, elapsed)| format!("{pname}:{}", format_duration(elapsed)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        (active.len(), summary)
    };

    log::info!(
        "[ondemand] checker={} round {} {} {phase}: \
         seed_size={} completed_since_round_start={} \
         completed={completed}/{} remaining={remaining} analyzed={analyzed_procs} \
         round_elapsed={} total_elapsed={} rate={throughput:.2} proc/s eta={eta} \
         active={active_count} active_top=[{active_top}]",
        progress.checker_id,
        progress.round_num,
        progress.kind.label(),
        progress.seed_size,
        completed_since_round,
        progress.total_procs,
        format_duration(progress.round_start.elapsed()),
        format_duration(total_elapsed),
    );
}

fn report_round_progress<S>(progress: &RoundProgress<'_, S>, stop_rx: mpsc::Receiver<()>)
where
    S: Send + Sync + 'static,
{
    while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(PROGRESS_LOG_INTERVAL) {
        log_round_snapshot("progress", progress);
    }
}

fn spawn_dynamic_proc<'scope, C>(
    scope: &rayon::Scope<'scope>,
    ctx: &'scope InterRunCtx<'scope, C>,
    pname: Procname,
    propagate_ready_callers: bool,
) where
    C: InterChecker + 'scope,
{
    scope.spawn(move |scope| {
        let pdesc = ctx
            .cfg
            .get_proc_desc(&pname)
            .expect("scheduled procedure should exist in cfg");
        let proc_start = Instant::now();
        ctx.active
            .lock()
            .expect("active set poisoned")
            .insert(pname.clone(), proc_start);
        let _summary = ctx.store.get_or_compute_arc(&pname, || {
            let analysis_ctx = AnalysisContext {
                tenv: ctx.tenv,
                summaries: ctx.store,
                cfg: ctx.cfg,
            };
            ctx.analyzed.fetch_add(1, Ordering::Relaxed);
            ctx.checker.analyze(pdesc, &analysis_ctx)
        });
        let proc_elapsed = proc_start.elapsed();
        ctx.active
            .lock()
            .expect("active set poisoned")
            .remove(&pname);
        if ondemand_progress_enabled() && proc_elapsed >= SLOW_PROC_LOG_THRESHOLD {
            log::info!(
                "[ondemand] checker={} slow proc done: {} elapsed={}",
                ctx.checker.id(),
                pname,
                format_duration(proc_elapsed),
            );
        }
        ctx.schedule.mark_completed(&pname);

        let mut newly_ready = Vec::new();
        if let Some(callers) = ctx.schedule.callers_of.get(&pname) {
            for caller in callers {
                let previous = ctx
                    .schedule
                    .dependency_counts
                    .get(caller)
                    .expect("dependency count should exist for caller")
                    .fetch_sub(1, Ordering::AcqRel);
                debug_assert!(previous > 0, "dependency count underflow for {caller}");
                if previous == 1
                    && propagate_ready_callers
                    && ctx.schedule.try_mark_scheduled(caller)
                {
                    newly_ready.push(caller.clone());
                }
            }
        }

        if propagate_ready_callers {
            for caller in newly_ready {
                spawn_dynamic_proc(scope, ctx, caller, true);
            }
        }
    });
}

/// Run an intraprocedural checker on all procedures in a Cfg.
///
/// All procedures are analyzed in parallel with no ordering constraints.
/// This is the simplest and fastest mode — ideal for checkers like liveness
/// that don't need callee summaries.
pub fn run_intra<C: IntraChecker>(
    checker: &C,
    cfg: &Cfg,
    tenv: &Tenv,
) -> (SummaryStore<C::Summary>, RunStats) {
    let start = Instant::now();
    let store = SummaryStore::new();
    let total = cfg.num_procs();
    let analyzed = AtomicUsize::new(0);

    let procs: Vec<(&Procname, &sil::procdesc::Procdesc)> = cfg.proc_descs.iter().collect();

    procs.par_iter().for_each(|(pname, pdesc)| {
        let summary = checker.analyze(pdesc, tenv);
        store.insert((*pname).clone(), summary);
        analyzed.fetch_add(1, Ordering::Relaxed);
    });

    let stats = RunStats {
        total_procs: total,
        analyzed_procs: analyzed.load(Ordering::Relaxed),
        num_waves: 1,
        elapsed_ms: start.elapsed().as_millis(),
    };

    (store, stats)
}

/// Run an interprocedural checker using dynamic bottom-up call graph scheduling.
///
/// Cross-ref: OCaml `backend/CallGraphScheduler.ml`.
///
/// Ready leaves are scheduled immediately, and when a procedure finishes its
/// callers are released as soon as their last remaining dependency completes.
/// This avoids the full-wave barriers of the old runner while keeping cycle
/// handling deterministic: when only cyclic SCCs remain, one SCC is cut and
/// analyzed as a round, after which newly unblocked callers can flow again.
///
/// Blocking deduplication still ensures each procedure is analyzed exactly
/// once: if two threads discover the same callee, the first computes it while
/// the second blocks on `OnceLock`.
pub fn run_inter<C: InterChecker>(
    checker: &C,
    cfg: &Cfg,
    tenv: &Tenv,
) -> (SummaryStore<C::Summary>, RunStats) {
    let start = Instant::now();
    let store = SummaryStore::new();
    let checker_id = checker.id().to_string();
    let total_procs = cfg.num_procs();
    let analyzed = AtomicUsize::new(0);
    let progress_enabled = ondemand_progress_enabled();
    if progress_enabled {
        log::info!("[ondemand] checker={checker_id} call graph start: procedures={total_procs}");
    }
    let call_graph_start = Instant::now();
    let cg = CallGraph::from_cfg(cfg);
    let call_graph_elapsed = call_graph_start.elapsed();
    if progress_enabled {
        let edge_count: usize = cg.edges.values().map(HashSet::len).sum();
        log::info!(
            "[ondemand] checker={checker_id} call graph done: caller_nodes={} known_procs={} edges={edge_count} elapsed={}",
            cg.edges.len(),
            cg.all_procs.len(),
            format_duration(call_graph_elapsed),
        );
    }
    let defined: HashSet<Procname> = cfg.proc_descs.keys().cloned().collect();
    if progress_enabled {
        log::info!(
            "[ondemand] checker={checker_id} logical schedule start: defined_procs={}",
            defined.len(),
        );
    }
    let logical_schedule_start = Instant::now();
    let logical_waves = cg.bottom_up_schedule(&defined);
    let num_waves = logical_waves.len();
    if progress_enabled {
        log::info!(
            "[ondemand] checker={checker_id} logical schedule done: waves={num_waves} elapsed={}",
            format_duration(logical_schedule_start.elapsed()),
        );
    }
    let dynamic_schedule_start = Instant::now();
    let schedule = DynamicSchedule::new(&cg, &defined);
    let active = Mutex::new(HashMap::new());
    if progress_enabled {
        log::info!(
            "[ondemand] checker={checker_id} dependency maps done: tracked_procs={} caller_buckets={} elapsed={}",
            schedule.remaining_count(),
            schedule.callers_of.len(),
            format_duration(dynamic_schedule_start.elapsed()),
        );
    }
    let run_ctx = InterRunCtx {
        checker,
        cfg,
        tenv,
        store: &store,
        analyzed: &analyzed,
        schedule: &schedule,
        active: &active,
    };

    if progress_enabled {
        let max_wave_size = logical_waves.iter().map(Vec::len).max().unwrap_or(0);
        log::info!(
            "[ondemand] checker={checker_id} scheduled {total_procs} procedure(s) \
             into {num_waves} logical wave(s); executing with dynamic callgraph scheduling; \
             max_logical_wave_size={max_wave_size}"
        );
    }

    let mut round_num = 0usize;
    while schedule.remaining_count() > 0 {
        let remaining = schedule.remaining_snapshot();
        let mut seed = schedule.collect_ready_seed();
        let kind = if seed.is_empty() {
            seed = cg.cycle_cut(&remaining);
            RoundKind::CycleCut
        } else {
            RoundKind::Dynamic
        };

        seed.retain(|pname| schedule.try_mark_scheduled(pname));
        if seed.is_empty() {
            continue;
        }

        round_num += 1;
        let completed_before_round = store.len();
        let progress = RoundProgress {
            checker_id: &checker_id,
            round_num,
            logical_waves: num_waves,
            kind,
            seed_size: seed.len(),
            completed_before_round,
            total_procs,
            analyzed: &analyzed,
            store: &store,
            remaining: &schedule.remaining,
            active: &active,
            analysis_start: start,
            round_start: Instant::now(),
        };

        if progress_enabled {
            log::info!(
                "[ondemand] checker={checker_id} round {round_num} {} start: \
                 seed_size={} completed={completed_before_round}/{total_procs} \
                 logical_waves={}",
                kind.label(),
                seed.len(),
                progress.logical_waves,
            );
            if log::log_enabled!(log::Level::Debug) {
                let members = seed.iter().map(ToString::to_string).collect::<Vec<_>>();
                log::debug!(
                    "[ondemand] checker={checker_id} round {round_num} {} seeds: {}",
                    kind.label(),
                    members.join(", ")
                );
            }
        }

        std::thread::scope(|thread_scope| {
            let stop_tx = if progress_enabled {
                let (stop_tx, stop_rx) = mpsc::channel();
                thread_scope.spawn(|| report_round_progress(&progress, stop_rx));
                Some(stop_tx)
            } else {
                None
            };

            rayon::scope(|scope| {
                for pname in seed {
                    spawn_dynamic_proc(scope, &run_ctx, pname, matches!(kind, RoundKind::Dynamic));
                }
            });

            if let Some(stop_tx) = stop_tx {
                let _ = stop_tx.send(());
            }
        });

        if progress_enabled {
            log_round_snapshot("done", &progress);
        }
    }

    let stats = RunStats {
        total_procs,
        analyzed_procs: analyzed.load(Ordering::Relaxed),
        num_waves,
        elapsed_ms: start.elapsed().as_millis(),
    };

    if progress_enabled {
        let total_elapsed = start.elapsed();
        let total_elapsed_secs = total_elapsed.as_secs_f64();
        let throughput = if total_elapsed_secs > 0.0 {
            stats.analyzed_procs as f64 / total_elapsed_secs
        } else {
            0.0
        };
        log::info!(
            "[ondemand] checker={checker_id} done: analyzed={}/{} logical_waves={} rounds={} elapsed={} \
             rate={throughput:.2} proc/s",
            stats.analyzed_procs,
            stats.total_procs,
            stats.num_waves,
            round_num,
            format_duration(total_elapsed),
        );
    }

    (store, stats)
}

/// Merge multiple `(Cfg, Tenv)` pairs, then run interprocedural analysis once
/// over the unified program.
pub fn run_inter_merged<C: InterChecker>(
    checker: &C,
    cfgs: Vec<(Cfg, Tenv)>,
) -> (SummaryStore<C::Summary>, RunStats) {
    let (mut merged_cfg, mut merged_tenv) = (Cfg::new(), Tenv::new());
    for (cfg, tenv) in cfgs {
        merged_cfg.merge(cfg);
        merged_tenv.merge(tenv);
    }
    run_inter(checker, &merged_cfg, &merged_tenv)
}

/// Run an analysis function on all procedures in parallel (closure-based API).
///
/// Simpler alternative to the trait-based API for one-off analyses.
pub fn run_parallel<S, F>(cfg: &Cfg, tenv: &Tenv, analyze: F) -> (SummaryStore<S>, RunStats)
where
    S: Send + Sync + 'static,
    F: Fn(&sil::procdesc::Procdesc, &Tenv) -> S + Send + Sync,
{
    let start = Instant::now();
    let store = SummaryStore::new();
    let total = cfg.num_procs();
    let analyzed = AtomicUsize::new(0);

    let procs: Vec<(&Procname, &sil::procdesc::Procdesc)> = cfg.proc_descs.iter().collect();

    procs.par_iter().for_each(|(pname, pdesc)| {
        let summary = analyze(pdesc, tenv);
        store.insert((*pname).clone(), summary);
        analyzed.fetch_add(1, Ordering::Relaxed);
    });

    let stats = RunStats {
        total_procs: total,
        analyzed_procs: analyzed.load(Ordering::Relaxed),
        num_waves: 1,
        elapsed_ms: start.elapsed().as_millis(),
    };

    (store, stats)
}

/// Run file-level callbacks after procedure-level analysis.
///
/// Groups procedures by source file (using `Procdesc.loc.file`), then runs
/// the file checker on each group in parallel. This is the Rust equivalent
/// of OCaml's `Callbacks.iterate_file_callbacks_and_store_issues`.
///
/// Typical usage: run `run_intra` or `run_inter` first, then pass the
/// resulting `SummaryStore` to this function.
pub fn run_file_callbacks<FC: FileChecker>(
    file_checker: &FC,
    cfg: &Cfg,
    tenv: &Tenv,
    proc_summaries: &SummaryStore<FC::ProcSummary>,
) -> SummaryStore<FC::FileSummary> {
    let file_store = SummaryStore::new();

    // Group procedures by source file
    let mut by_file: HashMap<SourceFile, Vec<&Procname>> = HashMap::new();
    for pdesc in cfg.iter_proc_descs() {
        by_file
            .entry(pdesc.loc.file.clone())
            .or_default()
            .push(&pdesc.proc_name);
    }

    let files: Vec<(SourceFile, Vec<&Procname>)> = by_file.into_iter().collect();

    files.par_iter().for_each(|(source_file, proc_names)| {
        // Collect procedure summaries for this file
        let summaries: Vec<(&Procname, FC::ProcSummary)> = proc_names
            .iter()
            .filter_map(|pname| proc_summaries.get(pname).map(|s| (*pname, s)))
            .collect();
        let summary_refs: Vec<(&Procname, &FC::ProcSummary)> =
            summaries.iter().map(|(p, s)| (*p, s)).collect();

        let file_summary = file_checker.analyze_file(source_file, &summary_refs, tenv);

        // Use source file Display as the "procname" key for file summaries
        let file_key = Procname::c_from_string(&format!("__file__{source_file}"));
        file_store.insert(file_key, file_summary);
    });

    file_store
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::time::Duration;

    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::ident::{Ident, IdentName};
    use sil::instr::Instr;
    use sil::location::Location;
    use sil::procdesc::{NodeKind, Procdesc, StmtNodeKind};
    use sil::typ::Typ;

    fn mk_simple_proc(name: &str) -> Procdesc {
        let pname = Procname::c_from_string(name);
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![],
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    fn mk_calling_proc(name: &str, callee: &str) -> Procdesc {
        let pname = Procname::c_from_string(name);
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        let instrs = vec![Instr::Call {
            ret: (
                Ident::create_normal(IdentName::from_string("n"), 0),
                Typ::void(),
            ),
            fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string(callee))),
            args: vec![],
            loc: Location::dummy(),
            flags: CallFlags::default(),
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

    /// Dummy checker that counts nodes in a procedure.
    struct NodeCounter;

    impl IntraChecker for NodeCounter {
        type Summary = usize;
        fn id(&self) -> &str {
            "node_counter"
        }
        fn analyze(&self, pdesc: &Procdesc, _tenv: &Tenv) -> usize {
            pdesc.nodes.len()
        }
    }

    /// Dummy interprocedural checker: returns 1 + max callee depth.
    struct DepthChecker;

    impl InterChecker for DepthChecker {
        type Summary = u32;
        fn id(&self) -> &str {
            "depth"
        }
        fn analyze(&self, pdesc: &Procdesc, ctx: &AnalysisContext<u32>) -> u32 {
            let mut max_callee_depth: u32 = 0;
            for (_node_id, instr) in pdesc.iter_instrs() {
                if let Instr::Call {
                    fun_exp: Exp::Const(Const::Cfun(callee)),
                    ..
                } = instr
                {
                    if let Some(callee_depth) = ctx.summaries.get(callee) {
                        max_callee_depth = max_callee_depth.max(callee_depth);
                    }
                }
            }
            1 + max_callee_depth
        }
    }

    #[test]
    fn test_run_intra() {
        let mut cfg = Cfg::new();
        cfg.add_proc_desc(mk_simple_proc("a"));
        cfg.add_proc_desc(mk_simple_proc("b"));
        cfg.add_proc_desc(mk_simple_proc("c"));
        let tenv = Tenv::new();

        let (store, stats) = run_intra(&NodeCounter, &cfg, &tenv);
        assert_eq!(stats.analyzed_procs, 3);
        assert_eq!(stats.total_procs, 3);
        assert_eq!(store.len(), 3);
        // Each proc has 3 nodes: start, exit, one statement node
        assert_eq!(store.get(&Procname::c_from_string("a")), Some(3));
    }

    #[test]
    fn test_run_inter_bottom_up() {
        let mut cfg = Cfg::new();
        cfg.add_proc_desc(mk_simple_proc("leaf"));
        cfg.add_proc_desc(mk_calling_proc("mid", "leaf"));
        cfg.add_proc_desc(mk_calling_proc("top", "mid"));
        let tenv = Tenv::new();

        let (store, stats) = run_inter(&DepthChecker, &cfg, &tenv);
        assert_eq!(stats.analyzed_procs, 3);
        assert!(stats.num_waves >= 2); // at least leaf first, then rest

        // leaf: depth 1, mid: 1 + leaf(1) = 2, top: 1 + mid(2) = 3
        assert_eq!(store.get(&Procname::c_from_string("leaf")), Some(1));
        assert_eq!(store.get(&Procname::c_from_string("mid")), Some(2));
        assert_eq!(store.get(&Procname::c_from_string("top")), Some(3));
    }

    #[test]
    fn test_run_inter_cycle() {
        let mut cfg = Cfg::new();
        cfg.add_proc_desc(mk_calling_proc("a", "b"));
        cfg.add_proc_desc(mk_calling_proc("b", "a"));
        let tenv = Tenv::new();

        let (store, stats) = run_inter(&DepthChecker, &cfg, &tenv);
        assert_eq!(stats.analyzed_procs, 2);
        // Both in a cycle — analyzed in same wave, callee summaries may be None
        // a calls b (not yet analyzed → None → max_callee_depth = 0 → depth 1)
        // b calls a (depends on scheduling order — either 1 or 2)
        assert!(store.contains(&Procname::c_from_string("a")));
        assert!(store.contains(&Procname::c_from_string("b")));
    }

    struct DynamicChainChecker {
        slow_done: Arc<AtomicBool>,
        top_started_before_slow_done: Arc<AtomicBool>,
    }

    impl InterChecker for DynamicChainChecker {
        type Summary = ();

        fn id(&self) -> &str {
            "dynamic_chain"
        }

        fn analyze(&self, pdesc: &Procdesc, _ctx: &AnalysisContext<Self::Summary>) {
            match format!("{}", pdesc.proc_name).as_str() {
                "slow" => {
                    std::thread::sleep(Duration::from_millis(200));
                    self.slow_done.store(true, AtomicOrdering::SeqCst);
                }
                "top" => {
                    if !self.slow_done.load(AtomicOrdering::SeqCst) {
                        self.top_started_before_slow_done
                            .store(true, AtomicOrdering::SeqCst);
                    }
                }
                _ => {}
            }
        }
    }

    #[test]
    fn test_run_inter_dynamically_releases_chain_before_unrelated_slow_leaf_finishes() {
        let mut cfg = Cfg::new();
        cfg.add_proc_desc(mk_simple_proc("slow"));
        cfg.add_proc_desc(mk_simple_proc("leaf"));
        cfg.add_proc_desc(mk_calling_proc("mid", "leaf"));
        cfg.add_proc_desc(mk_calling_proc("top", "mid"));
        let tenv = Tenv::new();

        let slow_done = Arc::new(AtomicBool::new(false));
        let top_started_before_slow_done = Arc::new(AtomicBool::new(false));
        let checker = DynamicChainChecker {
            slow_done: Arc::clone(&slow_done),
            top_started_before_slow_done: Arc::clone(&top_started_before_slow_done),
        };

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .expect("local rayon pool");
        let (_store, stats) = pool.install(|| run_inter(&checker, &cfg, &tenv));

        assert_eq!(stats.analyzed_procs, 4);
        assert!(
            top_started_before_slow_done.load(AtomicOrdering::SeqCst),
            "top should start before unrelated slow leaf finishes"
        );
    }

    /// Interprocedural checker that depends on both the merged call graph and
    /// the merged type environment.
    struct TenvDepthChecker;

    impl InterChecker for TenvDepthChecker {
        type Summary = u32;

        fn id(&self) -> &str {
            "tenv_depth"
        }

        fn analyze(&self, pdesc: &Procdesc, ctx: &AnalysisContext<u32>) -> u32 {
            let mut max_callee_depth: u32 = 0;
            for (_node_id, instr) in pdesc.iter_instrs() {
                if let Instr::Call {
                    fun_exp: Exp::Const(Const::Cfun(callee)),
                    ..
                } = instr
                {
                    if let Some(callee_depth) = ctx.summaries.get(callee) {
                        max_callee_depth = max_callee_depth.max(callee_depth);
                    }
                }
            }
            ctx.tenv.len() as u32 + max_callee_depth
        }
    }

    #[test]
    fn test_run_inter_merged_unifies_cfg_and_tenv() {
        let mut cfg_left = Cfg::new();
        cfg_left.add_proc_desc(mk_simple_proc("leaf"));
        let mut tenv_left = Tenv::new();
        tenv_left.insert(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "Left",
            )),
            sil::strukt::Struct::default(),
        );

        let mut cfg_right = Cfg::new();
        cfg_right.add_proc_desc(mk_calling_proc("top", "leaf"));
        let mut tenv_right = Tenv::new();
        tenv_right.insert(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "Right",
            )),
            sil::strukt::Struct::default(),
        );

        let (store, stats) = run_inter_merged(
            &TenvDepthChecker,
            vec![(cfg_left, tenv_left), (cfg_right, tenv_right)],
        );

        assert_eq!(stats.total_procs, 2);
        assert_eq!(stats.analyzed_procs, 2);
        assert_eq!(store.get(&Procname::c_from_string("leaf")), Some(2));
        assert_eq!(store.get(&Procname::c_from_string("top")), Some(4));
    }

    #[test]
    fn test_run_parallel_closure() {
        let mut cfg = Cfg::new();
        cfg.add_proc_desc(mk_simple_proc("a"));
        cfg.add_proc_desc(mk_simple_proc("b"));
        let tenv = Tenv::new();

        let (store, stats) = run_parallel(&cfg, &tenv, |pdesc, _tenv| pdesc.nodes.len());
        assert_eq!(stats.analyzed_procs, 2);
        assert_eq!(store.get(&Procname::c_from_string("a")), Some(3));
    }
}
