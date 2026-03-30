// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! infer-rs CLI: analyze Textual .sil files and report issues.

use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use diagnostics::issue::IssueLog;

/// infer-rs: Rust implementation of the Infer static analyzer.
///
/// Analyzes Textual .sil files and reports issues (null dereferences,
/// use-after-free, dead stores).
#[derive(Parser, Debug)]
#[command(name = "infer-rs", version, about)]
struct Cli {
    /// .sil files to analyze (Textual format).
    #[arg(long = "capture-textual", value_name = "FILE")]
    capture_textual: Vec<PathBuf>,

    /// Additional .sil files (positional).
    #[arg(value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Run only the Pulse checker (null deref, use-after-free).
    #[arg(long)]
    pulse_only: bool,

    /// Run only the liveness checker (dead stores).
    #[arg(long)]
    liveness_only: bool,

    /// Output directory for report.json.
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

    /// Analysis debug level: 0=quiet, 1=per-instruction, 2=full state dumps.
    /// Matches OCaml's --debug-level-analysis. Also controlled via RUST_LOG env.
    #[arg(long = "debug-level-analysis", default_value = "0")]
    debug_level_analysis: u8,

    /// Path to .inferconfig file (default: search upward from CWD).
    #[arg(long = "inferconfig-path")]
    inferconfig_path: Option<PathBuf>,
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

        c
    }
}

fn main() {
    let cli = Cli::parse();

    // Initialize logging: --debug-level-analysis maps to log levels,
    // but RUST_LOG env var takes precedence if set.
    // 0 = warn (default), 1 = debug (per-instruction), 2 = trace (full state)
    if std::env::var("RUST_LOG").is_err() {
        let level = match cli.debug_level_analysis {
            0 => "warn",
            1 => "pulse=debug",
            _ => "pulse=trace",
        };
        std::env::set_var("RUST_LOG", level);
    }
    env_logger::init();

    let sil_files: Vec<PathBuf> = cli
        .capture_textual
        .iter()
        .chain(cli.files.iter())
        .cloned()
        .collect();

    if sil_files.is_empty() {
        eprintln!("error: no .sil files specified");
        eprintln!("Usage: infer-rs [--capture-textual] <FILE.sil>...");
        process::exit(1);
    }

    // Initialize global config from .inferconfig + CLI args
    config::init(cli.to_config());
    let cfg = config::get();

    let run_pulse = cfg.pulse_only || !cfg.liveness_only;
    let run_liveness = cfg.liveness_only || !cfg.pulse_only;

    let mut all_issues = IssueLog::new();
    let mut total_procs = 0;
    let mut total_files = 0;

    for sil_path in &sil_files {
        if !cfg.quiet {
            eprintln!("Analyzing {}", sil_path.display());
        }

        match analyze_file(sil_path, run_pulse, run_liveness) {
            Ok((log, num_procs)) => {
                total_procs += num_procs;
                total_files += 1;
                all_issues.merge(log);
            }
            Err(e) => {
                eprintln!("error: {}: {e}", sil_path.display());
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

fn analyze_file(
    path: &Path,
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

    let (decls, decl_errors) = textual::decls::DeclEnv::from_module(&module);
    if !decl_errors.is_empty() {
        return Err(format!("declaration errors: {decl_errors:?}"));
    }

    // Run Textual transforms (let_propagation inlines __sil_* builtins)
    textual::transform::run(&mut module, &decls);

    let (sil_cfg, tenv) = textual::to_sil::module_to_sil(&module, &decls)
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
