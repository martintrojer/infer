// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Load `.sil` test fixture files from disk.
//!
//! Test fixtures live in `infer-rs/test-data/` and are organized by category:
//!
//! ```text
//! test-data/
//!   sil/           — .sil files from infer/tests/codetoanalyze/sil/
//!   pulse/         — Pulse-specific test .sil files
//!   liveness/      — Liveness-specific test .sil files
//! ```

use std::path::{Path, PathBuf};

/// Returns the path to the `test-data/` directory at the workspace root.
///
/// This is `infer-rs/test-data/` relative to any crate in the workspace.
pub fn test_data_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/test-harness/ → infer-rs/
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("could not find workspace root");
    workspace_root.join("test-data")
}

/// Check if the OCaml infer test directory is available.
///
/// Returns `false` in CI or environments where the OCaml infer tree
/// is not checked out alongside infer-rs.
pub fn has_ocaml_sil_tests() -> bool {
    ocaml_sil_test_dir().exists()
}

/// Skip a test if OCaml SIL test fixtures are not available.
///
/// Use at the start of any test that depends on `ocaml_sil_test_dir()`:
/// ```ignore
/// #[test]
/// fn test_needs_ocaml() {
///     test_harness::fixtures::skip_without_ocaml_sil!();
///     // ... test body
/// }
/// ```
#[macro_export]
macro_rules! skip_without_ocaml_sil {
    () => {
        if !$crate::fixtures::has_ocaml_sil_tests() {
            eprintln!("SKIPPED: OCaml SIL test fixtures not available");
            return;
        }
    };
}

/// Returns the path to the OCaml infer's test `.sil` files.
///
/// This is `infer/tests/codetoanalyze/sil/` relative to the repo root.
pub fn ocaml_sil_test_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    // crates/test-harness/ → infer-rs/ → (repo root)
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("could not find repo root");
    repo_root.join("infer/tests/codetoanalyze/sil")
}

/// Returns the path to the OCaml infer's C test directory.
///
/// This is `infer/tests/codetoanalyze/c/` relative to the repo root.
pub fn ocaml_c_test_dir() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("could not find repo root");
    repo_root.join("infer/tests/codetoanalyze/c")
}

/// List all `.sil` files in a directory (non-recursive).
pub fn list_sil_files(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("failed to read directory")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sil") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    files.sort();
    files
}

/// Load a `.sil` fixture file from the `test-data/` directory.
///
/// `relative_path` is relative to `test-data/`, e.g. `"sil/pulse/basic.sil"`.
///
/// Panics if the file does not exist.
pub fn load_fixture(relative_path: &str) -> String {
    let path = test_data_dir().join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()))
}

/// Load a `.sil` fixture file, returning `None` if it doesn't exist.
pub fn try_load_fixture(relative_path: &str) -> Option<String> {
    let path = test_data_dir().join(relative_path);
    std::fs::read_to_string(&path).ok()
}

/// An expected issue from OCaml's `issues.exp` file.
#[derive(Clone, Debug)]
pub struct ExpectedIssue {
    pub file: String,
    pub procedure: String,
    pub line: u32,
    pub issue_type: String,
}

/// Parse an OCaml `issues.exp` file.
///
/// Format: `file, procedure, line, issue_type, bucket, severity, [trace]`
pub fn parse_issues_exp(path: &Path) -> Vec<ExpectedIssue> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(7, ", ").collect();
            if parts.len() < 4 {
                return None;
            }
            Some(ExpectedIssue {
                file: parts[0].trim().to_string(),
                procedure: parts[1].trim().to_string(),
                line: parts[2].trim().parse().ok()?,
                issue_type: parts[3].trim().to_string(),
            })
        })
        .collect()
}

/// Filter expected issues to a single source file (by filename, not full path).
pub fn issues_for_file<'a>(issues: &'a [ExpectedIssue], filename: &str) -> Vec<&'a ExpectedIssue> {
    issues
        .iter()
        .filter(|i| i.file.ends_with(filename))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_dir_resolves() {
        let dir = test_data_dir();
        assert!(dir.ends_with("test-data"));
        assert!(
            dir.parent().unwrap().exists(),
            "parent of test-data should exist: {}",
            dir.parent().unwrap().display()
        );
    }

    #[test]
    fn test_ocaml_sil_test_dir() {
        let dir = ocaml_sil_test_dir();
        assert!(dir.ends_with("infer/tests/codetoanalyze/sil"));
        // Don't assert exists — CI may not have the OCaml tree
    }

    #[test]
    fn test_list_sil_files_nonexistent_dir() {
        let files = list_sil_files(Path::new("/nonexistent/path"));
        assert!(files.is_empty());
    }
}
