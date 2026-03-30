// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Test harness for infer-rs.
//!
//! Provides shared test utilities used across crates:
//!
//! - [`textual_utils`] — Parse `.sil` text and convert to SIL for analysis testing
//! - [`infer_runner`] — Run OCaml `infer` on `.sil` files and collect results
//! - [`fixtures`] — Load `.sil` test fixture files from disk

pub mod fixtures;
pub mod infer_runner;
pub mod summary_compare;
pub mod textual_utils;
