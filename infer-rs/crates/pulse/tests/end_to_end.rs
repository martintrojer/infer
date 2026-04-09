// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! End-to-end Pulse tests: Textual .sil → parse → to_sil → Pulse → diagnostics.

use std::sync::{LazyLock, Mutex};

use diagnostics::issue_type::IssueTypeId;
use test_harness::textual_utils;

// The end-to-end test binary exercises shared global analysis state (for
// example thread-local abstract-value allocation and global metadata caches).
// Running its per-procedure analyzers concurrently across unrelated tests is
// currently flaky, so serialize analysis inside this integration binary.
static ANALYZE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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
        let _guard = ANALYZE_LOCK
            .lock()
            .expect("end-to-end analyze lock poisoned");
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
    let mut global_initializers = std::collections::HashSet::new();
    for (_node_id, instr) in pdesc.iter_instrs() {
        collect_cfun_refs(instr, &ctx.summaries, &mut callee_summaries);
        collect_global_initializer_refs(instr, &mut global_initializers);
    }
    for init_pname in global_initializers {
        let Some(init_pdesc) = ctx.cfg.get_proc_desc(&init_pname) else {
            continue;
        };
        let summary = ctx.summaries.get_or_compute(&init_pname, || {
            analyze_with_spec_loop(init_pdesc, ctx, None, depth + 1)
        });
        let mut closure_targets = std::collections::HashSet::new();
        collect_summary_closure_pnames(&summary, &mut closure_targets);
        for closure_pname in closure_targets {
            let Some(closure_pdesc) = ctx.cfg.get_proc_desc(&closure_pname) else {
                continue;
            };
            // Do not block on `get_or_compute` here while we are already inside
            // the end-to-end analyzer lock: another worker may be computing the
            // same summary through `InterChecker::analyze`, which would invert
            // the lock order and deadlock. A direct recursive analysis keeps
            // the harness deterministic without relying on store timing.
            let closure_summary = ctx
                .summaries
                .get(&closure_pname)
                .unwrap_or_else(|| analyze_with_spec_loop(closure_pdesc, ctx, None, depth + 1));
            callee_summaries
                .entry(closure_pname)
                .or_insert(closure_summary);
        }
        callee_summaries.entry(init_pname).or_insert(summary);
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

    let (mut summary, mut spec_requests) = pulse::checker::analyze_with_specialization_and_requests(
        pdesc,
        &callee_summaries,
        specialization,
    );

    if depth >= MAX_SPEC_DEPTH {
        return summary;
    }

    loop {
        let mut added_any = false;
        for (callee_pname, spec) in &spec_requests {
            if callee_summaries
                .get(callee_pname)
                .is_some_and(|summary| summary.get_specialized(spec).is_some())
            {
                continue;
            }
            if let Some(callee_pdesc) = ctx.cfg.get_proc_desc(callee_pname) {
                // RECURSIVE: re-analyze callee with specialization AND the
                // specialization loop, so sub-callees can also be specialized.
                let spec_summary = analyze_with_spec_loop(callee_pdesc, ctx, Some(spec), depth + 1);
                let spec_summary_for_store = spec_summary.clone();
                if let Some(existing) = callee_summaries.get_mut(callee_pname) {
                    existing.add_specialized_summary(spec.clone(), spec_summary);
                    let _ = ctx.summaries.update(callee_pname, |stored| {
                        if stored.get_specialized(spec).is_none() {
                            stored.add_specialized_summary(spec.clone(), spec_summary_for_store);
                        }
                    });
                    added_any = true;
                }
            }
        }
        if !added_any {
            return summary;
        }
        // Re-analyze with the newly added specialized summaries, and collect
        // any follow-up requests from the actual caller states at their calls.
        (summary, spec_requests) = pulse::checker::analyze_with_specialization_and_requests(
            pdesc,
            &callee_summaries,
            specialization,
        );
        if spec_requests.is_empty() {
            return summary;
        }
    }
}

fn run_infer_rs_cli_report(
    sil_path: &std::path::Path,
    source_override: Option<&str>,
    cwd: &std::path::Path,
) -> Result<String, String> {
    let infer_rs_bin = test_harness::infer_runner::find_infer_rs_binary()
        .ok_or_else(|| "could not locate infer-rs binary".to_string())?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock error: {e}"))?
        .as_nanos();
    let out_dir = std::env::temp_dir().join(format!(
        "infer_rs_e2e_cli_{}_{}",
        std::process::id(),
        unique
    ));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("failed to create {}: {e}", out_dir.display()))?;

    let output = {
        let mut cmd = std::process::Command::new(&infer_rs_bin);
        cmd.current_dir(cwd)
            .arg("--pulse-only")
            .arg("--quiet")
            .arg("-o")
            .arg(&out_dir);
        if let Some(source_override) = source_override {
            cmd.arg("--source-override").arg(source_override);
        }
        cmd.arg(sil_path);
        cmd.output()
            .map_err(|e| format!("failed to run infer-rs: {e}"))?
    };

    let exit_code = output.status.code().unwrap_or(-1);
    if !matches!(exit_code, 0 | 2) {
        let _ = std::fs::remove_dir_all(&out_dir);
        return Err(format!(
            "infer-rs exited with {exit_code}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let report_path = out_dir.join("report.json");
    let report = if report_path.exists() {
        std::fs::read_to_string(&report_path)
            .map_err(|e| format!("failed to read {}: {e}", report_path.display()))?
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&out_dir);
    Ok(report)
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

fn collect_global_initializer_refs(
    instr: &sil::instr::Instr,
    out: &mut std::collections::HashSet<sil::procname::Procname>,
) {
    if let sil::instr::Instr::Load {
        e: sil::exp::Exp::Lvar(pvar),
        typ,
        ..
    } = instr
    {
        if pvar.is_global() && is_pointer_to_function_typ(typ) {
            if let Some(init_pname) = pvar.initializer_procname() {
                out.insert(init_pname);
            }
        }
    }
}

fn is_pointer_to_function_typ(typ: &sil::typ::Typ) -> bool {
    matches!(
        &*typ.desc,
        sil::typ::TypeDesc::Tptr(inner, _) if matches!(&*inner.desc, sil::typ::TypeDesc::Tfun(_))
    )
}

fn collect_summary_closure_pnames(
    summary: &pulse::summary::PulseSummary,
    out: &mut std::collections::HashSet<sil::procname::Procname>,
) {
    for pre_post in &summary.pre_posts {
        for (_addr, attrs) in pre_post.post.post.attrs.iter() {
            if let Some(pname) = attrs.get_closure_proc_name() {
                out.insert(pname.clone());
            }
        }
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
#[test]
fn test_e2e_pipeline_smoke() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"

        define null_deref_bad() : void {
          #entry:
            n0 = 0
            store n0 <- 42 : int
            ret null
        }

        define safe_store_ok(x: int) : void {
          #entry:
            store &x <- 5 : int
            ret null
        }
    "#,
    );

    let mut null_deref_issues = 0;
    let mut safe_issues = 0;
    for pdesc in tm.cfg.iter_proc_descs() {
        let proc_name = format!("{}", pdesc.proc_name);
        let summary = pulse::checker::analyze(pdesc);
        let issue_log = pulse::checker::to_issue_log(&summary, &proc_name);

        if proc_name.contains("null_deref_bad") {
            null_deref_issues = issue_log
                .issues
                .iter()
                .filter(|issue| issue.issue_type.id == IssueTypeId::NullptrDereference.id())
                .count();
        } else if proc_name.contains("safe_store_ok") {
            safe_issues = issue_log.len();
        }
    }

    assert_eq!(
        null_deref_issues, 1,
        "inline pipeline should report one NULL_DEREFERENCE"
    );
    assert_eq!(safe_issues, 0, "safe procedure should stay issue-free");
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
            if diag.get_issue_type_id() == IssueTypeId::NullptrDereference {
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
            issues.is_some_and(|v| v.iter().any(|i| i == IssueTypeId::NullptrDereference.id())),
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
            if !issues
                .iter()
                .any(|i| i == IssueTypeId::NullptrDereference.id())
            {
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
                .filter(|e| e.issue_type == IssueTypeId::NullptrDereference.id())
                .count();
            let rust_npe = rust_issues
                .iter()
                .filter(|i| i.as_str() == IssueTypeId::NullptrDereference.id())
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
                .filter(|e| e.issue_type == IssueTypeId::MemoryLeakC.id())
                .count();
            let rust_leak = rust_issues
                .iter()
                .filter(|i| i.as_str() == IssueTypeId::MemoryLeakC.id())
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
fn test_e2e_capture_metadata_noreturn_stub() {
    let mut tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "c"
        define no_return() : void {
          #entry:
            ret null
        }
        define no_return_wrapper() : void {
          #entry:
            n0 = no_return()
            ret null
        }
        define direct_no_return_ok() : void {
          local p: *int
          #entry:
            store &p <- 0 : *int
            n0 = no_return()
            n1 : *int = load &p
            n2 : int = load n1
            ret null
        }
        define indirect_no_return_ok() : void {
          local p: *int
          #entry:
            store &p <- 0 : *int
            n0 = no_return_wrapper()
            n1 : *int = load &p
            n2 : int = load n1
            ret null
        }
    "#,
    );

    tm.cfg
        .proc_descs
        .get_mut(&sil::procname::Procname::c_from_string("no_return"))
        .expect("no_return proc should exist")
        .is_no_return = true;

    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    for name in ["direct_no_return_ok", "indirect_no_return_ok"] {
        let found = store
            .to_vec()
            .into_iter()
            .find(|(p, _)| format!("{p}").contains(name));
        assert!(
            found.is_some_and(|(_, s)| s.diagnostics.is_empty()),
            "{name} should have no issues after metadata-marked noreturn call"
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "caller_deref_bad should report NULL_DEREFERENCE from callee returning null"
    );
}

#[test]
fn test_e2e_callee_local_abort_is_not_republished_on_caller() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        define bake(out: **int) : *int {
          local zero:*int
          #entry:
            store &zero <- 0:*int
            n0:*int = load &zero
            store n0 <- 3:int
            ret null
        }

        define skip_function_with_no_spec_ok() : *int {
          local x:*int
          #entry:
            store &x <- 0:*int
            n0 = bake(&x)
            jmp then_, else_
          #then_:
            prune __sil_eq(n0, 0)
            ret null
          #else_:
            prune __sil_lnot(__sil_eq(n0, 0))
            n1:*int = load &x
            n2:int = load n1
            ret n1
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let bake = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}") == "bake");
    assert!(
        bake.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "bake should report its own manifest NULL_DEREFERENCE"
    );

    let caller = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}") == "skip_function_with_no_spec_ok");
    assert!(
        caller.as_ref().is_some_and(|(_, s)| s.diagnostics.is_empty()),
        "skip_function_with_no_spec_ok should not republish bake's local manifest NULL_DEREFERENCE: {:?}",
        caller.map(|(_, s)| {
            (
                s.diagnostics
                    .iter()
                    .map(|d| (d.get_issue_type_id(), d.get_location().line, format!("{d}")))
                    .collect::<Vec<_>>(),
                s.pre_posts
                    .iter()
                    .map(|pp| format!("{:?}", pp.kind))
                    .collect::<Vec<_>>(),
            )
        })
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

#[test]
fn test_e2e_negated_actual_keeps_arithmetic_latent_summary() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare random() : int
        define exit(code: int) : void {
          #entry:
            ret null
        }
        define assume_non_negative(x: int) : void {
          #entry:
            n0:int = load &x
            jmp then_, else_
          #then_:
            prune __sil_lt(n0, 0)
            _ = exit(1)
            ret null
          #else_:
            prune __sil_lnot(__sil_lt(n0, 0))
            ret null
        }
        define if_negative_then_crash_latent(x: int) : void {
          local p: *int
          #entry:
            n0:int = load &x
            _ = assume_non_negative(__sil_neg(n0))
            store &p <- 0:*int
            n1:*int = load &p
            store n1 <- 42:int
            ret null
        }
        define caller_bad() : void {
          #entry:
            n0 = random()
            _ = if_negative_then_crash_latent(n0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summaries = store.to_vec();

    let latent_summary = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}") == "if_negative_then_crash_latent")
        .map(|(_, summary)| summary)
        .expect("if_negative_then_crash_latent summary missing");
    assert!(
        latent_summary.diagnostics.is_empty(),
        "if_negative_then_crash_latent should stay latent when its imported path condition depends on -x"
    );
    assert!(
        latent_summary.pre_posts.iter().any(|pp| {
            matches!(
                pp.kind,
                pulse::summary::PrePostKind::LatentAbortProgram
                    | pulse::summary::PrePostKind::LatentInvalidAccess
            ) && pp
                .diagnostic
                .as_ref()
                .is_some_and(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)
        }),
        "if_negative_then_crash_latent should export a latent NULL_DEREFERENCE pre/post"
    );

    let caller_summary = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}") == "caller_bad")
        .map(|(_, summary)| summary)
        .expect("caller_bad summary missing");
    assert!(
        caller_summary
            .diagnostics
            .iter()
            .any(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference),
        "caller_bad should report the manifest NULL_DEREFERENCE after applying the latent callee summary"
    );
}

#[test]
fn test_e2e_infer_fail_stub_does_not_force_noreturn_summary() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        declare __infer_fail(int) : void

        define my_assert(x: int) : void {
          #entry:
            n0:int = load &x
            jmp fail, ok
          #fail:
            prune __sil_lnot(n0)
            _ = __infer_fail(0)
            ret null
          #ok:
            prune n0
            ret null
        }

        define should_report_assertion_failure(x: int) : void {
          #entry:
            store &x <- 0:int
            n0:int = load &x
            _ = my_assert(n0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summaries = store.to_vec();

    let assert_summary = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}") == "my_assert")
        .map(|(_, summary)| summary)
        .expect("my_assert summary missing");
    assert_eq!(
        assert_summary.pre_posts.len(),
        2,
        "my_assert should keep both branch summaries when __infer_fail is just a stub"
    );
    assert!(
        assert_summary
            .pre_posts
            .iter()
            .all(|pp| pp.kind == pulse::summary::PrePostKind::ContinueProgram),
        "my_assert should not synthesize ExitProgram pre/posts from __infer_fail"
    );

    let caller_summary = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}") == "should_report_assertion_failure")
        .map(|(_, summary)| summary)
        .expect("should_report_assertion_failure summary missing");
    assert!(
        caller_summary.diagnostics.is_empty(),
        "should_report_assertion_failure should not report a spurious NULL_DEREFERENCE from my_assert"
    );
}

#[test]
fn test_e2e_empty_body_pure_int_call_preserves_integer_reasoning() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare pure_offset() : int
        define impossible_sum_ok() : int {
          local p: *int
          #entry:
            n0 = pure_offset()
            n1 = pure_offset()
            n2 = __sil_plusa_int(n0, n1)
            jmp then_, else_
          #then_:
            prune __sil_eq(n2, 1)
            store &p <- 0:*int
            n3:*int = load &p
            n4:int = load n3
            ret n4
          #else_:
            prune __sil_lnot(__sil_eq(n2, 1))
            ret 0
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summary = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("impossible_sum_ok"))
        .map(|(_, s)| s)
        .expect("impossible_sum_ok summary missing");
    assert!(
        summary.diagnostics.is_empty(),
        "impossible_sum_ok should have no diagnostics: repeated pure int calls imply x + x != 1"
    );
}

#[test]
fn test_e2e_looped_empty_body_pure_int_call_preserves_integer_reasoning() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare pure_offset() : int
        define impossible_loop_sum_ok() : int {
          local i: int, sum: int, p: *int
          #entry:
            store &sum <- 0:int
            store &i <- 0:int
            jmp loop_cond
          #loop_cond:
            n0:int = load &i
            jmp loop_body, loop_exit
          #loop_body:
            prune __sil_lt(n0, 2)
            n1 = pure_offset()
            n2:int = load &sum
            store &sum <- __sil_plusa_int(n2, n1):int
            n3:int = load &i
            store &i <- __sil_plusa_int(n3, 1):int
            jmp loop_cond
          #loop_exit:
            prune __sil_lnot(__sil_lt(n0, 2))
            n4:int = load &sum
            jmp then_, else_
          #then_:
            prune __sil_eq(n4, 1)
            store &p <- 0:*int
            n5:*int = load &p
            n6:int = load n5
            ret n6
          #else_:
            prune __sil_lnot(__sil_eq(n4, 1))
            ret 0
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summary = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("impossible_loop_sum_ok"))
        .map(|(_, s)| s)
        .expect("impossible_loop_sum_ok summary missing");
    assert!(
        summary.diagnostics.is_empty(),
        "impossible_loop_sum_ok should have no diagnostics: looped pure int calls imply x + x != 1"
    );
}

#[test]
fn test_e2e_offsetof_shaped_pure_int_loop_preserves_integer_reasoning() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare __builtin_offsetof() : int
        define impossible_offsetof_like_ok() : int {
          local p: *int, i: int, sum: int
          #entry:
            store &sum <- __sil_cast(<int>, 0):int
            store &i <- 0:int
            jmp loop_cond
          #loop_cond:
            n0:int = load &i
            jmp loop_body, loop_exit
          #loop_body:
            prune __sil_lt(n0, 2)
            n1 = __builtin_offsetof()
            n2:int = load &sum
            store &sum <- __sil_plusa_ulong(n2, n1):int
            n3:int = load &i
            store &i <- __sil_plusa_int(n3, 1):int
            jmp loop_cond
          #loop_exit:
            prune __sil_lnot(__sil_lt(n0, 2))
            n4:int = load &sum
            jmp then_, else_
          #then_:
            prune __sil_eq(n4, __sil_cast(<int>, 1))
            store &p <- 0:*int
            n5:*int = load &p
            n6:int = load n5
            ret n6
          #else_:
            prune __sil_lnot(__sil_eq(n4, __sil_cast(<int>, 1)))
            ret 0
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summary = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("impossible_offsetof_like_ok"))
        .map(|(_, s)| s)
        .expect("impossible_offsetof_like_ok summary missing");
    assert!(
        summary.diagnostics.is_empty(),
        "impossible_offsetof_like_ok should have no diagnostics: exported offsetof shape should still imply x + x != 1"
    );
}

/// Test function pointer dispatch via __call_c_function_ptr + __sil_cfun.
///
/// Tests that the dispatch infrastructure works: __sil_cfun creates Closure
/// attributes, __call_c_function_ptr resolves them, and the callee's summary
/// is applied. The callee returns null directly (not through pointer
/// indirection), which our biabduction handles correctly.
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "test_multilevel_bad should detect NULL_DEREFERENCE through 2-level function pointer chain"
    );
}

/// Regression: if a procedure locally rewrites its own formal slot through a
/// specialized function-pointer call chain, a later dereference of that formal
/// must stay manifest.
#[test]
fn test_e2e_funptr_multilevel_formal_write_stays_manifest() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare __call_c_function_ptr((fun _ -> _), **int) : void
        define assign_NULL(ptr: **int) : void {
          #entry:
            n0:**int = load &ptr
            store n0 <- 0:*int
            ret null
        }
        define call_funptr(fp: *(fun _ -> _), ptr: **int) : void {
          #entry:
            n0:*(fun _ -> _) = load &fp
            n1:**int = load &ptr
            _ = __call_c_function_ptr(n0, n1)
            ret null
        }
        define call_call_funptr(fp: *(fun _ -> _), ptr: **int) : void {
          #entry:
            n0:*(fun _ -> _) = load &fp
            n1:**int = load &ptr
            _ = call_funptr(n0, n1)
            ret null
        }
        define test_bad(ptr: *int) : void {
          #entry:
            _ = call_call_funptr(__sil_cfun("assign_NULL"), &ptr)
            n0:*int = load &ptr
            store n0 <- 42:int
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}") == "test_bad");
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "test_bad should report NULL_DEREFERENCE after assign_NULL rewrites the caller formal through the specialized funptr chain"
    );
}

/// Regression: imported scalar guard conditions must stay attached to the
/// caller's actual scalar, even when the same summary also writes back through
/// a by-ref out parameter.
#[test]
fn test_e2e_guarded_outparam_write_uses_matching_summary_branch() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define maybe_null(flag: int, out: **int) : void {
          #entry:
            n0:int = load &flag
            jmp zero_branch, nonzero_branch
          #zero_branch:
            prune __sil_eq(n0, 0)
            n1:**int = load &out
            store n1 <- 0:*int
            ret null
          #nonzero_branch:
            prune __sil_lnot(__sil_eq(n0, 0))
            ret null
        }
        define caller_bad() : int {
          local flag:int, x:int, p:*int
          #entry:
            store &flag <- 0:int
            store &x <- 1:int
            store &p <- &x:*int
            n0:int = load &flag
            _ = maybe_null(n0, &p)
            n1:*int = load &p
            n2:int = load n1
            ret n2
        }
        define caller_ok() : int {
          local flag:int, x:int, p:*int
          #entry:
            store &flag <- 1:int
            store &x <- 1:int
            store &p <- &x:*int
            n0:int = load &flag
            _ = maybe_null(n0, &p)
            n1:*int = load &p
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
        .find(|(p, _)| format!("{p}") == "caller_bad");
    assert!(
        bad.is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "caller_bad should report NULL_DEREFERENCE after the flag == 0 summary branch nulls the out param"
    );

    let ok = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}") == "caller_ok");
    assert!(
        ok.is_some_and(|(_, s)| s.diagnostics.is_empty()),
        "caller_ok should keep the non-null pointer when the flag != 0 summary branch applies"
    );
}

/// Test write-through-pointer: callee does *ptr = NULL, caller dereferences ptr.
/// This isolates the heap-effect propagation from function-pointer
/// specialization.
#[test]
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)
    });
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference);
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

#[test]
fn test_e2e_unknown_call_havoc_on_by_ref_formal_slot() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare external_slot(**int) : *void
        define by_ref_slot_ok(param: *int) : void {
          #entry:
            n0 = external_slot(&param)
            n1:*int = load &param
            n2:int = load n1
            ret null
        }
        define by_ref_slot_bad(param: *int) : void {
          #entry:
            n0:*int = load &param
            n1:int = load n0
            n2 = external_slot(&param)
            ret null
        }
        define caller_ok() : void {
          #entry:
            n0 = by_ref_slot_ok(0)
            ret null
        }
        define main() : void {
          #entry:
            n0 = by_ref_slot_bad(0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let caller_ok = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}").contains("caller_ok"));
    assert!(
        caller_ok.as_ref().is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .all(|d| d.get_issue_type_id() != IssueTypeId::NullptrDereference)),
        "caller_ok should stay clean after an unknown call on `&param`"
    );

    let main_bad = store
        .to_vec()
        .into_iter()
        .find(|(p, _)| format!("{p}") == "main");
    assert!(
        main_bad.as_ref().is_some_and(|(_, s)| s
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "main should still report the null dereference that happens before the unknown call"
    );
}

#[test]
fn test_e2e_imported_pure_call_condition_keeps_precondition_violation_latent() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        declare unknown(int) : int
        define unknown_conditional_dereference(x: int, p: *int) : void {
          #entry:
            n0:int = load &x
            n1 = unknown(n0)
            jmp then_, else_
          #then_:
            prune __sil_eq(n1, 999)
            n2:*int = load &p
            store n2 <- 42:int
            ret null
          #else_:
            prune __sil_lnot(__sil_eq(n1, 999))
            ret null
        }
        define unknown_from_parameters_latent(x: int) : void {
          #entry:
            n0:int = load &x
            _ = unknown_conditional_dereference(n0, 0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}").contains("unknown_from_parameters_latent"))
        .map(|(_, summary)| summary)
        .expect("unknown_from_parameters_latent summary should exist");

    assert!(
        summary.diagnostics.is_empty(),
        "caller-dependent precondition violation should stay latent, not manifest"
    );
    assert!(
        summary.pre_posts.iter().any(|pp| {
            matches!(pp.kind, pulse::summary::PrePostKind::LatentAbortProgram)
                && pp
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)
        }),
        "summary should keep a latent abort pre/post for the imported pure-call condition"
    );
}

#[test]
fn test_e2e_cyclic_field_write_reifies_latent_abort_in_caller() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        type node = {next: *node}
        define traverse_one_step_and_crash_if_equal_to_root(p: *node) : void {
          local old_p: *node, crash: *int
          #entry:
            n0:*node = load &p
            store &old_p <- n0:*node
            n1:*node = load &p
            n2:*node = load n1.node.next
            store &p <- n2:*node
            n3:*node = load &old_p
            n4:*node = load &p
            jmp equal, notequal
          #equal:
            prune __sil_eq(n3, n4)
            store &crash <- 0:*int
            n5:*int = load &crash
            store n5 <- 42:int
            ret null
          #notequal:
            prune __sil_lnot(__sil_eq(n3, n4))
            ret null
        }
        define crash_after_one_node_bad(q: *node) : void {
          #entry:
            n0:*node = load &q
            n1:*node = load &q
            store n1.node.next <- n0:*node
            _ = traverse_one_step_and_crash_if_equal_to_root(n0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let traverse = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}").contains("traverse_one_step"))
        .map(|(_, summary)| summary)
        .expect("traverse summary should exist");
    assert!(
        traverse.diagnostics.is_empty(),
        "callee should keep the null dereference latent before the caller shape is known"
    );

    let caller = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}").contains("crash_after_one_node_bad"))
        .map(|(_, summary)| summary)
        .expect("caller summary should exist");
    assert!(
        caller
            .pre_posts
            .iter()
            .any(|pp| pp.kind == pulse::summary::PrePostKind::AbortProgram),
        "caller should reify the latent abort once it writes the one-node cycle"
    );
    assert!(
        caller
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference),
        "caller should publish the reified null dereference"
    );
}

#[test]
fn test_e2e_two_hop_field_write_keeps_null_derefs_latent() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        type node = {next: *node}

        define crash_after_two_hops_latent(q: *node) : void {
          #entry:
            n0:*node = load &q
            n1:*node = load n0.node.next
            store n1.node.next <- n0:*node
            jmp abort
          #abort:
            store 0 <- 1:int
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}") == "crash_after_two_hops_latent")
        .map(|(_, summary)| summary)
        .expect("summary should exist");

    let latent_null_derefs = summary
        .pre_posts
        .iter()
        .filter(|pp| {
            pp.kind == pulse::summary::PrePostKind::LatentInvalidAccess
                && pp
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)
        })
        .count();
    assert_eq!(
        latent_null_derefs, 2,
        "expected one latent null deref for `q` and one for `q->next` once the field write reaches its own CFG node"
    );
    assert!(
        summary
            .diagnostics
            .iter()
            .any(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference),
        "expected the trailing local null abort to stay manifest"
    );
}

#[test]
fn test_e2e_latent_chain_stays_latent_until_manifest_callsite() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"
        define latent_dereference(a: int, p: *int) : void {
          #entry:
            n0:int = load &a
            jmp then_, else_
          #then_:
            prune __sil_eq(n0, 4)
            n1:*int = load &p
            store n1 <- 42:int
            ret null
          #else_:
            prune __sil_lnot(__sil_eq(n0, 4))
            ret null
        }
        define propagate_latent_1_latent(a1: int) : void {
          #entry:
            n0:int = load &a1
            _ = latent_dereference(n0, 0)
            ret null
        }
        define propagate_latent_2_latent(a2: int) : void {
          #entry:
            n0:int = load &a2
            _ = propagate_latent_1_latent(n0)
            ret null
        }
        define propagate_latent_3_latent(a3: int) : void {
          #entry:
            n0:int = load &a3
            _ = propagate_latent_2_latent(n0)
            ret null
        }
        define make_latent_manifest() : void {
          #entry:
            _ = propagate_latent_3_latent(4)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summaries = store.to_vec();

    let direct = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}").contains("latent_dereference"))
        .map(|(_, summary)| summary)
        .expect("latent_dereference summary should exist");
    assert!(
        direct.diagnostics.is_empty(),
        "latent_dereference should export only preconditions, not a manifest report"
    );

    for proc_name in [
        "propagate_latent_1_latent",
        "propagate_latent_2_latent",
        "propagate_latent_3_latent",
    ] {
        let summary = summaries
            .iter()
            .find(|(pname, _)| format!("{pname}").contains(proc_name))
            .map(|(_, summary)| summary)
            .unwrap_or_else(|| panic!("{proc_name} summary should exist"));
        let kinds: Vec<_> = summary
            .pre_posts
            .iter()
            .map(|pp| format!("{:?}", pp.kind))
            .collect();
        let conditions: Vec<_> = summary
            .pre_posts
            .iter()
            .map(|pp| format!("{:?}", pp.post.path_condition.conditions()))
            .collect();

        assert!(
            summary.diagnostics.is_empty(),
            "{proc_name} should stay latent, got diagnostics={:?} kinds={kinds:?} conditions={conditions:?}",
            summary
                .diagnostics
                .iter()
                .map(|d| d.get_issue_type())
                .collect::<Vec<_>>()
        );
        assert!(
            summary.pre_posts.iter().any(|pp| {
                matches!(
                    pp.kind,
                    pulse::summary::PrePostKind::LatentAbortProgram
                        | pulse::summary::PrePostKind::LatentInvalidAccess
                )
            }),
            "{proc_name} should keep a latent pre/post, got kinds={kinds:?} conditions={conditions:?}"
        );
    }

    let manifest = summaries
        .iter()
        .find(|(pname, _)| format!("{pname}").contains("make_latent_manifest"))
        .map(|(_, summary)| summary)
        .expect("make_latent_manifest summary should exist");
    assert!(
        manifest
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference),
        "make_latent_manifest should publish the reified null dereference"
    );
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
            .any(|d| d.get_issue_type_id() == IssueTypeId::MemoryLeakC);
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

/// Regression: duplicated null-deref reports in `memory_leak.c` must survive
/// dedup when they have different provenance histories.
///
/// Run explicitly:
///   cargo test -p pulse --test end_to_end test_e2e_memory_leak_realloc_reports_both_null_origins -- --ignored --nocapture
#[test]
#[ignore]
fn test_e2e_memory_leak_realloc_reports_both_null_origins() {
    use test_harness::infer_runner::InferRunner;

    let Some(runner) = InferRunner::new() else {
        eprintln!("skipping: infer binary not found");
        return;
    };

    let c_dir = test_harness::fixtures::ocaml_c_test_dir().join("pulse");
    let c_path = c_dir.join("memory_leak.c");
    if !c_path.exists() {
        eprintln!("skipping: memory_leak.c not found");
        return;
    }

    let sil_path = runner
        .dump_textual_for_c(&c_path)
        .expect("dump-textual should succeed for memory_leak.c");
    let report = run_infer_rs_cli_report(&sil_path, Some("memory_leak.c"), &c_dir)
        .expect("infer-rs CLI should analyze memory_leak.c");
    let proc_marker = "\"procedure\": \"realloc_no_check_bad\"";
    let first_qualifier =
        "\"qualifier\": \"address could be null (null value originating from line 105) and is dereferenced\"";
    let second_qualifier =
        "\"qualifier\": \"address could be null (null value originating from line 119) and is dereferenced\"";

    assert_eq!(
        report.matches(proc_marker).count(),
        2,
        "realloc_no_check_bad should report both null origins, got:\n{report}"
    );
    assert!(
        report.matches(first_qualifier).count() == 1,
        "missing first null origin in report:\n{report}"
    );
    assert!(
        report.matches(second_qualifier).count() == 1,
        "missing second null origin in report:\n{report}"
    );
}

/// Regression: keep reporting the real null dereference in
/// `FN_nullptr_deref_old_bad`, even though OCaml's `issues.exp` intentionally
/// omits it as a known false negative caused by recency forgetting.
///
/// Run explicitly:
///   cargo test -p pulse --test end_to_end test_e2e_nullptr_old_vector_element_is_still_tracked -- --ignored --nocapture
#[test]
#[ignore]
fn test_e2e_nullptr_old_vector_element_is_still_tracked() {
    use test_harness::infer_runner::InferRunner;

    let Some(runner) = InferRunner::new() else {
        eprintln!("skipping: infer binary not found");
        return;
    };

    let c_dir = test_harness::fixtures::ocaml_c_test_dir().join("pulse");
    let c_path = c_dir.join("nullptr.c");
    if !c_path.exists() {
        eprintln!("skipping: nullptr.c not found");
        return;
    }

    let sil_path = runner
        .dump_textual_for_c(&c_path)
        .expect("dump-textual should succeed for nullptr.c");
    let report = run_infer_rs_cli_report(&sil_path, Some("nullptr.c"), &c_dir)
        .expect("infer-rs CLI should analyze nullptr.c");

    let proc_marker = "\"procedure\": \"FN_nullptr_deref_old_bad\"";
    let qualifier =
        "\"qualifier\": \"address could be null (null value originating from line 72) and is dereferenced\"";

    assert_eq!(
        report.matches(proc_marker).count(),
        1,
        "FN_nullptr_deref_old_bad should stay reported as a real bug, got:\n{report}"
    );
    assert!(
        report.matches(qualifier).count() == 1,
        "missing expected old-element null-deref qualifier:\n{report}"
    );
}

#[test]
fn test_e2e_global_function_pointer_initializer_is_inlined() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        declare __call_c_function_ptr((fun _ -> _)) : *int

        global fp: void

        define return_null() : *int {
          #entry:
            ret 0
        }

        define __infer_globals_initializer_fp() : void {
          #entry:
            _ = __sil_metadata_variable_lifetime_begins(&fp, <*(fun _ -> _)>)
            store &fp <- __sil_cfun("return_null"):*(fun _ -> _)
            ret null
        }

        define call_via_global_bad() : void {
          #entry:
            n0:*(fun _ -> _) = load &fp
            n1 = __call_c_function_ptr(n0)
            n2:int = load n1
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let bad = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}").contains("call_via_global_bad"))
        .expect("call_via_global_bad summary should exist");
    assert!(
        bad.1
            .diagnostics
            .iter()
            .any(|d| d.get_issue_type_id() == IssueTypeId::NullptrDereference),
        "global function-pointer initializer should be visible before loading fp"
    );
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
    // Dump selected summaries for local debugging of latent/reporting parity.
    for (pname, summary) in store.to_vec() {
        let name = format!("{pname}");
        if name.contains("return_null")
            || name.contains("return_first")
            || name.contains("follow_value_by_ret")
            || name.contains("conditional_free_then_use")
            || name.contains("latent_dereference")
            || name.contains("propagate_latent")
            || name.contains("make_latent_manifest")
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
            let kinds: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.kind))
                .collect();
            let conditions: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.post.path_condition.conditions()))
                .collect();
            eprintln!(
                "  {name}: disjuncts={} kinds={kinds:?} conditions={conditions:?} null_ret={has_null} issues={issues:?}",
                summary.pre_posts.len(),
            );
        }
    }
}

#[test]
fn test_e2e_manifest_use_after_free_reports_only_uaf() {
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

        define manifest_use_after_free(x: *int) : void {
          #entry:
            n0:*int = load &x
            _ = latent_use_after_free(1, n0)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}") == "manifest_use_after_free")
        .map(|(_, summary)| summary)
        .expect("manifest_use_after_free summary should exist");
    let issue_types: Vec<_> = summary
        .diagnostics
        .iter()
        .map(|diag| diag.get_issue_type_id())
        .collect();

    assert!(
        issue_types.contains(&IssueTypeId::UseAfterFree),
        "expected manifest USE_AFTER_FREE, found {issue_types:?}"
    );
    assert!(
        !issue_types.contains(&IssueTypeId::NullptrDereference),
        "manifest_use_after_free should not publish a manifest NULL_DEREFERENCE, found {issue_types:?}"
    );
}

#[test]
fn test_e2e_local_zero_proof_on_formal_keeps_null_deref_manifest() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        define create_null_path_ok(p: *int) : void {
          #entry:
            n0:*int = load &p
            jmp is_null, nonnull
          #is_null:
            prune __sil_eq(n0, 0)
            ret null
          #nonnull:
            prune __sil_lnot(__sil_eq(n0, 0))
            store n0 <- 32:int
            ret null
        }

        define create_null_path2_bad_FN(p: *int) : void {
          #entry:
            n0:*int = load &p
            jmp is_null, nonnull
          #is_null:
            prune __sil_eq(n0, 0)
            n1:*int = load &p
            store n1 <- 52:int
            ret null
          #nonnull:
            prune __sil_lnot(__sil_eq(n0, 0))
            store n0 <- 32:int
            n2:*int = load &p
            store n2 <- 52:int
            ret null
        }

        define malloc_then_call_create_null_path_then_deref_unconditionally_bad_FN(p: *int) : void {
          local x:*int
          #entry:
            n0 = malloc(4)
            store &x <- n0
            n1:*int = load &p
            jmp is_null, nonnull
          #is_null:
            prune __sil_eq(n1, 0)
            _ = create_null_path_ok(n1)
            n3:*int = load &p
            store n3 <- 52:int
            n4:*int = load &x
            _ = free(n4)
            ret null
          #nonnull:
            prune __sil_lnot(__sil_eq(n1, 0))
            store n1 <- 32:int
            _ = create_null_path_ok(n1)
            n6:*int = load &p
            store n6 <- 52:int
            n7:*int = load &x
            _ = free(n7)
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    let summaries = store.to_vec();

    for proc_name in [
        "create_null_path2_bad_FN",
        "malloc_then_call_create_null_path_then_deref_unconditionally_bad_FN",
    ] {
        let is_manifest_invalid_access = |issue_type: IssueTypeId| {
            matches!(
                issue_type,
                IssueTypeId::NullptrDereference | IssueTypeId::ComparedToNullAndDereferenced
            )
        };
        let summary = summaries
            .iter()
            .find(|(pname, _)| format!("{pname}") == proc_name)
            .map(|(_, summary)| summary)
            .expect("summary should exist");
        let issue_types: Vec<_> = summary
            .diagnostics
            .iter()
            .map(|diag| diag.get_issue_type_id())
            .collect();
        let kinds: Vec<_> = summary
            .pre_posts
            .iter()
            .map(|pp| format!("{:?}", pp.kind))
            .collect();
        let conditions: Vec<_> = summary
            .pre_posts
            .iter()
            .map(|pp| format!("{:?}", pp.post.path_condition.conditions()))
            .collect();

        assert!(
            summary
                .diagnostics
                .iter()
                .any(|diag| is_manifest_invalid_access(diag.get_issue_type_id())),
            "{proc_name} should keep its locally-proven direct-formal null dereference manifest; issue_types={issue_types:?} kinds={kinds:?} conditions={conditions:?}"
        );
        assert!(
            summary
                .pre_posts
                .iter()
                .any(|pp| pp.kind == pulse::summary::PrePostKind::AbortProgram
                    && pp.diagnostic.as_ref().is_some_and(
                        |diag| is_manifest_invalid_access(diag.get_issue_type_id())
                    )),
            "{proc_name} should export an AbortProgram summary for the null dereference; issue_types={issue_types:?} kinds={kinds:?} conditions={conditions:?}"
        );
    }
}

#[test]
fn test_e2e_deref_then_free_then_deref_keeps_npe_latent() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        define deref_then_free_then_deref_bad(x: *int) : void {
          #entry:
            n0:*int = load &x
            store n0 <- 42:int
            _ = free(n0)
            n1:*int = load &x
            store n1 <- 42:int
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}") == "deref_then_free_then_deref_bad")
        .map(|(_, summary)| summary)
        .expect("deref_then_free_then_deref_bad summary should exist");
    let issue_types: Vec<_> = summary
        .diagnostics
        .iter()
        .map(|diag| diag.get_issue_type_id())
        .collect();

    assert!(
        issue_types.contains(&IssueTypeId::UseAfterFree),
        "expected manifest USE_AFTER_FREE, found {issue_types:?}"
    );
    assert!(
        !issue_types.contains(&IssueTypeId::NullptrDereference),
        "write-through on the pointee should not make the direct-formal null deref manifest: {issue_types:?}"
    );
    assert!(
        summary.pre_posts.iter().any(|pp| pp.kind
            == pulse::summary::PrePostKind::LatentInvalidAccess
            && pp
                .diagnostic
                .as_ref()
                .is_some_and(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)),
        "expected a latent NULL_DEREFERENCE pre/post to stay in the summary"
    );
}

#[test]
fn test_e2e_latent_error_only_summary_is_not_noreturn() {
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
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}") == "latent_use_after_free")
        .map(|(_, summary)| summary)
        .expect("latent_use_after_free summary should exist");

    assert!(
        !summary.is_noreturn,
        "latent/error-only summaries still need normal summary application at callers"
    );
    assert!(
        summary
            .pre_posts
            .iter()
            .any(|pp| matches!(pp.kind, pulse::summary::PrePostKind::LatentAbortProgram)),
        "latent_use_after_free should keep its latent UAF summary path"
    );
}

#[test]
fn test_e2e_access_use_after_free_keeps_manifest_uaf_and_suppressed_npes() {
    let tm = textual_utils::parse_and_convert(
        r#"
        .source_language = "C"

        type list = { next: *list; data: int }

        define access_use_after_free_bad(l: *list) : void {
          #entry:
            n0:*list = load &l
            n1:*list = load n0.list.next
            _ = free(n1)
            n2:*list = load n0.list.next
            store n2.list.next <- 0:*list
            ret null
        }
    "#,
    );
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);

    let summary = store
        .to_vec()
        .into_iter()
        .find(|(pname, _)| format!("{pname}") == "access_use_after_free_bad")
        .map(|(_, summary)| summary)
        .expect("access_use_after_free_bad summary should exist");

    let issue_types: Vec<_> = summary
        .diagnostics
        .iter()
        .map(|diag| diag.get_issue_type_id())
        .collect();

    assert!(
        issue_types.contains(&IssueTypeId::UseAfterFree),
        "expected manifest USE_AFTER_FREE, found {issue_types:?}"
    );
    assert!(
        issue_types.contains(&IssueTypeId::NullptrDereference),
        "expected the locally-proven null branch to stay manifest and suppressed, found {issue_types:?}"
    );
    assert!(
        summary.pre_posts.iter().any(|pp| {
            pp.kind == pulse::summary::PrePostKind::AbortProgram
                && pp
                    .diagnostic
                    .as_ref()
                    .is_some_and(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)
        }),
        "expected an AbortProgram NULL_DEREFERENCE pre/post to remain in the summary"
    );
    assert!(
        summary
            .diagnostics
            .iter()
            .find(|diag| diag.get_issue_type_id() == IssueTypeId::NullptrDereference)
            .is_some_and(pulse::diagnostic::Diagnostic::is_suppressed),
        "expected the manifest NULL_DEREFERENCE to stay suppressed in summary diagnostics"
    );
}

#[test]
fn test_debug_latent_summary() {
    let sil = std::path::Path::new("/tmp/interproc_debug/latent.sil");
    if !sil.exists() {
        eprintln!("skip");
        return;
    }
    let tm = textual_utils::parse_file_and_convert(sil);
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let name = format!("{pname}");
        if name.contains("latent")
            || name.contains("manifest_use_after_free")
            || name.contains("conditional_free2")
            || name.contains("access_use_after_free_bad")
            || name.contains("deref_then_free_then_deref")
            || name.contains("traverse_and_crash")
            || name.contains("crash_after")
            || name.contains("main")
        {
            let issues: Vec<_> = summary
                .diagnostics
                .iter()
                .map(|d| d.get_issue_type())
                .collect();
            let kinds: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.kind))
                .collect();
            let conditions: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.post.path_condition.conditions()))
                .collect();
            eprintln!(
                "  {name}: disjuncts={} kinds={kinds:?} conditions={conditions:?} issues={issues:?}",
                summary.pre_posts.len(),
            );
            for (i, pp) in summary.pre_posts.iter().enumerate() {
                let formals: Vec<_> = pp
                    .formals
                    .iter()
                    .map(|(pvar, addr)| format!("{pvar}->{addr}"))
                    .collect();
                let invalid_attrs: Vec<_> = pp
                    .post
                    .post
                    .attrs
                    .iter()
                    .filter_map(|(addr, attrs)| {
                        attrs.get_invalid().map(|(inv, _)| format!("{addr}:{inv}"))
                    })
                    .collect();
                if let Some(diag) = &pp.diagnostic {
                    let (diag_addr, access_history) = match diag {
                        pulse::diagnostic::Diagnostic::AccessToInvalidAddress {
                            addr,
                            access_history,
                            ..
                        } => (format!("{addr}"), access_history.signature()),
                        _ => ("-".to_string(), "-".to_string()),
                    };
                    let must_be_valid: Vec<_> = pp
                        .post
                        .must_be_valid
                        .iter()
                        .map(ToString::to_string)
                        .collect();
                    eprintln!(
                        "    pp[{i}] kind={:?} diag={} addr={} must_be_valid={must_be_valid:?} access_history={access_history} formals={formals:?} invalid_attrs={invalid_attrs:?}",
                        pp.kind,
                        diag.get_issue_type(),
                        diag_addr,
                    );
                } else if !invalid_attrs.is_empty() {
                    eprintln!(
                        "    pp[{i}] kind={:?} diag=- addr=- formals={formals:?} invalid_attrs={invalid_attrs:?}",
                        pp.kind,
                    );
                }
            }
        }
    }
}

#[test]
fn test_debug_specialization_summary() {
    let sil = std::path::Path::new("/tmp/interproc_debug/specialization.sil");
    if !sil.exists() {
        eprintln!("skip");
        return;
    }
    let tm = textual_utils::parse_file_and_convert(sil);
    let checker = PulseInterChecker;
    let (store, _) = ondemand::runner::run_inter(&checker, &tm.cfg, &tm.tenv);
    for (pname, summary) in store.to_vec() {
        let name = format!("{pname}");
        if name.contains("test_alias")
            || name.contains("test_unalias")
            || name.contains("call_test_alias")
            || name.contains("call_test_unalias")
            || name.contains("may_double_free")
            || name.contains("call_may_double_free")
        {
            let issues: Vec<_> = summary
                .diagnostics
                .iter()
                .map(|d| d.get_issue_type())
                .collect();
            let specs: Vec<_> = summary
                .specialized
                .iter()
                .map(|(spec, pps)| format!("{spec} disjuncts={}", pps.len()))
                .collect();
            let kinds: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.kind))
                .collect();
            let conditions: Vec<_> = summary
                .pre_posts
                .iter()
                .map(|pp| format!("{:?}", pp.post.path_condition.conditions()))
                .collect();
            eprintln!(
                "  {name}: issues={issues:?} kinds={kinds:?} conditions={conditions:?} specialized={specs:?}"
            );
        }
    }
}

/// Sweep using --store-textual + --export-textual: batch capture all C files,
/// export via manifest, analyze each .sil with line map remapping.
///
/// Run explicitly:
///   cargo test --test end_to_end test_store_textual_sweep -- --ignored --nocapture
#[test]
#[ignore]
fn test_store_textual_sweep() {
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

    let mut entries: Vec<_> = std::fs::read_dir(&c_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "c"))
        .collect();
    entries.sort_by_key(|e| e.file_name());
    let c_paths: Vec<_> = entries.iter().map(|e| e.path()).collect();
    let c_refs: Vec<&std::path::Path> = c_paths.iter().map(|p| p.as_path()).collect();

    eprintln!("Capturing {} C files with --store-textual...", c_refs.len());
    let (manifest_entries, export_dir, results_dir) = match runner.store_textual_and_export(&c_refs)
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("FAIL: store_textual_and_export: {e}");
            panic!("store_textual_and_export failed");
        }
    };
    eprintln!(
        "Exported {} files to {}",
        manifest_entries.len(),
        export_dir.display()
    );

    // Files known to hang (infinite loops / deep recursion exhaust fixpoint)
    let skip_files = ["infinite.c", "recursion.c", "recursion2.c"];

    let mut ok = 0;
    let mut fail_analyze = 0;
    let mut fail_timeout = 0;
    let mut total_procs = 0;
    let mut total_issues = 0;
    let mut file_results: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for entry in &manifest_entries {
        let source_name = std::path::Path::new(&entry.source)
            .file_name()
            .unwrap_or_default()
            .to_str()
            .unwrap_or("");

        if skip_files.iter().any(|s| *s == source_name) {
            eprintln!("  SKIP {source_name} (known hang)");
            continue;
        }

        let sil_path = export_dir.join(&entry.sil);
        let source_file = entry.source.clone();
        let source_dir = std::path::Path::new(&source_file)
            .parent()
            .unwrap_or(c_dir.as_path())
            .to_path_buf();
        let infer_results_dir = results_dir.clone();
        let proc_count = entry.procedures.len();

        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let issues = test_harness::infer_runner::run_infer_rs_on_textual(
                &sil_path,
                Some(&source_file),
                &source_dir,
                Some(&infer_results_dir),
                true,
            );
            let _ = tx.send((proc_count, issues));
        });

        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok((n_procs, Ok(issues))) => {
                handle.join().ok();
                let n_issues = issues.len();
                eprintln!("  OK {source_name}: {n_procs} procs, {n_issues} issues");
                ok += 1;
                total_procs += n_procs;
                total_issues += n_issues;
                file_results.insert(source_name.to_string(), issues);
            }
            Ok((_n_procs, Err(e))) => {
                handle.join().ok();
                eprintln!("  FAIL_ANALYZE {source_name}: {e}");
                fail_analyze += 1;
            }
            Err(_) => {
                eprintln!("  TIMEOUT {source_name}");
                fail_timeout += 1;
            }
        }
    }

    // Compare against issues.exp
    let exp_path = c_dir.join("issues.exp");
    if exp_path.exists() {
        let expected = test_harness::fixtures::parse_issues_exp(&exp_path);

        for (type_id, label) in [
            (IssueTypeId::NullptrDereference, "NPE"),
            (IssueTypeId::MemoryLeakC, "LEAK"),
            (IssueTypeId::UseAfterFree, "UAF"),
        ] {
            let id_str = type_id.id();
            let mut total_expected = 0;
            let mut total_found = 0;
            let mut diffs = Vec::new();
            for (filename, rust_issues) in &file_results {
                let exp = test_harness::fixtures::issues_for_file(&expected, filename);
                let exp_count = exp.iter().filter(|e| e.issue_type == id_str).count();
                let rust_count = rust_issues.iter().filter(|i| i.as_str() == id_str).count();
                total_expected += exp_count;
                total_found += rust_count;
                if exp_count != rust_count {
                    diffs.push(format!(
                        "    {filename}: expected {exp_count}, found {rust_count}"
                    ));
                }
            }
            eprintln!("\n=== {label}: expected {total_expected}, found {total_found} ===");
            if !diffs.is_empty() {
                diffs.sort();
                eprintln!("  Differences:");
                for d in &diffs {
                    eprintln!("{d}");
                }
            }
        }
    }

    eprintln!("\n=== Store-textual sweep ===");
    eprintln!("  OK: {ok}, FAIL_ANALYZE: {fail_analyze}, TIMEOUT: {fail_timeout}");
    eprintln!("  {total_procs} procs analyzed, {total_issues} issues found");
    assert!(ok > 0, "should have at least one passing file");
}
