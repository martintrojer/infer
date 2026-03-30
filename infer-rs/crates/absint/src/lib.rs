// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Abstract interpretation framework for the Infer static analyzer.
//!
//! Mirrors OCaml's `absint/` modules:
//! - `domain` — abstract domain traits and combinators
//! - `transfer` — transfer function traits
//! - `interp` — fixpoint computation engines (RPO, WTO)

pub mod disjunctive;
pub mod domain;
pub mod interp;
pub mod transfer;
pub mod wto;
