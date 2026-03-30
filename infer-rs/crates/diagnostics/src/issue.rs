// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Issue representation and issue log.
//!
//! An `Issue` is a single reported finding. An `IssueLog` collects issues
//! from a checker run and supports serialization to JSON (matching OCaml's
//! `report.json` format for comparison).

use serde::{Deserialize, Serialize};

use sil::location::Location;

use crate::issue_type::IssueType;

/// A single reported issue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// The issue type (DEAD_STORE, NULL_DEREFERENCE, etc.).
    pub issue_type: IssueType,
    /// Human-readable description.
    pub qualifier: String,
    /// Source file where the issue was found.
    pub file: String,
    /// Line number.
    pub line: u32,
    /// Column number.
    pub column: u32,
    /// Procedure where the issue was found.
    pub procedure: String,
    /// Short trace description (e.g. "Write of unused value (type `int`)").
    pub trace: String,
}

/// A collection of issues from one or more checker runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IssueLog {
    pub issues: Vec<Issue>,
}

impl IssueLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&mut self, issue: Issue) {
        self.issues.push(issue);
    }

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn len(&self) -> usize {
        self.issues.len()
    }

    /// Merge another issue log into this one.
    pub fn merge(&mut self, other: IssueLog) {
        self.issues.extend(other.issues);
    }

    /// Sort issues by (file, line, issue_type) for deterministic output.
    pub fn sort(&mut self) {
        self.issues.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.issue_type.id.cmp(&b.issue_type.id))
        });
    }

    /// Serialize to JSON matching OCaml's `report.json` format.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.issues).unwrap_or_else(|_| "[]".to_string())
    }

    /// Format as `issues.exp` lines for comparison with OCaml test expectations.
    ///
    /// Format: `file, procedure, line, ISSUE_TYPE, no_bucket, SEVERITY, [trace]`
    pub fn to_issues_exp(&self) -> String {
        let mut sorted = self.issues.clone();
        sorted.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.procedure.cmp(&b.procedure))
                .then(a.line.cmp(&b.line))
                .then(a.issue_type.id.cmp(&b.issue_type.id))
        });
        sorted
            .iter()
            .map(|i| {
                format!(
                    "{}, {}, {}, {}, no_bucket, {}, [{}]",
                    i.file, i.procedure, i.line, i.issue_type.id, i.issue_type.severity, i.trace
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Create an issue from a SIL location.
    pub fn issue_at(
        issue_type: IssueType,
        qualifier: String,
        trace: String,
        loc: &Location,
        procedure: &str,
    ) -> Issue {
        Issue {
            issue_type,
            qualifier,
            file: format!("{}", loc.file),
            line: loc.line as u32,
            column: loc.col as u32,
            procedure: procedure.to_string(),
            trace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_type::IssueType;

    #[test]
    fn test_issue_log_basic() {
        let mut log = IssueLog::new();
        assert!(log.is_empty());

        log.report(Issue {
            issue_type: IssueType::dead_store(),
            qualifier: "unused value".into(),
            file: "test.c".into(),
            line: 10,
            column: 5,
            procedure: "foo".into(),
            trace: "Write of unused value (type `int`)".into(),
        });

        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
    }

    #[test]
    fn test_issues_exp_format() {
        let mut log = IssueLog::new();
        log.report(Issue {
            issue_type: IssueType::dead_store(),
            qualifier: "unused".into(),
            file: "test.c".into(),
            line: 5,
            column: 3,
            procedure: "bar".into(),
            trace: "Write of unused value (type `int`)".into(),
        });

        let exp = log.to_issues_exp();
        assert!(exp.contains("test.c, bar, 5, DEAD_STORE, no_bucket, ERROR"));
    }

    #[test]
    fn test_issue_log_sort() {
        let mut log = IssueLog::new();
        log.report(Issue {
            issue_type: IssueType::dead_store(),
            qualifier: "".into(),
            file: "b.c".into(),
            line: 10,
            column: 0,
            procedure: "f".into(),
            trace: "".into(),
        });
        log.report(Issue {
            issue_type: IssueType::dead_store(),
            qualifier: "".into(),
            file: "a.c".into(),
            line: 5,
            column: 0,
            procedure: "g".into(),
            trace: "".into(),
        });

        log.sort();
        assert_eq!(log.issues[0].file, "a.c");
        assert_eq!(log.issues[1].file, "b.c");
    }
}
