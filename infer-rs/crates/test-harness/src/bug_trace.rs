// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Opt-in helpers for asserting structured `bug_trace` events on a single
//! issue inside an infer-rs `report.json`.
//!
//! The default end-to-end fixtures only check the *issue type* and
//! *procedure* of each report, so regressions in interprocedural trace
//! locations (for example, [`pulse::ValueHistory`] changes that misroute
//! frame line numbers) can slip through. This module provides a focused
//! matcher meant to be applied to *individual* fixtures where the trace
//! shape is informative — not a blanket assertion.
//!
//! See the per-task notes in `mu task notes test_trace_step_assertions
//! -w infer-rs` for the design context, including the original
//! `test_e2e_latent_real_bug_trace_matches_ocaml_subset` precedent that
//! this helper generalizes.

use serde_json::Value;

/// One expected entry in an issue's `bug_trace` array.
///
/// Mirrors the subset of fields the harness asserts on:
///
/// * `level` — caller depth (1 = top-most frame).
/// * `line` — 1-indexed source line in the originating file.
/// * `desc` — human-readable description (substring is *not* enough;
///   this is checked for exact equality so wording regressions surface).
#[derive(Debug, Clone, Copy)]
pub struct TraceStep<'a> {
    pub level: u64,
    pub line: u64,
    pub desc: &'a str,
}

impl<'a> TraceStep<'a> {
    pub const fn new(level: u64, line: u64, desc: &'a str) -> Self {
        Self { level, line, desc }
    }
}

/// Assert that the issue identified by `(procedure, bug_type)` in a parsed
/// infer-rs `report.json` has exactly the expected sequence of bug-trace
/// steps.
///
/// On mismatch the actual trace is pretty-printed alongside the expected
/// trace so the failure message is actionable; the full report is also
/// included as a fallback.
///
/// # Panics
///
/// * If `report` is not a JSON array of issues.
/// * If no issue with the given `(procedure, bug_type)` pair is present.
/// * If the matched issue's `bug_trace` does not equal `expected`.
pub fn assert_bug_trace(report: &Value, proc: &str, bug_type: &str, expected: &[TraceStep<'_>]) {
    let issues = report
        .as_array()
        .expect("report.json should be a JSON array");

    let issue = issues
        .iter()
        .find(|issue| {
            issue["procedure"].as_str() == Some(proc)
                && issue["bug_type"].as_str() == Some(bug_type)
        })
        .unwrap_or_else(|| {
            panic!(
                "no {bug_type} issue found for procedure {proc:?}; \
                 issues present: {present:?}",
                present = summarize_issues(issues)
            )
        });

    let bug_trace = issue["bug_trace"]
        .as_array()
        .unwrap_or_else(|| panic!("missing bug_trace for {proc} ({bug_type}): {issue}"));

    let actual: Vec<(u64, u64, String)> = bug_trace
        .iter()
        .map(|entry| {
            (
                entry["level"].as_u64().unwrap_or_default(),
                entry["line_number"].as_u64().unwrap_or_default(),
                entry["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();

    let expected_owned: Vec<(u64, u64, String)> = expected
        .iter()
        .map(|s| (s.level, s.line, s.desc.to_string()))
        .collect();

    if actual != expected_owned {
        panic!(
            "unexpected bug_trace for {proc} ({bug_type})\n\
             expected:\n{expected_pp}\n\
             actual:\n{actual_pp}\n\
             full issue: {issue}",
            expected_pp = pretty_print_trace(&expected_owned),
            actual_pp = pretty_print_trace(&actual),
        );
    }
}

fn pretty_print_trace(trace: &[(u64, u64, String)]) -> String {
    if trace.is_empty() {
        return "  <empty>".to_string();
    }
    trace
        .iter()
        .map(|(level, line, desc)| format!("  (level={level}, line={line}) {desc}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize_issues(issues: &[Value]) -> Vec<(String, String)> {
    issues
        .iter()
        .map(|issue| {
            (
                issue["procedure"].as_str().unwrap_or("?").to_string(),
                issue["bug_type"].as_str().unwrap_or("?").to_string(),
            )
        })
        .collect()
}
