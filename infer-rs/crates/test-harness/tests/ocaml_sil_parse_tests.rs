// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Tests that parse OCaml's `.sil` test files through the Rust pipeline.
//!
//! These tests verify that the Rust Textual parser can handle the same `.sil`
//! files that OCaml infer uses.

use std::path::Path;

use test_harness::fixtures;

/// Parse all `.sil` files in a directory, asserting each one parses without error.
/// Files whose names start with "error" or "syntax_error" are expected to fail
/// and are excluded.
fn assert_all_parse(dir: &Path) {
    let sil_files = fixtures::list_sil_files(dir);
    assert!(
        !sil_files.is_empty(),
        "no .sil files found in {}",
        dir.display()
    );

    let mut failures = Vec::new();
    let mut tested = 0;
    for path in &sil_files {
        let filename = path.file_name().unwrap().to_str().unwrap();
        // Skip intentionally malformed test files
        if filename.starts_with("error") || filename.starts_with("syntax_error") {
            continue;
        }
        tested += 1;
        let src = std::fs::read_to_string(path).unwrap();
        if let Err(e) = textual::parse_module(&src, filename) {
            failures.push(format!("{}: {e}", path.display()));
        }
    }

    assert!(
        tested > 0,
        "no non-error .sil files found in {}",
        dir.display()
    );
    assert!(
        failures.is_empty(),
        "{}/{} files failed to parse:\n  {}",
        failures.len(),
        tested,
        failures.join("\n  ")
    );
}

#[test]
fn test_parse_ocaml_sil_pulse_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("pulse");
    assert_all_parse(&dir);
}

#[test]
fn test_parse_ocaml_sil_verif_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("verif");
    assert_all_parse(&dir);
}
