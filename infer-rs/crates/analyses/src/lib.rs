// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Intraprocedural analyses for the Infer static analyzer.
//!
//! Maps to OCaml's `infer/src/checkers/` directory. Renamed from "checkers"
//! because these are analysis implementations (liveness, purity, lineage, etc.),
//! not the heavier analysis engines like Pulse or RacerD which get their own crates.

pub mod liveness;
