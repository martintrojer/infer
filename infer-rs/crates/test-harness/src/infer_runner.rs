// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Run OCaml `infer` on `.sil` (Textual) files and collect results.
//!
//! This module provides [`InferRunner`], which invokes the OCaml `infer` binary
//! to capture and analyze `.sil` files, then makes the results available for
//! comparison with the Rust pipeline.
//!
//! ## Typical workflow
//!
//! ```text
//! .sil file
//!     |
//!     +---> [OCaml: infer --capture-textual + analyze] ---> issues_ocaml (report.json)
//!     |
//!     +---> [Rust: parse → to_sil → analyze] ---> issues_rust
//!     |
//!     v
//! [Compare results]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Locates the `infer` binary relative to the workspace root.
///
/// Tries these locations in order:
/// 1. `INFER_BIN` environment variable
/// 2. `../infer/bin/infer` relative to the workspace root
/// 3. `infer` on `PATH`
pub fn find_infer_binary() -> Option<PathBuf> {
    // 1. Environment variable
    if let Ok(bin) = std::env::var("INFER_BIN") {
        let p = PathBuf::from(bin);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Relative to workspace root (infer-rs/infer-rs/../infer/bin/infer)
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/
        .and_then(|p| p.parent()); // infer-rs/
    if let Some(root) = workspace_root {
        let relative = root.join("../infer/bin/infer");
        if relative.exists() {
            return Some(relative);
        }
    }

    // 3. On PATH
    if Command::new("infer")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        return Some(PathBuf::from("infer"));
    }

    None
}

/// Result of running OCaml infer on a set of `.sil` files.
#[derive(Debug)]
pub struct InferResult {
    /// The infer-out directory containing results.
    pub out_dir: PathBuf,
    /// Whether infer exited successfully.
    pub success: bool,
    /// Combined stdout.
    pub stdout: String,
    /// Combined stderr.
    pub stderr: String,
}

/// An issue reported by OCaml infer (parsed from `report.json`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct InferIssue {
    pub bug_type: String,
    pub qualifier: String,
    pub file: String,
    pub line: u32,
    pub procedure: String,
}

/// Counter for unique temp directory names within a process.
static DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Runner for invoking the OCaml `infer` binary.
pub struct InferRunner {
    infer_bin: PathBuf,
    /// Temporary directory for infer-out. Cleaned up on drop if `cleanup` is true.
    tmp_dir: PathBuf,
    cleanup: bool,
}

impl InferRunner {
    /// Create a new runner. Returns `None` if the infer binary cannot be found.
    pub fn new() -> Option<Self> {
        let infer_bin = find_infer_binary()?;
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_dir =
            std::env::temp_dir().join(format!("infer_rs_test_{}_{n}", std::process::id()));
        Some(Self {
            infer_bin,
            tmp_dir,
            cleanup: true,
        })
    }

    /// Create a runner with a specific infer binary path.
    pub fn with_binary(infer_bin: PathBuf) -> Self {
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_dir =
            std::env::temp_dir().join(format!("infer_rs_test_{}_{n}", std::process::id()));
        Self {
            infer_bin,
            tmp_dir,
            cleanup: true,
        }
    }

    /// Keep the output directory after the runner is dropped (useful for debugging).
    pub fn keep_output(mut self) -> Self {
        self.cleanup = false;
        self
    }

    /// Capture and analyze `.sil` files with the given checker.
    ///
    /// Runs: `infer --<checker>-only --capture-textual f1.sil --capture-textual f2.sil ...`
    pub fn capture_and_analyze(
        &self,
        sil_files: &[&Path],
        checker: &str,
    ) -> Result<InferResult, String> {
        let out_dir = self.tmp_dir.join("infer-out");

        let mut cmd = Command::new(&self.infer_bin);
        cmd.arg("--quiet")
            .arg("--no-progress-bar")
            .arg(format!("--{checker}-only"))
            .arg("--no-pulse-force-continue")
            .arg("-o")
            .arg(&out_dir);

        for sil_file in sil_files {
            cmd.arg("--capture-textual").arg(sil_file);
        }

        let output = cmd
            .output()
            .map_err(|e| format!("failed to run infer: {e}"))?;

        Ok(InferResult {
            out_dir,
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Capture a C/C++ source file and dump Textual `.sil` output.
    ///
    /// Runs: `infer capture --dump-textual -o <out> -- clang -c <source>`
    ///
    /// Returns the path to the generated `.sil` file (next to the source file copy).
    /// The `.sil` content has location annotations stripped so the Rust parser can
    /// handle it.
    pub fn dump_textual_for_c(&self, source: &Path) -> Result<PathBuf, String> {
        let out_dir = self.tmp_dir.join("capture-out");
        let _ = std::fs::create_dir_all(&self.tmp_dir);

        // Copy source into tmp_dir so the .sil is generated there
        let src_copy = self.tmp_dir.join(source.file_name().unwrap());
        std::fs::copy(source, &src_copy).map_err(|e| format!("failed to copy source: {e}"))?;

        let output = Command::new(&self.infer_bin)
            .current_dir(&self.tmp_dir) // keep .o files in tmp dir
            .arg("capture")
            .arg("--dump-textual")
            .arg("-j")
            .arg("1")
            .arg("-o")
            .arg(&out_dir)
            .arg("--")
            .arg("clang")
            .arg("-c")
            .arg(&src_copy)
            .output()
            .map_err(|e| format!("failed to run infer capture: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "infer capture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // The .sil file is generated next to the source file
        let sil_path = src_copy.with_extension("sil");
        if !sil_path.exists() {
            return Err(format!("expected .sil not found at {}", sil_path.display()));
        }

        Ok(sil_path)
    }

    /// Run Pulse analysis on a C source file and return the summary JSON path.
    ///
    /// Runs: `infer -j 1 --pulse-only -o <out> -- clang -c <source>`
    /// Then: `infer debug -j 1 --dump-json-summaries -o <out>`
    ///
    /// Returns path to `all_summaries.json`.
    pub fn analyze_pulse_c(&self, source: &Path) -> Result<PathBuf, String> {
        let out_dir = self.tmp_dir.join("pulse-out");
        let _ = std::fs::create_dir_all(&self.tmp_dir);

        let src_copy = self.tmp_dir.join(source.file_name().unwrap());
        std::fs::copy(source, &src_copy).map_err(|e| format!("failed to copy source: {e}"))?;

        // Run pulse analysis
        let output = Command::new(&self.infer_bin)
            .current_dir(&self.tmp_dir)
            .arg("-j")
            .arg("1")
            .arg("--pulse-only")
            .arg("-o")
            .arg(&out_dir)
            .arg("--")
            .arg("clang")
            .arg("-c")
            .arg(&src_copy)
            .output()
            .map_err(|e| format!("failed to run infer: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "infer analysis failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        // Dump summaries
        let output = Command::new(&self.infer_bin)
            .current_dir(&self.tmp_dir)
            .arg("debug")
            .arg("-j")
            .arg("1")
            .arg("--dump-json-summaries")
            .arg("-o")
            .arg(&out_dir)
            .output()
            .map_err(|e| format!("failed to run infer debug: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "infer debug failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let summary_path = out_dir.join("all_summaries.json");
        if !summary_path.exists() {
            return Err("all_summaries.json not found".to_string());
        }

        Ok(summary_path)
    }

    /// Capture `.sil` files without analyzing (capture only).
    ///
    /// Useful for testing that Textual files are valid according to OCaml infer.
    pub fn capture_only(&self, sil_files: &[&Path]) -> Result<InferResult, String> {
        let out_dir = self.tmp_dir.join("infer-out");

        let mut cmd = Command::new(&self.infer_bin);
        cmd.arg("capture").arg("--quiet").arg("-o").arg(&out_dir);

        for sil_file in sil_files {
            cmd.arg("--capture-textual").arg(sil_file);
        }

        let output = cmd
            .output()
            .map_err(|e| format!("failed to run infer: {e}"))?;

        Ok(InferResult {
            out_dir,
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

impl Drop for InferRunner {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_dir_all(&self.tmp_dir);
        }
    }
}

impl InferResult {
    /// Parse `report.json` from the infer output directory.
    ///
    /// Returns issues sorted by (file, line, bug_type) for deterministic comparison.
    pub fn parse_report(&self) -> Result<Vec<InferIssue>, String> {
        let report_path = self.out_dir.join("report.json");
        if !report_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&report_path)
            .map_err(|e| format!("failed to read report.json: {e}"))?;

        parse_report_json(&content)
    }
}

/// Parse an infer `report.json` string into a sorted list of issues.
pub fn parse_report_json(json: &str) -> Result<Vec<InferIssue>, String> {
    let raw: Vec<serde_json::Value> =
        serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;

    let mut issues: Vec<InferIssue> = raw
        .iter()
        .filter_map(|obj| {
            let bug_type = obj.get("bug_type")?.as_str()?.to_string();
            Some(InferIssue {
                bug_type,
                qualifier: obj
                    .get("qualifier")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                file: obj
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                line: obj.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                procedure: obj
                    .get("procedure")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect();

    issues.sort();
    Ok(issues)
}

/// Compare two sets of issues, ignoring qualifier text (which can differ).
///
/// Returns a map of `(file, line, bug_type)` → `(in_left, in_right)`.
pub fn compare_issues(
    left: &[InferIssue],
    right: &[InferIssue],
) -> BTreeMap<(String, u32, String), (bool, bool)> {
    let mut result = BTreeMap::new();

    for issue in left {
        let key = (issue.file.clone(), issue.line, issue.bug_type.clone());
        result.entry(key).or_insert((false, false)).0 = true;
    }

    for issue in right {
        let key = (issue.file.clone(), issue.line, issue.bug_type.clone());
        result.entry(key).or_insert((false, false)).1 = true;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_infer_binary() {
        let result = find_infer_binary();
        // In this repo the binary should be available
        if let Some(path) = result {
            assert!(
                path.exists(),
                "returned path should exist: {}",
                path.display()
            );
        }
    }

    #[test]
    fn test_parse_report_json_empty() {
        let issues = parse_report_json("[]").unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn test_parse_report_json() {
        let json = r#"[
            {
                "bug_type": "NULL_DEREFERENCE",
                "qualifier": "null dereference of x",
                "file": "test.c",
                "line": 10,
                "procedure": "foo"
            },
            {
                "bug_type": "MEMORY_LEAK",
                "qualifier": "leaked memory",
                "file": "test.c",
                "line": 20,
                "procedure": "bar"
            }
        ]"#;
        let issues = parse_report_json(json).unwrap();
        assert_eq!(issues.len(), 2);
        // Sorted by derived Ord on InferIssue (bug_type, qualifier, file, line, procedure)
        assert_eq!(issues[0].bug_type, "MEMORY_LEAK");
        assert_eq!(issues[0].line, 20);
        assert_eq!(issues[1].bug_type, "NULL_DEREFERENCE");
        assert_eq!(issues[1].line, 10);
    }

    #[test]
    fn test_compare_issues() {
        let left = vec![InferIssue {
            bug_type: "NPE".into(),
            qualifier: "".into(),
            file: "a.c".into(),
            line: 5,
            procedure: "f".into(),
        }];
        let right = vec![
            InferIssue {
                bug_type: "NPE".into(),
                qualifier: "".into(),
                file: "a.c".into(),
                line: 5,
                procedure: "f".into(),
            },
            InferIssue {
                bug_type: "LEAK".into(),
                qualifier: "".into(),
                file: "a.c".into(),
                line: 10,
                procedure: "g".into(),
            },
        ];

        let cmp = compare_issues(&left, &right);
        assert_eq!(cmp.len(), 2);
        // NPE at line 5: in both
        assert_eq!(cmp[&("a.c".into(), 5, "NPE".into())], (true, true));
        // LEAK at line 10: only in right
        assert_eq!(cmp[&("a.c".into(), 10, "LEAK".into())], (false, true));
    }
}
