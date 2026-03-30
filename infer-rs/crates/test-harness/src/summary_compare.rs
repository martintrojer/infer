// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Compare Pulse summaries between OCaml and Rust.
//!
//! Parses OCaml's `all_summaries.json` and extracts key facts for comparison
//! with Rust-side Pulse summaries.

use std::collections::HashMap;
use std::path::Path;

/// Key facts extracted from a Pulse summary for comparison.
#[derive(Clone, Debug)]
pub struct SummaryFacts {
    /// Number of execution paths (pre/post pairs).
    pub num_disjuncts: usize,
    /// Execution state types for each disjunct.
    pub exec_states: Vec<String>,
    /// Whether any disjunct has null/invalid attributes.
    pub has_null_attrs: bool,
    /// Stack variable names in the post-state (first disjunct).
    pub post_stack_vars: Vec<String>,
    /// Is the procedure noreturn (no ContinueProgram disjuncts)?
    pub is_noreturn: bool,
}

/// Parse OCaml's `all_summaries.json` and extract per-procedure facts.
pub fn parse_ocaml_summaries(path: &Path) -> HashMap<String, SummaryFacts> {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let data: serde_json::Value =
        serde_json::from_str(&content).unwrap_or_else(|e| panic!("invalid JSON: {e}"));

    let mut result = HashMap::new();

    let empty = vec![];
    let entries = data.as_array().unwrap_or(&empty);
    for entry in entries {
        let entry = match entry.as_array() {
            Some(a) if a.len() == 2 => a,
            _ => continue,
        };

        let procname = extract_procname(&entry[0]);
        let summaries = match entry[1].as_array() {
            Some(a) => a,
            None => continue,
        };

        for summary_pair in summaries {
            let pair = match summary_pair.as_array() {
                Some(a) if a.len() == 2 => a,
                _ => continue,
            };
            if pair[0].as_str() != Some("pulse") {
                continue;
            }

            let pulse = &pair[1];
            let pre_post_list = pulse
                .pointer("/main/pre_post_list")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut exec_states = Vec::new();
            let mut has_null_attrs = false;
            let mut post_stack_vars = Vec::new();

            for (i, pp) in pre_post_list.iter().enumerate() {
                let pp = match pp.as_array() {
                    Some(a) if !a.is_empty() => a,
                    _ => continue,
                };
                let exec_state = pp[0].as_str().unwrap_or("Unknown").to_string();
                exec_states.push(exec_state.clone());

                // Extract facts from first ContinueProgram disjunct
                if i == 0 || exec_state == "ContinueProgram" {
                    if let Some(detail) = pp.get(1).and_then(|v| v.as_object()) {
                        // Check for null/invalid attrs
                        if let Some(attrs) = detail
                            .get("post")
                            .and_then(|p| p.get("attrs"))
                            .and_then(|a| a.as_array())
                        {
                            for attr_entry in attrs {
                                if let Some(attr_list) = attr_entry
                                    .as_array()
                                    .and_then(|a| a.get(1))
                                    .and_then(|v| v.as_array())
                                {
                                    for attr in attr_list {
                                        if let Some(a) = attr.as_array() {
                                            if a.first().and_then(|v| v.as_str()) == Some("Invalid")
                                            {
                                                has_null_attrs = true;
                                            }
                                        }
                                        if attr.as_str() == Some("Invalid") {
                                            has_null_attrs = true;
                                        }
                                    }
                                }
                            }
                        }

                        // Extract stack var names
                        if post_stack_vars.is_empty() {
                            if let Some(stack) = detail
                                .get("post")
                                .and_then(|p| p.get("stack"))
                                .and_then(|s| s.as_array())
                            {
                                for stack_entry in stack {
                                    if let Some(var_info) = stack_entry
                                        .as_array()
                                        .and_then(|a| a.first())
                                        .and_then(|v| v.as_array())
                                    {
                                        if let Some(name) = var_info
                                            .get(1)
                                            .and_then(|v| v.as_object())
                                            .and_then(|o| o.get("plain"))
                                            .and_then(|v| v.as_str())
                                        {
                                            post_stack_vars.push(name.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let is_noreturn =
                !exec_states.is_empty() && !exec_states.iter().any(|s| s == "ContinueProgram");

            result.insert(
                procname.clone(),
                SummaryFacts {
                    num_disjuncts: pre_post_list.len(),
                    exec_states,
                    has_null_attrs,
                    post_stack_vars,
                    is_noreturn,
                },
            );
        }
    }

    result
}

/// Extract procedure name from the JSON procname structure.
fn extract_procname(value: &serde_json::Value) -> String {
    let arr = match value.as_array() {
        Some(a) if a.len() == 2 => a,
        _ => return "unknown".to_string(),
    };
    match arr[0].as_str() {
        Some("C") => arr[1]
            .get("c_name")
            .and_then(|v| v.as_array())
            .and_then(|a| a.last())
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        _ => format!("{}", arr[1]),
    }
}

/// Compare OCaml and Rust summary facts, returning a report.
pub fn compare_summaries(
    ocaml: &HashMap<String, SummaryFacts>,
    rust: &HashMap<String, SummaryFacts>,
) -> ComparisonReport {
    let mut report = ComparisonReport::default();

    let all_procs: std::collections::BTreeSet<&str> = ocaml
        .keys()
        .chain(rust.keys())
        .map(|s| s.as_str())
        .collect();

    for proc_name in all_procs {
        let o = ocaml.get(proc_name);
        let r = rust.get(proc_name);

        match (o, r) {
            (Some(o), Some(r)) => {
                let mut diffs = Vec::new();

                if o.num_disjuncts != r.num_disjuncts {
                    diffs.push(format!(
                        "disjuncts: ocaml={}, rust={}",
                        o.num_disjuncts, r.num_disjuncts
                    ));
                }

                if o.is_noreturn != r.is_noreturn {
                    diffs.push(format!(
                        "noreturn: ocaml={}, rust={}",
                        o.is_noreturn, r.is_noreturn
                    ));
                }

                if o.has_null_attrs != r.has_null_attrs {
                    diffs.push(format!(
                        "null_attrs: ocaml={}, rust={}",
                        o.has_null_attrs, r.has_null_attrs
                    ));
                }

                if diffs.is_empty() {
                    report.matching += 1;
                } else {
                    report.differences.push(ProcDiff {
                        proc_name: proc_name.to_string(),
                        diffs,
                    });
                }
            }
            (Some(_), None) => {
                report.ocaml_only.push(proc_name.to_string());
            }
            (None, Some(_)) => {
                report.rust_only.push(proc_name.to_string());
            }
            (None, None) => {}
        }
    }

    report
}

/// Result of comparing OCaml and Rust summaries.
#[derive(Default, Debug)]
pub struct ComparisonReport {
    pub matching: usize,
    pub differences: Vec<ProcDiff>,
    pub ocaml_only: Vec<String>,
    pub rust_only: Vec<String>,
}

#[derive(Debug)]
pub struct ProcDiff {
    pub proc_name: String,
    pub diffs: Vec<String>,
}

impl std::fmt::Display for ComparisonReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Matching: {}", self.matching)?;
        if !self.differences.is_empty() {
            writeln!(f, "Differences ({}):", self.differences.len())?;
            for d in &self.differences {
                writeln!(f, "  {}: {}", d.proc_name, d.diffs.join(", "))?;
            }
        }
        if !self.ocaml_only.is_empty() {
            writeln!(f, "OCaml only ({}):", self.ocaml_only.len())?;
            for p in &self.ocaml_only {
                writeln!(f, "  {p}")?;
            }
        }
        if !self.rust_only.is_empty() {
            writeln!(f, "Rust only ({}):", self.rust_only.len())?;
            for p in &self.rust_only {
                writeln!(f, "  {p}")?;
            }
        }
        Ok(())
    }
}

/// Build SummaryFacts from raw data (for Rust-side summaries).
impl SummaryFacts {
    pub fn new(
        num_disjuncts: usize,
        exec_states: Vec<String>,
        has_null_attrs: bool,
        post_stack_vars: Vec<String>,
        is_noreturn: bool,
    ) -> Self {
        Self {
            num_disjuncts,
            exec_states,
            has_null_attrs,
            post_stack_vars,
            is_noreturn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ocaml_summaries() {
        let path = Path::new("/tmp/infer_summary_test/all_summaries.json");
        if !path.exists() {
            eprintln!("skipping: summary file not found");
            return;
        }
        let summaries = parse_ocaml_summaries(path);
        for (name, facts) in &summaries {
            eprintln!(
                "  {name}: {} disjuncts, states={:?}, null={}, noreturn={}",
                facts.num_disjuncts, facts.exec_states, facts.has_null_attrs, facts.is_noreturn
            );
        }
        assert!(!summaries.is_empty());
        assert!(summaries.contains_key("return_null"));
        assert!(summaries["return_null"].has_null_attrs);
    }
}
