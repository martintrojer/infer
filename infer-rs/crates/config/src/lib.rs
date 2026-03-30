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
/// All fields have sane defaults matching OCaml's behavior.
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

    /// Disable inter-procedural analysis in Pulse.
    /// OCaml: `--pulse-intraprocedural-only` (default false)
    #[serde(rename = "pulse-intraprocedural-only")]
    pub pulse_intraprocedural_only: bool,

    /// Run only the Pulse checker.
    /// OCaml: `--pulse-only` (default false)
    #[serde(rename = "pulse-only")]
    pub pulse_only: bool,

    /// Run only the liveness checker.
    /// OCaml: `--liveness-only` (default false)
    #[serde(rename = "liveness-only")]
    pub liveness_only: bool,

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

    // ---- Output ----
    /// Suppress progress output.
    #[serde(rename = "quiet")]
    pub quiet: bool,
}

impl Default for InferConfig {
    fn default() -> Self {
        Self {
            pulse_max_disjuncts: 20,
            pulse_widen_threshold: 3,
            pulse_intraprocedural_only: false,
            pulse_only: false,
            liveness_only: false,
            max_widens: 10_000,
            debug_level_analysis: 0,
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
        assert_eq!(config.max_widens, 10_000);
        assert!(!config.pulse_intraprocedural_only);
        assert!(!config.pulse_only);
        assert!(!config.liveness_only);
        assert!(!config.quiet);
    }

    #[test]
    fn test_from_json_known_fields() {
        let json = r#"{"pulse-max-disjuncts": 50, "quiet": true}"#;
        let config = InferConfig::from_json(json);
        assert_eq!(config.pulse_max_disjuncts, 50);
        assert!(config.quiet);
        // Other fields should be defaults
        assert_eq!(config.max_widens, 10_000);
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
    fn test_find_inferconfig_nonexistent() {
        let path = InferConfig::find_inferconfig(Path::new("/nonexistent/path"));
        // Should not find anything (unless INFERCONFIG env is set)
        if std::env::var(INFERCONFIG_ENV).is_err() {
            assert!(path.is_none());
        }
    }
}
