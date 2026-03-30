// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Diagnostics for infer-rs: issue types, severity, and issue reporting.
//!
//! Mirrors the roles of OCaml's `IssueType.ml`, `Errlog.ml`, `Reporting.ml`,
//! and `IssueLog.ml`. Provides the output types that analyses produce.

pub mod issue;
pub mod issue_type;
