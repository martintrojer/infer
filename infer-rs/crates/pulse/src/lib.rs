// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse analysis engine for infer-rs.
//!
//! A separation-logic-based abstract interpreter that detects memory safety
//! bugs (null dereferences, use-after-free, memory leaks) and retain cycles.
//!
//! Mirrors OCaml's `infer/src/pulse/` modules.

pub mod abductive;
pub mod abstract_value;
pub mod access;
pub mod attribute;
pub mod base_attrs;
pub mod base_domain;
pub mod base_memory;
pub mod base_stack;
pub mod checker;
pub mod diagnostic;
pub mod execution_domain;
pub mod formula;
pub mod interproc;
pub mod invalidation;
pub mod models;
pub mod operations;
pub mod pulse_result;
pub mod sat_unsat;
pub mod specialization;
pub mod summary;
pub mod transfer;
