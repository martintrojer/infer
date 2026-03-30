// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! infer-rs CLI: Rust implementation of the Infer static analyzer.
//!
//! Two modes of operation:
//!
//! 1. Full pipeline (capture + analyze):
//!    `infer-rs --pulse-only -- clang -c file.c`
//!    Shells out to `infer --store-textual`, exports textual, analyzes.
//!
//! 2. Analyze only (capture.db already exists):
//!    `infer-rs --pulse-only`
//!    Exports textual from existing capture.db, analyzes.
//!
//! 3. Direct .sil files (debugging):
//!    `infer-rs --pulse-only file.sil`
//!    Analyzes the given .sil files directly.

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use diagnostics::issue::IssueLog;

/// infer-rs: Rust implementation of the Infer static analyzer.
///
/// Run with `-- <build command>` to capture and analyze, or without to
/// analyze an existing capture.db. Pass .sil files directly for debugging.
#[derive(Parser, Debug)]
#[command(name = "infer-rs", version, about)]
struct Cli {
    /// .sil files to analyze directly (bypasses capture/export).
    #[arg(long = "capture-textual", value_name = "FILE")]
    capture_textual: Vec<PathBuf>,

    /// Positional args: .sil files, or after `--` the build command.
    #[arg(value_name = "ARG", trailing_var_arg = true)]
    args: Vec<String>,

    /// Run only the Pulse checker (null deref, use-after-free).
    #[arg(long)]
    pulse_only: bool,

    /// Run only the liveness checker (dead stores).
    #[arg(long)]
    liveness_only: bool,

    /// Output directory for report.json and exported textual.
    #[arg(short = 'o', long = "output", default_value = "infer-rs-out")]
    out_dir: PathBuf,

    /// Suppress progress output.
    #[arg(short = 'q', long)]
    quiet: bool,

    /// Maximum number of disjuncts per program point (default: 20).
    #[arg(long = "pulse-max-disjuncts")]
    pulse_max_disjuncts: Option<usize>,

    /// Disable inter-procedural analysis in Pulse.
    #[arg(long = "pulse-intraprocedural-only")]
    pulse_intraprocedural_only: bool,

    /// Maximum widenings before fixpoint gives up (default: 10000).
    #[arg(long = "max-widens")]
    max_widens: Option<usize>,

    /// Number of parallel analysis jobs (default: number of CPUs).
    #[arg(short = 'j', long = "jobs")]
    jobs: Option<usize>,

    /// Analysis debug level: 0=quiet, 1=per-instruction, 2=full state dumps.
    /// Matches OCaml's --debug-level-analysis. Also controlled via RUST_LOG env.
    #[arg(long = "debug-level-analysis", default_value = "0")]
    debug_level_analysis: u8,

    /// Path to .inferconfig file (default: search upward from CWD).
    #[arg(long = "inferconfig-path")]
    inferconfig_path: Option<PathBuf>,

    /// Path to infer binary (default: auto-detect).
    #[arg(long = "infer-bin")]
    infer_bin: Option<PathBuf>,

    /// Infer results directory (default: infer-out).
    #[arg(long = "results-dir")]
    results_dir: Option<PathBuf>,
}

impl Cli {
    /// Build an `InferConfig` from CLI args overlaid on .inferconfig.
    fn to_config(&self) -> config::InferConfig {
        // Load base config from .inferconfig
        let mut c = match &self.inferconfig_path {
            Some(path) => config::InferConfig::load_from_file(path),
            None => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                config::InferConfig::load(&cwd)
            }
        };

        // CLI args override .inferconfig
        if self.pulse_only {
            c.pulse_only = true;
        }
        if self.liveness_only {
            c.liveness_only = true;
        }
        if self.quiet {
            c.quiet = true;
        }
        if self.pulse_intraprocedural_only {
            c.pulse_intraprocedural_only = true;
        }
        if let Some(v) = self.pulse_max_disjuncts {
            c.pulse_max_disjuncts = v;
        }
        if let Some(v) = self.max_widens {
            c.max_widens = v;
        }
        if self.debug_level_analysis > 0 {
            c.debug_level_analysis = self.debug_level_analysis;
        }
        if self.jobs.is_some() {
            c.jobs = self.jobs;
        }

        c
    }

    /// Determine what mode we're running in based on args.
    fn mode(&self) -> Mode {
        // If we have --capture-textual files, analyze those directly
        if !self.capture_textual.is_empty() {
            return Mode::DirectSil(self.capture_textual.clone());
        }

        // Check if args start with "--" (trailing_var_arg captures everything after --)
        // or are .sil files
        if !self.args.is_empty() {
            // If all args are .sil files, treat as direct mode
            let all_sil = self.args.iter().all(|a| a.ends_with(".sil"));
            if all_sil {
                return Mode::DirectSil(self.args.iter().map(PathBuf::from).collect());
            }
            // Otherwise it's a build command
            return Mode::CaptureAndAnalyze(self.args.clone());
        }

        // No args: analyze existing capture.db
        Mode::AnalyzeExisting
    }
}

enum Mode {
    /// `infer-rs -- clang -c file.c` — capture then analyze
    CaptureAndAnalyze(Vec<String>),
    /// `infer-rs` — export from existing capture.db then analyze
    AnalyzeExisting,
    /// `infer-rs file.sil` — analyze .sil files directly
    DirectSil(Vec<PathBuf>),
}

/// A file to analyze: the .sil path and optionally the original source filename.
struct AnalysisFile {
    sil_path: PathBuf,
    /// Original source file (e.g. "test.c"). None = use .sil filename.
    source: Option<String>,
}

fn main() {
    let cli = Cli::parse();

    // Initialize logging
    if std::env::var("RUST_LOG").is_err() {
        let level = match cli.debug_level_analysis {
            0 => "warn",
            1 => "pulse=debug",
            _ => "pulse=trace",
        };
        std::env::set_var("RUST_LOG", level);
    }
    env_logger::init();

    // Initialize global config from .inferconfig + CLI args
    config::init(cli.to_config());
    let cfg = config::get();

    // Configure rayon thread pool
    if let Some(j) = cfg.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(j)
            .build_global()
            .expect("failed to initialize rayon thread pool");
    }

    let mode = cli.mode();
    let files: Vec<AnalysisFile> = match mode {
        Mode::CaptureAndAnalyze(ref build_cmd) => {
            let infer_out = cli
                .results_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("infer-out"));
            run_capture(build_cmd, &infer_out, cli.infer_bin.as_deref(), cfg.quiet);
            export_textual(
                &infer_out,
                &cli.out_dir,
                cli.infer_bin.as_deref(),
                cfg.quiet,
            )
        }
        Mode::AnalyzeExisting => {
            let infer_out = cli
                .results_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("infer-out"));
            if !infer_out.join("capture.db-wal").exists() && !infer_out.join("capture.db").exists()
            {
                eprintln!(
                    "error: no capture.db found in {}. Run with -- <build command> first.",
                    infer_out.display()
                );
                process::exit(1);
            }
            export_textual(
                &infer_out,
                &cli.out_dir,
                cli.infer_bin.as_deref(),
                cfg.quiet,
            )
        }
        Mode::DirectSil(paths) => paths
            .into_iter()
            .map(|p| AnalysisFile {
                sil_path: p,
                source: None,
            })
            .collect(),
    };

    if files.is_empty() {
        eprintln!("error: no .sil files to analyze");
        process::exit(1);
    }

    let run_pulse = cfg.pulse_only || !cfg.liveness_only;
    let run_liveness = cfg.liveness_only || !cfg.pulse_only;

    let mut all_issues = IssueLog::new();
    let mut total_procs = 0;
    let mut total_files = 0;

    for af in &files {
        if !cfg.quiet {
            eprintln!("Analyzing {}", af.sil_path.display());
        }

        match analyze_file(&af.sil_path, af.source.as_deref(), run_pulse, run_liveness) {
            Ok((log, num_procs)) => {
                total_procs += num_procs;
                total_files += 1;
                all_issues.merge(log);
            }
            Err(e) => {
                eprintln!("error: {}: {e}", af.sil_path.display());
            }
        }
    }

    all_issues.sort();

    // Write report.json
    if let Err(e) = std::fs::create_dir_all(&cli.out_dir) {
        eprintln!(
            "warning: failed to create output directory {}: {e}",
            cli.out_dir.display()
        );
    }
    let report_path = cli.out_dir.join("report.json");
    let json = all_issues.to_json();
    if let Err(e) = std::fs::write(&report_path, &json) {
        eprintln!("error writing {}: {e}", report_path.display());
        process::exit(1);
    }

    if !cfg.quiet {
        eprintln!(
            "Found {} issue(s) in {} procedure(s) across {} file(s)",
            all_issues.len(),
            total_procs,
            total_files,
        );
        eprintln!("Report: {}", report_path.display());
    }

    // Print issues to stdout in issues.exp format
    if !all_issues.is_empty() {
        println!("{}", all_issues.to_issues_exp());
    }

    if total_files == 0 {
        process::exit(1);
    }
    if !all_issues.is_empty() {
        process::exit(2);
    }
}

// ---------------------------------------------------------------------------
// Capture & export
// ---------------------------------------------------------------------------

/// Find the infer binary. Checks: explicit path, INFER_BIN env, relative path, PATH.
fn find_infer(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit {
        if p.exists() {
            return p.to_path_buf();
        }
        eprintln!("error: infer binary not found at {}", p.display());
        process::exit(1);
    }
    if let Ok(bin) = std::env::var("INFER_BIN") {
        let p = PathBuf::from(&bin);
        if p.exists() {
            return p;
        }
    }
    // Relative to workspace root: ../infer/bin/infer
    let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    if let Some(root) = ws_root {
        let candidate = root.join("infer/bin/infer");
        if candidate.exists() {
            return candidate;
        }
    }
    // Fall back to PATH
    if let Ok(output) = std::process::Command::new("which").arg("infer").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    eprintln!("error: cannot find infer binary. Set --infer-bin or INFER_BIN.");
    process::exit(1);
}

/// Run `infer capture --store-textual -- <build_cmd>`.
fn run_capture(build_cmd: &[String], infer_out: &Path, infer_bin: Option<&Path>, quiet: bool) {
    let infer = find_infer(infer_bin);
    if !quiet {
        eprintln!(
            "Capturing: infer --store-textual -o {} -- {}",
            infer_out.display(),
            build_cmd.join(" ")
        );
    }

    let status = std::process::Command::new(&infer)
        .arg("--store-textual")
        .arg("-o")
        .arg(infer_out)
        .arg("-j")
        .arg("1")
        .arg("--")
        .args(build_cmd)
        .status();

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("error: infer capture exited with {s}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: failed to run infer: {e}");
            process::exit(1);
        }
    }
}

/// Run `infer debug --export-textual <dir> -o <infer_out>` and return analysis files.
fn export_textual(
    infer_out: &Path,
    out_dir: &Path,
    infer_bin: Option<&Path>,
    quiet: bool,
) -> Vec<AnalysisFile> {
    let infer = find_infer(infer_bin);
    let export_dir = out_dir.join("textual");
    if let Err(e) = std::fs::create_dir_all(&export_dir) {
        eprintln!("error: failed to create export directory: {e}");
        process::exit(1);
    }

    if !quiet {
        eprintln!(
            "Exporting textual: infer debug --export-textual {} -o {}",
            export_dir.display(),
            infer_out.display()
        );
    }

    let output = std::process::Command::new(&infer)
        .arg("debug")
        .arg("--export-textual")
        .arg(&export_dir)
        .arg("-o")
        .arg(infer_out)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            if !quiet {
                let msg = String::from_utf8_lossy(&o.stdout);
                if !msg.is_empty() {
                    eprint!("{msg}");
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            eprintln!("error: infer debug --export-textual failed: {stderr}");
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: failed to run infer debug: {e}");
            process::exit(1);
        }
    }

    // Read manifest.json
    let manifest_path = export_dir.join("manifest.json");
    read_manifest(&manifest_path, &export_dir)
}

/// Parse manifest.json and return analysis files with source mappings.
fn read_manifest(manifest_path: &Path, base_dir: &Path) -> Vec<AnalysisFile> {
    let entries = match config::manifest::read_manifest(manifest_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    entries
        .iter()
        .map(|e| AnalysisFile {
            sil_path: base_dir.join(&e.sil),
            source: Some(e.source.clone()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

fn analyze_file(
    path: &Path,
    source_override: Option<&str>,
    run_pulse: bool,
    run_liveness: bool,
) -> Result<(IssueLog, usize), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("failed to read: {e}"))?;
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input.sil");

    let mut module =
        textual::parse_module(&src, filename).map_err(|e| format!("parse error: {e}"))?;

    // Override source_file with original filename from manifest
    if let Some(source) = source_override {
        module.source_file = source.to_string();
    }

    let (decls, decl_errors) = textual::decls::DeclEnv::from_module(&module);
    if !decl_errors.is_empty() {
        return Err(format!("declaration errors: {decl_errors:?}"));
    }

    // Run Textual transforms (let_propagation inlines __sil_* builtins)
    textual::transform::run(&mut module, &decls);

    // Build line map for source location remapping (@[line:col] and // .line)
    let line_map = textual::line_map::LineMap::create(&src);
    let line_map_ref = if line_map.is_empty() {
        None
    } else {
        Some(&line_map)
    };

    let (sil_cfg, tenv) =
        textual::to_sil::module_to_sil_with_line_map(&module, &decls, line_map_ref)
            .map_err(|e| format!("conversion errors: {e:?}"))?;

    let mut log = IssueLog::new();
    let num_procs = sil_cfg.num_procs();

    if run_pulse {
        if config::get().pulse_intraprocedural_only {
            for pdesc in sil_cfg.iter_proc_descs() {
                let summary = pulse::checker::analyze(pdesc);
                log.merge(pulse::checker::to_issue_log(
                    &summary,
                    &format!("{}", pdesc.proc_name),
                ));
            }
        } else {
            let checker = PulseInterChecker;
            let (store, _stats) = ondemand::runner::run_inter(&checker, &sil_cfg, &tenv);
            for (pname, summary) in store.to_vec() {
                log.merge(pulse::checker::to_issue_log(&summary, &format!("{pname}")));
            }
        }
    }

    if run_liveness {
        for pdesc in sil_cfg.iter_proc_descs() {
            log.merge(analyses::liveness::report_dead_stores(pdesc));
        }
    }

    Ok((log, num_procs))
}

/// Collect Cfun summaries from all expressions in an instruction.
fn collect_cfun_summaries(
    instr: &sil::instr::Instr,
    ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
    summaries: &mut std::collections::HashMap<
        sil::procname::Procname,
        pulse::summary::PulseSummary,
    >,
) {
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::instr::Instr;

    fn collect_from_exp(
        exp: &Exp,
        ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
        summaries: &mut std::collections::HashMap<
            sil::procname::Procname,
            pulse::summary::PulseSummary,
        >,
    ) {
        match exp {
            Exp::Const(Const::Cfun(pname)) => {
                if let Some(summary) = ctx.summaries.get(pname) {
                    summaries.entry(pname.clone()).or_insert(summary);
                }
            }
            Exp::UnOp(_, inner, _) | Exp::Cast(_, inner) | Exp::Exn(inner) => {
                collect_from_exp(inner, ctx, summaries)
            }
            Exp::BinOp(_, lhs, rhs) => {
                collect_from_exp(lhs, ctx, summaries);
                collect_from_exp(rhs, ctx, summaries);
            }
            Exp::Lfield(data, _, _) => collect_from_exp(&data.exp, ctx, summaries),
            Exp::Lindex(base, idx) => {
                collect_from_exp(base, ctx, summaries);
                collect_from_exp(idx, ctx, summaries);
            }
            _ => {}
        }
    }

    match instr {
        Instr::Load { e, .. } => collect_from_exp(e, ctx, summaries),
        Instr::Store { e1, e2, .. } => {
            collect_from_exp(e1, ctx, summaries);
            collect_from_exp(e2, ctx, summaries);
        }
        Instr::Call { fun_exp, args, .. } => {
            collect_from_exp(fun_exp, ctx, summaries);
            for (arg, _) in args {
                collect_from_exp(arg, ctx, summaries);
            }
        }
        Instr::Prune { exp, .. } => collect_from_exp(exp, ctx, summaries),
        _ => {}
    }
}

/// Analyze with the specialization loop, matching the test's behavior.
///
/// 1. Analyze normally
/// 2. Check if any callee needs specialization (unresolved function pointers)
/// 3. If yes, build specialization from caller's Closure attributes
/// 4. Re-analyze callee with specialization, then re-analyze caller
///
/// Cross-ref: OCaml Pulse.ml iter_call + request_specialization.
fn analyze_with_spec_loop(
    pdesc: &sil::procdesc::Procdesc,
    ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
    specialization: Option<&sil::specialization::PulseSpecialization>,
    depth: usize,
) -> pulse::summary::PulseSummary {
    const MAX_SPEC_DEPTH: usize = 5;

    let mut callee_summaries = std::collections::HashMap::new();
    for (_node_id, instr) in pdesc.iter_instrs() {
        collect_cfun_summaries(instr, ctx, &mut callee_summaries);
    }
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

    // Post-analysis: check if any callee needs specialization we can provide
    let spec_requests: Vec<_> = callee_summaries
        .iter()
        .filter(|(_, cs)| !cs.needs_specialization.is_empty())
        .filter_map(|(callee_pname, callee_summary)| {
            let first_pp = callee_summary.pre_posts.first()?;
            // Find the Call instruction to this callee
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
            // Build state up to the call to evaluate actuals
            let mut eval_state = pulse::abductive::AbductiveDomain::mk_initial(pdesc);
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
                match pre_instr {
                    sil::instr::Instr::Store { e1, e2, loc, .. } => {
                        let rhs = pulse::operations::eval_or_fresh(e2, loc, &mut eval_state);
                        let lhs = pulse::operations::eval_or_fresh(e1, loc, &mut eval_state);
                        eval_state.write_heap(lhs, pulse::access::Access::Dereference, rhs);
                    }
                    sil::instr::Instr::Load { id, e, loc, .. } => {
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
            )?;
            if callee_summary.get_specialized(&spec).is_some() {
                return None;
            }
            Some((callee_pname.clone(), spec))
        })
        .collect();

    if !spec_requests.is_empty() {
        for (callee_pname, spec) in &spec_requests {
            if let Some(callee_pdesc) = ctx.cfg.get_proc_desc(callee_pname) {
                let spec_summary = analyze_with_spec_loop(callee_pdesc, ctx, Some(spec), depth + 1);
                if let Some(existing) = callee_summaries.get_mut(callee_pname) {
                    existing.add_specialized(spec.clone(), spec_summary.pre_posts);
                }
            }
        }
        summary =
            pulse::checker::analyze_with_specialization(pdesc, &callee_summaries, specialization);
    }

    summary
}

/// Adapter: implements `ondemand::InterChecker` for Pulse.
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
