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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use diagnostics::issue::IssueLog;
use rayon::prelude::*;
use regex::Regex;
use sil::procname::Procname;
use sil::source_file::SourceFile;

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

    /// Override the reported source file for direct .sil analysis.
    #[arg(long = "source-override", hide = true)]
    source_override: Option<String>,

    /// Positional args: .sil files, or after `--` the build command.
    #[arg(value_name = "ARG", trailing_var_arg = true)]
    args: Vec<String>,

    /// Run only the Pulse checker (null deref, use-after-free).
    #[arg(long)]
    pulse_only: bool,

    /// Run only the liveness checker (dead stores).
    #[arg(long)]
    liveness_only: bool,

    /// Report suppressed issues as distinguished test-only reports.
    #[arg(long = "pulse-report-issues-for-tests")]
    pulse_report_issues_for_tests: bool,

    /// Force known calls with no continuing summary path to fall back to
    /// unknown-call semantics when the summary was incomplete.
    #[arg(long = "pulse-force-continue")]
    pulse_force_continue: Option<bool>,

    /// Regex of methods to model as wrappers to `free(3)`.
    #[arg(long = "pulse-model-free-pattern")]
    pulse_model_free_pattern: Option<String>,

    /// Regex of methods to model as wrappers to `malloc(3)`.
    #[arg(long = "pulse-model-malloc-pattern")]
    pulse_model_malloc_pattern: Option<String>,

    /// Regex of methods to model as wrappers to `realloc(3)`.
    #[arg(long = "pulse-model-realloc-pattern")]
    pulse_model_realloc_pattern: Option<String>,

    /// Exact procnames to model as non-returning calls.
    #[arg(long = "pulse-model-abort")]
    pulse_model_abort: Vec<String>,

    /// Exact procnames to model as unreachable.
    #[arg(long = "pulse-model-unreachable")]
    pulse_model_unreachable: Vec<String>,

    /// Regex of methods to model as returning a non-null value.
    #[arg(long = "pulse-model-return-nonnull")]
    pulse_model_return_nonnull: Option<String>,

    /// Regex of methods to model as returning the receiver (`this` / `self`).
    #[arg(long = "pulse-model-return-this")]
    pulse_model_return_this: Option<String>,

    /// Regex of methods to model as returning the first source-language arg.
    #[arg(long = "pulse-model-return-first-arg")]
    pulse_model_return_first_arg: Option<String>,

    /// Regex of methods to model as returning either null or a fresh value.
    #[arg(long = "pulse-model-return-nullable")]
    pulse_model_return_nullable: Option<String>,

    /// Regex of methods to skip and treat as unknown calls.
    #[arg(long = "pulse-model-skip-pattern")]
    pulse_model_skip_pattern: Option<String>,

    /// Regexes of methods to model as unknown pure calls.
    #[arg(long = "pulse-model-unknown-pure")]
    pulse_model_unknown_pure: Vec<String>,

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

    /// Maximum number of recently modified heap edges retained per address.
    #[arg(long = "pulse-recency-limit")]
    pulse_recency_limit: Option<usize>,

    /// Larger CFGs than this are skipped in Pulse (default: 15000).
    #[arg(long = "pulse-max-cfg-size")]
    pulse_max_cfg_size: Option<usize>,

    /// Prune unreachable interval/is-int formula facts in large intermediate
    /// Pulse states. Reduces memory on DES-family procedures but can cost wall
    /// time on capped whole-program runs.
    #[arg(long = "pulse-intermediate-formula-gc")]
    pulse_intermediate_formula_gc: bool,

    /// Maximum delta in process peak RSS (megabytes) that a single Pulse
    /// procedure analysis is allowed to consume before being aborted.
    /// Defaults to `2048` (2 GB) when unset; pass `0` to disable the cap
    /// entirely. Cross-ref: OCaml `--pulse-max-heap`.
    #[arg(long = "pulse-max-heap-mb")]
    pulse_max_heap_mb: Option<usize>,

    /// Maximum wall-clock seconds a single Pulse procedure analysis is
    /// allowed to consume before being aborted. Defaults to `60` when
    /// unset; pass `0` to disable the cap entirely. Complements
    /// `--pulse-max-heap-mb` for procedures whose fixpoint does not
    /// converge quickly but whose RSS stays low.
    #[arg(long = "pulse-max-wall-secs")]
    pulse_max_wall_secs: Option<u64>,

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

    /// Emit scheduler progress/debug information from the on-demand runner.
    /// Matches OCaml's --trace-ondemand.
    #[arg(long = "trace-ondemand")]
    trace_ondemand: bool,

    /// OCaml-compatible procedure regex filter.
    /// Either `proc_regex` or `source_regex:proc_regex`.
    #[arg(long = "procedures-filter")]
    procedures_filter: Option<String>,

    /// Dump final retained fixpoint pre/post states for selected CFG node IDs.
    /// Accepts a comma-separated list, e.g. `--debug-fixpoint-nodes 18,20,22`.
    #[arg(long = "debug-fixpoint-nodes", value_delimiter = ',')]
    debug_fixpoint_nodes: Vec<u32>,

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
        if self.pulse_report_issues_for_tests {
            c.pulse_report_issues_for_tests = true;
        }
        if let Some(v) = self.pulse_force_continue {
            c.pulse_force_continue = v;
        }
        if let Some(v) = &self.pulse_model_free_pattern {
            c.pulse_model_free_pattern = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_malloc_pattern {
            c.pulse_model_malloc_pattern = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_realloc_pattern {
            c.pulse_model_realloc_pattern = Some(v.clone());
        }
        if !self.pulse_model_abort.is_empty() {
            c.pulse_model_abort = self.pulse_model_abort.clone();
        }
        if !self.pulse_model_unreachable.is_empty() {
            c.pulse_model_unreachable = self.pulse_model_unreachable.clone();
        }
        if let Some(v) = &self.pulse_model_return_nonnull {
            c.pulse_model_return_nonnull = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_return_this {
            c.pulse_model_return_this = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_return_first_arg {
            c.pulse_model_return_first_arg = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_return_nullable {
            c.pulse_model_return_nullable = Some(v.clone());
        }
        if let Some(v) = &self.pulse_model_skip_pattern {
            c.pulse_model_skip_pattern = Some(v.clone());
        }
        if !self.pulse_model_unknown_pure.is_empty() {
            c.pulse_model_unknown_pure = self.pulse_model_unknown_pure.clone();
        }
        if self.quiet {
            c.quiet = true;
        }
        if self.pulse_intraprocedural_only {
            c.pulse_intraprocedural_only = true;
        }
        if let Some(v) = self.pulse_recency_limit {
            c.pulse_recency_limit = Some(v);
        }
        if let Some(v) = self.pulse_max_disjuncts {
            c.pulse_max_disjuncts = v;
        }
        if let Some(v) = self.pulse_max_cfg_size {
            c.pulse_max_cfg_size = v;
        }
        if self.pulse_intermediate_formula_gc {
            c.pulse_intermediate_formula_gc = true;
        }
        if let Some(v) = self.pulse_max_heap_mb {
            // `--pulse-max-heap-mb 0` is the documented escape hatch to
            // disable the cap entirely, overriding the config default.
            c.pulse_max_heap_mb = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = self.pulse_max_wall_secs {
            // Same escape hatch for the wall-time cap.
            c.pulse_max_wall_secs = if v == 0 { None } else { Some(v) };
        }
        if let Some(v) = self.max_widens {
            c.max_widens = v;
        }
        if self.debug_level_analysis > 0 {
            c.debug_level_analysis = self.debug_level_analysis;
        }
        if self.trace_ondemand {
            c.trace_ondemand = true;
        }
        if let Some(v) = &self.procedures_filter {
            c.procedures_filter = Some(v.clone());
        }
        if !self.debug_fixpoint_nodes.is_empty() {
            c.debug_fixpoint_nodes = self.debug_fixpoint_nodes.clone();
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

fn default_rust_log_filter(cfg: &config::InferConfig) -> String {
    let mut directives = vec!["warn".to_string()];
    match cfg.debug_level_analysis {
        0 => {}
        1 => directives.push("pulse=debug".to_string()),
        _ => directives.push("pulse=trace".to_string()),
    }
    if cfg.trace_ondemand && !cfg.quiet {
        directives.push("ondemand=info".to_string());
    }
    directives.join(",")
}

const PARSE_PROGRESS_EVERY: usize = 25;
const MERGE_PROGRESS_EVERY: usize = 25;

fn ondemand_trace_enabled() -> bool {
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

fn filtered_cfg(cfg: &sil::cfg::Cfg, proc_names: &HashSet<Procname>) -> sil::cfg::Cfg {
    sil::cfg::Cfg {
        proc_descs: cfg
            .proc_descs
            .iter()
            .filter(|(pname, _)| proc_names.contains(*pname))
            .map(|(pname, pdesc)| (pname.clone(), pdesc.clone()))
            .collect(),
    }
}

fn retained_callee_closure(cfg: &sil::cfg::Cfg, roots: &HashSet<Procname>) -> HashSet<Procname> {
    let call_graph = ondemand::callgraph::CallGraph::from_cfg(cfg);
    let mut retained = roots.clone();
    let mut worklist: Vec<_> = roots.iter().cloned().collect();

    while let Some(proc_name) = worklist.pop() {
        for callee in call_graph.callees(&proc_name) {
            if cfg.get_proc_desc(callee).is_none() {
                continue;
            }
            if retained.insert(callee.clone()) {
                worklist.push(callee.clone());
            }
        }
    }

    retained
}

fn apply_procedures_filter(
    cfg: &sil::cfg::Cfg,
    procedures_filter: &ProceduresFilter,
    include_transitive_callees: bool,
) -> Result<AnalysisSelection, String> {
    let matched_roots: HashSet<_> = cfg
        .iter_proc_descs()
        .filter(|pdesc| procedures_filter.matches(&pdesc.loc.file, &pdesc.proc_name))
        .map(|pdesc| pdesc.proc_name.clone())
        .collect();

    if matched_roots.is_empty() {
        return Err(format!(
            "--procedures-filter `{}` matched no procedures",
            procedures_filter.raw
        ));
    }

    let retained = if include_transitive_callees {
        retained_callee_closure(cfg, &matched_roots)
    } else {
        matched_roots.clone()
    };

    Ok(AnalysisSelection {
        cfg: filtered_cfg(cfg, &retained),
        matched_roots: matched_roots.len(),
    })
}

fn count_source_files(cfg: &sil::cfg::Cfg) -> usize {
    cfg.iter_proc_descs()
        .map(|pdesc| pdesc.loc.file.clone())
        .collect::<HashSet<_>>()
        .len()
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

struct ParsedAnalysisUnit {
    cfg: sil::cfg::Cfg,
    tenv: sil::tenv::Tenv,
}

/// OCaml-compatible procedure filter from `Filtering.mk_procedure_name_filter`.
#[derive(Debug)]
struct ProceduresFilter {
    raw: String,
    source_file_regex: Option<Regex>,
    proc_name_regex: Option<Regex>,
}

impl ProceduresFilter {
    fn compile(cfg: &config::InferConfig) -> Result<Option<Self>, String> {
        let Some(raw) = cfg.procedures_filter.as_ref() else {
            return Ok(None);
        };

        let (source_file_regex, proc_name_regex) = match raw.split_once(':') {
            Some((source_file_filter, proc_name_filter)) => (
                Some(Regex::new(source_file_filter).map_err(|e| {
                    format!("invalid --procedures-filter source regex `{source_file_filter}`: {e}")
                })?),
                Some(Regex::new(proc_name_filter).map_err(|e| {
                    format!("invalid --procedures-filter proc regex `{proc_name_filter}`: {e}")
                })?),
            ),
            None => (
                None,
                Some(
                    Regex::new(raw)
                        .map_err(|e| format!("invalid --procedures-filter regex `{raw}`: {e}"))?,
                ),
            ),
        };

        Ok(Some(Self {
            raw: raw.clone(),
            source_file_regex,
            proc_name_regex,
        }))
    }

    fn matches(&self, source_file: &SourceFile, proc_name: &Procname) -> bool {
        let source_matches = self
            .source_file_regex
            .as_ref()
            .is_none_or(|regex| regex.is_match(&source_file.to_string()));
        let proc_matches = self
            .proc_name_regex
            .as_ref()
            .is_none_or(|regex| regex.is_match(&proc_name.to_string()));
        source_matches && proc_matches
    }
}

struct AnalysisSelection {
    cfg: sil::cfg::Cfg,
    matched_roots: usize,
}

/// Procedure metadata recovered from the original capture database.
///
/// Store-textual export drops some proc and local attributes such as
/// `is_no_return` and `has_cleanup_attribute`.
/// When we still have the originating `capture.db`, recover those facts from
/// `infer debug --procedures --procedures-attributes` and re-apply them after
/// Textual-to-SIL conversion.
#[derive(Clone, Debug, Default)]
struct CaptureProcMetadata {
    no_return: HashSet<(String, String)>,
    cleanup_locals: HashSet<(String, String, String)>,
}

impl CaptureProcMetadata {
    fn insert_no_return(&mut self, source_file: &str, proc_name: &str) {
        for source_key in canonical_source_keys(source_file) {
            self.no_return.insert((source_key, proc_name.to_string()));
        }
    }

    fn insert_cleanup_local(&mut self, source_file: &str, proc_name: &str, local_name: &str) {
        for source_key in canonical_source_keys(source_file) {
            self.cleanup_locals
                .insert((source_key, proc_name.to_string(), local_name.to_string()));
        }
    }

    fn is_no_return(&self, source_file: &str, proc_name: &Procname) -> bool {
        let proc_name = proc_name.to_string();
        canonical_source_keys(source_file)
            .into_iter()
            .any(|source_key| self.no_return.contains(&(source_key, proc_name.clone())))
    }

    fn local_has_cleanup_attribute(
        &self,
        source_file: &str,
        proc_name: &Procname,
        local_name: &str,
    ) -> bool {
        let proc_name = proc_name.to_string();
        canonical_source_keys(source_file)
            .into_iter()
            .any(|source_key| {
                self.cleanup_locals.contains(&(
                    source_key,
                    proc_name.clone(),
                    local_name.to_string(),
                ))
            })
    }
}

fn main() {
    let cli = Cli::parse();
    let resolved_config = cli.to_config();

    // Initialize logging
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", default_rust_log_filter(&resolved_config));
    }
    env_logger::init();

    // Initialize global config from .inferconfig + CLI args
    config::init(resolved_config);
    let cfg = config::get();
    let procedures_filter = ProceduresFilter::compile(cfg).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        process::exit(1);
    });

    // Configure rayon thread pool
    if let Some(j) = cfg.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(j)
            .build_global()
            .expect("failed to initialize rayon thread pool");
    }

    let mode = cli.mode();
    let mode_label = match &mode {
        Mode::CaptureAndAnalyze(_) => "capture-and-analyze",
        Mode::AnalyzeExisting => "analyze-existing",
        Mode::DirectSil(_) => "direct-sil",
    };
    let (files, metadata_results_dir): (Vec<AnalysisFile>, Option<PathBuf>) = match mode {
        Mode::CaptureAndAnalyze(ref build_cmd) => {
            let infer_out = cli
                .results_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("infer-out"));
            run_capture(build_cmd, &infer_out, cli.infer_bin.as_deref(), cfg.quiet);
            (
                export_textual(
                    &infer_out,
                    &cli.out_dir,
                    cli.infer_bin.as_deref(),
                    cfg.quiet,
                ),
                Some(infer_out),
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
            (
                export_textual(
                    &infer_out,
                    &cli.out_dir,
                    cli.infer_bin.as_deref(),
                    cfg.quiet,
                ),
                Some(infer_out),
            )
        }
        Mode::DirectSil(paths) => (
            paths
                .into_iter()
                .enumerate()
                .map(|(idx, p)| AnalysisFile {
                    sil_path: p,
                    source: if idx == 0 {
                        cli.source_override.clone()
                    } else {
                        None
                    },
                })
                .collect(),
            cli.results_dir.clone(),
        ),
    };

    if files.is_empty() {
        eprintln!("error: no .sil files to analyze");
        process::exit(1);
    }
    if cli.source_override.is_some() && files.len() != 1 {
        eprintln!("error: --source-override requires exactly one .sil file");
        process::exit(1);
    }

    let run_pulse = cfg.pulse_only || !cfg.liveness_only;
    let run_liveness = cfg.liveness_only || !cfg.pulse_only;
    let capture_proc_metadata = match metadata_results_dir.as_deref() {
        Some(results_dir) if has_capture_db(results_dir) => {
            let infer = find_infer(cli.infer_bin.as_deref());
            Some(
                load_capture_proc_metadata(results_dir, &infer).unwrap_or_else(|e| {
                    eprintln!(
                        "error: failed to load capture metadata from {}: {e}",
                        results_dir.display()
                    );
                    process::exit(1);
                }),
            )
        }
        Some(results_dir) => {
            if !cfg.quiet {
                eprintln!(
                    "warning: no capture.db found in {}, skipping proc metadata augmentation",
                    results_dir.display()
                );
            }
            None
        }
        None => None,
    };

    let mut all_issues = IssueLog::new();
    let mut total_procs = 0;
    let mut total_files = 0;
    let trace_ondemand = ondemand_trace_enabled();

    if !cfg.quiet {
        for af in &files {
            eprintln!("Analyzing {}", af.sil_path.display());
        }
    }

    let parse_start = Instant::now();
    let parse_completed = AtomicUsize::new(0);
    let parse_ok = AtomicUsize::new(0);
    let parse_errors = AtomicUsize::new(0);
    let total_parse_targets = files.len();
    if trace_ondemand {
        log::info!(
            target: "ondemand",
            "[ondemand] cli parse start: mode={mode_label} files={} pulse={} liveness={} jobs={:?}",
            files.len(),
            run_pulse,
            run_liveness,
            cfg.jobs,
        );
    }

    let parse_results: Vec<_> = files
        .par_iter()
        .map(|af| {
            let result = parse_file(af, capture_proc_metadata.as_ref());
            match &result {
                Ok(_) => {
                    parse_ok.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    parse_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            let completed = parse_completed.fetch_add(1, Ordering::Relaxed) + 1;
            if trace_ondemand
                && (completed == total_parse_targets
                    || completed.is_multiple_of(PARSE_PROGRESS_EVERY))
            {
                log::info!(
                    target: "ondemand",
                    "[ondemand] cli parse progress: completed={completed}/{total_parse_targets} ok={} errors={} elapsed={}",
                    parse_ok.load(Ordering::Relaxed),
                    parse_errors.load(Ordering::Relaxed),
                    format_duration(parse_start.elapsed()),
                );
            }

            (af.sil_path.clone(), result)
        })
        .collect();

    let total_parse_results = parse_results.len();
    let mut parsed_units = Vec::new();
    for (sil_path, result) in parse_results {
        match result {
            Ok(unit) => parsed_units.push(unit),
            Err(e) => eprintln!("error: {}: {e}", sil_path.display()),
        }
    }
    let parse_error_count = total_parse_results.saturating_sub(parsed_units.len());
    if trace_ondemand {
        log::info!(
            target: "ondemand",
            "[ondemand] cli parse done: parsed={}/{} errors={} elapsed={}",
            parsed_units.len(),
            total_parse_results,
            parse_error_count,
            format_duration(parse_start.elapsed()),
        );
    }

    if !parsed_units.is_empty() {
        let (merged_cfg, merged_tenv) = merge_parsed_units(parsed_units);
        let include_transitive_callees = run_pulse && !config::get().pulse_intraprocedural_only;
        let analysis_selection = procedures_filter
            .as_ref()
            .map(|filter| apply_procedures_filter(&merged_cfg, filter, include_transitive_callees))
            .transpose()
            .unwrap_or_else(|e| {
                eprintln!("error: {e}");
                process::exit(1);
            });
        let matched_roots = analysis_selection
            .as_ref()
            .map_or(0, |selection| selection.matched_roots);
        let analysis_cfg = match analysis_selection {
            Some(selection) => selection.cfg,
            None => merged_cfg,
        };
        total_procs = analysis_cfg.num_procs();
        total_files = count_source_files(&analysis_cfg);
        if let Some(filter) = &procedures_filter {
            let closure_note = if include_transitive_callees {
                " plus transitive callees"
            } else {
                ""
            };
            if !cfg.quiet {
                eprintln!(
                    "Using --procedures-filter `{}`: matched {} root proc(s), retained {} proc(s) across {} file(s){}",
                    filter.raw,
                    matched_roots,
                    total_procs,
                    total_files,
                    closure_note,
                );
            }
            if trace_ondemand {
                log::info!(
                    target: "ondemand",
                    "[ondemand] cli procedures filter: filter=`{}` matched_roots={} retained_procs={} files={} include_transitive_callees={}",
                    filter.raw,
                    matched_roots,
                    total_procs,
                    total_files,
                    include_transitive_callees,
                );
            }
        }
        if trace_ondemand {
            log::info!(
                target: "ondemand",
                "[ondemand] cli analysis input ready: files={} procedures={} types={}",
                total_files,
                total_procs,
                merged_tenv.len(),
            );
        }

        if run_pulse {
            if config::get().pulse_intraprocedural_only {
                let pulse_start = Instant::now();
                if trace_ondemand {
                    log::info!(
                        target: "ondemand",
                        "[ondemand] cli pulse intraprocedural start: procedures={total_procs}"
                    );
                }
                for pdesc in analysis_cfg.iter_proc_descs() {
                    let summary = pulse::checker::analyze(pdesc);
                    all_issues.merge(pulse::checker::to_issue_log_with_pdesc(&summary, pdesc));
                }
                if trace_ondemand {
                    log::info!(
                        target: "ondemand",
                        "[ondemand] cli pulse intraprocedural done: issues_so_far={} elapsed={}",
                        all_issues.len(),
                        format_duration(pulse_start.elapsed()),
                    );
                }
            } else {
                let checker = PulseInterChecker;
                let pulse_start = Instant::now();
                if trace_ondemand {
                    log::info!(
                        target: "ondemand",
                        "[ondemand] cli pulse interprocedural start: procedures={} types={}",
                        total_procs,
                        merged_tenv.len(),
                    );
                }
                let (store, _stats) =
                    ondemand::runner::run_inter(&checker, &analysis_cfg, &merged_tenv);
                for (pname, summary) in store.to_vec() {
                    let Some(pdesc) = analysis_cfg.get_proc_desc(&pname) else {
                        continue;
                    };
                    all_issues.merge(pulse::checker::to_issue_log_with_pdesc(&summary, pdesc));
                }
                if trace_ondemand {
                    log::info!(
                        target: "ondemand",
                        "[ondemand] cli pulse interprocedural done: summaries={} issues_so_far={} elapsed={}",
                        store.len(),
                        all_issues.len(),
                        format_duration(pulse_start.elapsed()),
                    );
                }
            }
        }

        if run_liveness {
            let liveness_start = Instant::now();
            if trace_ondemand {
                log::info!(
                    target: "ondemand",
                    "[ondemand] cli liveness start: procedures={total_procs}"
                );
            }
            for pdesc in analysis_cfg.iter_proc_descs() {
                all_issues.merge(analyses::liveness::report_dead_stores(pdesc));
            }
            if trace_ondemand {
                log::info!(
                    target: "ondemand",
                    "[ondemand] cli liveness done: issues_so_far={} elapsed={}",
                    all_issues.len(),
                    format_duration(liveness_start.elapsed()),
                );
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

fn workspace_root() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
}

fn workspace_relative_infer(ws_root: &Path) -> PathBuf {
    ws_root.join("../infer/bin/infer")
}

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
    let ws_root = workspace_root();
    if let Some(root) = ws_root {
        let candidate = workspace_relative_infer(&root);
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

fn has_capture_db(results_dir: &Path) -> bool {
    results_dir.join("capture.db").exists() || results_dir.join("capture.db-wal").exists()
}

fn load_capture_proc_metadata(
    results_dir: &Path,
    infer_bin: &Path,
) -> Result<CaptureProcMetadata, String> {
    let output = std::process::Command::new(infer_bin)
        .arg("debug")
        .arg("--results-dir")
        .arg(results_dir)
        .arg("--procedures")
        .arg("--procedures-attributes")
        .arg("--select")
        .arg("all")
        .output()
        .map_err(|e| format!("failed to run infer debug: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(parse_capture_proc_metadata(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_capture_proc_metadata(debug_output: &str) -> CaptureProcMetadata {
    fn finish_proc(
        metadata: &mut CaptureProcMetadata,
        current_proc: &mut Option<String>,
        current_source: &mut Option<String>,
        current_is_no_return: &mut bool,
        current_cleanup_locals: &mut Vec<String>,
    ) {
        if let (Some(proc_name), Some(source_file)) =
            (current_proc.as_deref(), current_source.as_deref())
        {
            if *current_is_no_return {
                metadata.insert_no_return(source_file, proc_name);
            }
            for local_name in current_cleanup_locals.drain(..) {
                metadata.insert_cleanup_local(source_file, proc_name, &local_name);
            }
        }
        *current_proc = None;
        *current_source = None;
        *current_is_no_return = false;
        current_cleanup_locals.clear();
    }

    fn extract_cleanup_locals(line: &str) -> Vec<String> {
        let mut locals = Vec::new();
        let mut search_start = 0;
        let needle = "has_cleanup_attribute= true";
        let name_needle = "name= ";
        while let Some(attr_rel) = line[search_start..].find(needle) {
            let attr_idx = search_start + attr_rel;
            if let Some(name_idx) = line[..attr_idx].rfind(name_needle) {
                let name_start = name_idx + name_needle.len();
                if let Some(name) = line[name_start..]
                    .split([';', '}', ']', ','])
                    .next()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                {
                    locals.push(name.to_string());
                }
            }
            search_start = attr_idx + needle.len();
        }
        locals
    }

    let mut metadata = CaptureProcMetadata::default();
    let mut current_proc = None;
    let mut current_source = None;
    let mut current_is_no_return = false;
    let mut current_cleanup_locals = Vec::new();

    for line in debug_output.lines().chain(std::iter::once("")) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            finish_proc(
                &mut metadata,
                &mut current_proc,
                &mut current_source,
                &mut current_is_no_return,
                &mut current_cleanup_locals,
            );
            continue;
        }

        if !line.starts_with(' ') && !line.starts_with('\t') {
            finish_proc(
                &mut metadata,
                &mut current_proc,
                &mut current_source,
                &mut current_is_no_return,
                &mut current_cleanup_locals,
            );
            current_proc = Some(trimmed.to_string());
            continue;
        }

        if let Some(source_file) = trimmed.strip_prefix("source_file: ") {
            current_source = Some(source_file.to_string());
            continue;
        }

        if trimmed == "; is_no_return= true" || trimmed == "is_no_return= true" {
            current_is_no_return = true;
        }

        current_cleanup_locals.extend(extract_cleanup_locals(trimmed));
    }

    metadata
}

fn canonical_source_keys(source_file: &str) -> Vec<String> {
    let mut keys = vec![source_file.to_string()];
    if let Some(base) = Path::new(source_file)
        .file_name()
        .and_then(|name| name.to_str())
    {
        if keys.iter().all(|existing| existing != base) {
            keys.push(base.to_string());
        }
    }
    keys
}

fn apply_capture_proc_metadata(
    sil_cfg: &mut sil::cfg::Cfg,
    source_file: &str,
    metadata: &CaptureProcMetadata,
) {
    for pdesc in sil_cfg.iter_proc_descs_mut() {
        if metadata.is_no_return(source_file, &pdesc.proc_name) {
            pdesc.is_no_return = true;
        }
        for local in &mut pdesc.locals {
            if metadata.local_has_cleanup_attribute(
                source_file,
                &pdesc.proc_name,
                &local.name.plain,
            ) {
                local.has_cleanup_attribute = true;
            }
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

fn merge_parsed_units(units: Vec<ParsedAnalysisUnit>) -> (sil::cfg::Cfg, sil::tenv::Tenv) {
    let trace_ondemand = ondemand_trace_enabled();
    let total_units = units.len();
    let merge_start = Instant::now();
    let mut merged_cfg = sil::cfg::Cfg::new();
    let mut merged_tenv = sil::tenv::Tenv::new();
    if trace_ondemand {
        log::info!(
            target: "ondemand",
            "[ondemand] cli merge start: files={total_units}"
        );
    }
    for (idx, unit) in units.into_iter().enumerate() {
        merged_cfg.merge(unit.cfg);
        merged_tenv.merge(unit.tenv);
        let merged_units = idx + 1;
        if trace_ondemand
            && (merged_units == total_units || merged_units % MERGE_PROGRESS_EVERY == 0)
        {
            log::info!(
                target: "ondemand",
                "[ondemand] cli merge progress: merged={merged_units}/{total_units} procedures={} types={} elapsed={}",
                merged_cfg.num_procs(),
                merged_tenv.len(),
                format_duration(merge_start.elapsed()),
            );
        }
    }
    (merged_cfg, merged_tenv)
}

fn parse_file(
    analysis_file: &AnalysisFile,
    capture_proc_metadata: Option<&CaptureProcMetadata>,
) -> Result<ParsedAnalysisUnit, String> {
    let src = std::fs::read_to_string(&analysis_file.sil_path)
        .map_err(|e| format!("failed to read: {e}"))?;
    let filename = analysis_file
        .sil_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input.sil");

    let mut module =
        textual::parse_module(&src, filename).map_err(|e| format!("parse error: {e}"))?;

    // Override source_file with original filename from manifest
    if let Some(source) = analysis_file.source.as_deref() {
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

    let (mut sil_cfg, tenv) =
        textual::to_sil::module_to_sil_with_line_map(&module, &decls, line_map_ref)
            .map_err(|e| format!("conversion errors: {e:?}"))?;

    if let Some(metadata) = capture_proc_metadata {
        apply_capture_proc_metadata(&mut sil_cfg, &module.source_file, metadata);
    }

    Ok(ParsedAnalysisUnit { cfg: sil_cfg, tenv })
}

/// Collect Cfun summaries from all expressions in an instruction.
fn collect_cfun_summaries(
    instr: &sil::instr::Instr,
    ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
    depth: usize,
    summaries: &mut std::collections::HashMap<
        sil::procname::Procname,
        Arc<pulse::summary::PulseSummary>,
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
            Arc<pulse::summary::PulseSummary>,
        >,
    ) {
        match exp {
            Exp::Const(Const::Cfun(pname)) => {
                if let Some(summary) = ctx.summaries.get_arc(pname) {
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

    fn collect_virtual_targets(
        callee: &sil::procname::Procname,
        ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
        depth: usize,
        summaries: &mut std::collections::HashMap<
            sil::procname::Procname,
            Arc<pulse::summary::PulseSummary>,
        >,
    ) {
        const MAX_VIRTUAL_TARGET_DEPTH: usize = 5;
        if depth >= MAX_VIRTUAL_TARGET_DEPTH {
            return;
        }
        for pdesc in ctx.cfg.iter_proc_descs() {
            if virtual_target_name_matches(callee, &pdesc.proc_name) {
                let summary = Arc::new(analyze_with_spec_loop(pdesc, ctx, None, depth + 1));
                summaries.entry(pdesc.proc_name.clone()).or_insert(summary);
            }
        }
    }

    match instr {
        Instr::Load { e, .. } => collect_from_exp(e, ctx, summaries),
        Instr::Store { e1, e2, .. } => {
            collect_from_exp(e1, ctx, summaries);
            collect_from_exp(e2, ctx, summaries);
        }
        Instr::Call {
            fun_exp,
            args,
            flags,
            ..
        } => {
            collect_from_exp(fun_exp, ctx, summaries);
            if flags.cf_virtual {
                if let Exp::Const(Const::Cfun(callee)) = fun_exp {
                    collect_virtual_targets(callee, ctx, depth, summaries);
                }
            }
            for (arg, _) in args {
                collect_from_exp(arg, ctx, summaries);
            }
        }
        Instr::Prune { exp, .. } => collect_from_exp(exp, ctx, summaries),
        _ => {}
    }
}

fn virtual_target_name_matches(
    callee: &sil::procname::Procname,
    target: &sil::procname::Procname,
) -> bool {
    match (callee, target) {
        (sil::procname::Procname::Hack(callee), sil::procname::Procname::Hack(target)) => {
            callee.function_name == target.function_name && callee.arity == target.arity
        }
        (sil::procname::Procname::Java(callee), sil::procname::Procname::Java(target)) => {
            callee.method_name == target.method_name && callee.parameters == target.parameters
        }
        (sil::procname::Procname::Python(callee), sil::procname::Procname::Python(target)) => {
            callee.function_name == target.function_name && callee.arity == target.arity
        }
        _ => false,
    }
}

fn root_global_pvar(exp: &sil::exp::Exp) -> Option<&sil::pvar::Pvar> {
    match exp {
        sil::exp::Exp::Lvar(pvar) if pvar.is_global() => Some(pvar),
        sil::exp::Exp::Lfield(data, _, _) => root_global_pvar(&data.exp),
        sil::exp::Exp::Lindex(base, _) | sil::exp::Exp::Cast(_, base) => root_global_pvar(base),
        _ => None,
    }
}

fn collect_global_initializer_refs(
    instr: &sil::instr::Instr,
    out: &mut std::collections::HashSet<sil::procname::Procname>,
) {
    if let sil::instr::Instr::Load { e, .. } = instr {
        if let Some(init_pname) =
            root_global_pvar(e).and_then(sil::pvar::Pvar::initializer_procname)
        {
            out.insert(init_pname);
        }
    }
}

fn collect_summary_closure_summaries(
    summary: &pulse::summary::PulseSummary,
    ctx: &ondemand::checker::AnalysisContext<pulse::summary::PulseSummary>,
    out: &mut std::collections::HashMap<sil::procname::Procname, Arc<pulse::summary::PulseSummary>>,
) {
    for pre_post in &summary.pre_posts {
        for (_addr, attrs) in pre_post.post.post.attrs.iter() {
            if let Some(pname) = attrs.get_closure_proc_name() {
                if let Some(summary) = ctx.summaries.get_arc(pname) {
                    out.entry(pname.clone()).or_insert(summary);
                }
            }
        }
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

    let mut callee_summaries: std::collections::HashMap<
        sil::procname::Procname,
        Arc<pulse::summary::PulseSummary>,
    > = std::collections::HashMap::new();
    let mut global_initializers = std::collections::HashSet::new();
    for (_node_id, instr) in pdesc.iter_instrs() {
        collect_cfun_summaries(instr, ctx, depth, &mut callee_summaries);
        collect_global_initializer_refs(instr, &mut global_initializers);
    }
    for init_pname in global_initializers {
        let Some(init_pdesc) = ctx.cfg.get_proc_desc(&init_pname) else {
            continue;
        };
        let summary = ctx.summaries.get_or_compute_arc(&init_pname, || {
            analyze_with_spec_loop(init_pdesc, ctx, None, depth + 1)
        });
        collect_summary_closure_summaries(summary.as_ref(), ctx, &mut callee_summaries);
        callee_summaries.entry(init_pname).or_insert(summary);
    }
    if let Some(spec) = specialization {
        for type_name in spec.dynamic_types.values() {
            let pname = sil::procname::Procname::c_from_string(&format!("{type_name}"));
            if let Some(s) = ctx.summaries.get_arc(&pname) {
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
                let spec_summary = analyze_with_spec_loop(callee_pdesc, ctx, Some(spec), depth + 1);
                let spec_summary_for_store = spec_summary.clone();
                if let Some(existing) = callee_summaries.get_mut(callee_pname) {
                    Arc::make_mut(existing).add_specialized_summary(spec.clone(), spec_summary);
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

#[cfg(test)]
mod tests {
    use super::*;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::{Exp, LfieldObjData};
    use sil::fieldname::Fieldname;
    use sil::ident::{Ident, IdentName};
    use sil::instr::Instr;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::{NodeKind, StmtNodeKind};
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::typ::{Typ, TypeName};

    fn test_loc(source_file: &str) -> Location {
        Location {
            file: SourceFile::new(source_file),
            line: 1,
            col: 1,
            macro_file_opt: None,
            macro_line: -1,
        }
    }

    fn mk_proc_in_file(name: &str, source_file: &str) -> sil::procdesc::Procdesc {
        let loc = test_loc(source_file);
        let mut pdesc =
            sil::procdesc::Procdesc::new(Procname::c_from_string(name), Typ::void(), loc.clone());
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc,
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    fn mk_calling_proc_in_file(
        name: &str,
        callee: &str,
        source_file: &str,
    ) -> sil::procdesc::Procdesc {
        let loc = test_loc(source_file);
        let mut pdesc =
            sil::procdesc::Procdesc::new(Procname::c_from_string(name), Typ::void(), loc.clone());
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Call {
                ret: (
                    Ident::create_normal(IdentName::from_string("n"), 0),
                    Typ::void(),
                ),
                fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string(callee))),
                args: vec![],
                loc: loc.clone(),
                flags: CallFlags::default(),
            }],
            loc,
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    fn mk_global_load_proc_in_file(
        name: &str,
        global: &str,
        source_file: &str,
    ) -> sil::procdesc::Procdesc {
        let loc = test_loc(source_file);
        let mut pdesc =
            sil::procdesc::Procdesc::new(Procname::c_from_string(name), Typ::void(), loc.clone());
        let global_pvar = sil::pvar::Pvar::mk_global(Mangled::from_string(global));
        let field = Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_parts(vec!["Global".to_string()])),
            "field",
        );
        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::Load {
                id: Ident::create_normal(IdentName::from_string("n"), 0),
                e: Exp::Lfield(
                    LfieldObjData {
                        exp: Box::new(Exp::Lvar(global_pvar)),
                        is_implicit: false,
                    },
                    field,
                    Typ::int(sil::typ::IKind::IInt),
                ),
                typ: Typ::int(sil::typ::IKind::IInt),
                loc: loc.clone(),
            }],
            loc,
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    #[test]
    fn test_workspace_relative_infer_uses_sibling_repo() {
        let ws_root = workspace_root().expect("workspace root should be resolvable");
        assert_eq!(
            workspace_relative_infer(&ws_root),
            ws_root.join("../infer/bin/infer")
        );
        assert_ne!(
            workspace_relative_infer(&ws_root),
            ws_root.join("infer/bin/infer")
        );
    }

    #[test]
    fn test_parse_capture_proc_metadata_extracts_no_return() {
        let debug_output = r#"no_ret
  source_file: nullptr_more.c
  defined: true
  attributes:
    { proc_name= no_ret
    ; translation_unit= nullptr_more.c
    ; formals= []
    ; is_defined= true
    ; loc= nullptr_more.c:137:1
    ; locals= []
    ; ret_type= void
    ; proc_id= no_ret }

will_not_return
  source_file: nullptr_more.c
  defined: false
  attributes:
    { proc_name= will_not_return
    ; translation_unit= nullptr_more.c
    ; formals= []
    ; is_no_return= true
    ; loc= nullptr_more.c:127:1
    ; locals= []
    ; ret_type= void
    ; proc_id= will_not_return }
"#;

        let metadata = parse_capture_proc_metadata(debug_output);

        assert!(metadata.is_no_return(
            "/tmp/some/path/nullptr_more.c",
            &Procname::c_from_string("will_not_return")
        ));
        assert!(!metadata.is_no_return(
            "/tmp/some/path/nullptr_more.c",
            &Procname::c_from_string("no_ret")
        ));
    }

    #[test]
    fn test_parse_capture_proc_metadata_extracts_cleanup_locals() {
        let debug_output = r#"cleanup_malloc_ok
  source_file: cleanup_attribute.c
  defined: true
  attributes:
    { proc_name= cleanup_malloc_ok
    ; translation_unit= cleanup_attribute.c
    ; formals= []
    ; is_defined= true
    ; loc= cleanup_attribute.c:16:1
    ; locals= [{ name= x; typ= int*; modify_in_block= false; is_declared_unused= false; is_structured_binding= false; has_cleanup_attribute= true }]
    ; ret_type= void
    ; proc_id= cleanup_malloc_ok }

plain_local
  source_file: cleanup_attribute.c
  defined: true
  attributes:
    { proc_name= plain_local
    ; translation_unit= cleanup_attribute.c
    ; formals= []
    ; is_defined= true
    ; loc= cleanup_attribute.c:40:1
    ; locals= [{ name= y; typ= int*; modify_in_block= false; is_declared_unused= false; is_structured_binding= false; has_cleanup_attribute= false }]
    ; ret_type= void
    ; proc_id= plain_local }
"#;

        let metadata = parse_capture_proc_metadata(debug_output);

        assert!(metadata.local_has_cleanup_attribute(
            "/tmp/some/path/cleanup_attribute.c",
            &Procname::c_from_string("cleanup_malloc_ok"),
            "x"
        ));
        assert!(!metadata.local_has_cleanup_attribute(
            "/tmp/some/path/cleanup_attribute.c",
            &Procname::c_from_string("plain_local"),
            "y"
        ));
    }

    #[test]
    fn test_apply_capture_proc_metadata_marks_procdesc() {
        let pname = Procname::c_from_string("will_not_return");
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(sil::procdesc::Procdesc::new(
            pname.clone(),
            sil::typ::Typ::void(),
            sil::location::Location::dummy(),
        ));

        let mut metadata = CaptureProcMetadata::default();
        metadata.insert_no_return("nullptr_more.c", "will_not_return");
        apply_capture_proc_metadata(&mut cfg, "/tmp/src/nullptr_more.c", &metadata);

        assert!(
            cfg.get_proc_desc(&pname)
                .expect("proc should exist")
                .is_no_return
        );
    }

    #[test]
    fn test_apply_capture_proc_metadata_marks_cleanup_locals() {
        let pname = Procname::c_from_string("cleanup_malloc_ok");
        let mut pdesc = sil::procdesc::Procdesc::new(
            pname.clone(),
            sil::typ::Typ::void(),
            sil::location::Location::dummy(),
        );
        pdesc.locals.push(sil::procdesc::VarData {
            name: sil::mangled::Mangled::from_string("x"),
            typ: sil::typ::Typ::int(sil::typ::IKind::IInt),
            modify_in_block: false,
            is_constexpr: false,
            is_declared_unused: false,
            is_structured_binding: false,
            has_cleanup_attribute: false,
        });

        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(pdesc);

        let mut metadata = CaptureProcMetadata::default();
        metadata.insert_cleanup_local("cleanup_attribute.c", "cleanup_malloc_ok", "x");
        apply_capture_proc_metadata(&mut cfg, "/tmp/src/cleanup_attribute.c", &metadata);

        assert!(
            cfg.get_proc_desc(&pname).expect("proc should exist").locals[0].has_cleanup_attribute
        );
    }

    #[test]
    fn test_default_rust_log_filter_adds_ondemand_info_for_trace_flag() {
        let cfg = config::InferConfig {
            trace_ondemand: true,
            ..config::InferConfig::default()
        };
        assert_eq!(default_rust_log_filter(&cfg), "warn,ondemand=info");
    }

    #[test]
    fn test_default_rust_log_filter_combines_pulse_and_ondemand() {
        let cfg = config::InferConfig {
            debug_level_analysis: 2,
            trace_ondemand: true,
            ..config::InferConfig::default()
        };
        assert_eq!(
            default_rust_log_filter(&cfg),
            "warn,pulse=trace,ondemand=info"
        );
    }

    #[test]
    fn test_procedures_filter_matches_proc_only() {
        let cfg = config::InferConfig {
            procedures_filter: Some("target".to_string()),
            ..config::InferConfig::default()
        };
        let filter = ProceduresFilter::compile(&cfg)
            .expect("filter should compile")
            .expect("filter should exist");

        assert!(filter.matches(
            &SourceFile::new("foo.c"),
            &Procname::c_from_string("target_proc"),
        ));
        assert!(!filter.matches(
            &SourceFile::new("foo.c"),
            &Procname::c_from_string("other_proc"),
        ));
    }

    #[test]
    fn test_procedures_filter_matches_source_and_proc() {
        let cfg = config::InferConfig {
            procedures_filter: Some("foo\\.c:target".to_string()),
            ..config::InferConfig::default()
        };
        let filter = ProceduresFilter::compile(&cfg)
            .expect("filter should compile")
            .expect("filter should exist");

        assert!(filter.matches(
            &SourceFile::new("/tmp/src/foo.c"),
            &Procname::c_from_string("target_proc"),
        ));
        assert!(!filter.matches(
            &SourceFile::new("/tmp/src/bar.c"),
            &Procname::c_from_string("target_proc"),
        ));
        assert!(!filter.matches(
            &SourceFile::new("/tmp/src/foo.c"),
            &Procname::c_from_string("other_proc"),
        ));
    }

    #[test]
    fn test_apply_procedures_filter_keeps_transitive_callees() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_calling_proc_in_file("caller", "callee", "foo.c"));
        cfg.add_proc_desc(mk_proc_in_file("callee", "bar.c"));
        cfg.add_proc_desc(mk_proc_in_file("unrelated", "baz.c"));

        let filter = ProceduresFilter {
            raw: "caller".to_string(),
            source_file_regex: None,
            proc_name_regex: Some(Regex::new("caller").expect("regex should compile")),
        };

        let selection =
            apply_procedures_filter(&cfg, &filter, true).expect("filter should match caller");

        assert_eq!(selection.matched_roots, 1);
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("caller"))
            .is_some());
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("callee"))
            .is_some());
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("unrelated"))
            .is_none());
    }

    #[test]
    fn test_apply_procedures_filter_without_transitive_callees_keeps_roots_only() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_calling_proc_in_file("caller", "callee", "foo.c"));
        cfg.add_proc_desc(mk_proc_in_file("callee", "bar.c"));

        let filter = ProceduresFilter {
            raw: "caller".to_string(),
            source_file_regex: None,
            proc_name_regex: Some(Regex::new("caller").expect("regex should compile")),
        };

        let selection =
            apply_procedures_filter(&cfg, &filter, false).expect("filter should match caller");

        assert_eq!(selection.matched_roots, 1);
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("caller"))
            .is_some());
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("callee"))
            .is_none());
    }

    #[test]
    fn test_collect_global_initializer_refs_keeps_rooted_global_load() {
        let pdesc = mk_global_load_proc_in_file("caller", "g", "foo.c");
        let mut refs = std::collections::HashSet::new();
        for (_node_id, instr) in pdesc.iter_instrs() {
            collect_global_initializer_refs(instr, &mut refs);
        }

        assert_eq!(
            refs,
            std::collections::HashSet::from([Procname::c_from_string(
                "__infer_globals_initializer_g",
            )])
        );
    }

    #[test]
    fn test_apply_procedures_filter_keeps_implicit_global_initializer_callee() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_global_load_proc_in_file("caller", "g", "foo.c"));
        cfg.add_proc_desc(mk_proc_in_file("__infer_globals_initializer_g", "foo.c"));
        cfg.add_proc_desc(mk_proc_in_file("unrelated", "baz.c"));

        let filter = ProceduresFilter {
            raw: "caller".to_string(),
            source_file_regex: None,
            proc_name_regex: Some(Regex::new("caller").expect("regex should compile")),
        };

        let selection =
            apply_procedures_filter(&cfg, &filter, true).expect("filter should match caller");

        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("caller"))
            .is_some());
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("__infer_globals_initializer_g"))
            .is_some());
        assert!(selection
            .cfg
            .get_proc_desc(&Procname::c_from_string("unrelated"))
            .is_none());
    }
}
