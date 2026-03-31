// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Analysis runner: parallel procedure analysis.
//!
//! Replaces OCaml's `InferAnalyze.ml` + `ondemand.ml` with a Rust-native
//! design using rayon for parallelism and DashMap for summary storage.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use sil::cfg::Cfg;
use sil::procname::Procname;
use sil::source_file::SourceFile;
use sil::tenv::Tenv;

use crate::callgraph::CallGraph;
use crate::checker::{AnalysisContext, FileChecker, InterChecker, IntraChecker};
use crate::summary::SummaryStore;

/// Statistics from an analysis run.
#[derive(Debug)]
pub struct RunStats {
    pub total_procs: usize,
    pub analyzed_procs: usize,
    pub num_waves: usize,
    pub elapsed_ms: u128,
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

/// Run an interprocedural checker on all procedures in bottom-up call graph order.
///
/// Procedures are grouped into "waves" — each wave contains procedures whose
/// callees have all been analyzed in prior waves. Procedures within a wave
/// run in parallel.
///
/// Within a wave (especially cycle waves), blocking deduplication ensures each
/// procedure is analyzed exactly once: if two threads discover the same callee,
/// the first computes it while the second blocks on `OnceLock`.
pub fn run_inter<C: InterChecker>(
    checker: &C,
    cfg: &Cfg,
    tenv: &Tenv,
) -> (SummaryStore<C::Summary>, RunStats) {
    let start = Instant::now();
    let store = SummaryStore::new();
    let cg = CallGraph::from_cfg(cfg);
    let defined: HashSet<Procname> = cfg.proc_descs.keys().cloned().collect();
    let waves = cg.bottom_up_schedule(&defined);
    let num_waves = waves.len();
    let analyzed = AtomicUsize::new(0);

    for wave in &waves {
        wave.par_iter().for_each(|pname| {
            // Use get_or_compute for blocking dedup: if another thread in this
            // wave is already analyzing this procedure, we wait for it.
            if let Some(pdesc) = cfg.get_proc_desc(pname) {
                store.get_or_compute(pname, || {
                    let ctx = AnalysisContext {
                        tenv,
                        summaries: &store,
                        cfg,
                    };
                    analyzed.fetch_add(1, Ordering::Relaxed);
                    checker.analyze(pdesc, &ctx)
                });
            }
        });
    }

    let stats = RunStats {
        total_procs: cfg.num_procs(),
        analyzed_procs: analyzed.load(Ordering::Relaxed),
        num_waves,
        elapsed_ms: start.elapsed().as_millis(),
    };

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
                if let Instr::Call { fun_exp, .. } = instr {
                    if let Exp::Const(Const::Cfun(callee)) = fun_exp {
                        if let Some(callee_depth) = ctx.summaries.get(callee) {
                            max_callee_depth = max_callee_depth.max(callee_depth);
                        }
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
                if let Instr::Call { fun_exp, .. } = instr {
                    if let Exp::Const(Const::Cfun(callee)) = fun_exp {
                        if let Some(callee_depth) = ctx.summaries.get(callee) {
                            max_callee_depth = max_callee_depth.max(callee_depth);
                        }
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
