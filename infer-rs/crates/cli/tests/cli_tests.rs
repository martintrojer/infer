// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Integration tests for the infer-rs CLI binary.
//!
//! These tests invoke the actual binary and check its output.

use diagnostics::issue_type::IssueTypeId;
use std::path::{Path, PathBuf};
use std::process::Command;

fn infer_rs_bin() -> PathBuf {
    // cargo test builds the binary in the same target dir
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove deps/
    path.push("infer-rs");
    path
}

fn test_data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test-data")
}

fn ocaml_sil_dir() -> PathBuf {
    test_harness::fixtures::ocaml_sil_test_dir()
}

fn run_infer_rs(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(infer_rs_bin())
        .args(args)
        .output()
        .expect("failed to run infer-rs");
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (code, stdout, stderr)
}

#[test]
fn test_help() {
    let (code, stdout, _stderr) = run_infer_rs(&["--help"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("infer-rs") || stdout.contains("Usage"),
        "help output should contain program name or usage: {stdout}"
    );
}

#[test]
fn test_no_args_exits_with_error() {
    let (code, _stdout, stderr) = run_infer_rs(&[]);
    assert_ne!(code, 0, "no args should fail");
    assert!(
        stderr.contains("no .sil files")
            || stderr.contains("no capture.db")
            || stderr.contains("error"),
        "should mention missing files or capture.db: {stderr}"
    );
}

#[test]
fn test_nonexistent_file() {
    let (code, _stdout, stderr) = run_infer_rs(&["nonexistent.sil"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("failed to read") || stderr.contains("error"),
        "should report read error: {stderr}"
    );
}

#[test]
fn test_liveness_on_c_fixture() {
    let fixture = test_data_dir().join("c-liveness/dead_stores_simple.sil");
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let tmp_dir = TempDir::new();
    let (code, stdout, stderr) = run_infer_rs(&[
        "--liveness-only",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    // Should find dead stores
    assert_eq!(code, 2, "should exit 2 when issues found. stderr: {stderr}");
    assert!(
        stdout.contains(IssueTypeId::DeadStore.id()),
        "should report DEAD_STORE: {stdout}"
    );

    // report.json should exist
    let report = tmp_dir.join("report.json");
    assert!(report.exists(), "report.json should be created");
    let content = std::fs::read_to_string(&report).unwrap();
    assert!(
        content.contains(IssueTypeId::DeadStore.id()),
        "report.json should contain DEAD_STORE"
    );
}

#[test]
fn test_pulse_on_safe_sil() {
    test_harness::skip_without_ocaml_sil!();
    let fixture = ocaml_sil_dir().join("pulse/basic.sil");

    let tmp_dir = TempDir::new();
    let (code, _stdout, stderr) = run_infer_rs(&[
        "--pulse-only",
        "-q",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    // basic.sil has taint flow tests — Pulse finds no NPEs in it.
    // Exit 0 = no issues found (correct for this file).
    assert!(
        code == 0,
        "safe file should exit 0 (no issues). code={code} stderr: {stderr}"
    );

    let report = tmp_dir.join("report.json");
    assert!(report.exists(), "report.json should be created");
}

#[test]
fn test_multiple_files() {
    let null_deref = test_data_dir().join("pulse/null_deref.sil");
    let safe = test_data_dir().join("pulse/basic_safe.sil");
    let dead_store = test_data_dir().join("c-liveness/dead_stores_simple.sil");

    let tmp_dir = TempDir::new();
    let (code, stdout, stderr) = run_infer_rs(&[
        "-o",
        tmp_dir.to_str().unwrap(),
        "--capture-textual",
        null_deref.to_str().unwrap(),
        "--capture-textual",
        safe.to_str().unwrap(),
        "--capture-textual",
        dead_store.to_str().unwrap(),
    ]);

    assert_eq!(
        code, 2,
        "multiple-file run should surface issues. stderr: {stderr}"
    );
    assert!(
        stdout.contains(IssueTypeId::NullptrDereference.id()),
        "should report NULL_DEREFERENCE from null_deref.sil: {stdout}"
    );
    assert!(
        stdout.contains(IssueTypeId::DeadStore.id()),
        "should report DEAD_STORE from dead_stores_simple.sil: {stdout}"
    );
    assert!(
        stderr.contains("across 3 file(s)"),
        "summary should report all analyzed files: {stderr}"
    );

    let report = tmp_dir.join("report.json");
    assert!(report.exists(), "report.json should be created");
    let content = std::fs::read_to_string(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let issues = parsed
        .as_array()
        .expect("report.json should be a JSON array");
    assert!(
        issues
            .iter()
            .any(|issue| { issue["issue_type"]["id"] == IssueTypeId::NullptrDereference.id() }),
        "report.json should include NULL_DEREFERENCE: {content}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue["issue_type"]["id"] == IssueTypeId::DeadStore.id()),
        "report.json should include DEAD_STORE: {content}"
    );
}

#[test]
fn test_multiple_files_unify_cross_file_pulse_analysis() {
    let inputs = TempDir::new();
    let callee = inputs.join("callee.sil");
    let caller = inputs.join("caller.sil");

    std::fs::write(
        &callee,
        r#".source_language = "java"

type cell = { value:int }

define .static callee_return_null(): *cell {
  #entry:
    ret null
}
"#,
    )
    .unwrap();

    std::fs::write(
        &caller,
        r#".source_language = "java"

type cell = { value:int }

declare .static callee_return_null(): *cell

define .static caller_bad(): void {
  #entry:
    n0 = callee_return_null()
    n1: int = load n0.cell.value
    ret null
}
"#,
    )
    .unwrap();

    let out_dir = TempDir::new();
    let (code, stdout, stderr) = run_infer_rs(&[
        "--pulse-only",
        "-o",
        out_dir.to_str().unwrap(),
        caller.to_str().unwrap(),
        callee.to_str().unwrap(),
    ]);

    assert_eq!(
        code, 2,
        "cross-file unified analysis should surface the caller-side null dereference. stderr: {stderr}"
    );
    assert!(
        stdout.contains(IssueTypeId::NullptrDereference.id()),
        "expected NULL_DEREFERENCE from the cross-file call: {stdout}"
    );
    assert!(
        stdout.contains("caller_bad"),
        "expected the cross-file issue to be reported on caller_bad: {stdout}"
    );
}

#[test]
fn test_quiet_mode() {
    let fixture = test_data_dir().join("c-liveness/dead_stores_simple.sil");
    let tmp_dir = TempDir::new();

    let (_, _, stderr_normal) = run_infer_rs(&[
        "--liveness-only",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    let tmp_dir2 = TempDir::new();
    let (_, _, stderr_quiet) = run_infer_rs(&[
        "--liveness-only",
        "-q",
        "-o",
        tmp_dir2.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    // Normal mode should have progress output
    assert!(
        stderr_normal.contains("Analyzing") || stderr_normal.contains("Found"),
        "normal mode should print progress: {stderr_normal}"
    );
    // Quiet mode should suppress progress output
    assert!(
        !stderr_quiet.contains("Analyzing"),
        "quiet mode should suppress 'Analyzing' progress: {stderr_quiet}"
    );
    assert!(
        !stderr_quiet.contains("Found"),
        "quiet mode should suppress 'Found' summary: {stderr_quiet}"
    );
}

#[test]
fn test_report_json_valid() {
    let fixture = test_data_dir().join("c-liveness/dead_stores_simple.sil");
    let tmp_dir = TempDir::new();

    run_infer_rs(&[
        "--liveness-only",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    let report = tmp_dir.join("report.json");
    let content = std::fs::read_to_string(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("report.json is not valid JSON: {e}\ncontent: {content}"));
    assert!(parsed.is_array(), "report.json should be a JSON array");
}

#[test]
fn test_pulse_detects_null_deref() {
    let fixture = test_data_dir().join("pulse/null_deref.sil");
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let tmp_dir = TempDir::new();
    let (code, stdout, stderr) = run_infer_rs(&[
        "--pulse-only",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    assert_eq!(
        code, 2,
        "should exit 2 when null deref found. stderr: {stderr}"
    );
    assert!(
        stdout.contains(IssueTypeId::NullptrDereference.id()),
        "should report NULL_DEREFERENCE: {stdout}"
    );
    assert!(
        stdout.contains("null_deref_bad"),
        "issue should be in null_deref_bad: {stdout}"
    );
    // safe_store_ok should NOT appear in issues
    assert!(
        !stdout.contains("safe_store_ok"),
        "safe_store_ok should have no issues: {stdout}"
    );

    let report = std::fs::read_to_string(tmp_dir.join("report.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&report).unwrap();
    let issues = parsed
        .as_array()
        .expect("report.json should be a JSON array");
    assert!(
        issues.iter().any(|issue| {
            issue["issue_type"]["id"].as_str() == Some(IssueTypeId::NullptrDereference.id())
                && issue["bug_type"].as_str() == Some(IssueTypeId::NullptrDereference.id())
                && issue["severity"].as_str() == Some("ERROR")
                && issue["bug_trace"].is_array()
                && issue["bug_trace_length"]
                    .as_u64()
                    .is_some_and(|len| len > 0)
                && issue["bug_trace_max_depth"]
                    .as_u64()
                    .is_some_and(|depth| depth > 0)
        }),
        "null-deref reports should include structured bug trace metadata: {report}"
    );
}

#[test]
fn test_pulse_report_issues_for_tests_surfaces_suppressed_reports() {
    let inputs = TempDir::new();
    let fixture = inputs.join("suppressed_null.sil");
    std::fs::write(
        &fixture,
        r#".source_language = "C"

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
"#,
    )
    .unwrap();

    let default_out = TempDir::new();
    let (default_code, _default_stdout, default_stderr) = run_infer_rs(&[
        "--pulse-only",
        "-q",
        "-o",
        default_out.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);
    assert_eq!(
        default_code, 0,
        "suppressed issues should stay out of default reporting. stderr: {default_stderr}"
    );
    let default_report = std::fs::read_to_string(default_out.join("report.json")).unwrap();
    let default_issues: serde_json::Value = serde_json::from_str(&default_report).unwrap();
    assert_eq!(
        default_issues.as_array().map(Vec::len),
        Some(0),
        "default reporting should omit suppressed issues: {default_report}"
    );

    let test_out = TempDir::new();
    let (test_code, _test_stdout, test_stderr) = run_infer_rs(&[
        "--pulse-only",
        "--pulse-report-issues-for-tests",
        "-q",
        "-o",
        test_out.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);
    assert_eq!(
        test_code, 2,
        "test-reporting mode should surface suppressed issues. stderr: {test_stderr}"
    );
    let test_report = std::fs::read_to_string(test_out.join("report.json")).unwrap();
    let test_issues: serde_json::Value = serde_json::from_str(&test_report).unwrap();
    let issues = test_issues
        .as_array()
        .expect("report.json should be a JSON array");
    assert!(
        issues.iter().any(|issue| {
            matches!(
                issue["issue_type"]["id"].as_str(),
                Some(id)
                    if id == IssueTypeId::NullptrDereference.id()
                        || id == IssueTypeId::ComparedToNullAndDereferenced.id()
            )
                && issue["trace"]
                    .as_str()
                    .is_some_and(|trace| trace.starts_with("*** SUPPRESSED ***"))
        }),
        "test-reporting mode should include suppressed null-dereference-family issues: {test_report}"
    );
}

#[test]
fn test_source_override_sets_reported_file() {
    let fixture = test_data_dir().join("pulse/null_deref.sil");
    let tmp_dir = TempDir::new();

    let (code, _stdout, stderr) = run_infer_rs(&[
        "--pulse-only",
        "--source-override",
        "/tmp/override.c",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    assert_eq!(
        code, 2,
        "source-override run should still report issues. stderr: {stderr}"
    );

    let report = tmp_dir.join("report.json");
    let content = std::fs::read_to_string(&report).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let issues = parsed
        .as_array()
        .expect("report.json should be a JSON array");
    assert!(
        issues
            .iter()
            .any(|issue| issue["file"] == "/tmp/override.c"),
        "report.json should use the override path as the file: {content}"
    );
}

#[test]
fn test_pulse_no_issues_on_safe() {
    let fixture = test_data_dir().join("pulse/basic_safe.sil");
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let tmp_dir = TempDir::new();
    let (code, stdout, _stderr) = run_infer_rs(&[
        "--pulse-only",
        "-q",
        "-o",
        tmp_dir.to_str().unwrap(),
        fixture.to_str().unwrap(),
    ]);

    assert_eq!(code, 0, "safe code should exit 0 (no issues)");
    assert!(stdout.is_empty(), "no issues should be printed: {stdout}");
}

#[test]
fn test_both_checkers_together() {
    let null_deref = test_data_dir().join("pulse/null_deref.sil");
    let dead_stores = test_data_dir().join("c-liveness/dead_stores_simple.sil");

    let tmp_dir = TempDir::new();
    let (code, stdout, _stderr) = run_infer_rs(&[
        "-o",
        tmp_dir.to_str().unwrap(),
        null_deref.to_str().unwrap(),
        dead_stores.to_str().unwrap(),
    ]);

    assert_eq!(code, 2, "should find issues from both checkers");
    assert!(
        stdout.contains(IssueTypeId::NullptrDereference.id()),
        "should have Pulse issues: {stdout}"
    );
    assert!(
        stdout.contains(IssueTypeId::DeadStore.id()),
        "should have liveness issues: {stdout}"
    );
}

/// RAII temp directory — cleaned up on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("infer_rs_cli_test_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl std::ops::Deref for TempDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
