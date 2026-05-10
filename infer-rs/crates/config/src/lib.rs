// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Configuration for infer-rs analysis.
//!
//! Mirrors OCaml's `Config.ml` — a single struct holding all analysis
//! configuration. Values can come from:
//! 1. Defaults (matching OCaml's defaults)
//! 2. `.inferconfig` JSON file (searched upward from CWD)
//! 3. `INFERCONFIG` environment variable (path to config file)
//! 4. Command-line arguments (highest priority)
//!
//! Unknown keys in `.inferconfig` are silently ignored, allowing
//! `.inferconfig` files shared with OCaml infer to work without
//! modification.

pub mod manifest;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// The `.inferconfig` filename.
pub const INFERCONFIG_FILE: &str = ".inferconfig";

/// Environment variable for custom `.inferconfig` path.
pub const INFERCONFIG_ENV: &str = "INFERCONFIG";

/// Global config instance. Set once at startup via `init()`, read anywhere via `get()`.
static GLOBAL_CONFIG: OnceLock<InferConfig> = OnceLock::new();

/// Initialize the global config. Call once at program startup.
///
/// Panics if called more than once (use `init_or_default` for test-friendly init).
pub fn init(config: InferConfig) {
    GLOBAL_CONFIG
        .set(config)
        .expect("config::init called more than once");
}

/// Initialize with defaults if not already set. Safe to call multiple times.
/// Useful in tests and library code.
pub fn init_or_default() {
    let _ = GLOBAL_CONFIG.set(InferConfig::default());
}

/// Get a reference to the global config.
///
/// Returns defaults if `init()` was never called — this makes library
/// code work without requiring explicit initialization.
pub fn get() -> &'static InferConfig {
    GLOBAL_CONFIG.get_or_init(InferConfig::default)
}

/// All analysis configuration.
///
/// Fields match OCaml's `Config.ml` flags where applicable.
/// Defaults follow OCaml unless a field documents an intentional Rust-side
/// divergence.
/// NOTE: The `#[serde(rename = "...")]` attributes are the single source
/// of truth for flag names. CLI flags in `cli/main.rs` must use the same
/// names in their `#[arg(long = "...")]` attributes. The names match
/// OCaml's `Config.ml` flag names (hyphenated, e.g. `pulse-max-disjuncts`).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct InferConfig {
    // ---- Pulse ----
    /// Maximum number of disjuncts per program point.
    /// OCaml: `--pulse-max-disjuncts` (default 20)
    #[serde(rename = "pulse-max-disjuncts")]
    pub pulse_max_disjuncts: usize,

    /// Widening threshold for Pulse (stop exploring after N loop iterations).
    /// OCaml: `--pulse-widen-threshold` (default 3)
    #[serde(rename = "pulse-widen-threshold")]
    pub pulse_widen_threshold: usize,

    /// Larger CFGs than this are skipped in Pulse.
    /// OCaml: `--pulse-max-cfg-size` (default 15000)
    #[serde(rename = "pulse-max-cfg-size")]
    pub pulse_max_cfg_size: usize,

    /// Disable inter-procedural analysis in Pulse.
    /// OCaml: `--pulse-intraprocedural-only` (default false)
    #[serde(rename = "pulse-intraprocedural-only")]
    pub pulse_intraprocedural_only: bool,

    /// Maximum number of recently modified heap edges retained per address.
    /// OCaml flag: `--pulse-recency-limit` (default 32).
    /// Rust leaves this unset by default to preserve the current
    /// correctness-positive behavior until that precision tradeoff is chosen
    /// explicitly.
    #[serde(rename = "pulse-recency-limit")]
    pub pulse_recency_limit: Option<usize>,

    /// At each Pulse CFG node exit, drop `Var::LogicalVar(_)` post-stack
    /// bindings whose Ident is not live-out of the node, mirroring the
    /// effect of OCaml's `Metadata (ExitScope ids)` cleanup that the
    /// textual exporter currently strips. Defaults to `true` because the
    /// exported textual SIL never carries the cleanup metadata, and
    /// without this pass the post stack accumulates every `n$N:NN`
    /// logical-temp for the entire procedure, dominating per-disjunct
    /// unique-value count on long encryption-style basic blocks. Tests
    /// that exercise the per-instruction analysis directly without the
    /// liveness-driven cleanup can disable it via `.inferconfig`.
    #[serde(rename = "pulse-drop-dead-logical-vars", default = "default_true")]
    pub pulse_drop_dead_logical_vars: bool,

    /// In large stored intermediate Pulse states, also prune high-volume
    /// formula facts (`intervals`, `is_int`, `term_value_index`,
    /// `fn_app_eqs`, atoms over only-unreachable vars) and dead
    /// `const_cache` entries whose values are no longer reachable from the
    /// retained post graph (after transitive expansion across
    /// `linear_eqs` / `fn_app_eqs`). Load-bearing canonicalization
    /// families (`var_eqs`, `linear_eqs`, `term_eqs`) are deliberately
    /// preserved. Defaults to `false`: it reduces memory on DES-family
    /// procedures but costs wall time on capped whole-program OpenSSL
    /// runs. Enable when memory headroom is more important than wall
    /// time.
    #[serde(rename = "pulse-intermediate-formula-gc")]
    pub pulse_intermediate_formula_gc: bool,

    /// Maximum delta in process peak RSS (megabytes) that a single Pulse
    /// procedure analysis is allowed to consume before being aborted with
    /// the partial state retained as a summary. Defaults to `2048` (2 GB)
    /// so the binary is usable out of the box without explicitly tuning
    /// flags; pass `--pulse-max-heap-mb 0` (or set the field to `Some(0)`)
    /// to effectively disable the cap.
    ///
    /// Cross-ref: OCaml `--pulse-max-heap` checks `Gc.quick_stat ()`
    /// `heap_words` before every instruction; we use `getrusage`
    /// `ru_maxrss` since Rust does not expose a per-allocator heap-words
    /// counter as cheaply.
    #[serde(rename = "pulse-max-heap-mb", default = "default_pulse_max_heap_mb")]
    pub pulse_max_heap_mb: Option<usize>,

    /// Maximum wall-clock seconds a single Pulse procedure analysis is
    /// allowed to consume before being aborted (with the partial state
    /// retained as a summary). Defaults to `120` so the binary is usable
    /// out of the box without explicitly tuning flags; complements
    /// `pulse_max_heap_mb` for procedures whose fixpoint does not
    /// converge quickly but whose RSS stays low (e.g., recursive
    /// bsearch-family procedures with thousands of WTO revisits per
    /// loop body).
    #[serde(
        rename = "pulse-max-wall-secs",
        default = "default_pulse_max_wall_secs"
    )]
    pub pulse_max_wall_secs: Option<u64>,

    /// Run only the Pulse checker.
    /// OCaml: `--pulse-only` (default false)
    #[serde(rename = "pulse-only")]
    pub pulse_only: bool,

    /// Run only the liveness checker.
    /// OCaml: `--liveness-only` (default false)
    #[serde(rename = "liveness-only")]
    pub liveness_only: bool,

    /// Report suppressed issues as distinguished test-only reports.
    /// OCaml: `--pulse-report-issues-for-tests` (default false)
    #[serde(rename = "pulse-report-issues-for-tests")]
    pub pulse_report_issues_for_tests: bool,

    /// Force analysis to continue past known calls that produced no
    /// ContinueProgram summary, treating the callee as an unknown call when
    /// OCaml would do the same.
    /// OCaml: `--pulse-force-continue` (default true)
    #[serde(rename = "pulse-force-continue")]
    pub pulse_force_continue: bool,

    /// Regex of methods that should be modelled as wrappers to `free(3)`.
    /// OCaml: `--pulse-model-free-pattern` (default none)
    #[serde(rename = "pulse-model-free-pattern")]
    pub pulse_model_free_pattern: Option<String>,

    /// Regex of methods that should be modelled as wrappers to `malloc(3)`.
    /// OCaml: `--pulse-model-malloc-pattern` (default none)
    #[serde(rename = "pulse-model-malloc-pattern")]
    pub pulse_model_malloc_pattern: Option<String>,

    /// Regex of methods that should be modelled as wrappers to `realloc(3)`.
    /// OCaml: `--pulse-model-realloc-pattern` (default none)
    #[serde(rename = "pulse-model-realloc-pattern")]
    pub pulse_model_realloc_pattern: Option<String>,

    /// Exact procnames that should be modelled as non-returning calls.
    /// OCaml: `--pulse-model-abort` (default empty)
    #[serde(rename = "pulse-model-abort")]
    pub pulse_model_abort: Vec<String>,

    /// Exact procnames that should be treated as unreachable.
    /// OCaml: `--pulse-model-unreachable` (default empty)
    #[serde(rename = "pulse-model-unreachable")]
    pub pulse_model_unreachable: Vec<String>,

    /// Regex of methods modelled as returning a non-null value.
    /// OCaml: `--pulse-model-return-nonnull` (default none)
    #[serde(rename = "pulse-model-return-nonnull")]
    pub pulse_model_return_nonnull: Option<String>,

    /// Regex of methods modelled as returning the receiver (`this` / `self`).
    /// OCaml: `--pulse-model-return-this` (default none)
    #[serde(rename = "pulse-model-return-this")]
    pub pulse_model_return_this: Option<String>,

    /// Regex of methods modelled as returning the first source-language
    /// argument. For Java/ObjC instance methods this is SIL actual index 1.
    /// OCaml: `--pulse-model-return-first-arg` (default none)
    #[serde(rename = "pulse-model-return-first-arg")]
    pub pulse_model_return_first_arg: Option<String>,

    /// Regex of methods modelled as returning either null or a fresh
    /// non-null value.
    /// OCaml: `--pulse-model-return-nullable` (default none)
    #[serde(rename = "pulse-model-return-nullable")]
    pub pulse_model_return_nullable: Option<String>,

    /// Regex of methods to skip and treat as unknown calls.
    /// OCaml: `--pulse-model-skip-pattern` (default none)
    #[serde(rename = "pulse-model-skip-pattern")]
    pub pulse_model_skip_pattern: Option<String>,

    /// Regexes of methods to model as unknown pure calls. These should keep
    /// pointer actuals stable and return a FunctionApplication result.
    /// OCaml: `--pulse-model-unknown-pure` (default empty)
    #[serde(rename = "pulse-model-unknown-pure")]
    pub pulse_model_unknown_pure: Vec<String>,

    // ---- Abstract interpretation ----
    /// Maximum number of widenings before the fixpoint engine gives up.
    /// OCaml: hardcoded `Config.max_widens = 10000`
    #[serde(rename = "max-widens")]
    pub max_widens: usize,

    // ---- Debug ----
    /// Analysis debug level. 0=quiet, 1=medium, 2=verbose.
    /// OCaml: `--debug-level-analysis` (default 0)
    /// At level ≥1: per-instruction trace with disjunct counts.
    /// At level ≥2: full formula/heap state dumps.
    #[serde(rename = "debug-level-analysis")]
    pub debug_level_analysis: u8,

    /// Emit debug information for the on-demand analysis scheduler.
    /// OCaml: `--trace-ondemand` (default false)
    #[serde(rename = "trace-ondemand")]
    pub trace_ondemand: bool,

    /// Regex filter for selecting procedures to analyze/debug.
    /// OCaml: `--procedures-filter` (default none)
    #[serde(rename = "procedures-filter")]
    pub procedures_filter: Option<String>,

    /// Specific CFG node IDs whose final retained fixpoint states should be
    /// logged after analysis.
    ///
    /// Debug-only aid for comparing Rust's WTO/disjunctive retention against
    /// OCaml on hot procedures.
    #[serde(rename = "debug-fixpoint-nodes")]
    pub debug_fixpoint_nodes: Vec<u32>,

    // ---- Parallelism ----
    /// Number of parallel analysis jobs. None = use all available CPUs.
    /// OCaml: `-j` / `--jobs` (default: number of CPUs)
    #[serde(rename = "jobs")]
    pub jobs: Option<usize>,

    // ---- Output ----
    /// Suppress progress output.
    #[serde(rename = "quiet")]
    pub quiet: bool,
}

fn default_true() -> bool {
    true
}

/// Default `pulse-max-heap-mb`: 2 GB. Conservative enough not to
/// abort the bulk of well-behaved procedures (the largest
/// per-procedure peaks we have seen on the 74-file partial OpenSSL
/// corpus are `~830 MB` for `fcrypt_body` and similar) but tight
/// enough to keep runaway block-cipher procedures from exhausting
/// host memory.
fn default_pulse_max_heap_mb() -> Option<usize> {
    Some(2048)
}

/// Default `pulse-max-wall-secs`: 60s. Most procedures complete in
/// well under a second; the long-tail (`DES_ofb_encrypt`,
/// `DES_ede3_cbcm_encrypt`, etc.) takes minutes without the cap.
/// Setting this to `60s` keeps the binary usable out of the box on
/// whole-program OpenSSL-sized corpora while still giving ordinary
/// procedures ample time to converge.
fn default_pulse_max_wall_secs() -> Option<u64> {
    Some(60)
}

impl Default for InferConfig {
    fn default() -> Self {
        Self {
            pulse_max_disjuncts: 20,
            pulse_widen_threshold: 3,
            pulse_max_cfg_size: 15_000,
            pulse_drop_dead_logical_vars: true,
            pulse_intermediate_formula_gc: false,
            pulse_max_heap_mb: default_pulse_max_heap_mb(),
            pulse_max_wall_secs: default_pulse_max_wall_secs(),
            pulse_intraprocedural_only: false,
            pulse_recency_limit: None,
            pulse_only: false,
            liveness_only: false,
            pulse_report_issues_for_tests: false,
            pulse_force_continue: true,
            pulse_model_free_pattern: None,
            pulse_model_malloc_pattern: None,
            pulse_model_realloc_pattern: None,
            pulse_model_abort: Vec::new(),
            pulse_model_unreachable: Vec::new(),
            pulse_model_return_nonnull: None,
            pulse_model_return_this: None,
            pulse_model_return_first_arg: None,
            pulse_model_return_nullable: None,
            pulse_model_skip_pattern: None,
            pulse_model_unknown_pure: Vec::new(),
            max_widens: 10_000,
            debug_level_analysis: 0,
            trace_ondemand: false,
            procedures_filter: None,
            debug_fixpoint_nodes: Vec::new(),
            jobs: None,
            quiet: false,
        }
    }
}

impl InferConfig {
    /// Load config from `.inferconfig`, environment, and defaults.
    ///
    /// Search order for `.inferconfig`:
    /// 1. `INFERCONFIG` environment variable
    /// 2. Walk upward from `start_dir` looking for `.inferconfig`
    ///
    /// Unknown fields in the JSON are ignored (via `flatten` + skip),
    /// allowing shared `.inferconfig` files with OCaml infer.
    pub fn load(start_dir: &Path) -> Self {
        let path = Self::find_inferconfig(start_dir);
        match path {
            Some(p) => Self::load_from_file(&p),
            None => Self::default(),
        }
    }

    /// Load config from a specific file path.
    pub fn load_from_file(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => Self::from_json(&content),
            Err(_) => Self::default(),
        }
    }

    /// Parse config from a JSON string.
    ///
    /// Unknown fields are silently ignored (via `#[serde(default)]` without
    /// `deny_unknown_fields`) so that `.inferconfig` files shared with
    /// OCaml infer work without modification.
    ///
    /// The `#[serde(rename)]` attributes on each field define the canonical
    /// JSON key names, matching OCaml's `Config.ml` flag names.
    pub fn from_json(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    /// Find the `.inferconfig` file by searching upward from `start_dir`.
    fn find_inferconfig(start_dir: &Path) -> Option<PathBuf> {
        // Check environment variable first
        if let Ok(env_path) = std::env::var(INFERCONFIG_ENV) {
            let p = PathBuf::from(&env_path);
            if p.exists() {
                return Some(p);
            }
        }

        // Walk upward from start_dir
        let mut dir = start_dir.to_path_buf();
        loop {
            let candidate = dir.join(INFERCONFIG_FILE);
            if candidate.exists() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let config = InferConfig::default();
        assert_eq!(config.pulse_max_disjuncts, 20);
        assert_eq!(config.pulse_widen_threshold, 3);
        assert_eq!(config.pulse_max_cfg_size, 15_000);
        assert!(!config.pulse_intermediate_formula_gc);
        assert_eq!(config.pulse_max_heap_mb, Some(2048));
        assert_eq!(config.pulse_max_wall_secs, Some(60));
        assert_eq!(config.max_widens, 10_000);
        assert!(!config.pulse_intraprocedural_only);
        assert_eq!(config.pulse_recency_limit, None);
        assert!(!config.pulse_only);
        assert!(!config.liveness_only);
        assert!(!config.pulse_report_issues_for_tests);
        assert!(config.pulse_force_continue);
        assert!(config.pulse_model_free_pattern.is_none());
        assert!(config.pulse_model_malloc_pattern.is_none());
        assert!(config.pulse_model_realloc_pattern.is_none());
        assert!(config.pulse_model_abort.is_empty());
        assert!(config.pulse_model_unreachable.is_empty());
        assert!(config.pulse_model_return_nonnull.is_none());
        assert!(config.pulse_model_return_this.is_none());
        assert!(config.pulse_model_return_first_arg.is_none());
        assert!(config.pulse_model_return_nullable.is_none());
        assert!(config.pulse_model_skip_pattern.is_none());
        assert!(config.pulse_model_unknown_pure.is_empty());
        assert!(!config.trace_ondemand);
        assert!(config.procedures_filter.is_none());
        assert!(config.debug_fixpoint_nodes.is_empty());
        assert!(!config.quiet);
    }

    #[test]
    fn test_from_json_known_fields() {
        let json = r#"{"pulse-max-disjuncts": 50, "pulse-max-cfg-size": 1234, "pulse-recency-limit": 17, "quiet": true, "pulse-force-continue": false}"#;
        let config = InferConfig::from_json(json);
        assert_eq!(config.pulse_max_disjuncts, 50);
        assert_eq!(config.pulse_max_cfg_size, 1234);
        assert_eq!(config.pulse_recency_limit, Some(17));
        assert!(config.quiet);
        assert!(!config.pulse_force_continue);
        // Other fields should be defaults
        assert_eq!(config.max_widens, 10_000);
    }

    #[test]
    fn test_from_json_custom_model_patterns() {
        let json = r#"{
            "pulse-model-free-pattern": "^my_free$",
            "pulse-model-malloc-pattern": "\\(my\\|a\\)_malloc",
            "pulse-model-realloc-pattern": "my_realloc",
            "pulse-model-abort": ["ns1::ns2::fun_abort"],
            "pulse-model-unreachable": ["handle_failure"],
            "pulse-report-issues-for-tests": true,
            "pulse-force-continue": false,
            "pulse-model-return-nonnull": "Handle::get",
            "pulse-model-return-this": "ModelClass.initWith:",
            "pulse-model-return-first-arg": "release\\|.*release:",
            "pulse-model-return-nullable": "dangerous",
            "pulse-model-skip-pattern": "skip_model::SkipAll::.*\\|.*SkipSome<.*>::skip_me",
            "pulse-model-unknown-pure": ["get_value_pure", "read_only_helper"]
        }"#;
        let config = InferConfig::from_json(json);
        assert_eq!(
            config.pulse_model_free_pattern.as_deref(),
            Some("^my_free$")
        );
        assert_eq!(
            config.pulse_model_malloc_pattern.as_deref(),
            Some("\\(my\\|a\\)_malloc")
        );
        assert_eq!(
            config.pulse_model_realloc_pattern.as_deref(),
            Some("my_realloc")
        );
        assert_eq!(config.pulse_model_abort, vec!["ns1::ns2::fun_abort"]);
        assert_eq!(config.pulse_model_unreachable, vec!["handle_failure"]);
        assert_eq!(
            config.pulse_model_return_nonnull.as_deref(),
            Some("Handle::get")
        );
        assert_eq!(
            config.pulse_model_return_this.as_deref(),
            Some("ModelClass.initWith:")
        );
        assert_eq!(
            config.pulse_model_return_first_arg.as_deref(),
            Some("release\\|.*release:")
        );
        assert_eq!(
            config.pulse_model_return_nullable.as_deref(),
            Some("dangerous")
        );
        assert_eq!(
            config.pulse_model_skip_pattern.as_deref(),
            Some("skip_model::SkipAll::.*\\|.*SkipSome<.*>::skip_me")
        );
        assert_eq!(
            config.pulse_model_unknown_pure,
            vec!["get_value_pure", "read_only_helper"]
        );
        assert!(config.pulse_report_issues_for_tests);
        assert!(!config.pulse_force_continue);
        assert!(!config.trace_ondemand);
    }

    #[test]
    fn test_from_json_unknown_fields_ignored() {
        // OCaml-only fields should be silently ignored
        let json = r#"{
            "pulse-max-disjuncts": 30,
            "buck": true,
            "debug-exceptions": true,
            "pulse-taint-config": "something",
            "no-progress-bar": true
        }"#;
        let config = InferConfig::from_json(json);
        assert_eq!(config.pulse_max_disjuncts, 30);
    }

    #[test]
    fn test_from_json_invalid_json() {
        let config = InferConfig::from_json("not valid json {{{");
        // Should return defaults
        assert_eq!(config.pulse_max_disjuncts, 20);
    }

    #[test]
    fn test_from_json_empty() {
        let config = InferConfig::from_json("{}");
        assert_eq!(config.pulse_max_disjuncts, 20);
    }

    #[test]
    fn test_from_json_trace_ondemand() {
        let config = InferConfig::from_json(r#"{"trace-ondemand": true}"#);
        assert!(config.trace_ondemand);
    }

    #[test]
    fn test_from_json_procedures_filter() {
        let config = InferConfig::from_json(r#"{"procedures-filter": "foo\\.c:target"}"#);
        assert_eq!(config.procedures_filter.as_deref(), Some("foo\\.c:target"));
    }

    #[test]
    fn test_from_json_debug_fixpoint_nodes() {
        let config = InferConfig::from_json(r#"{"debug-fixpoint-nodes": [18, 20, 22]}"#);
        assert_eq!(config.debug_fixpoint_nodes, vec![18, 20, 22]);
    }

    #[test]
    fn test_from_json_pulse_intermediate_formula_gc() {
        let config = InferConfig::from_json(r#"{"pulse-intermediate-formula-gc": true}"#);
        assert!(config.pulse_intermediate_formula_gc);
    }

    #[test]
    fn test_find_inferconfig_nonexistent() {
        let path = InferConfig::find_inferconfig(Path::new("/nonexistent/path"));
        // Should not find anything (unless INFERCONFIG env is set)
        if std::env::var(INFERCONFIG_ENV).is_err() {
            assert!(path.is_none());
        }
    }
}
