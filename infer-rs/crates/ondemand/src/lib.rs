// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! On-demand analysis runner for infer-rs.
//!
//! Provides parallel procedure analysis with on-demand inter-procedural support.
//! Replaces OCaml's `backend/ondemand.ml` + `backend/InferAnalyze.ml` with a
//! Rust-native design that uses rayon for work-stealing parallelism and
//! DashMap for lock-free summary caching.
//!
//! Key differences from OCaml:
//! - No SQLite in the analysis loop (optional persistence after completion)
//! - No fork-based parallelism — shared-memory with rayon
//! - No file-system locks — atomic state transitions via DashMap
//! - No global mutable state save/restore — per-analysis context

pub mod callgraph;
pub mod checker;
pub mod runner;
pub mod summary;
