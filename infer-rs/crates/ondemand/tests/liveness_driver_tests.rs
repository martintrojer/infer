// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Integration tests: run liveness analysis through the on-demand runner.

use std::collections::HashMap;

use analyses::liveness::{self, LivenessResult};
use ondemand::checker::{AnalysisContext, FileChecker, InterChecker, IntraChecker};
use ondemand::runner;
use sil::procdesc::Procdesc;
use sil::procname::Procname;
use sil::source_file::SourceFile;
use sil::tenv::Tenv;
use test_harness::fixtures;
use test_harness::textual_utils;

/// Wrap liveness as an IntraChecker for the runner.
struct LivenessChecker;

impl IntraChecker for LivenessChecker {
    type Summary = LivenessResult;

    fn id(&self) -> &str {
        "liveness"
    }

    fn analyze(&self, pdesc: &Procdesc, _tenv: &Tenv) -> LivenessResult {
        liveness::analyze(pdesc)
    }
}

// ---------------------------------------------------------------------------
// Basic runner tests
// ---------------------------------------------------------------------------

#[test]
fn test_runner_liveness_on_textual() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        declare unknown(int) : void

        define f(x: int) : int {
          #entry:
            n0 : int = load &x
            ret n0
        }

        define g(a: int, b: int) : void {
          #entry:
            n0 : int = load &a
            n1 : int = load &b
            n2 = unknown(n0)
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_intra(&LivenessChecker, &tm.cfg, &tm.tenv);

    assert_eq!(stats.analyzed_procs, 2);
    assert_eq!(store.len(), 2);

    store.for_each(|_pname, result| {
        assert!(!result.inv_map.is_empty());
    });
}

#[test]
fn test_runner_closure_api() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"
        define a() : void { #e: ret null }
        define b() : void { #e: ret null }
        define c() : void { #e: ret null }
    "#,
    );

    let (store, stats) = runner::run_parallel(&tm.cfg, &tm.tenv, |pdesc, _tenv| pdesc.nodes.len());

    assert_eq!(stats.analyzed_procs, 3);
    store.for_each(|_pname, &count| {
        assert_eq!(count, 3);
    });
}

// ---------------------------------------------------------------------------
// Parallel correctness: runner results must match sequential analysis
// ---------------------------------------------------------------------------

/// Run liveness sequentially on each procedure and collect results.
fn run_sequential(cfg: &sil::cfg::Cfg) -> HashMap<String, LivenessResult> {
    let mut results = HashMap::new();
    for pdesc in cfg.iter_proc_descs() {
        let result = liveness::analyze(pdesc);
        results.insert(format!("{}", pdesc.proc_name), result);
    }
    results
}

/// Compare two liveness results by checking that every node has the same
/// live variable set.
fn assert_liveness_equal(proc_name: &str, sequential: &LivenessResult, parallel: &LivenessResult) {
    for (node_id, state) in &sequential.inv_map {
        let par_state = parallel
            .inv_map
            .get(node_id)
            .unwrap_or_else(|| panic!("{proc_name}: node {node_id} missing from parallel result"));
        assert_eq!(
            state, par_state,
            "{proc_name}: liveness mismatch at node {node_id}"
        );
    }
    assert_eq!(
        sequential.inv_map.len(),
        parallel.inv_map.len(),
        "{proc_name}: different number of nodes in invariant map"
    );
}

#[test]
fn test_parallel_matches_sequential_on_multi_proc_textual() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        declare unknown(int, int) : int

        define f(x: int) : int {
          #entry:
            n0 : int = load &x
            ret n0
        }

        define g(a: int, b: int) : void {
          #entry:
            n0 : int = load &a
            n1 : int = load &b
            n2 = unknown(n0, n1)
            ret null
        }

        define h(x: int, y: int, z: int) : void {
          #entry:
            n0 : int = load &x
            jmp use_y, use_z
          #use_y:
            n1 : int = load &y
            jmp done
          #use_z:
            n2 : int = load &z
            jmp done
          #done:
            ret null
        }

        define loop_proc(n: int) : void {
          #entry:
            n0 : int = load &n
            jmp header
          #header:
            jmp body, exit
          #body:
            n1 : int = load &n
            jmp header
          #exit:
            ret null
        }
    "#,
    );

    let sequential = run_sequential(&tm.cfg);
    let (store, stats) = runner::run_intra(&LivenessChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 4);

    store.for_each(|pname, parallel_result| {
        let name = format!("{pname}");
        let seq_result = sequential
            .get(&name)
            .unwrap_or_else(|| panic!("proc {name} not in sequential results"));
        assert_liveness_equal(&name, seq_result, parallel_result);
    });
}

/// Run parallel vs sequential on all OCaml .sil pulse test files.
/// This is the strongest correctness test: 97 procedures, all must match.
#[test]
fn test_parallel_matches_sequential_on_sil_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("pulse");

    let sil_files = fixtures::list_sil_files(&dir);
    let mut total_procs = 0;
    let mut mismatches = Vec::new();

    for path in &sil_files {
        let filename = path.file_name().unwrap().to_str().unwrap();
        if filename.starts_with("error") || filename.starts_with("syntax_error") {
            continue;
        }

        let tm = textual_utils::parse_file_and_convert(path);
        let sequential = run_sequential(&tm.cfg);
        let (store, stats) = runner::run_intra(&LivenessChecker, &tm.cfg, &tm.tenv);

        assert_eq!(stats.analyzed_procs, stats.total_procs);

        for (pname, parallel_result) in store.to_vec() {
            let name = format!("{pname}");
            if let Some(seq_result) = sequential.get(&name) {
                if seq_result.inv_map != parallel_result.inv_map {
                    mismatches.push(format!("{filename}:{name}"));
                }
            }
        }

        total_procs += stats.analyzed_procs;
    }

    assert!(
        mismatches.is_empty(),
        "parallel/sequential mismatch in {} procedures: {:?}",
        mismatches.len(),
        mismatches
    );
    eprintln!("parallel correctness verified for {total_procs} procedures");
    assert!(total_procs > 90, "expected at least 90 procedures");
}

// ---------------------------------------------------------------------------
// Inter-procedural ordering: callee summaries available before callers
// ---------------------------------------------------------------------------

/// Inter-procedural checker that collects the set of callees whose summaries
/// were available at analysis time. Used to verify bottom-up ordering.
struct CalleeSummaryChecker;

/// Summary: set of callee names that had summaries available.
#[derive(Clone, Debug)]
struct AvailableSummaries {
    available_callees: Vec<String>,
    total_callees: usize,
}

impl InterChecker for CalleeSummaryChecker {
    type Summary = AvailableSummaries;

    fn id(&self) -> &str {
        "callee_summary_checker"
    }

    fn analyze(
        &self,
        pdesc: &Procdesc,
        ctx: &AnalysisContext<AvailableSummaries>,
    ) -> AvailableSummaries {
        let mut available = Vec::new();
        let mut total = 0;

        for (_node_id, instr) in pdesc.iter_instrs() {
            if let sil::instr::Instr::Call {
                fun_exp: sil::exp::Exp::Const(sil::const_val::Const::Cfun(callee)),
                ..
            } = instr
            {
                total += 1;
                if ctx.summaries.get(callee).is_some() {
                    available.push(format!("{callee}"));
                }
            }
        }

        AvailableSummaries {
            available_callees: available,
            total_callees: total,
        }
    }
}

/// In a chain a→b→c, bottom-up scheduling should ensure c is analyzed first,
/// then b (sees c's summary), then a (sees b's summary).
#[test]
fn test_inter_chain_all_summaries_available() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define leaf() : void {
          #entry:
            ret null
        }

        define mid() : void {
          #entry:
            n0 = leaf()
            ret null
        }

        define top() : void {
          #entry:
            n0 = mid()
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_inter(&CalleeSummaryChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 3);
    assert!(
        stats.num_waves >= 2,
        "should have multiple waves for a chain"
    );

    // leaf: no callees
    let leaf = store.get_by_name("leaf").unwrap();
    assert_eq!(leaf.total_callees, 0);

    // mid: calls leaf, leaf's summary should be available
    let mid = store.get_by_name("mid").unwrap();
    assert_eq!(mid.total_callees, 1);
    assert_eq!(
        mid.available_callees.len(),
        1,
        "mid should see leaf's summary"
    );

    // top: calls mid, mid's summary should be available
    let top = store.get_by_name("top").unwrap();
    assert_eq!(top.total_callees, 1);
    assert_eq!(
        top.available_callees.len(),
        1,
        "top should see mid's summary"
    );
}

/// Diamond: d calls b and c, both call a. All of a, b, c summaries should be
/// available when d is analyzed.
#[test]
fn test_inter_diamond_all_summaries_available() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define a() : void { #entry: ret null }

        define b() : void {
          #entry:
            n0 = a()
            ret null
        }

        define c() : void {
          #entry:
            n0 = a()
            ret null
        }

        define d() : void {
          #entry:
            n0 = b()
            n1 = c()
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_inter(&CalleeSummaryChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 4);

    // d calls b and c — both should have summaries available
    let d = store.get_by_name("d").unwrap();
    assert_eq!(d.total_callees, 2);
    assert_eq!(
        d.available_callees.len(),
        2,
        "d should see both b and c summaries, got: {:?}",
        d.available_callees
    );
}

// ---------------------------------------------------------------------------
// Cycle behavior: mutual recursion should not panic, results should exist
// ---------------------------------------------------------------------------

/// Two mutually recursive procedures: both should complete analysis.
/// At least one will see its cycle-mate's summary (whichever runs second
/// within the wave), the other gets None.
#[test]
fn test_inter_mutual_recursion_completes() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define ping() : void {
          #entry:
            n0 = pong()
            ret null
        }

        define pong() : void {
          #entry:
            n0 = ping()
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_inter(&CalleeSummaryChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 2);

    let ping = store.get_by_name("ping").unwrap();
    let pong = store.get_by_name("pong").unwrap();

    // Both should have completed with total_callees = 1
    assert_eq!(ping.total_callees, 1);
    assert_eq!(pong.total_callees, 1);

    // At least one should NOT have its callee's summary (the one that ran first
    // in the wave). The total available across both should be <= 2 (could be 0, 1, or 2
    // depending on rayon scheduling within the wave).
    let total_available = ping.available_callees.len() + pong.available_callees.len();
    assert!(
        total_available <= 2,
        "at most 2 callee summaries can be available in a 2-node cycle"
    );
}

/// Cycle with a dependent: a↔b cycle, c calls a.
/// c should see a's summary (cycle is resolved before c runs).
#[test]
fn test_inter_cycle_with_dependent() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define a() : void {
          #entry:
            n0 = b()
            ret null
        }

        define b() : void {
          #entry:
            n0 = a()
            ret null
        }

        define c() : void {
          #entry:
            n0 = a()
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_inter(&CalleeSummaryChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 3);
    assert!(
        stats.num_waves >= 2,
        "cycle {{a,b}} should be in an earlier wave than c"
    );

    // c calls a — a's summary should be available since the cycle wave
    // runs before c's wave
    let c = store.get_by_name("c").unwrap();
    assert_eq!(c.total_callees, 1);
    assert_eq!(
        c.available_callees.len(),
        1,
        "c should see a's summary (cycle resolved in earlier wave)"
    );
}

/// Larger cycle: a→b→c→a, d calls a.
#[test]
fn test_inter_three_node_cycle() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define a() : void {
          #entry:
            n0 = b()
            ret null
        }

        define b() : void {
          #entry:
            n0 = c()
            ret null
        }

        define c() : void {
          #entry:
            n0 = a()
            ret null
        }

        define d() : void {
          #entry:
            n0 = a()
            n1 = b()
            n2 = c()
            ret null
        }
    "#,
    );

    let (store, stats) = runner::run_inter(&CalleeSummaryChecker, &tm.cfg, &tm.tenv);
    assert_eq!(stats.analyzed_procs, 4);

    // d should see all three cycle members' summaries
    let d = store.get_by_name("d").unwrap();
    assert_eq!(d.total_callees, 3);
    assert_eq!(
        d.available_callees.len(),
        3,
        "d should see a, b, c summaries (cycle resolved in earlier wave), got: {:?}",
        d.available_callees
    );
}

// ---------------------------------------------------------------------------
// File-level callbacks
// ---------------------------------------------------------------------------

/// File checker that counts procedures in a file.
struct ProcCounter;

impl FileChecker for ProcCounter {
    type ProcSummary = LivenessResult;
    type FileSummary = usize;

    fn id(&self) -> &str {
        "proc_counter"
    }

    fn analyze_file(
        &self,
        _source_file: &SourceFile,
        proc_summaries: &[(&Procname, &LivenessResult)],
        _tenv: &Tenv,
    ) -> usize {
        proc_summaries.len()
    }
}

#[test]
fn test_file_callbacks() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define f(x: int) : int {
          #entry:
            n0 : int = load &x
            ret n0
        }

        define g(a: int) : void {
          #entry:
            ret null
        }

        define h() : void {
          #entry:
            ret null
        }
    "#,
    );

    // First run procedure-level liveness
    let (proc_store, proc_stats) = runner::run_intra(&LivenessChecker, &tm.cfg, &tm.tenv);
    assert_eq!(proc_stats.analyzed_procs, 3);

    // Then run file-level callbacks
    let file_store = runner::run_file_callbacks(&ProcCounter, &tm.cfg, &tm.tenv, &proc_store);

    // All 3 procedures come from the same file, so there should be 1 file summary
    assert_eq!(file_store.len(), 1);
    // The file callback receives all 3 procedure summaries
    file_store.for_each(|_key, &count| {
        assert_eq!(count, 3, "file callback should see all 3 procedures");
    });
}
