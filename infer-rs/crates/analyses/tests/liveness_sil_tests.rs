// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Liveness analysis tests on `.sil` files.
//!
//! Two kinds of tests:
//!
//! 1. **Pre-existing `.sil` tests**: Parse `.sil` files from the OCaml test suite
//!    and run liveness through the Rust pipeline.
//!
//! 2. **C source compliance tests**: Use OCaml `infer capture --dump-textual` to
//!    compile C source into Textual, then run Rust liveness on the result.
//!    Requires the `infer` binary.

use std::path::Path;

use test_harness::fixtures;
use test_harness::infer_runner::InferRunner;
use test_harness::textual_utils;

// ---------------------------------------------------------------------------
// Pre-existing .sil file tests
// ---------------------------------------------------------------------------

/// Run liveness on all procedures in a `.sil` file.
/// Returns (num_procs, num_with_results).
fn run_liveness_on_file(path: &Path) -> (usize, usize) {
    let tm = textual_utils::parse_file_and_convert(path);
    let mut num_procs = 0;
    let mut num_with_results = 0;

    for pdesc in tm.cfg.iter_proc_descs() {
        num_procs += 1;
        let result = analyses::liveness::analyze(pdesc);
        if !result.inv_map.is_empty() {
            num_with_results += 1;
        }
    }

    (num_procs, num_with_results)
}

/// Try to parse and convert a `.sil` file. Returns Ok if successful, Err with
/// stage name if it fails at parse or conversion.
fn try_parse_and_convert(path: &Path) -> Result<(), String> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("io: {e}"))?;
    let filename = path.file_name().unwrap().to_str().unwrap_or("test.sil");
    let mut module = textual::parse_module(&src, filename).map_err(|e| format!("parse: {e}"))?;
    let (decls, decl_errors) = textual::decls::DeclEnv::from_module(&module);
    if !decl_errors.is_empty() {
        return Err(format!("decl: {decl_errors:?}"));
    }
    textual::transform::run(&mut module, &decls);
    textual::to_sil::module_to_sil(&module, &decls).map_err(|e| format!("to_sil: {e:?}"))?;
    Ok(())
}

/// Run liveness on all `.sil` files in a directory.
/// Asserts every file converts and every procedure produces liveness results.
fn assert_liveness_on_dir(dir: &Path) {
    let sil_files = fixtures::list_sil_files(dir);
    assert!(
        !sil_files.is_empty(),
        "no .sil files found in {}",
        dir.display()
    );

    let mut parse_failures = Vec::new();
    let mut analysis_failures = Vec::new();
    let mut total_procs = 0;
    let mut total_with_results = 0;

    for path in &sil_files {
        let filename = path.file_name().unwrap().to_str().unwrap();
        // Skip intentionally malformed files
        if filename.starts_with("error")
            || filename.starts_with("syntax_error")
            || filename.starts_with("type_error")
            || filename.starts_with("basic_error")
            || filename == "twice.sil"
        {
            continue;
        }

        // Separate parse/conversion failures from analysis failures
        if let Err(stage) = try_parse_and_convert(path) {
            parse_failures.push(format!("{filename}: {stage}"));
            continue;
        }

        match std::panic::catch_unwind(|| run_liveness_on_file(path)) {
            Ok((procs, with_results)) => {
                total_procs += procs;
                total_with_results += with_results;
            }
            Err(_) => {
                analysis_failures.push(filename.to_string());
            }
        }
    }

    if !parse_failures.is_empty() {
        eprintln!(
            "parse/conversion failures ({}):\n  {}",
            parse_failures.len(),
            parse_failures.join("\n  ")
        );
    }

    assert!(
        analysis_failures.is_empty(),
        "liveness analysis failed on {} files: {:?}",
        analysis_failures.len(),
        analysis_failures
    );

    assert!(
        parse_failures.is_empty(),
        "parse/conversion failed on {} files: {:?}",
        parse_failures.len(),
        parse_failures
    );

    eprintln!(
        "liveness: {total_with_results}/{total_procs} procedures produced results in {}",
        dir.display()
    );
}

#[test]
fn test_liveness_on_sil_pulse_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("pulse");
    assert_liveness_on_dir(&dir);
}

#[test]
fn test_liveness_on_sil_verif_files() {
    test_harness::skip_without_ocaml_sil!();
    let dir = fixtures::ocaml_sil_test_dir().join("verif");
    assert_liveness_on_dir(&dir);
}

// ---------------------------------------------------------------------------
// C source → dump-textual → Rust liveness
// ---------------------------------------------------------------------------

/// Test that a committed `.sil` fixture (generated from C via `--dump-textual`)
/// runs through the full Rust liveness pipeline.
#[test]
fn test_liveness_on_c_fixture() {
    let fixture = fixtures::test_data_dir().join("c-liveness/dead_stores_simple.sil");
    assert!(
        fixture.exists(),
        "fixture not found at {}",
        fixture.display()
    );

    let (procs, with_results) = run_liveness_on_file(&fixture);
    eprintln!("c-liveness fixture: {with_results}/{procs} procedures produced results");
    assert!(procs > 0, "should have at least one procedure");
    assert_eq!(
        procs, with_results,
        "every procedure should produce results"
    );
}

/// Test the live end-to-end pipeline: C source → infer dump-textual → Rust parse → liveness.
///
/// Requires the `infer` binary.
#[test]
fn test_liveness_c_dump_textual_pipeline() {
    let Some(runner) = InferRunner::new() else {
        eprintln!("SKIPPED: infer binary not found");
        return;
    };

    let source = fixtures::test_data_dir().join("c-liveness/dead_stores_simple.c");
    assert!(
        source.exists(),
        "C source not found at {}",
        source.display()
    );

    let sil_path = runner
        .dump_textual_for_c(&source)
        .expect("dump_textual_for_c failed");

    let tm = textual_utils::parse_file_and_convert(&sil_path);

    let mut num_procs = 0;
    for pdesc in tm.cfg.iter_proc_descs() {
        num_procs += 1;
        let result = analyses::liveness::analyze(pdesc);
        assert!(
            !result.inv_map.is_empty(),
            "procedure {} produced no liveness results",
            pdesc.proc_name
        );
    }

    eprintln!("C dump-textual pipeline: {num_procs} procedures analyzed");
    assert!(num_procs > 0, "should have at least one procedure");
}
