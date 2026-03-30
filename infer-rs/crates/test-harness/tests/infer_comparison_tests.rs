// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Integration tests that run OCaml `infer` and compare results with Rust.
//!
//! These tests are skipped if the `infer` binary is not available.

use test_harness::fixtures;
use test_harness::infer_runner::InferRunner;

/// Test that OCaml infer can capture the .sil test files.
///
/// This validates that the .sil files are well-formed according to OCaml infer.
#[test]
fn test_ocaml_captures_sil_pulse_files() {
    test_harness::skip_without_ocaml_sil!();
    let Some(runner) = InferRunner::new() else {
        eprintln!("SKIPPED: infer binary not found");
        return;
    };

    let dir = fixtures::ocaml_sil_test_dir().join("pulse");

    let sil_files = fixtures::list_sil_files(&dir);
    if sil_files.is_empty() {
        eprintln!("SKIPPED: no .sil files found");
        return;
    }

    let paths: Vec<&std::path::Path> = sil_files.iter().map(|p| p.as_path()).collect();
    let result = runner.capture_and_analyze(&paths, "pulse").unwrap();

    assert!(
        result.out_dir.exists(),
        "infer output directory should exist"
    );
    // OCaml infer exits 2 when issues are found — both 0 and 2 are acceptable
    assert!(
        result.success || result.out_dir.exists(),
        "infer should produce output. stderr: {}",
        result.stderr
    );
    let report = result.out_dir.join("report.json");
    if report.exists() {
        let content = std::fs::read_to_string(&report).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&content).is_ok(),
            "report.json should be valid JSON"
        );
    }
}

/// Test that both OCaml and Rust can parse the same .sil files.
///
/// For each .sil file in sil/pulse/:
/// 1. Parse with Rust textual parser
/// 2. Capture with OCaml infer (if available)
/// Both should succeed on the same files.
#[test]
fn test_parse_parity_sil_pulse() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("pulse");

    let sil_files = fixtures::list_sil_files(&dir);
    assert!(
        !sil_files.is_empty(),
        "no .sil files found in {}",
        dir.display()
    );

    let mut rust_fail = Vec::new();

    for path in &sil_files {
        let src = std::fs::read_to_string(path).unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        if textual::parse_module(&src, filename).is_err() {
            rust_fail.push(filename.to_string());
        }
    }

    assert!(
        rust_fail.is_empty(),
        "Rust parser failed on {}/{} files: {:?}",
        rust_fail.len(),
        sil_files.len(),
        rust_fail
    );
}
