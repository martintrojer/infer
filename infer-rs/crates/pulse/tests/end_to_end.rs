// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! End-to-end Pulse tests: Textual .sil → parse → to_sil → Pulse → diagnostics.

use test_harness::textual_utils;

/// Adapter for running Pulse through the ondemand interprocedural runner.
struct PulseInterChecker;

impl ondemand::checker::InterChecker for PulseInterChecker {
    type Summary = pulse::summary::PulseSummary;
    fn id(&self) -> &str {
        "pulse"
    }
    fn analyze(
        &self,
        pdesc: &sil::procdesc::Procdesc,
        ctx: &ondemand::checker::AnalysisContext<Self::Summary>,
    ) -> Self::Summary {
        analyze_with_spec_loop(pdesc, ctx, None, 0)
    }
}

/// Analyze a procedure with the specialization loop, recursively specializing
/// sub-callees as needed. This mirrors OCaml's iter_call + request_specialization
/// which can recurse through multi-level call chains.
///
/// `specialization`: if Some, apply this specialization to the initial state.
/// `depth`: recursion depth limit (prevents infinite loops).
fn analyze_with_spec_loop(
    pdesc: &sil::procdesc::Procdesc,
    ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
    specialization: Option<&sil::specialization::PulseSpecialization>,
    depth: usize,
) -> pulse::summary::PulseSummary {
    const MAX_SPEC_DEPTH: usize = 5;

    let mut callee_summaries = std::collections::HashMap::new();
    for (_node_id, instr) in pdesc.iter_instrs() {
        collect_cfun_refs(instr, &ctx.summaries, &mut callee_summaries);
    }
    // When specialization is provided, add summaries for the target procedures
    // referenced in the specialization's dynamic_types. These are the functions
    // that __call_c_function_ptr will dispatch to after resolution.
    if let Some(spec) = specialization {
        for type_name in spec.dynamic_types.values() {
            let pname = sil::procname::Procname::c_from_string(&format!("{type_name}"));
            if let Some(s) = ctx.summaries.get(&pname) {
                callee_summaries.insert(pname, s);
            }
        }
    }

    let mut summary =
        pulse::checker::analyze_with_specialization(pdesc, &callee_summaries, specialization);

    if depth >= MAX_SPEC_DEPTH {
        return summary;
    }

    // Post-analysis: check if any callee summaries need specialization
    // that we can now provide. Collect requests first, then apply.
    let spec_requests: Vec<_> = callee_summaries
        .iter()
        .filter(|(_, cs)| !cs.needs_specialization.is_empty())
        .filter_map(|(callee_pname, callee_summary)| {
            let first_pp = callee_summary.pre_posts.first()?;
            let call_args = pdesc.iter_instrs().find_map(|(_nid, instr)| {
                if let sil::instr::Instr::Call {
                    fun_exp: sil::exp::Exp::Const(sil::const_val::Const::Cfun(cp)),
                    args,
                    ..
                } = instr
                {
                    if cp == callee_pname {
                        Some(args.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })?;
            // Build state up to the call to evaluate actuals for Closure attrs
            let mut eval_state = pulse::abductive::AbductiveDomain::mk_initial(pdesc);
            // Apply specialization to the eval state so Closure attrs are visible
            if let Some(spec) = specialization {
                pulse::specialization::apply(spec, &mut eval_state);
            }
            for (_nid, pre_instr) in pdesc.iter_instrs() {
                if let sil::instr::Instr::Call {
                    fun_exp: sil::exp::Exp::Const(sil::const_val::Const::Cfun(cp)),
                    ..
                } = pre_instr
                {
                    if cp == callee_pname {
                        break;
                    }
                }
                // Replay Store and Load instructions to build eval_state
                match pre_instr {
                    sil::instr::Instr::Store { e1, e2, loc, .. } => {
                        let rhs = pulse::operations::eval_or_fresh(e2, loc, &mut eval_state);
                        let lhs = pulse::operations::eval_or_fresh(e1, loc, &mut eval_state);
                        eval_state.write_heap(lhs, pulse::access::Access::Dereference, rhs);
                    }
                    sil::instr::Instr::Load { id, e, loc, .. } => {
                        // Replay Load matching exec_load's deref semantics:
                        // Lvar/Var deref through the address, others eval directly.
                        let needs_deref = matches!(
                            e,
                            sil::exp::Exp::Lvar(_)
                                | sil::exp::Exp::Lfield(..)
                                | sil::exp::Exp::Lindex(..)
                                | sil::exp::Exp::Var(_)
                        );
                        let val = if needs_deref {
                            match pulse::operations::eval_deref(e, loc, &mut eval_state) {
                                pulse::pulse_result::PulseResult::Ok(v) => v,
                                _ => pulse::operations::eval_or_fresh(e, loc, &mut eval_state),
                            }
                        } else {
                            pulse::operations::eval_or_fresh(e, loc, &mut eval_state)
                        };
                        pulse::operations::write_id(id, val, &mut eval_state);
                    }
                    _ => {}
                }
            }
            let spec = pulse::specialization::make_specialization_from_caller(
                &callee_summary.needs_specialization,
                &eval_state,
                &first_pp.formals,
                &call_args,
            );
            let spec = spec?;
            if callee_summary.get_specialized(&spec).is_some() {
                return None;
            }
            Some((callee_pname.clone(), spec))
        })
        .collect();

    if !spec_requests.is_empty() {
        for (callee_pname, spec) in &spec_requests {
            if let Some(callee_pdesc) = ctx.cfg.get_proc_desc(callee_pname) {
                // RECURSIVE: re-analyze callee with specialization AND the
                // specialization loop, so sub-callees can also be specialized.
                let spec_summary = analyze_with_spec_loop(callee_pdesc, ctx, Some(spec), depth + 1);
                if let Some(existing) = callee_summaries.get_mut(callee_pname) {
                    existing.add_specialized(spec.clone(), spec_summary.pre_posts);
                }
            }
        }
        // Re-analyze with specialized summaries
        summary =
            pulse::checker::analyze_with_specialization(pdesc, &callee_summaries, specialization);
    }

    summary
}

/// Collect all Cfun procname references from an instruction and look up summaries.
fn collect_cfun_refs(
    instr: &sil::instr::Instr,
    store: &ondemand::summary::SummaryStore<pulse::summary::PulseSummary>,
    out: &mut std::collections::HashMap<sil::procname::Procname, pulse::summary::PulseSummary>,
) {
    match instr {
        sil::instr::Instr::Call { fun_exp, args, .. } => {
            collect_cfun_refs_exp(fun_exp, store, out);
            for (arg_exp, _) in args {
                collect_cfun_refs_exp(arg_exp, store, out);
            }
        }
        sil::instr::Instr::Store { e1, e2, .. } => {
            collect_cfun_refs_exp(e1, store, out);
            collect_cfun_refs_exp(e2, store, out);
        }
        sil::instr::Instr::Load { e, .. } => {
            collect_cfun_refs_exp(e, store, out);
        }
        _ => {}
    }
}

fn collect_cfun_refs_exp(
    exp: &sil::exp::Exp,
    store: &ondemand::summary::SummaryStore<pulse::summary::PulseSummary>,
    out: &mut std::collections::HashMap<sil::procname::Procname, pulse::summary::PulseSummary>,
) {
    match exp {
        sil::exp::Exp::Const(sil::const_val::Const::Cfun(pname)) => {
            if let Some(summary) = store.get(pname) {
                out.insert(pname.clone(), summary);
            }
        }
        sil::exp::Exp::BinOp(_, l, r) => {
            collect_cfun_refs_exp(l, store, out);
            collect_cfun_refs_exp(r, store, out);
        }
        sil::exp::Exp::UnOp(_, inner, _)
        | sil::exp::Exp::Cast(_, inner)
        | sil::exp::Exp::Exn(inner) => {
            collect_cfun_refs_exp(inner, store, out);
        }
        sil::exp::Exp::Lfield(data, _, _) => {
            collect_cfun_refs_exp(&data.exp, store, out);
        }
        sil::exp::Exp::Lindex(base, idx) => {
            collect_cfun_refs_exp(base, store, out);
            collect_cfun_refs_exp(idx, store, out);
        }
        _ => {}
    }
}

/// Smoke test: Textual → parse → to_sil → Pulse pipeline does not panic.
///
/// Uses a global `__null` variable — detection depends on global handling.
/// The definitive null deref test is `test_e2e_null_deref_fixture`.
#[test]
fn test_e2e_pipeline_smoke() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        define null_deref() : void {
          #entry:
            n0 : *void = load &__null
            store n0 <- 42 : int
            ret null
        }

        global __null : *void
    "#,
    );

    // Pipeline should not panic
    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        // Just verify analyze completes — detection tested by test_e2e_null_deref_fixture
        let _ = pulse::checker::to_issue_log(&summary, &format!("{}", pdesc.proc_name));
    }
}

/// Safe procedure: no bugs.
#[test]
fn test_e2e_safe_procedure() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"
        define safe(x: int) : int {
          #entry:
            n0 : int = load &x
            ret n0
        }
    "#,
    );

    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        assert!(
            summary.diagnostics.is_empty(),
            "safe procedure should have no diagnostics, got: {:?}",
            summary.diagnostics
        );
    }
}

/// Multiple procedures: only the buggy one reports.
#[test]
fn test_e2e_multiple_procs() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "java"

        define good(x: int) : int {
          #entry:
            n0 : int = load &x
            ret n0
        }

        define also_good(a: int, b: int) : void {
          #entry:
            n0 : int = load &a
            n1 : int = load &b
            ret null
        }
    "#,
    );

    let mut total_issues = 0;
    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        total_issues += summary.diagnostics.len();
    }
    assert_eq!(total_issues, 0, "all procedures are safe");
}

/// Pulse on null_deref.sil fixture: should find NULL_DEREFERENCE.
#[test]
fn test_e2e_null_deref_fixture() {
    let fixture = test_harness::fixtures::test_data_dir().join("pulse/null_deref.sil");
    assert!(fixture.exists());

    let tm = textual_utils::parse_file_and_convert(&fixture);
    let mut found_null_deref = false;
    let mut false_positive_on_safe = false;

    for pdesc in tm.cfg.iter_proc_descs() {
        let proc_name = format!("{}", pdesc.proc_name);
        let summary = pulse::checker::analyze(pdesc);
        for diag in &summary.diagnostics {
            if diag.get_issue_type() == "NULL_DEREFERENCE" {
                found_null_deref = true;
            }
            if proc_name.contains("safe_store_ok") {
                false_positive_on_safe = true;
            }
        }
    }

    assert!(
        found_null_deref,
        "should detect null dereference in null_deref_bad"
    );
    assert!(
        !false_positive_on_safe,
        "should not report on safe_store_ok"
    );
}

/// Pulse on basic_safe.sil fixture: should find no issues.
#[test]
fn test_e2e_basic_safe_fixture() {
    let fixture = test_harness::fixtures::test_data_dir().join("pulse/basic_safe.sil");
    assert!(fixture.exists());

    let tm = textual_utils::parse_file_and_convert(&fixture);
    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        assert!(
            summary.diagnostics.is_empty(),
            "safe procedure {} should have no diagnostics, got: {:?}",
            pdesc.proc_name,
            summary.diagnostics
        );
    }
}

/// Branch-aware NPE tests from npe_branching.sil fixture.
///
/// Tests that the CFG-aware checker properly handles:
/// - Simple null dereference (basic_bad)
/// - Conditional null dereference (conditional_bad)
/// - Null propagation through stores (store_bad)
/// - Safe procedures produce no false positives
/// - Diamond CFGs are handled correctly
#[test]
fn test_e2e_npe_branching_fixture() {
    let fixture = test_harness::fixtures::test_data_dir().join("pulse/npe_branching.sil");
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let tm = textual_utils::parse_file_and_convert(&fixture);

    // Use interprocedural analysis via ondemand runner (bottom-up call graph order)
    let checker = PulseInterChecker;
    let (store, _stats) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let mut results: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (pname, summary) in store.to_vec() {
        let issues: Vec<String> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type().to_string())
            .collect();
        results.insert(format!("{pname}"), issues);
    }

    // _bad procedures should have NULL_DEREFERENCE
    let bad_procs = [
        "basic_bad",
        "if_test_lt_bad",
        "if_test_eq_bad",
        "conditional_bad",
        "store_bad",
        "load_bad",
        "load_internal_bad",
        "array_bad",
        "loop_null_deref_bad",
        "call_and_npe_bad",
        "use_get_next_bad", // now detected via biabduction pre-condition checking
    ];
    for name in bad_procs {
        let issues = results
            .iter()
            .find(|(k, _)| k.contains(name))
            .map(|(_, v)| v);
        assert!(
            issues.is_some_and(|v| v.iter().any(|i| i == "NULL_DEREFERENCE")),
            "{name} should report NULL_DEREFERENCE, got: {issues:?}"
        );
    }

    // _ok procedures should have no issues
    let ok_procs = [
        "safe_load_ok",
        "diamond_safe_ok",
        "array_ok",
        "loop_safe_ok",
        "allocate_ok",
        "allocate_and_use_ok",
        "return_null_ok",
        "return_cell_ok",
        "call_and_no_npe_ok",
    ];
    for name in ok_procs {
        let issues = results
            .iter()
            .find(|(k, _)| k.contains(name))
            .map(|(_, v)| v);
        assert!(
            issues.is_some_and(|v| v.is_empty()),
            "{name} should have no issues, got: {issues:?}"
        );
    }

    // Path-sensitive tests: constant folding + prune unsatisfiability
    // eliminates unreachable branches, preventing false positives.
    let path_sensitive_ok = ["if_test_lt_ok", "if_test_eq_ok"];
    for name in path_sensitive_ok {
        let issues = results
            .iter()
            .find(|(k, _)| k.contains(name))
            .map(|(_, v)| v);
        assert!(
            issues.is_some_and(|v| v.is_empty()),
            "{name} should have no issues (path sensitivity), got: {issues:?}"
        );
    }
}

/// Helper: run Pulse on a .sil file using interprocedural analysis,
/// assert `_bad` procs report NULL_DEREFERENCE and `_ok` procs are clean.
/// Procs not matching either convention are checked for non-panic only.
/// `skip` lists procs to skip assertion on (known limitations).
fn assert_pulse_file(path: &std::path::Path, skip: &[&str]) {
    let label = path.file_name().unwrap().to_str().unwrap();
    assert!(path.exists(), "fixture missing: {}", path.display());

    let tm = textual_utils::parse_file_and_convert(path);
    let checker = PulseInterChecker;
    let (store, _stats) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let mut results: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (pname, summary) in store.to_vec() {
        let issues: Vec<String> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type().to_string())
            .collect();
        results.insert(format!("{pname}"), issues);
    }

    let mut failures = Vec::new();
    for (proc_name, issues) in &results {
        if skip.iter().any(|s| proc_name.contains(s)) {
            continue;
        }
        if proc_name.contains("_bad") || proc_name.contains("Bad") {
            if !issues.iter().any(|i| i == "NULL_DEREFERENCE") {
                failures.push(format!(
                    "{proc_name}: expected NULL_DEREFERENCE, got {issues:?}"
                ));
            }
        } else if proc_name.contains("_ok")
            || proc_name.contains("Ok")
            || proc_name.contains("_good")
        {
            if !issues.is_empty() {
                failures.push(format!("{proc_name}: expected no issues, got {issues:?}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{label}: {} failures:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );

    eprintln!(
        "{label}: {} procs analyzed, all assertions passed",
        results.len()
    );
}

/// Run assertions on a fixture in test-data/pulse/.
fn assert_pulse_fixture(fixture_name: &str, skip: &[&str]) {
    let path = test_harness::fixtures::test_data_dir().join(format!("pulse/{fixture_name}"));
    assert_pulse_file(&path, skip);
}

/// Run assertions on an OCaml SIL test file in infer/tests/codetoanalyze/sil/pulse/.
fn assert_ocaml_pulse_test(filename: &str, skip: &[&str]) {
    test_harness::skip_without_ocaml_sil!();
    let path = test_harness::fixtures::ocaml_sil_test_dir()
        .join("pulse")
        .join(filename);
    assert_pulse_file(&path, skip);
}

/// npe_with_load_in_exp.sil: same as npe.sil but with `l->field` (arrow) syntax.
///
/// The `->` syntax produces `Load { Field { Lvar(x), f } }`. The type_check
/// pass inserts an intermediate Load dereference when the inner expression is
/// `Ptr(Ptr(Struct))`, matching OCaml's TextualTypeVerification.
#[test]
fn test_e2e_npe_with_load_in_exp() {
    assert_ocaml_pulse_test(
        "npe_with_load_in_exp.sil",
        &[
            "external_call_and_npe_bad", // declared-only callee
        ],
    );
}

/// npe_without_types.sil: same as npe.sil but without type annotations.
#[test]
fn test_e2e_npe_without_types() {
    assert_ocaml_pulse_test(
        "npe_without_types.sil",
        &[
            "external_call_and_npe_bad", // declared-only callee, no summary
        ],
    );
}

/// alloc.sil: allocation smoke tests.
#[test]
fn test_e2e_alloc() {
    assert_ocaml_pulse_test("alloc.sil", &[]);
}

/// to_sil_bug.sil: interprocedural double-indirection tests.
#[test]
fn test_e2e_to_sil_bug() {
    assert_ocaml_pulse_test(
        "to_sil_bug.sil",
        &[
            "use_get_next_on_pure_var_bad",    // deep interproc
            "use_get_next_on_program_var_bad", // deep interproc
        ],
    );
}

/// Run Pulse on OCaml's .sil test files without panicking.
#[test]
fn test_e2e_pulse_on_sil_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = test_harness::fixtures::ocaml_sil_test_dir().join("pulse");

    let sil_files = test_harness::fixtures::list_sil_files(&dir);
    let mut total_procs = 0;
    let mut total_issues = 0;

    for path in &sil_files {
        let filename = path.file_name().unwrap().to_str().unwrap();
        if filename.starts_with("error") || filename.starts_with("syntax_error") {
            continue;
        }

        let tm = textual_utils::parse_file_and_convert(path);
        for pdesc in tm.cfg.iter_proc_descs() {
            let summary = pulse::checker::analyze(pdesc);
            total_procs += 1;
            total_issues += summary.diagnostics.len();
        }
    }

    eprintln!("Pulse analyzed {total_procs} procedures, found {total_issues} issues");
    assert!(
        total_procs > 0,
        "should have analyzed at least one procedure"
    );
    assert!(
        total_issues > 0,
        "should have found at least one issue (these files contain known bugs)"
    );
}

/// npe.sil: canonical OCaml NPE test (references OCaml source directly).
///
/// Tests `.static` method attribute handling, path-sensitive branching,
/// interprocedural call-and-npe, arrays, and local vars.
/// OCaml expects: 10 _bad procs with NULL_DEREFERENCE.
/// We detect 9/10 — external_call_and_npe_bad needs cross-file resolution.
#[test]
fn test_e2e_npe() {
    assert_ocaml_pulse_test(
        "npe.sil",
        &[
            "external_call_and_npe_bad", // callee defined in externals.sil (cross-file)
        ],
    );
}

/// npe_external_oo.sil: merged npeWithExternalObjOrient + externalObjOrientRetNull.
///
/// Tests interprocedural OO dispatch: calling a method defined on a class
/// that returns null, then dereferencing the result.
/// OCaml expects: external_call_and_npe_bad → NULL_DEREFERENCE.
#[test]
fn test_e2e_npe_external_oo() {
    assert_pulse_fixture("npe_external_oo.sil", &[]);
}

/// ocaml_model.sil: unmodeled call handling (references OCaml source).
///
/// Tests that Pulse still detects null dereferences even when
/// an unmodeled call ($builtins.not_modeled) intervenes.
/// OCaml expects: use_not_modeled_bad → NULL_DEREFERENCE.
#[test]
fn test_e2e_ocaml_model() {
    assert_ocaml_pulse_test("ocaml_model.sil", &[]);
}

/// static_types.sil: virtual dispatch via chained loads (references OCaml source).
///
/// Tests `n1: int = load n0.OO.get_null().B.f` which chains a virtual
/// method call inside a load expression. Requires virtual dispatch
/// resolution via type hierarchy.
/// OCaml expects: with_dyntype_bad, with_statictype_bad → NULL_DEREFERENCE.
/// We currently miss both — virtual method calls in load expressions
/// are not yet resolved.
#[test]
fn test_e2e_static_types() {
    assert_ocaml_pulse_test(
        "static_types.sil",
        &[
            "with_dyntype_bad",    // chained virtual call in load
            "with_statictype_bad", // chained virtual call in load
        ],
    );
}

/// virt.sil: devirtualization and dynamic type specialization (references OCaml source).
///
/// Tests virtual method dispatch through class hierarchies, dynamic type
/// inference, and summary specialization. Exercises `extends`, `.abstract`,
/// `.final`, and `$static` type modifiers.
/// OCaml expects: 5 _bad procs, 4 _good procs clean.
/// We detect 4/5 _bad (missing plusBad which needs virtual dispatch through
/// interprocedural call chains). We have false positives on _good procs
/// because we don't yet evaluate return values from devirtualized calls
/// in prune conditions.
#[test]
fn test_e2e_virt() {
    assert_ocaml_pulse_test(
        "virt.sil",
        &[
            "plusBad",                                      // virtual dispatch through call chain
            "plusOk", // FP: biabduction pre-check on virtual dispatch
            "test_dyntype_specialization_good", // FP: needs devirt return value
            "test_dyntype_specialization_from_caller_good", // FP: needs devirt return value
            "devirtualize_with_final_good", // FP: needs devirt return value
            "devirtualize_with_static_call_good", // FP: needs devirt return value
        ],
    );
}

/// Sweep: try to parse all C dump-textual output through our pipeline.
/// Heavy test — spawns `infer` for each file. Run explicitly:
///   cargo test --test end_to_end test_c_dump_textual_sweep -- --ignored --nocapture
#[test]
#[ignore]
fn test_c_dump_textual_sweep() {
    use test_harness::infer_runner::InferRunner;
    let Some(runner) = InferRunner::new() else {
        eprintln!("skipping: infer binary not found");
        return;
    };

    let c_dir = test_harness::fixtures::ocaml_c_test_dir().join("pulse");
    if !c_dir.exists() {
        eprintln!("skipping: OCaml C test dir not found");
        return;
    }

    let mut ok = 0;
    let mut fail_dump = 0;
    let mut fail_parse = 0;
    let mut fail_timeout = 0;
    let mut total_procs = 0;
    let mut total_issues = 0;
    let mut file_results: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    // Files known to hang (infinite loops exhaust our fixpoint)
    let skip_files = ["infinite.c", "recursion.c", "recursion2.c"];

    let mut entries: Vec<_> = std::fs::read_dir(&c_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "c"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        let name = path.file_name().unwrap().to_str().unwrap();

        if skip_files.iter().any(|s| *s == name) {
            eprintln!("  SKIP {name} (known hang)");
            continue;
        }

        match runner.dump_textual_for_c(&path) {
            Err(e) => {
                let short = if e.len() > 80 { &e[..80] } else { &e };
                eprintln!("  FAIL_DUMP {name}: {short}");
                fail_dump += 1;
            }
            Ok(sil_path) => {
                match std::panic::catch_unwind(|| textual_utils::parse_file_and_convert(&sil_path))
                {
                    Err(_) => {
                        eprintln!("  FAIL_PARSE {name}");
                        fail_parse += 1;
                    }
                    Ok(tm) => {
                        // Run with a 10s timeout per file
                        let (tx, rx) = std::sync::mpsc::channel();
                        let tm_clone = tm;
                        let handle = std::thread::spawn(move || {
                            let checker = PulseInterChecker;
                            let (store, _) = ondemand::runner::run_inter(
                                &checker,
                                &tm_clone.cfg,
                                &tm_clone.tenv,
                            );
                            let mut n_procs = 0;
                            let mut issues = Vec::new();
                            for (_pname, summary) in store.to_vec() {
                                n_procs += 1;
                                for d in &summary.diagnostics {
                                    issues.push(d.get_issue_type().to_string());
                                }
                            }
                            let _ = tx.send((n_procs, issues));
                        });

                        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
                            Ok((n_procs, issues)) => {
                                handle.join().ok();
                                let n_issues = issues.len();
                                eprintln!("  OK {name}: {n_procs} procs, {n_issues} issues");
                                ok += 1;
                                total_procs += n_procs;
                                total_issues += n_issues;
                                file_results.insert(name.to_string(), issues);
                            }
                            Err(_) => {
                                eprintln!("  TIMEOUT {name}");
                                fail_timeout += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    // Compare against issues.exp
    let exp_path = c_dir.join("issues.exp");
    if exp_path.exists() {
        let expected = test_harness::fixtures::parse_issues_exp(&exp_path);

        // Compare NULLPTR_DEREFERENCE
        let mut total_expected = 0;
        let mut total_found = 0;
        let mut files_with_diff = Vec::new();
        for (filename, rust_issues) in &file_results {
            let exp = test_harness::fixtures::issues_for_file(&expected, filename);
            let exp_npe = exp
                .iter()
                .filter(|e| e.issue_type == "NULLPTR_DEREFERENCE")
                .count();
            let rust_npe = rust_issues
                .iter()
                .filter(|i| i.as_str() == "NULL_DEREFERENCE")
                .count();
            total_expected += exp_npe;
            total_found += rust_npe;
            if exp_npe != rust_npe {
                files_with_diff.push(format!(
                    "    {filename}: expected {exp_npe}, found {rust_npe}"
                ));
            }
        }
        eprintln!("\n=== NULLPTR_DEREFERENCE: expected {total_expected}, found {total_found} ===");
        if !files_with_diff.is_empty() {
            files_with_diff.sort();
            eprintln!("  Differences:");
            for d in &files_with_diff {
                eprintln!("{d}");
            }
        }

        // Compare MEMORY_LEAK_C
        let mut leak_expected = 0;
        let mut leak_found = 0;
        let mut leak_diff = Vec::new();
        for (filename, rust_issues) in &file_results {
            let exp = test_harness::fixtures::issues_for_file(&expected, filename);
            let exp_leak = exp
                .iter()
                .filter(|e| e.issue_type == "MEMORY_LEAK_C")
                .count();
            let rust_leak = rust_issues
                .iter()
                .filter(|i| i.as_str() == "MEMORY_LEAK_C")
                .count();
            leak_expected += exp_leak;
            leak_found += rust_leak;
            if exp_leak != rust_leak {
                leak_diff.push(format!(
                    "    {filename}: expected {exp_leak}, found {rust_leak}"
                ));
            }
        }
        eprintln!("\n=== MEMORY_LEAK_C: expected {leak_expected}, found {leak_found} ===");
        if !leak_diff.is_empty() {
            leak_diff.sort();
            eprintln!("  Differences:");
            for d in &leak_diff {
                eprintln!("{d}");
            }
        }
    }

    eprintln!("\n=== C dump-textual sweep ===");
    eprintln!(
        "  OK: {ok}, FAIL_DUMP: {fail_dump}, FAIL_PARSE: {fail_parse}, TIMEOUT: {fail_timeout}"
    );
    eprintln!("  {total_procs} procs analyzed, {total_issues} issues found");
    assert!(ok > 0, "should have at least one passing file");
}

/// Test that exit/abort models prevent false positives on unreachable code.
/// Direct calls and indirect calls (via noreturn wrapper) both work.
/// Note: the indirect case requires single-node CFGs; multi-node CFGs
/// from dump-textual have an empty-jmp-to-exit issue that prevents
/// ExitProgram from reaching the exit node (tracked separately).
#[test]
fn test_e2e_exit_noreturn() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        define exit(code: int) : void {
          #entry:
            ret null
        }
        define exit_wrapper() : void {
          #entry:
            n0 = exit(1)
            ret null
        }
        define direct_exit_ok() : void {
          local p: *int
          #entry:
            store &p <- 0 : *int
            n0 = exit(1)
            n1 : *int = load &p
            n2 : int = load n1
            ret null
        }
        define indirect_exit_ok() : void {
          local p: *int
          #entry:
            store &p <- 0 : *int
            n0 = exit_wrapper()
            n1 : *int = load &p
            n2 : int = load n1
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    for name in ["direct_exit_ok", "indirect_exit_ok"] {
        let found = store
            .to_vec()
            .into_iter()
            .find(|(p, _)| format!("{p}").contains(name));
        assert!(
            found.is_some_and(|(_, s)| s.diagnostics.is_empty()),
            "{name} should have no issues after exit/wrapper call"
        );
    }
}

#[test]
fn test_e2e_fopen_null_deref() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        define fopen(path: *void, mode: *void) : *void {
          #entry:
            ret null
        }
        define getc(f: *void) : int {
          #entry:
            ret 0
        }
        define no_fopen_check_bad() : void {
          local f: *void
          #entry:
            n0 = fopen("test", "r")
            store &f <- n0 : *void
            n1 : *void = load &f
            n2 = getc(n1)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let issues: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type())
            .collect();
        eprintln!("  {pname}: {issues:?}");
    }
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("no_fopen_check_bad"));
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE")),
        "no_fopen_check_bad should report NULL_DEREFERENCE"
    );
}

/// Summary comparison: run OCaml and Rust on same C files, compare summaries.
/// Heavy test — spawns infer for each file.
///   cargo test --test end_to_end test_summary_comparison -- --ignored --nocapture
#[test]
#[ignore]
fn test_summary_comparison() {
    use test_harness::infer_runner::InferRunner;
    use test_harness::summary_compare;

    let Some(runner) = InferRunner::new() else {
        eprintln!("skipping: infer binary not found");
        return;
    };

    let c_dir = test_harness::fixtures::ocaml_c_test_dir().join("pulse");
    if !c_dir.exists() {
        eprintln!("skipping: OCaml C test dir not found");
        return;
    }

    // All files with NPE or leak differences plus key files
    let test_files = [
        "nullptr.c",
        "interprocedural.c",
        "exit_example.c",
        "assert.c",
        "angelism.c",
        "nullptr_more.c",
        "traces.c",
        "memory_leak.c",
        "latent.c",
        "funptr.c",
        "initlistexpr.c",
        "compound_literal.c",
        "abduce.c",
        "specialization.c",
        "fopen.c",
    ];

    for filename in test_files {
        let c_path = c_dir.join(filename);
        if !c_path.exists() {
            continue;
        }

        eprintln!("\n--- {filename} ---");

        // 1. Run OCaml pulse and get summaries
        let ocaml_summaries_path = match runner.analyze_pulse_c(&c_path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  OCaml analysis failed: {e}");
                continue;
            }
        };
        let ocaml_facts = summary_compare::parse_ocaml_summaries(&ocaml_summaries_path);

        // 2. Run dump-textual and get the .sil, then run Rust pipeline
        let sil_path = match runner.dump_textual_for_c(&c_path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  dump-textual failed: {e}");
                continue;
            }
        };
        let tm = textual_utils::parse_file_and_convert(&sil_path);
        let checker = PulseInterChecker;
        let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

        // 3. Extract Rust summary facts
        let mut rust_facts = std::collections::HashMap::new();
        for (pname, summary) in store.to_vec() {
            let name = format!("{pname}");
            // Strip any qualifiers to match OCaml's simple name
            let simple_name = name.split('.').last().unwrap_or(&name).to_string();

            let exec_states = if summary.is_noreturn {
                vec!["ExitProgram".to_string()]
            } else if summary.diagnostics.is_empty() {
                vec!["ContinueProgram".to_string()]
            } else {
                vec!["AbortProgram".to_string()]
            };

            // Check if any value in any disjunct's post-state has Invalid attrs
            let has_null_attrs = summary.pre_posts.iter().any(|pp| {
                pp.post
                    .post
                    .attrs
                    .iter()
                    .any(|(_, attrs)| attrs.get_invalid().is_some())
            });

            rust_facts.insert(
                simple_name,
                summary_compare::SummaryFacts::new(
                    summary.pre_posts.len().max(1), // at least 1 disjunct
                    exec_states,
                    has_null_attrs,
                    vec![],
                    summary.is_noreturn,
                ),
            );
        }

        // 4. Compare
        let report = summary_compare::compare_summaries(&ocaml_facts, &rust_facts);
        eprintln!("{report}");
    }
}

/// Test that null attributes propagate through summaries.
/// When callee returns null, caller should see the return value as invalid.
#[test]
fn test_e2e_null_attrs_propagation() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        type cell = { value: int }
        define return_null() : *cell {
          #entry:
            ret null
        }
        define caller_deref_bad() : void {
          #entry:
            n0 = return_null()
            n1 : int = load n0.cell.value
            ret null
        }
        define caller_check_ok() : void {
          #entry:
            n0 = return_null()
            n1 = __sil_ne(n0, 0)
            jmp then_, else_
          #then_:
            prune n1
            n2 : int = load n0.cell.value
            ret null
          #else_:
            prune ! n1
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let issues: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type())
            .collect();
        let null = summary
            .pre_posts
            .iter()
            .any(|pp| pp.result.is_some_and(|rv| pp.post.check_valid(rv).is_err()));
        eprintln!(
            "  {pname}: {issues:?} null_ret={null} disjuncts={}",
            summary.pre_posts.len()
        );
    }

    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("caller_deref_bad"));
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE")),
        "caller_deref_bad should report NULL_DEREFERENCE from callee returning null"
    );
}

/// Test interprocedural path condition: callee exits if x < 0,
/// caller should know ret >= 0 on the surviving path.
#[test]
fn test_e2e_interproc_path_condition() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        declare random() : int
        define exit(code: int) : void {
          #entry:
            ret null
        }
        define return_non_negative() : int {
          #entry:
            n0 = random()
            n1 = __sil_lt(n0, 0)
            jmp then_, else_
          #then_:
            prune n1
            n2 = exit(1)
            ret null
          #else_:
            prune ! n1
            ret n0
        }
        define caller_ok() : void {
          local p: *int
          #entry:
            n0 = return_non_negative()
            n1 = __sil_lt(n0, 0)
            jmp then_, else_
          #then_:
            prune n1
            store &p <- 0 : *int
            n2 : *int = load &p
            n3 : int = load n2
            ret null
          #else_:
            prune ! n1
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let issues: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type())
            .collect();
        eprintln!(
            "  {pname}: {issues:?} noreturn={} disjuncts={}",
            summary.is_noreturn,
            summary.pre_posts.len()
        );
    }
    let caller = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("caller_ok"));
    assert!(
        caller.is_some_and(|(_, s)| s.diagnostics.is_empty()),
        "caller_ok should have no issues (return_non_negative guarantees ret >= 0)"
    );
}

/// Test function pointer dispatch via __call_c_function_ptr + __sil_cfun.
///
/// Tests that the dispatch infrastructure works: __sil_cfun creates Closure
/// attributes, __call_c_function_ptr resolves them, and the callee's summary
/// is applied. The callee returns null directly (not through pointer
/// indirection), which our biabduction handles correctly.
///
/// Note: write-through-pointer patterns (like assign_NULL which does
/// `*ptr = NULL`) don't propagate correctly yet because our biabduction
/// pre-materialization maps formal values to existing heap targets at the
/// wrong indirection level. This is tracked in TODO.md.
#[test]
fn test_e2e_funptr_dispatch() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define return_null() : *int {
          #entry:
            ret null
        }
        define return_one() : *int {
          local x: int
          #entry:
            store &x <- 1:int
            ret &x
        }
        declare __call_c_function_ptr((fun _ -> _)) : *int
        define funptr_deref_bad() : int {
          local funptr: *(fun _ -> _)
          #entry:
            store &funptr <- __sil_cfun("return_null"):*(fun _ -> _)
            n0:*(fun _ -> _) = load &funptr
            n1 = __call_c_function_ptr(n0)
            n2:int = load n1
            ret n2
        }
        define funptr_deref_good() : int {
          local funptr: *(fun _ -> _)
          #entry:
            store &funptr <- __sil_cfun("return_one"):*(fun _ -> _)
            n0:*(fun _ -> _) = load &funptr
            n1 = __call_c_function_ptr(n0)
            n2:int = load n1
            ret n2
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("funptr_deref_bad"));
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE")),
        "funptr_deref_bad should report NULL_DEREFERENCE via function pointer returning null"
    );
    let good = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("funptr_deref_good"));
    assert!(
        good.is_some_and(|(_, s)| s.diagnostics.is_empty()),
        "funptr_deref_good should have no issues"
    );
}

/// Test multi-level function pointer dispatch via recursive specialization.
///
/// call_funptr → __call_c_function_ptr (needs specialization)
/// call_call_funptr → call_funptr (propagates need upward)
/// test_bad → call_call_funptr with known __sil_cfun (triggers specialization chain)
///
/// Verifies the full recursive specialization chain works end-to-end.
#[test]
fn test_e2e_funptr_multilevel() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define return_null() : *int {
          #entry:
            ret null
        }
        declare __call_c_function_ptr((fun _ -> _)) : *int
        define call_funptr(fp: *(fun _ -> _)) : *int {
          #entry:
            n0:*(fun _ -> _) = load &fp
            n1 = __call_c_function_ptr(n0)
            ret n1
        }
        define call_call_funptr(fp: *(fun _ -> _)) : *int {
          #entry:
            n0:*(fun _ -> _) = load &fp
            n1 = call_funptr(n0)
            ret n1
        }
        define test_multilevel_bad() : int {
          #entry:
            n0 = call_call_funptr(__sil_cfun("return_null"))
            n1:int = load n0
            ret n1
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("test_multilevel_bad"));
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE")),
        "test_multilevel_bad should detect NULL_DEREFERENCE through 2-level function pointer chain"
    );
}

/// Test write-through-pointer: callee does *ptr = NULL, caller dereferences ptr.
/// This isolates the biabduction issue from function pointer dispatch.
/// Currently fails: biabduction doesn't propagate write-through-formal-pointer.
#[test]
#[ignore]
fn test_e2e_write_through_ptr() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define assign_NULL(ptr: **int) : void {
          #entry:
            n0 : **int = load &ptr
            store n0 <- 0:*int
            ret null
        }
        define direct_call_bad() : int {
          local ptr: *int, x: int
          #entry:
            store &x <- 0:int
            store &ptr <- &x:*int
            n0 = assign_NULL(&ptr)
            n1:*int = load &ptr
            n2:int = load n1
            ret n2
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("direct_call_bad"));
    let has_npe = bad.as_ref().is_some_and(|(_, s)| {
        s.diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE")
    });
    eprintln!("direct_call_bad: has_npe={has_npe}");
    // This currently fails due to biabduction indirection issue
    // but documents the expected behavior.
    assert!(
        has_npe,
        "direct_call_bad should detect NULL_DEREFERENCE through write-through-pointer"
    );
}

/// Test that __sil_plusa_int (type-suffixed builtin) is properly constant-folded.
/// Without this, `store &foo_g <- __sil_plusa_int(2, 10)` stores an unknown
/// value instead of 12, breaking comparison-based path pruning.
#[test]
fn test_e2e_typed_binop_builtins() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define enum_values_ok() : void {
          local p: *int, foo_g: int
          #entry:
              store &foo_g <- __sil_plusa_int(2, 10):int
              n0:int = load &foo_g
              jmp ne_check, eq_check
          #ne_check:
              prune __sil_ne(n0, 12)
              store &p <- 0:*int
              n2:*int = load &p
              store n2 <- 42:int
              jmp done
          #eq_check:
              prune __sil_lnot(__sil_ne(n0, 12))
              jmp done
          #done:
              ret null
        }
    "#,
    );
    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        let issues: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|d| d.get_issue_type().to_string())
            .collect();
        eprintln!("enum_values_ok: {issues:?}");
        assert!(
            summary.diagnostics.is_empty(),
            "enum_values_ok should have no issues (2+10=12, so 12!=12 is unsat), got: {issues:?}"
        );
    }
}

/// Test unknown call havoc: external C function with &ptr arg should make
/// ptr's value unknown, preventing false null-deref reports.
#[test]
fn test_e2e_unknown_call_havoc() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare external_func(*void) : *void
        define havoc_ok() : int {
          local ptr: *int
          #entry:
            store &ptr <- 0:*int
            n0 = external_func(&ptr)
            n1:*int = load &ptr
            n2:int = load n1
            ret n2
        }
        define havoc_struct_ok() : int {
          local cake: *void
          #entry:
            store &cake <- 0:*void
            n3 = external_func(&cake)
            jmp non_null, is_null
          #is_null:
            prune __sil_eq(n3, 0)
            ret 0
          #non_null:
            prune __sil_lnot(__sil_eq(n3, 0))
            n4:*void = load &cake
            n5:int = load n4
            ret n5
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let name = format!("{pname}");
        let has_npe = summary
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "NULL_DEREFERENCE");
        eprintln!(
            "{name}: has_npe={has_npe}, diags={}",
            summary.diagnostics.len()
        );
        if name.contains("_ok") {
            assert!(
                !has_npe,
                "{name} should NOT detect NULL_DEREFERENCE — external call may have changed ptr"
            );
        }
    }
}

/// Test memory leak detection: malloc without free should report MEMORY_LEAK_C.
#[test]
fn test_e2e_memory_leak() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define malloc_no_free_bad() : void {
          local p: *void
          #entry:
            n0 = malloc(8)
            store &p <- n0:*void
            ret null
        }
        define malloc_then_free_ok() : void {
          local p: *void
          #entry:
            n0 = malloc(8)
            store &p <- n0:*void
            n1:*void = load &p
            n2 = free(n1)
            ret null
        }
        define malloc_returned_ok() : *void {
          local p: *void
          #entry:
            n0 = malloc(8)
            store &p <- n0:*void
            n1:*void = load &p
            ret n1
        }
        define create_p_multinode_ok() : *void {
          local p: *void
          #entry:
            n0 = malloc(8)
            store &p <- n0:*void
            jmp load_ret
          #load_ret:
            n1:*void = load &p
            ret n1
        }
        define malloc_cast_returned_ok() : *int {
          local p: *int
          #entry:
            n0 = malloc(8)
            store &p <- __sil_cast(<*int>, n0):*int
            n1:*int = load &p
            ret n1
        }
    "#,
    );
    for pdesc in tm.cfg.iter_proc_descs() {
        let summary = pulse::checker::analyze(pdesc);
        let name = format!("{}", pdesc.proc_name);
        let has_leak = summary
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type() == "MEMORY_LEAK_C");
        eprintln!(
            "{name}: has_leak={has_leak}, diags={:?}",
            summary
                .diagnostics
                .iter()
                .map(|d| d.get_issue_type())
                .collect::<Vec<_>>()
        );
        if name.contains("_bad") {
            assert!(has_leak, "{name} should report MEMORY_LEAK_C");
        }
        if name.contains("_ok") {
            assert!(!has_leak, "{name} should NOT report MEMORY_LEAK_C");
        }
    }
}

#[test]
fn test_debug_follow_ret() {
    let sil = std::path::Path::new("/tmp/interproc_debug/interprocedural.sil");
    if !sil.exists() {
        eprintln!("skip");
        return;
    }
    let tm = textual_utils::parse_file_and_convert(sil);
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    // Check return_null and return_first summaries
    for (pname, summary) in store.to_vec() {
        let name = format!("{pname}");
        if name.contains("return_null")
            || name.contains("return_first")
            || name.contains("follow_value_by_ret")
        {
            let has_null = summary
                .pre_posts
                .iter()
                .any(|pp| pp.result.is_some_and(|rv| pp.post.check_valid(rv).is_err()));
            let issues: Vec<_> = summary
                .diagnostics
                .iter()
                .map(|d| d.get_issue_type())
                .collect();
            eprintln!(
                "  {name}: disjuncts={} null_ret={has_null} issues={issues:?}",
                summary.pre_posts.len()
            );
        }
    }
}
